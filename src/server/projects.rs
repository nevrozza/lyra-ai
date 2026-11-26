use std::{
    collections::VecDeque,
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

use super::App;

mod folder_picker;

type HttpError = (StatusCode, String);
type Result<T> = std::result::Result<T, HttpError>;

pub(super) struct Store {
    directory: PathBuf,
    lock: Mutex<()>,
}

#[derive(Default, Deserialize, Serialize)]
pub(super) struct Catalog {
    next_id: u64,
    pub(super) projects: Vec<Project>,
}

#[derive(Clone, Deserialize, Serialize)]
pub(super) struct Project {
    pub(super) id: u64,
    pub(super) name: String,
    pub(super) folders: Vec<PathBuf>,
    pub(super) search_skill: Option<String>,
    pub(super) sessions: Vec<Session>,
    #[serde(default)]
    archived: bool,
}

#[derive(Clone, Deserialize, Serialize)]
pub(super) struct Session {
    pub(super) id: u64,
    name: String,
    folder: PathBuf,
    #[serde(default)]
    archived: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SaveProject {
    id: Option<u64>,
    name: String,
    folders: Vec<PathBuf>,
    search_skill: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct NewSession {
    project_id: u64,
    name: String,
    folder: Option<PathBuf>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SessionKey {
    project_id: u64,
    project_session_id: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Archive {
    project_id: u64,
    project_session_id: Option<u64>,
    archived: bool,
}

#[derive(Serialize)]
pub(super) struct Message {
    role: &'static str,
    text: String,
}

pub(super) struct Task {
    pub(super) project: Project,
    pub(super) workspace: PathBuf,
    pub(super) session_file: Option<PathBuf>,
}

pub(super) fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/projects", get(list).post(save))
        .route("/projects/pick-folder", post(folder_picker::pick))
        .route("/projects/sessions", post(create_session))
        .route("/projects/archive", post(archive))
        .route("/projects/{project_id}/sessions/{session_id}", get(history))
        .route("/projects/run", post(super::mcp::run_project_chat))
}

async fn list(State(app): State<Arc<App>>) -> Result<Json<Catalog>> {
    work(app, |app| app.projects.list()).await.map(Json)
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

async fn archive(State(app): State<Arc<App>>, Json(input): Json<Archive>) -> Result<Json<Project>> {
    work(app, |app| set_archive(app, input)).await.map(Json)
}

async fn history(
    State(app): State<Arc<App>>,
    RoutePath((project_id, project_session_id)): RoutePath<(u64, u64)>,
) -> Result<Json<VecDeque<Message>>> {
    work(app, move |app| {
        app.projects.history(SessionKey {
            project_id,
            project_session_id,
        })
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
    if let Some(skill) = &input.search_skill {
        app.skills
            .paths(Some(std::slice::from_ref(skill)))
            .map_err(invalid)?;
    }
    let project = app.projects.save(input)?;
    app.task.send_modify(crate::run::Snapshot::projects_changed);
    Ok(project)
}

pub(super) fn add_session(app: &App, input: NewSession) -> Result<Session> {
    let session = app.projects.create_session(input)?;
    app.task.send_modify(crate::run::Snapshot::projects_changed);
    Ok(session)
}

pub(super) fn set_archive(app: &App, input: Archive) -> Result<Project> {
    let project = app.projects.set_archive(input)?;
    app.task.send_modify(crate::run::Snapshot::projects_changed);
    Ok(project)
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
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(storage),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Catalog::default()),
            Err(error) => Err(storage(error)),
        }
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
                project.name = name;
                project.folders = folders;
                project.search_skill = input.search_skill;
                return Ok(project.clone());
            }
            catalog.next_id += 1;
            let project = Project {
                id: catalog.next_id,
                name,
                folders,
                search_skill: input.search_skill,
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
            let folder = project.workspace(input.folder.as_deref())?;
            catalog.next_id += 1;
            let session = Session {
                id: catalog.next_id,
                name,
                folder,
                archived: false,
            };
            project.sessions.push(session.clone());
            Ok(session)
        })
    }

    fn set_archive(&self, input: Archive) -> Result<Project> {
        self.change(|catalog| {
            let project = catalog
                .projects
                .iter_mut()
                .find(|project| project.id == input.project_id)
                .ok_or_else(|| missing("Project"))?;
            if let Some(id) = input.project_session_id {
                let session = project
                    .sessions
                    .iter_mut()
                    .find(|session| session.id == id)
                    .ok_or_else(|| missing("Session"))?;
                session.archived = input.archived;
            } else {
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
        let (workspace, session_file) = if let Some(id) = session_id {
            let session = project.session(id)?;
            if session.archived {
                return Err((
                    StatusCode::CONFLICT,
                    "Чат в архиве. Восстановите его, чтобы продолжить.".into(),
                ));
            }
            let folder = &session.folder;
            if let Some(workspace) = workspace
                && project.workspace(Some(workspace))? != *folder
            {
                return Err(invalid("Use the folder assigned to this session"));
            }
            (
                project.workspace(Some(folder))?,
                Some(self.session_file(id)),
            )
        } else {
            (project.workspace(workspace)?, None)
        };
        Ok(Task {
            project,
            workspace,
            session_file,
        })
    }

    fn session_file(&self, id: u64) -> PathBuf {
        self.directory.join("sessions").join(format!("{id}.jsonl"))
    }

    pub(super) fn history(&self, key: SessionKey) -> Result<VecDeque<Message>> {
        let catalog = self.list()?;
        let project = catalog
            .projects
            .iter()
            .find(|project| project.id == key.project_id)
            .ok_or_else(|| missing("Project"))?;
        project.session(key.project_session_id)?;
        let mut messages = VecDeque::new();
        let file = match fs::File::open(self.session_file(key.project_session_id)) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(messages),
            Err(error) => return Err(storage(error)),
        };
        for line in BufReader::new(file).lines() {
            let line = line.map_err(storage)?;
            let Ok(entry) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if entry["type"] != "message" {
                continue;
            }
            let message = &entry["message"];
            let role = match message["role"].as_str() {
                Some("user") => "user",
                Some("assistant") => "assistant",
                _ => continue,
            };
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
            if !text.is_empty() {
                if messages.len() == 100 {
                    messages.pop_front();
                }
                messages.push_back(Message {
                    role,
                    text: text[..text.floor_char_boundary(24_000)].to_owned(),
                });
            }
        }
        Ok(messages)
    }
}

impl Project {
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
