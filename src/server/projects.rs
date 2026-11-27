use std::{
    collections::{HashMap, VecDeque},
    fs,
    io::{self, BufRead, BufReader},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use axum::{
    Json, Router,
    extract::{Path as RoutePath, State},
    http::StatusCode,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{benchmark, context, run::CHAT_THINKING_LIMIT};

use super::App;

mod folder_picker;
pub(super) mod queue;
pub(super) mod write_scope;

type HttpError = (StatusCode, String);
type Result<T> = std::result::Result<T, HttpError>;

pub(super) struct Store {
    directory: PathBuf,
    lock: Mutex<()>,
}

#[derive(Default, Deserialize, Serialize)]
pub(super) struct Catalog {
    next_id: u64,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    user_stops: HashMap<u64, Vec<u64>>,
    pub(super) projects: Vec<Project>,
}

#[derive(Clone, Deserialize, Serialize)]
pub(super) struct Project {
    pub(super) id: u64,
    pub(super) name: String,
    pub(super) folders: Vec<PathBuf>,
    pub(super) sessions: Vec<Session>,
    #[serde(default)]
    archived: bool,
}

#[derive(Clone, Deserialize, Serialize)]
pub(super) struct ChatGroup {
    id: u64,
    name: String,
}

#[derive(Clone, Deserialize, Serialize)]
pub(super) struct Session {
    pub(super) id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    group: Option<ChatGroup>,
    name: String,
    folder: PathBuf,
    #[serde(default)]
    archived: bool,
    #[serde(default)]
    mode: ChatMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    job: Option<queue::Job>,
}

#[derive(Clone, Copy, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ChatMode {
    Chat,
    #[default]
    Work,
}

impl ChatMode {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Work => "work",
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SaveProject {
    id: Option<u64>,
    name: String,
    folders: Vec<PathBuf>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct NewSession {
    project_id: u64,
    group_id: Option<u64>,
    group_name: Option<String>,
    name: String,
    #[serde(default)]
    mode: ChatMode,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SessionSettings {
    project_id: u64,
    project_session_id: u64,
    name: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SessionKey {
    pub(super) project_id: u64,
    pub(super) project_session_id: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SetSessionMode {
    project_id: u64,
    project_session_id: u64,
    mode: ChatMode,
}

#[derive(Serialize)]
pub(super) struct SessionMode {
    project_id: u64,
    project_session_id: u64,
    mode: ChatMode,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Archive {
    project_id: u64,
    project_session_id: Option<u64>,
    group_id: Option<u64>,
    archived: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GroupSettings {
    project_id: u64,
    group_id: u64,
    name: String,
}

#[derive(Serialize)]
pub(super) struct Message {
    role: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    timestamp: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool: Option<ToolTrace>,
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    origin: Option<MessageOrigin>,
    #[serde(skip_serializing_if = "Option::is_none")]
    request_id: Option<String>,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum MessageOrigin {
    Dashboard,
    Orchestrator,
    Lyra,
}

impl MessageOrigin {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Dashboard => "dashboard",
            Self::Orchestrator => "orchestrator",
            Self::Lyra => "lyra",
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MessageOriginEntry {
    message_timestamp: u64,
    origin: MessageOrigin,
    request_id: Option<String>,
    model: Option<String>,
}

pub(super) struct Task {
    pub(super) project: Project,
    pub(super) workspace: PathBuf,
    pub(super) folders: Vec<PathBuf>,
    pub(super) session_file: Option<PathBuf>,
    pub(super) mode: Option<ChatMode>,
}

pub(super) fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/projects", get(list).post(save))
        .route("/projects/pick-folder", post(folder_picker::pick))
        .route("/projects/sessions", post(create_session))
        .route("/projects/sessions/settings", post(update_session_settings))
        .route("/projects/sessions/mode", post(update_session_mode))
        .route("/projects/archive", post(archive))
        .route("/projects/groups/settings", post(update_group))
        .route("/projects/{project_id}/sessions/{session_id}", get(history))
        .route("/projects/run", post(super::mcp::run_project_chat))
        .route("/projects/run/stop", post(super::mcp::stop_project_chat))
        .route("/projects/tasks", post(queue::enqueue_http))
        .route("/projects/tasks/cancel", post(queue::cancel_http))
}

async fn list(State(app): State<Arc<App>>) -> Result<Json<Catalog>> {
    work(app, |app| app.projects.catalog_summary())
        .await
        .map(Json)
}

async fn save(
    State(app): State<Arc<App>>,
    Json(input): Json<SaveProject>,
) -> Result<Json<Project>> {
    work(app, |app| save_project(app, input)).await.map(Json)
}

async fn create_session(
    State(app): State<Arc<App>>,
    Json(input): Json<NewSession>,
) -> Result<Json<Session>> {
    work(app, |app| add_session(app, input)).await.map(Json)
}

async fn update_session_settings(
    State(app): State<Arc<App>>,
    Json(input): Json<SessionSettings>,
) -> Result<Json<Session>> {
    work(app, |app| set_session_settings(app, input))
        .await
        .map(Json)
}

async fn update_session_mode(
    State(app): State<Arc<App>>,
    Json(input): Json<SetSessionMode>,
) -> Result<Json<SessionMode>> {
    work(app, |app| set_session_mode(app, input))
        .await
        .map(Json)
}

async fn update_group(State(app): State<Arc<App>>, Json(input): Json<GroupSettings>) -> Result<Json<Project>> {
    work(app, |app| {
        let project = app.projects.rename_group(input)?;
        app.task.send_modify(crate::run::Snapshot::projects_changed);
        Ok(Json(project.summary()))
    }).await
}

async fn archive(State(app): State<Arc<App>>, Json(input): Json<Archive>) -> Result<Json<Project>> {
    work(app, |app| set_archive(app, input)).await.map(Json)
}

async fn history(
    State(app): State<Arc<App>>,
    RoutePath((project_id, project_session_id)): RoutePath<(u64, u64)>,
) -> Result<Json<VecDeque<Message>>> {
    work(app, move |app| {
        app.projects.history(
            SessionKey {
                project_id,
                project_session_id,
            },
            &app.benchmarks,
            true,
        )
    })
    .await
    .map(Json)
}

pub(super) async fn work<T: Send + 'static>(
    app: Arc<App>,
    job: impl FnOnce(&App) -> Result<T> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(move || job(&app))
        .await
        .map_err(storage)?
}

pub(super) fn save_project(app: &App, input: SaveProject) -> Result<Project> {
    let project = app.projects.save(input)?;
    app.task.send_modify(crate::run::Snapshot::projects_changed);
    Ok(project.summary())
}

pub(super) fn add_session(app: &App, input: NewSession) -> Result<Session> {
    let session = app.projects.create_session(input)?;
    app.task.send_modify(crate::run::Snapshot::projects_changed);
    Ok(session.summary())
}

pub(super) fn set_session_settings(app: &App, input: SessionSettings) -> Result<Session> {
    if app.task.borrow().project_chat_active(input.project_session_id) {
        return Err(invalid("Дождитесь завершения ответа перед изменением настроек."));
    }
    let session = app.projects.set_session_settings(input)?;
    app.task.send_modify(crate::run::Snapshot::projects_changed);
    Ok(session.summary())
}

pub(super) fn set_session_mode(app: &App, input: SetSessionMode) -> Result<SessionMode> {
    let mode = app.projects.set_session_mode(input)?;
    app.task.send_modify(crate::run::Snapshot::projects_changed);
    Ok(mode)
}

pub(super) fn set_archive(app: &App, input: Archive) -> Result<Project> {
    if input.archived {
        let catalog = app.projects.list()?;
        let project = catalog.projects.iter().find(|project| project.id == input.project_id)
            .ok_or_else(|| missing("Project"))?;
        let snapshot = app.task.borrow();
        if project.sessions.iter().any(|session| {
            input.project_session_id.is_none_or(|id| session.id == id)
                && input.group_id.is_none_or(|id| session.group.as_ref().is_some_and(|group| group.id == id))
                && snapshot.project_chat_active(session.id)
        }) {
            return Err(invalid("Дождитесь завершения работы чатов перед архивацией."));
        }
    }
    let project = app.projects.set_archive(input)?;
    app.task.send_modify(crate::run::Snapshot::projects_changed);
    Ok(project.summary())
}

impl Store {
    pub(super) fn new(directory: PathBuf) -> Self {
        Self {
            directory,
            lock: Mutex::new(()),
        }
    }

    pub(super) fn list(&self) -> Result<Catalog> {
        match fs::read(self.directory.join("projects.json")) {
            Ok(bytes) => {
                let mut catalog: Catalog = serde_json::from_slice(&bytes).map_err(storage)?;
                for project in &mut catalog.projects {
                    project.restore_legacy_groups();
                }
                Ok(catalog)
            },
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Catalog::default()),
            Err(error) => Err(storage(error)),
        }
    }

    pub(super) fn catalog_summary(&self) -> Result<Catalog> {
        let mut catalog = self.list()?;
        catalog.projects = catalog.projects.into_iter().map(Project::summary).collect();
        Ok(catalog)
    }

    fn change<T>(&self, update: impl FnOnce(&mut Catalog) -> Result<T>) -> Result<T> {
        let _guard = self.lock.lock().expect("project store lock");
        let mut catalog = self.list()?;
        let result = update(&mut catalog)?;
        fs::create_dir_all(&self.directory).map_err(storage)?;
        let file = tempfile::NamedTempFile::new_in(&self.directory).map_err(storage)?;
        serde_json::to_writer(file.as_file(), &catalog).map_err(storage)?;
        file.as_file().sync_all().map_err(storage)?;
        file.persist(self.directory.join("projects.json"))
            .map_err(storage)?;
        Ok(result)
    }

    fn save(&self, input: SaveProject) -> Result<Project> {
        let name = checked_name(input.name)?;
        if input.folders.is_empty() || input.folders.len() > 16 {
            return Err(invalid("Specify between 1 and 16 project folders"));
        }
        let mut folders = Vec::new();
        for folder in input.folders {
            let folder = canonical_folder(&folder)?;
            if !folders.contains(&folder) {
                folders.push(folder);
            }
        }

        self.change(|catalog| {
            if let Some(id) = input.id {
                let project = catalog
                    .projects
                    .iter_mut()
                    .find(|project| project.id == id)
                    .ok_or_else(|| missing("Project"))?;
                project.ensure_active()?;
                if project.sessions.iter().any(|session| session.job.as_ref().is_some_and(queue::Job::pending) && !folders.contains(&session.folder)) {
                    return Err(invalid("Нельзя убрать рабочую папку незавершённой задачи."));
                }
                project.name = name;
                project.folders = folders;
                return Ok(project.clone());
            }
            catalog.next_id += 1;
            let project = Project {
                id: catalog.next_id,
                name,
                folders,
                sessions: Vec::new(),
                archived: false,
            };
            catalog.projects.push(project.clone());
            Ok(project)
        })
    }

    fn create_session(&self, input: NewSession) -> Result<Session> {
        let name = checked_name(input.name)?;
        self.change(|catalog| {
            let project = catalog
                .projects
                .iter_mut()
                .find(|project| project.id == input.project_id)
                .ok_or_else(|| missing("Project"))?;
            project.ensure_active()?;
            let folder = project.workspace(None)?;
            let group = choose_group(project, &mut catalog.next_id, input.group_id, input.group_name)?;
            catalog.next_id += 1;
            let session = Session {
                id: catalog.next_id,
                group,
                name,
                folder,
                archived: false,
                mode: input.mode,
                job: None,
            };
            project.sessions.push(session.clone());
            Ok(session)
        })
    }

    fn set_session_settings(&self, input: SessionSettings) -> Result<Session> {
        self.change(|catalog| {
            let project = catalog
                .projects
                .iter_mut()
                .find(|project| project.id == input.project_id)
                .ok_or_else(|| missing("Project"))?;
            project.ensure_active()?;
            if project.has_pending_task(input.project_session_id) {
                return Err(invalid("Дождитесь завершения задачи перед изменением настроек."));
            }
            let session = project
                .sessions
                .iter_mut()
                .find(|session| session.id == input.project_session_id)
                .ok_or_else(|| missing("Session"))?;
            session.ensure_active()?;
            session.name = checked_name(input.name)?;
            Ok(session.clone())
        })
    }

    pub(super) fn session_mode(&self, key: SessionKey) -> Result<SessionMode> {
        let catalog = self.list()?;
        let project = catalog
            .projects
            .iter()
            .find(|project| project.id == key.project_id)
            .ok_or_else(|| missing("Project"))?;
        Ok(SessionMode {
            project_id: key.project_id,
            project_session_id: key.project_session_id,
            mode: project.session(key.project_session_id)?.mode,
        })
    }

    fn set_session_mode(&self, input: SetSessionMode) -> Result<SessionMode> {
        self.change(|catalog| {
            let project = catalog
                .projects
                .iter_mut()
                .find(|project| project.id == input.project_id)
                .ok_or_else(|| missing("Project"))?;
            project.ensure_active()?;
            if input.mode == ChatMode::Chat && project.has_pending_task(input.project_session_id) {
                return Err((
                    StatusCode::CONFLICT,
                    "Задача в очереди использует Work до завершения или отмены.".into(),
                ));
            }
            let session = project
                .sessions
                .iter_mut()
                .find(|session| session.id == input.project_session_id)
                .ok_or_else(|| missing("Session"))?;
            session.ensure_active()?;
            session.mode = input.mode;
            Ok(SessionMode {
                project_id: input.project_id,
                project_session_id: input.project_session_id,
                mode: session.mode,
            })
        })
    }

    fn rename_group(&self, input: GroupSettings) -> Result<Project> {
        let name = checked_name(input.name)?;
        self.change(|catalog| {
            let project = catalog.projects.iter_mut().find(|project| project.id == input.project_id)
                .ok_or_else(|| missing("Project"))?;
            project.ensure_active()?;
            let mut found = false;
            for session in &mut project.sessions {
                if let Some(group) = &mut session.group
                    && group.id == input.group_id
                {
                    found = true;
                    group.name = name.clone();
                }
            }
            if !found { return Err(missing("Group")); }
            Ok(project.clone())
        })
    }

    fn set_archive(&self, input: Archive) -> Result<Project> {
        self.change(|catalog| {
            let project = catalog
                .projects
                .iter_mut()
                .find(|project| project.id == input.project_id)
                .ok_or_else(|| missing("Project"))?;
            if input.group_id.is_some() && input.project_session_id.is_some() {
                return Err(invalid("Укажите group_id или project_session_id, но не оба."));
            }
            if let Some(id) = input.group_id {
                project.ensure_active()?;
                let members: Vec<_> = project.sessions.iter()
                    .filter(|session| session.group.as_ref().is_some_and(|group| group.id == id))
                    .map(|session| session.id).collect();
                if members.is_empty() { return Err(missing("Group")); }
                if input.archived && members.iter().any(|id| project.has_pending_task(*id)) {
                    return Err(invalid("Остановите задачи группы перед архивацией."));
                }
                for session in &mut project.sessions {
                    if members.contains(&session.id) {
                        session.archived = input.archived;
                    }
                }
            } else if let Some(id) = input.project_session_id {
                project.ensure_active()?;
                if input.archived && project.has_pending_task(id) {
                    return Err(invalid("Остановите задачу перед архивацией."));
                }
                let session = project
                    .sessions
                    .iter_mut()
                    .find(|session| session.id == id)
                    .ok_or_else(|| missing("Session"))?;
                session.archived = input.archived;
            } else {
                if input.archived
                    && project
                        .sessions
                        .iter()
                        .any(|session| session.job.as_ref().is_some_and(queue::Job::pending))
                {
                    return Err(invalid("Остановите задачи проекта перед архивацией."));
                }
                project.archived = input.archived;
            }
            Ok(project.clone())
        })
    }

    pub(super) fn task(
        &self,
        project_id: u64,
        session_id: Option<u64>,
        workspace: Option<&Path>,
    ) -> Result<Task> {
        let catalog = self.list()?;
        let project = catalog
            .projects
            .into_iter()
            .find(|project| project.id == project_id)
            .ok_or_else(|| missing("Project"))?;
        project.ensure_active()?;
        let (workspace, folders, session_file, mode) = if let Some(id) = session_id {
            let session = project.session(id)?;
            session.ensure_active()?;
            // Ordinary chats inherit the current project folders. Queue steps keep
            // their execution root so relative reservations and artifacts remain valid.
            let owner = if session.job.is_some() { Some(session) } else {
                project.sessions.iter().find(|candidate| candidate.job.as_ref().is_some_and(|job| job.chat_session_id == Some(id)))
            };
            let folder = if let Some(owner) = owner.filter(|owner| owner.job.as_ref().is_some_and(queue::Job::pending)) {
                let folder = project.workspace(Some(&owner.folder))?;
                if let Some(requested) = workspace
                    && project.workspace(Some(requested))? != folder
                {
                    return Err(invalid("Use the queued task's workspace"));
                }
                folder
            } else {
                project.workspace(workspace)?
            };
            (
                folder.clone(),
                project.folders_with_primary(&folder),
                Some(self.session_file(id)),
                Some(session.mode),
            )
        } else {
            (
                project.workspace(workspace)?,
                project.folders.clone(),
                None,
                None,
            )
        };
        Ok(Task {
            project,
            workspace,
            folders,
            session_file,
            mode,
        })
    }

    // Record the UI stop independently of Pi's JSONL. Killing a child may prevent
    // it from flushing an assistant message; the cancellation must still survive.
    pub(super) fn record_user_stop(&self, owner_id: u64) -> Result<()> {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
        self.change(|catalog| {
            let session = catalog.projects.iter().find_map(|project| project.session(owner_id).ok())
                .ok_or_else(|| missing("Session"))?;
            let chat_id = session.job.as_ref().and_then(|job| job.chat_session_id).unwrap_or(owner_id);
            for id in std::iter::once(owner_id).chain((chat_id != owner_id).then_some(chat_id)) {
                let stops = catalog.user_stops.entry(id).or_default();
                stops.push(timestamp);
                if stops.len() > 100 { stops.remove(0); }
            }
            Ok(())
        })
    }

    fn session_file(&self, id: u64) -> PathBuf {
        self.directory.join("sessions").join(format!("{id}.jsonl"))
    }

    pub(super) fn history(
        &self,
        key: SessionKey,
        benchmarks: &benchmark::Store,
        dashboard: bool,
    ) -> Result<VecDeque<Message>> {
        let catalog = self.list()?;
        let project = catalog
            .projects
            .iter()
            .find(|project| project.id == key.project_id)
            .ok_or_else(|| missing("Project"))?;
        let session = project.session(key.project_session_id)?;
        let queued_message = session
            .job
            .as_ref()
            .and_then(|job| job.initial_message(session.id));
        let mut stops = catalog.user_stops.get(&key.project_session_id)
            .into_iter().flatten().copied().peekable();
        let status_message = session.job.as_ref().and_then(|job| {
            if dashboard || job.user_cancelled() { job.status_message() } else { None }
        });
        // Running cancellations have an independently persisted timestamped notice.
        let status_message = status_message.filter(|message| {
            !(stops.peek().is_some() && message.text == USER_STOP_TEXT)
        });
        let mut saw_queued_prompt = false;
        let mut messages = VecDeque::new();
        let file = match fs::File::open(self.session_file(key.project_session_id)) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                messages.extend(queued_message);
                for timestamp in stops { push_user_stop(&mut messages, timestamp); }
                messages.extend(status_message);
                return Ok(messages);
            }
            Err(error) => return Err(storage(error)),
        };
        let mut pending_origin = None;
        let mut skill_reads = SkillReads::default();
        let mut turn_model = None;
        let mut model_aliases: Option<HashMap<String, String>> = None;
        for line in BufReader::new(file).lines() {
            let line = line.map_err(storage)?;
            let Ok(entry) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if let Some(timestamp) = entry_timestamp(&entry) {
                while stops.peek().is_some_and(|stopped| *stopped < timestamp) {
                    push_user_stop(&mut messages, stops.next().expect("peeked stop"));
                }
            }
            if entry["type"] == "custom" && entry["customType"] == "lyra-message-origin" {
                pending_origin =
                    serde_json::from_value::<MessageOriginEntry>(entry["data"].clone()).ok();
                continue;
            }
            if dashboard && entry["type"] == "compaction" {
                if messages.len() == 100 {
                    messages.pop_front();
                }
                messages.push_back(Message {
                    tool: None,
                    role: "notice",
                    timestamp: entry_timestamp(&entry),
                    text: "История сжата для модели. Исходная переписка сохранена.".into(),
                    thinking: None,
                    model: None,
                    origin: None,
                    request_id: None,
                });
                continue;
            }
            if entry["type"] != "message" {
                continue;
            }
            let message = &entry["message"];
            if dashboard && let Some(skill) = skill_reads.observe(message) {
                if messages.len() == 100 {
                    messages.pop_front();
                }
                messages.push_back(Message {
                    tool: None,
                    role: "notice",
                    timestamp: entry_timestamp(&entry),
                    text: format!("Использован скилл: {skill}"),
                    thinking: None,
                    model: None,
                    origin: None,
                    request_id: None,
                });
            }
            if dashboard && message["role"] == "toolResult" {
                finish_tool(&mut messages, message, entry_timestamp(&entry));
            }
            let role = match message["role"].as_str() {
                Some("user") => "user",
                Some("assistant") => "assistant",
                _ => continue,
            };
            let metadata = if role == "user" {
                pending_origin.take().filter(|origin| {
                    Some(origin.message_timestamp) == message["timestamp"].as_u64()
                })
            } else {
                None
            };
            if role == "user" {
                if let Some(queued) = &queued_message {
                    saw_queued_prompt |= metadata
                        .as_ref()
                        .is_some_and(|meta| meta.request_id == queued.request_id);
                }
                turn_model = metadata
                    .as_ref()
                    .and_then(|metadata| metadata.model.clone());
            }
            let content = &message["content"];
            let text = content.as_str().map(str::to_owned).unwrap_or_else(|| {
                content
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|part| part["type"] == "text")
                    .filter_map(|part| part["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
            });
            let thinking = (dashboard && role == "assistant")
                .then(|| {
                    content
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter(|part| part["type"] == "thinking")
                        .filter_map(|part| part["thinking"].as_str())
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .filter(|thinking| !thinking.trim().is_empty());
            if !text.is_empty() || thinking.is_some() {
                if messages.len() == 100 {
                    messages.pop_front();
                }
                let model = if role == "assistant" {
                    turn_model.clone().or_else(|| {
                        let model = message["model"].as_str()?;
                        if !context::is_alias(model) {
                            return Some(model.to_owned());
                        }
                        model_aliases
                            .get_or_insert_with(|| {
                                benchmarks
                                    .known_models()
                                    .unwrap_or_default()
                                    .into_iter()
                                    .map(|name| (context::alias(&name), name))
                                    .collect()
                            })
                            .get(context::profile_base(model)?)
                            .cloned()
                    })
                } else {
                    None
                };
                messages.push_back(Message {
                    tool: None,
                    role,
                    timestamp: entry_timestamp(&entry),
                    text: text[..text.floor_char_boundary(24_000)].to_owned(),
                    thinking: thinking.map(|mut text| {
                        text.truncate(text.floor_char_boundary(CHAT_THINKING_LIMIT));
                        text
                    }),
                    model,
                    origin: metadata.as_ref().map(|metadata| metadata.origin),
                    request_id: metadata.and_then(|metadata| metadata.request_id),
                });
            }
            if dashboard && role == "assistant" {
                append_tool_calls(&mut messages, content, entry_timestamp(&entry));
            }
        }
        if !saw_queued_prompt
            && messages.len() < 100
            && let Some(message) = queued_message
        {
            messages.push_front(message);
        }
        for timestamp in stops { push_user_stop(&mut messages, timestamp); }
        if let Some(message) = status_message {
            if messages.len() == 100 {
                messages.pop_front();
            }
            messages.push_back(message);
        }
        Ok(messages)
    }
}

pub(super) const USER_STOP_REASON: &str = "cancelled_by_user";
pub(super) const USER_STOP_TEXT: &str = "Остановлено пользователем. Работа намеренно прервана; не возобновляйте её автоматически без нового запроса пользователя.";

fn push_user_stop(messages: &mut VecDeque<Message>, timestamp: u64) {
    if messages.len() == 100 { messages.pop_front(); }
    // A pending call at the time of the interruption is not a model failure.
    for message in messages.iter_mut() {
        if let Some(tool) = &mut message.tool
            && tool.status == "pending"
        {
            tool.status = "cancelled";
            tool.finished_at = Some(timestamp);
        }
    }
    messages.push_back(Message {
        role: "notice", tool: None, timestamp: Some(timestamp),
        text: USER_STOP_TEXT.into(), thinking: None, model: None,
        origin: None, request_id: None,
    });
}

// Pi messages use Unix milliseconds; session entries also carry an ISO timestamp.
fn entry_timestamp(entry: &Value) -> Option<u64> {
    entry["message"]["timestamp"].as_u64().or_else(|| {
        let timestamp = chrono::DateTime::parse_from_rfc3339(entry["timestamp"].as_str()?).ok()?;
        u64::try_from(timestamp.timestamp_millis()).ok()
    })
}

const TOOL_PREVIEW_LIMIT: usize = 8_000;

#[derive(Serialize)]
struct ToolTrace {
    id: String,
    name: String,
    arguments: String,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    finished_at: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<String>,
}

fn tool_preview(mut text: String) -> String {
    if text.len() > TOOL_PREVIEW_LIMIT {
        text.truncate(text.floor_char_boundary(TOOL_PREVIEW_LIMIT));
        text.push_str("\n… (вывод сокращён)");
    }
    text
}

fn skill_read_name(part: &Value) -> Option<&str> {
    if part["name"] != "read" {
        return None;
    }
    let path = part["arguments"]["path"].as_str()?;
    let mut parts = path.rsplit(['/', '\\']);
    if !parts.next()?.eq_ignore_ascii_case("SKILL.md") {
        return None;
    }
    parts.next().filter(|name| !name.is_empty() && *name != "." && *name != "..")
}

fn append_tool_calls(messages: &mut VecDeque<Message>, content: &Value, timestamp: Option<u64>) {
    for part in content.as_array().into_iter().flatten() {
        if part["type"] != "toolCall" || skill_read_name(part).is_some() {
            continue;
        }
        let (Some(id), Some(name)) = (part["id"].as_str(), part["name"].as_str()) else {
            continue;
        };
        if messages.len() == 100 {
            messages.pop_front();
        }
        messages.push_back(Message {
            role: "tool",
            timestamp,
            tool: Some(ToolTrace {
                id: id.to_owned(),
                name: name.to_owned(),
                arguments: tool_preview(serde_json::to_string_pretty(&part["arguments"]).unwrap_or_default()),
                status: "pending",
                finished_at: None,
                result: None,
            }),
            text: String::new(),
            thinking: None,
            model: None,
            origin: None,
            request_id: None,
        });
    }
}

fn finish_tool(messages: &mut VecDeque<Message>, message: &Value, timestamp: Option<u64>) {
    let (Some(id), Some(name)) = (message["toolCallId"].as_str(), message["toolName"].as_str()) else {
        return;
    };
    let Some(tool) = messages.iter_mut().rev().filter_map(|entry| entry.tool.as_mut())
        .find(|tool| tool.id == id && tool.name == name && tool.status == "pending") else {
        return;
    };
    tool.status = if message["isError"] == true { "error" } else { "ok" };
    let content = &message["content"];
    let text = content.as_str().map(str::to_owned).unwrap_or_else(|| {
        content.as_array().into_iter().flatten().map(|part| {
            match part["type"].as_str() {
                Some("text") => part["text"].as_str().unwrap_or_default(),
                Some("image") => "[изображение]",
                _ => "[нетекстовый результат]",
            }
        }).collect::<Vec<_>>().join("\n")
    });
    tool.result = Some(tool_preview(text));
    tool.finished_at = timestamp;
}

// Pi exposes skill descriptions in its prompt and reads the actual SKILL.md
// through its normal read tool. Match the persisted call to its successful result
// so failed reads and mere mentions never create a dashboard notice.
#[derive(Default)]
struct SkillReads {
    pending: HashMap<String, String>,
}

impl SkillReads {
    fn observe(&mut self, message: &Value) -> Option<String> {
        match message["role"].as_str()? {
            "user" => self.pending.clear(),
            "assistant" => {
                for part in message["content"].as_array().into_iter().flatten() {
                    if part["type"] != "toolCall" || part["name"] != "read" {
                        continue;
                    }
                    let Some(id) = part["id"].as_str() else { continue };
                    if let Some(name) = skill_read_name(part) {
                        self.pending.insert(id.to_owned(), name.to_owned());
                    }
                }
            }
            "toolResult" => {
                let skill = self.pending.remove(message["toolCallId"].as_str()?)?;
                if message["toolName"] == "read" && message["isError"] == false {
                    return Some(skill);
                }
            }
            _ => {}
        }
        None
    }
}

impl Project {
    fn summary(mut self) -> Self {
        self.sessions = self.sessions.into_iter().map(Session::summary).collect();
        self
    }

    pub(super) fn has_pending_task(&self, id: u64) -> bool {
        self.sessions.iter().any(|session| {
            session.job.as_ref().is_some_and(|job| {
                job.pending() && (session.id == id || job.chat_session_id == Some(id))
            })
        })
    }

    fn ensure_active(&self) -> Result<()> {
        if self.archived {
            return Err((
                StatusCode::CONFLICT,
                "Проект в архиве. Восстановите его, чтобы продолжить.".into(),
            ));
        }
        Ok(())
    }

    fn session(&self, id: u64) -> Result<&Session> {
        self.sessions
            .iter()
            .find(|session| session.id == id)
            .ok_or_else(|| missing("Session"))
    }

    fn folders_with_primary(&self, primary: &Path) -> Vec<PathBuf> {
        std::iter::once(primary.to_path_buf())
            .chain(self.folders.iter().filter(|path| *path != primary).cloned())
            .collect()
    }

    fn workspace(&self, requested: Option<&Path>) -> Result<PathBuf> {
        let folder = requested
            .or_else(|| self.folders.first().map(PathBuf::as_path))
            .ok_or_else(|| invalid("Project has no registered folders"))?;
        let folder = canonical_folder(folder)?;
        if self.folders.contains(&folder) {
            Ok(folder)
        } else {
            Err(invalid("Choose one of the project's registered folders"))
        }
    }
}

impl Session {
    fn summary(mut self) -> Self {
        if let Some(job) = &mut self.job {
            job.omit_payload();
        }
        self
    }

    fn ensure_active(&self) -> Result<()> {
        if self.archived {
            return Err((
                StatusCode::CONFLICT,
                "Чат в архиве. Восстановите его, чтобы продолжить.".into(),
            ));
        }
        Ok(())
    }
}

fn canonical_folder(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute() {
        return Err(invalid("Project folders must be absolute paths"));
    }
    let folder =
        fs::canonicalize(path).map_err(|error| invalid(format!("{}: {error}", path.display())))?;
    if !folder.is_dir() {
        return Err(invalid(format!("{} is not a directory", path.display())));
    }
    // Rust uses verbatim paths on Windows. Pi and the dashboard use normal paths.
    #[cfg(windows)]
    {
        use std::path::{Component, Prefix};
        if let Some(Component::Prefix(prefix)) = folder.components().next() {
            let base = match prefix.kind() {
                Prefix::VerbatimDisk(disk) => {
                    Some(PathBuf::from(format!("{}:\\", char::from(disk))))
                }
                Prefix::VerbatimUNC(server, share) => {
                    Some(Path::new(r"\\").join(server).join(share))
                }
                _ => None,
            };
            if let Some(mut base) = base {
                base.extend(folder.components().skip(2));
                return Ok(base);
            }
        }
    }
    Ok(folder)
}

fn choose_group(
    project: &Project,
    next_id: &mut u64,
    id: Option<u64>,
    name: Option<String>,
) -> Result<Option<ChatGroup>> {
    match (id, name) {
        (Some(_), Some(_)) => Err(invalid("Укажите group_id или group_name, но не оба.")),
        (Some(id), None) => project.sessions.iter()
            .filter(|session| !session.archived)
            .filter_map(|session| session.group.as_ref())
            .find(|group| group.id == id)
            .cloned()
            .map(Some)
            .ok_or_else(|| missing("Group")),
        (None, Some(name)) => {
            let name = checked_name(name)?;
            *next_id += 1;
            Ok(Some(ChatGroup { id: *next_id, name }))
        }
        (None, None) => Ok(None),
    }
}

fn checked_name(name: String) -> Result<String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 120 {
        return Err(invalid("Enter a name of between 1 and 120 characters"));
    }
    Ok(name.into())
}

fn invalid(error: impl Into<String>) -> HttpError {
    (StatusCode::BAD_REQUEST, error.into())
}

fn missing(label: &str) -> HttpError {
    (StatusCode::NOT_FOUND, format!("{label} not found"))
}

fn storage(error: impl std::fmt::Display) -> HttpError {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        format!("Project storage error: {error}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_chats_inherit_current_project_folders() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first");
        let second = dir.path().join("second");
        fs::create_dir(&first).unwrap();
        fs::create_dir(&second).unwrap();
        let store = Store::new(dir.path().join("data"));
        let project = store.save(SaveProject {
            id: None, name: "Project".into(), folders: vec![first.clone(), second.clone()],
        }).unwrap();
        let chat = store.create_session(serde_json::from_value(serde_json::json!({
            "project_id":project.id, "name":"Chat"
        })).unwrap()).unwrap();
        let second = canonical_folder(&second).unwrap();
        let first = canonical_folder(&first).unwrap();
        let task = store.task(project.id, Some(chat.id), Some(&second)).unwrap();
        assert_eq!(task.workspace, second);
        assert_eq!(task.folders, vec![second.clone(), first.clone()]);
        store.save(SaveProject {
            id: Some(project.id), name: project.name, folders: vec![second.clone()],
        }).unwrap();
        let task = store.task(project.id, Some(chat.id), None).unwrap();
        assert_eq!(task.workspace, second);
        assert_eq!(task.folders, vec![second]);
        assert!(store.task(project.id, Some(chat.id), Some(&first)).is_err());
    }

    #[test]
    fn group_settings_and_archive_apply_to_all_members_only() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path().join("data"));
        let project = store.save(SaveProject {
            id: None, name: "Project".into(),
            folders: vec![dir.path().to_path_buf()],
        }).unwrap();
        let create = |name: &str, group: serde_json::Value| {
            let mut input = serde_json::json!({"project_id": project.id, "name": name});
            input.as_object_mut().unwrap().extend(group.as_object().unwrap().clone());
            store.create_session(serde_json::from_value(input).unwrap()).unwrap()
        };
        let first = create("First", serde_json::json!({"group_name":"Feature"}));
        let group_id = first.group.as_ref().unwrap().id;
        let second = create("Second", serde_json::json!({"group_id":group_id}));
        let ordinary = create("Ordinary", serde_json::json!({}));
        let updated = store.rename_group(GroupSettings {
            project_id: project.id, group_id, name: "Renamed".into(),
        }).unwrap();
        for id in [first.id, second.id] {
            assert_eq!(updated.session(id).unwrap().group.as_ref().unwrap().name, "Renamed");
        }
        let archive = |archived| Archive {
            project_id: project.id, project_session_id: None, group_id: Some(group_id), archived,
        };
        let archived = store.set_archive(archive(true)).unwrap();
        assert!(archived.session(first.id).unwrap().archived);
        assert!(archived.session(second.id).unwrap().archived);
        assert!(!archived.session(ordinary.id).unwrap().archived);
        let restored = store.set_archive(archive(false)).unwrap();
        assert!(!restored.session(first.id).unwrap().archived);
        assert!(!restored.session(second.id).unwrap().archived);
        let renamed = store.set_session_settings(SessionSettings {
            project_id: project.id, project_session_id: second.id,
            name: "New chat name".into(),
        }).unwrap();
        assert_eq!(renamed.name, "New chat name");
        assert_eq!(renamed.group.unwrap().id, group_id);
    }

    #[test]
    fn history_timestamp_uses_pi_message_or_saved_entry_time() {
        assert_eq!(entry_timestamp(&serde_json::json!({"message":{"timestamp":1234}})), Some(1234));
        assert_eq!(entry_timestamp(&serde_json::json!({"timestamp":"1970-01-01T00:00:02Z"})), Some(2000));
        assert_eq!(entry_timestamp(&serde_json::json!({})), None);
    }

    #[test]
    fn tool_calls_match_results_by_id_and_hide_skill_reads() {
        let mut messages = VecDeque::new();
        let calls = serde_json::json!([
            {"type":"toolCall","id":"a","name":"read","arguments":{"path":"src/main.rs"}},
            {"type":"toolCall","id":"b","name":"run_command","arguments":{"command":"cargo test"}},
            {"type":"toolCall","id":"c","name":"read","arguments":{"path":"skills/review/SKILL.md"}}
        ]);
        append_tool_calls(&mut messages, &calls, Some(1000));
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].tool.as_ref().unwrap().status, "pending");
        assert_eq!(messages[0].timestamp, Some(1000));
        finish_tool(&mut messages, &serde_json::json!({
            "toolCallId":"b","toolName":"run_command","isError":true,
            "content":[{"type":"text","text":"failed"}]
        }), Some(2000));
        assert_eq!(messages[1].tool.as_ref().unwrap().status, "error");
        assert_eq!(messages[1].tool.as_ref().unwrap().result.as_deref(), Some("failed"));
        finish_tool(&mut messages, &serde_json::json!({
            "toolCallId":"a","toolName":"read","isError":false,
            "content":[{"type":"text","text":"code"}]
        }), Some(2000));
        assert_eq!(messages[0].tool.as_ref().unwrap().status, "ok");
        assert_eq!(messages[0].tool.as_ref().unwrap().finished_at, Some(2000));
        assert_eq!(messages[0].tool.as_ref().unwrap().result.as_deref(), Some("code"));
    }

    #[test]
    fn skill_notices_follow_successful_reads_only() {
        let mut reads = SkillReads::default();
        let call = serde_json::json!({"role":"assistant","content":[
            {"type":"text","text":"Use a skill"},
            {"type":"toolCall","id":"a","name":"read","arguments":{"path":r"C:\skills\ast-index\SKILL.md"}},
            {"type":"toolCall","id":"b","name":"read","arguments":{"path":"/skills/review/SKILL.md"}},
            {"type":"toolCall","id":"c","name":"read","arguments":{"path":"/skills/review/reference.md"}}
        ]});
        assert_eq!(reads.observe(&call), None);
        let result = |id: &str, error: bool| serde_json::json!({
            "role":"toolResult","toolCallId":id,"toolName":"read","isError":error
        });
        assert_eq!(reads.observe(&result("b", true)), None);
        assert_eq!(reads.observe(&result("c", false)), None);
        assert_eq!(reads.observe(&result("a", false)).as_deref(), Some("ast-index"));
        assert_eq!(reads.observe(&result("a", false)), None);
    }

    #[test]
    fn project_chat_modes_default_to_work_and_are_saved_per_session() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data");
        let store = Store::new(path.clone());
        let project = store
            .save(SaveProject {
                id: None,
                name: "Project".into(),
                folders: vec![dir.path().to_path_buf()],
            })
            .unwrap();
        let create = |name: &str| {
            let input = serde_json::from_value(serde_json::json!({
                "project_id": project.id,
                "name": name,
            }))
            .unwrap();
            store.create_session(input).unwrap()
        };
        let first = create("First");
        let second = create("Second");
        assert!(first.mode == ChatMode::Work);
        let mut legacy = serde_json::to_value(&first).unwrap();
        legacy.as_object_mut().unwrap().remove("mode");
        assert!(serde_json::from_value::<Session>(legacy).unwrap().mode == ChatMode::Work);

        store
            .set_session_mode(SetSessionMode {
                project_id: project.id,
                project_session_id: first.id,
                mode: ChatMode::Chat,
            })
            .unwrap();
        let reopened = Store::new(path);
        let mode = |session| {
            reopened
                .session_mode(SessionKey {
                    project_id: project.id,
                    project_session_id: session,
                })
                .unwrap()
                .mode
        };
        assert!(mode(first.id) == ChatMode::Chat);
        assert!(mode(second.id) == ChatMode::Work);
    }
}
