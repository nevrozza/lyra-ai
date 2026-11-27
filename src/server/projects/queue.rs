use std::{
    collections::HashMap,
    num::NonZeroU32,
    path::{Component, PathBuf},
    sync::Arc,
    time::Duration,
};

use axum::{Json, extract::State, http::StatusCode};
use serde::{Deserialize, Serialize};

use super::{
    App, Message, MessageOrigin, Project, Result, Session, SessionKey, Store, checked_name,
    invalid, missing, work, write_scope,
};
use crate::{
    context, ollama,
    run::{ContextNotice, DelegationScope, Snapshot},
    server::{ai_settings::AI_SETTINGS, mcp, pi},
};

mod workflow;

#[derive(Clone, Copy, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(in crate::server) enum Status {
    Queued,
    Running,
    ContextFull,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
    Blocked,
}

#[derive(Clone, Deserialize, Serialize)]
pub(in crate::server) struct Job {
    model: String,
    parallel: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    write_paths: Vec<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    role: Option<pi::Role>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) chat_session_id: Option<u64>,
    origin: MessageOrigin,
    status: Status,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    batch_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    input: Option<Prompt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    result: Option<JobResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    workflow: Option<workflow::Workflow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    key: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    after: Vec<u64>,
    #[serde(default, skip_serializing_if = "is_false")]
    external: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    artifacts: Vec<PathBuf>,
}

fn is_false(value: &bool) -> bool {
    !value
}

#[derive(Clone, Deserialize, Serialize)]
pub(in crate::server) struct Prompt {
    pub(in crate::server) prompt: String,
    pub(in crate::server) max_output_tokens: Option<u32>,
    pub(in crate::server) skills: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(in crate::server) context_tokens: Option<NonZeroU32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    retry_prompt: Option<String>,
}

#[derive(Clone, Deserialize, Serialize)]
pub(in crate::server) struct JobResult {
    pub text: String,
    pub stop_reason: Option<String>,
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(in crate::server) context: Option<mcp::PiContext>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::server) struct Enqueue {
    project_id: u64,
    batch_id: Option<String>,
    tasks: Vec<NewJob>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewJob {
    name: String,
    #[serde(default)]
    model: String,
    prompt: String,
    folders: Option<Vec<PathBuf>>,
    max_output_tokens: Option<u32>,
    context_tokens: Option<NonZeroU32>,
    skills: Option<Vec<String>>,
    #[serde(default)]
    parallel: bool,
    write_paths: Option<Vec<PathBuf>>,
    role: Option<pi::Role>,
    follow_up: Option<workflow::Config>,
    key: Option<String>,
    #[serde(default)]
    after: Vec<Prerequisite>,
    #[serde(default)]
    external: bool,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Prerequisite {
    Key(String),
    Id(u64),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::server) struct CompleteExternal {
    project_id: u64,
    project_session_id: u64,
    succeeded: bool,
    summary: String,
    #[serde(default)]
    artifacts: Vec<PathBuf>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::server) struct Retry {
    project_id: u64,
    project_session_id: u64,
    context_tokens: Option<NonZeroU32>,
    #[serde(default)]
    fresh_chat: bool,
    prompt: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::server) struct Query {
    project_id: u64,
    task_ids: Option<Vec<u64>>,
    #[serde(default)]
    wait_seconds: u64,
}

#[derive(Serialize)]
pub(in crate::server) struct TaskView {
    project_id: u64,
    project_session_id: u64,
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    context_notice: Option<ContextNotice>,
    #[serde(flatten)]
    job: Job,
}

impl Job {
    pub(super) fn pending(&self) -> bool {
        matches!(
            self.status,
            Status::Queued | Status::Running | Status::ContextFull
        )
    }

    pub(super) fn omit_payload(&mut self) {
        self.input = None;
        self.result = None;
        self.batch_id = None;
        if let Some(workflow) = &mut self.workflow {
            workflow.omit_config();
        }
    }

    pub(super) fn initial_message(&self, id: u64) -> Option<Message> {
        let text = &self.input.as_ref()?.prompt;
        Some(Message {
            role: "user",
            text: text[..text.floor_char_boundary(24_000)].to_owned(),
            thinking: None,
            model: None,
            origin: Some(self.origin),
            request_id: Some(format!("queue-{id}")),
        })
    }

    pub(super) fn status_message(&self) -> Option<Message> {
        if !matches!(
            self.status,
            Status::Failed | Status::Blocked | Status::ContextFull
        ) && !(self.external && self.status == Status::Completed)
        {
            return None;
        }
        Some(Message {
            role: "notice",
            text: if self.artifacts.is_empty() {
                self.result.as_ref()?.text.clone()
            } else {
                format!(
                    "{}\n{}",
                    self.result.as_ref()?.text,
                    self.artifacts
                        .iter()
                        .map(|path| path.display().to_string())
                        .collect::<Vec<_>>()
                        .join("\n")
                )
            },
            thinking: None,
            model: None,
            origin: None,
            request_id: None,
        })
    }
}

impl TaskView {
    fn from_session(project_id: u64, session: &Session) -> Option<Self> {
        Some(Self {
            project_id,
            project_session_id: session.id,
            name: session.name.clone(),
            context_notice: None,
            job: session.job.clone()?,
        })
    }

    fn key(&self) -> SessionKey {
        SessionKey {
            project_id: self.project_id,
            project_session_id: self.project_session_id,
        }
    }
}

pub(super) async fn enqueue_http(
    State(app): State<Arc<App>>,
    Json(input): Json<Enqueue>,
) -> Result<(StatusCode, Json<Vec<TaskView>>)> {
    enqueue(app, input, MessageOrigin::Dashboard)
        .await
        .map(|tasks| (StatusCode::ACCEPTED, Json(tasks)))
}

pub(super) async fn cancel_http(
    State(app): State<Arc<App>>,
    Json(key): Json<SessionKey>,
) -> Result<StatusCode> {
    cancel(app, key).await.map(|()| StatusCode::NO_CONTENT)
}

pub(in crate::server) async fn enqueue(
    app: Arc<App>,
    mut input: Enqueue,
    origin: MessageOrigin,
) -> Result<Vec<TaskView>> {
    if input.tasks.is_empty() || input.tasks.len() > 16 {
        return Err(invalid("Передайте от 1 до 16 задач."));
    }
    if input
        .batch_id
        .as_deref()
        .is_some_and(|id| !mcp::is_session_id(id))
    {
        return Err(invalid("Некорректный batch_id."));
    }
    work(app, move |app| {
        if app.task.borrow().closed() {
            return Err((StatusCode::SERVICE_UNAVAILABLE, "Lyra остановлена.".into()));
        }
        let installed = if input.tasks.iter().any(|task| !task.external) {
            ollama::installed_models()
                .ok_or((StatusCode::SERVICE_UNAVAILABLE, "Ollama недоступна.".into()))?
        } else {
            Vec::new()
        };
        for task in &mut input.tasks {
            if task.key.as_deref().is_some_and(|key| !mcp::is_session_id(key)) {
                return Err(invalid("Некорректный ключ задачи."));
            }
            if task.after.len() > 16 || task.prompt.trim().is_empty() || task.prompt.len() > 32_000 {
                return Err(invalid("Укажите промпт до 32000 байт и не более 16 зависимостей."));
            }
            if task.external {
                if task.follow_up.is_some()
                    || task.skills.is_some()
                    || task.max_output_tokens.is_some()
                    || task.context_tokens.is_some()
                    || task.parallel
                    || task.write_paths.is_some()
                    || task.role.is_some()
                {
                    return Err(invalid("Внешний шаг выполняется Кодексом или его инструментом. Настройки Pi к нему неприменимы."));
                }
                if !task.model.is_empty() {
                    task.model = checked_name(std::mem::take(&mut task.model))?;
                }
                continue;
            }
            if task.parallel && task.write_paths.is_none() {
                return Err(invalid("Для параллельной задачи укажите write_paths."));
            }
            if matches!(task.role, Some(pi::Role::Review))
                && (task.write_paths.is_some() || task.follow_up.is_some())
            {
                return Err(invalid("Отдельный шаг проверки выполняется без write_paths и follow_up."));
            }
            let model = crate::model_name(&task.model).map_err(invalid)?;
            task.model = mcp::resolve_model(&installed, &model).map_err(invalid)?;
            mcp::validate_pi_generation(&task.prompt, task.max_output_tokens).map_err(invalid)?;
            app.skills.paths(task.skills.as_deref()).map_err(invalid)?;
            if task.follow_up.is_none()
                && !matches!(task.role, Some(pi::Role::Review))
                && matches!(origin, MessageOrigin::Dashboard)
            {
                task.follow_up = Some(workflow::Config::for_dashboard(
                    task.model.clone(),
                    &task.prompt,
                ));
            }
            if let Some(follow_up) = &mut task.follow_up {
                follow_up.validate(&installed)?;
            }
        }
        let tasks = app.projects.enqueue(input, origin)?;
        app.task.send_modify(Snapshot::projects_changed);
        Ok(tasks)
    })
    .await
}

pub(in crate::server) async fn complete_external(
    app: Arc<App>,
    input: CompleteExternal,
) -> Result<TaskView> {
    work(app, move |app| {
        let result = app.projects.complete_external(input)?;
        app.task.send_modify(Snapshot::projects_changed);
        Ok(result)
    })
    .await
}

pub(in crate::server) async fn query(app: Arc<App>, input: Query) -> Result<Vec<TaskView>> {
    let mut state = app.task.subscribe();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(input.wait_seconds.min(25));
    let input = Arc::new(input);
    loop {
        state.borrow_and_update();
        let query = input.clone();
        let mut tasks = work(app.clone(), move |app| app.projects.query_tasks(&query)).await?;
        if input.wait_seconds == 0
            || state.borrow().closed()
            || tasks
                .iter()
                .any(|task| task.job.status == Status::ContextFull)
            || tasks.iter().all(|task| !task.job.pending())
            || tokio::time::timeout_at(deadline, state.changed())
                .await
                .is_err()
        {
            let snapshot = state.borrow();
            for task in tasks
                .iter_mut()
                .filter(|task| task.job.status == Status::Running)
            {
                task.context_notice = snapshot.project_context_notice(task.project_session_id);
            }
            return Ok(tasks);
        }
    }
}

fn context_retry_conflict() -> (StatusCode, String) {
    (
        StatusCode::CONFLICT,
        "Задача не ожидает восстановления контекста.".into(),
    )
}

pub(in crate::server) async fn retry(app: Arc<App>, input: Retry) -> Result<TaskView> {
    work(app, move |app| {
        if app.task.borrow().closed() {
            return Err((StatusCode::SERVICE_UNAVAILABLE, "Lyra остановлена.".into()));
        }
        if let Some(tokens) = input.context_tokens {
            let model = app
                .projects
                .list()?
                .projects
                .into_iter()
                .find(|project| project.id == input.project_id)
                .ok_or_else(|| missing("Project"))?
                .session(input.project_session_id)?
                .job
                .as_ref()
                .filter(|job| job.status == Status::ContextFull)
                .ok_or_else(context_retry_conflict)?
                .model
                .clone();
            context::validate_window(&model, tokens.get(), context::estimate(&model).as_ref())
                .map_err(invalid)?;
        }
        let task = app.projects.retry_task(input)?;
        app.task.send_modify(Snapshot::projects_changed);
        Ok(task)
    })
    .await
}

pub(in crate::server) async fn cancel(app: Arc<App>, key: SessionKey) -> Result<()> {
    work(app, move |app| {
        if !app.projects.set_job(
            &key,
            &[Status::Queued, Status::ContextFull],
            Status::Cancelled,
            None,
        )? {
            return Err((
                StatusCode::CONFLICT,
                "Задача уже запущена или завершена.".into(),
            ));
        }
        app.task.send_modify(Snapshot::projects_changed);
        Ok(())
    })
    .await
}

pub(in crate::server) async fn dispatch(app: Arc<App>) {
    let mut state = app.task.subscribe();
    while !state.borrow_and_update().closed() {
        if let Err((_, error)) = start_next(&app).await {
            eprintln!("Could not dispatch project task: {error}");
            tokio::time::sleep(Duration::from_secs(1)).await;
            continue;
        }
        if state.changed().await.is_err() {
            break;
        }
    }
}

async fn start_next(app: &Arc<App>) -> Result<()> {
    if work(app.clone(), |app| app.projects.block_failed_dependencies()).await? {
        app.task.send_modify(Snapshot::projects_changed);
    }
    let ready = work(app.clone(), |app| app.projects.ready_tasks()).await?;
    for (task, writes) in ready {
        let writes = match writes {
            Ok(writes) => writes,
            Err(error) => {
                let key = task.key();
                work(app.clone(), move |app| {
                    app.projects.set_job(
                        &key,
                        &[Status::Queued],
                        Status::Failed,
                        Some(JobResult {
                            text: format!("Не удалось зарезервировать файлы: {error}"),
                            stop_reason: None,
                            truncated: false,
                            context: None,
                        }),
                    )
                })
                .await?;
                app.task.send_modify(Snapshot::projects_changed);
                continue;
            }
        };
        let parallel = task.job.parallel && !writes.is_empty();
        let scope = DelegationScope::Queued {
            session_id: task.project_session_id,
            parallel,
            writes: &writes,
            context_tokens: task
                .job
                .input
                .as_ref()
                .and_then(|input| input.context_tokens)
                .map(NonZeroU32::get),
            max_tasks: AI_SETTINGS.parallel_project_tasks,
            max_models: AI_SETTINGS.parallel_project_models,
        };
        let mut delegation = None;
        if !app.task.send_if_modified(|state| {
            delegation = state.begin_delegation(&task.job.model, scope);
            delegation.is_some()
        }) {
            if !parallel {
                break;
            }
            continue;
        }
        let delegation = delegation.expect("queue reservation");
        let key = task.key();
        let claimed = work(app.clone(), move |app| {
            app.projects
                .set_job(&key, &[Status::Queued], Status::Running, None)
        })
        .await;
        if !matches!(claimed, Ok(true)) {
            app.task
                .send_modify(|state| state.finish_delegation(delegation, false));
            claimed?;
            continue;
        }
        app.task.send_modify(Snapshot::projects_changed);
        tokio::spawn(run_task(app.clone(), task, delegation, writes));
    }
    Ok(())
}

async fn run_task(app: Arc<App>, mut task: TaskView, delegation: u64, writes: Vec<PathBuf>) {
    let key = task.key();
    let mut candidates = Vec::new();
    let (status, result) = if let Some(mut input) = task.job.input.take() {
        if task
            .job
            .workflow
            .as_ref()
            .is_some_and(workflow::Workflow::is_follow_up)
        {
            // Older automatic follow-ups may still carry Lyra's former default cap.
            input.max_output_tokens = None;
        }
        if let Some(handoff) = input.retry_prompt.take() {
            input.prompt = match &task.job.workflow {
                Some(flow) if flow.is_follow_up() => format!(
                    "{}\n\nContinuation after context overflow. Check the current project files before repeating any work.\n{handoff}",
                    input.prompt
                ),
                _ => handoff,
            };
        }
        let reviewing = task
            .job
            .workflow
            .as_ref()
            .is_some_and(workflow::Workflow::is_review);
        let release = task
            .job
            .workflow
            .as_ref()
            .filter(|_| !task.job.parallel)
            .and_then(|flow| flow.reviewer_to_release(&task.job.model));
        let metrics = if reviewing || release.is_some() {
            let mut cache = app.metrics.lock().await;
            cache.invalidate();
            Some(cache.sample().await)
        } else {
            None
        };
        if reviewing && let Some(metrics) = &metrics {
            let reviewer = task.job.model.clone();
            let sample = metrics.clone();
            candidates = work(app.clone(), move |app| {
                Ok(workflow::candidates(app, &reviewer, &sample))
            })
            .await
            .unwrap_or_default();
            let preferred = app.project_models.get().await;
            workflow::add_candidates(
                &mut input.prompt,
                &candidates,
                metrics,
                &task.job.model,
                &preferred,
            );
        }
        let preparation = if let (Some(reviewer), Some(metrics)) = (release, metrics) {
            let reviewer = reviewer.to_owned();
            let successor = task.job.model.clone();
            work(app.clone(), move |app| {
                workflow::release_reviewer(app, &metrics, &reviewer, &successor).map_err(invalid)
            })
            .await
        } else {
            Ok(())
        };
        let preparation = if preparation.is_ok() && !task.job.after.is_empty() {
            let dependencies = task.job.after.clone();
            let project_id = task.project_id;
            work(app.clone(), move |app| {
                app.projects.dependency_artifacts(project_id, &dependencies)
            })
            .await
            .map(|artifacts| {
                if !artifacts.is_empty() {
                    input.prompt.push_str(
                        "\n\nFiles produced by completed prerequisite steps in this project:\n",
                    );
                    input.prompt.push_str(
                        &serde_json::to_string(&artifacts).expect("serialize project artifacts"),
                    );
                }
            })
        } else {
            preparation
        };
        if let Err((_, error)) = preparation {
            fail_dispatch(&app, delegation, error)
        } else {
            let request = mcp::queued_request(
                &key,
                task.job.model,
                input,
                task.job
                    .workflow
                    .as_ref()
                    .map_or(Some(task.job.role.unwrap_or(pi::Role::Implement)), |flow| {
                        flow.role(task.job.role.unwrap_or(pi::Role::Implement))
                    }),
                writes,
                task.job.chat_session_id,
            );
            mcp::run_queued(app.clone(), request, task.job.origin, delegation).await
        }
    } else {
        fail_dispatch(
            &app,
            delegation,
            "У задачи отсутствует сохранённый запрос.".into(),
        )
    };
    if let Err((_, error)) = work(app.clone(), move |app| {
        app.projects.complete_job(&key, status, result, &candidates)
    })
    .await
    {
        eprintln!("Could not save project task result: {error}");
    }
    app.task.send_modify(Snapshot::projects_changed);
}

fn fail_dispatch(app: &App, delegation: u64, text: String) -> (Status, JobResult) {
    app.task
        .send_modify(|state| state.finish_delegation(delegation, false));
    (
        Status::Failed,
        JobResult {
            text,
            stop_reason: None,
            truncated: false,
            context: None,
        },
    )
}

impl Store {
    fn retry_task(&self, input: Retry) -> Result<TaskView> {
        if input.prompt.trim().is_empty() || input.prompt.len() > 32_000 {
            return Err(invalid(
                "Укажите краткое состояние и оставшуюся работу до 32000 байт.",
            ));
        }
        self.change(|catalog| {
            let project = catalog
                .projects
                .iter_mut()
                .find(|project| project.id == input.project_id)
                .ok_or_else(|| missing("Project"))?;
            project.ensure_active()?;
            let session = project
                .sessions
                .iter_mut()
                .find(|session| session.id == input.project_session_id)
                .ok_or_else(|| missing("Task"))?;
            session.ensure_active()?;
            let job = session
                .job
                .as_mut()
                .filter(|job| job.status == Status::ContextFull)
                .ok_or_else(context_retry_conflict)?;
            let previous = job
                .result
                .as_ref()
                .and_then(|result| result.context.as_ref())
                .ok_or_else(|| invalid("Размер предыдущего окна контекста неизвестен."))?
                .context_tokens;
            if !input.fresh_chat
                && !input
                    .context_tokens
                    .is_some_and(|tokens| tokens.get() > previous)
            {
                return Err(invalid(format!(
                    "Для продолжения в том же чате укажите контекст больше {previous} токенов."
                )));
            }
            let prompt = job
                .input
                .as_mut()
                .ok_or_else(|| invalid("Исходный запрос задачи недоступен."))?;
            prompt.context_tokens = input.context_tokens.or_else(|| NonZeroU32::new(previous));
            prompt.retry_prompt = Some(input.prompt);
            job.status = Status::Queued;
            job.result = None;

            let new_chat = input.fresh_chat.then(|| {
                catalog.next_id += 1;
                job.chat_session_id = Some(catalog.next_id);
                Session {
                    id: catalog.next_id,
                    name: format!("Продолжение задачи #{}", session.id),
                    folder: session.folder.clone(),
                    folders: session.folders.clone(),
                    archived: false,
                    mode: super::ChatMode::Work,
                    job: None,
                }
            });
            let mut view = TaskView::from_session(project.id, session).expect("retry task");
            view.job.omit_payload();
            project.sessions.extend(new_chat);
            Ok(view)
        })
    }

    fn complete_external(&self, input: CompleteExternal) -> Result<TaskView> {
        if input.summary.trim().is_empty()
            || input.summary.len() > 4_000
            || input.artifacts.len() > 16
            || !input.succeeded && !input.artifacts.is_empty()
        {
            return Err(invalid(
                "Укажите краткий итог. Готовые файлы можно передать после успешного шага.",
            ));
        }
        self.change(|catalog| {
            let project = catalog
                .projects
                .iter_mut()
                .find(|project| project.id == input.project_id)
                .ok_or_else(|| missing("Project"))?;
            project.ensure_active()?;
            let session = project.session(input.project_session_id)?;
            let job = session
                .job
                .as_ref()
                .filter(|job| job.external)
                .ok_or_else(|| missing("External step"))?;
            let status = if input.succeeded {
                Status::Completed
            } else {
                Status::Failed
            };
            if job.status == status {
                return Ok(TaskView::from_session(project.id, session).expect("external task"));
            }
            if job.status != Status::Queued
                || job
                    .after
                    .iter()
                    .any(|id| project.dependency_state(*id) != Dependency::Ready)
            {
                return Err((
                    StatusCode::CONFLICT,
                    "Внешний шаг уже завершён или его зависимости ещё не готовы.".into(),
                ));
            }
            let root = (!input.artifacts.is_empty())
                .then(|| session.folder.canonicalize())
                .transpose()
                .map_err(super::storage)?;
            let mut artifacts = Vec::new();
            for relative in input.artifacts {
                if relative.as_os_str().is_empty()
                    || !relative
                        .components()
                        .all(|part| matches!(part, Component::Normal(_)))
                {
                    return Err(invalid(
                        "Укажите путь к файлу относительно основной папки чата.",
                    ));
                }
                let path = session.folder.join(relative);
                let resolved = path
                    .canonicalize()
                    .map_err(|_| invalid(format!("Файл не найден: {}", path.display())))?;
                if !root.as_ref().is_some_and(|root| resolved.starts_with(root))
                    || !resolved.is_file()
                {
                    return Err(invalid(
                        "Результат должен быть файлом внутри рабочей папки.",
                    ));
                }
                if !artifacts.contains(&path) {
                    artifacts.push(path);
                }
            }
            let session = project
                .sessions
                .iter_mut()
                .find(|session| session.id == input.project_session_id)
                .expect("external session");
            let job = session.job.as_mut().expect("external task");
            job.status = status;
            job.artifacts = artifacts;
            job.result = Some(JobResult {
                text: input.summary,
                stop_reason: None,
                truncated: false,
                context: None,
            });
            Ok(TaskView::from_session(project.id, session).expect("external task"))
        })
        .map(|mut view| {
            view.job.input = None;
            view.job.batch_id = None;
            view
        })
    }

    fn dependency_artifacts(&self, project_id: u64, after: &[u64]) -> Result<Vec<PathBuf>> {
        let catalog = self.list()?;
        let project = catalog
            .projects
            .iter()
            .find(|project| project.id == project_id)
            .ok_or_else(|| missing("Project"))?;
        let mut artifacts = Vec::new();
        for id in after {
            let session = project.session(*id)?;
            if let Some(job) = &session.job
                && !job.artifacts.is_empty()
            {
                let root = session.folder.canonicalize().map_err(super::storage)?;
                for file in &job.artifacts {
                    if !file
                        .canonicalize()
                        .is_ok_and(|path| path.starts_with(&root) && path.is_file())
                    {
                        return Err(invalid(format!(
                            "Результат шага #{id} больше недоступен: {}",
                            file.display()
                        )));
                    }
                    if !artifacts.contains(file) {
                        artifacts.push(file.clone());
                    }
                }
            }
        }
        Ok(artifacts)
    }

    fn block_failed_dependencies(&self) -> Result<bool> {
        let blocked = |project: &Project| {
            project
                .sessions
                .iter()
                .filter_map(|session| {
                    let job = session
                        .job
                        .as_ref()
                        .filter(|job| job.status == Status::Queued)?;
                    let failed = job
                        .after
                        .iter()
                        .find(|id| project.dependency_state(**id) == Dependency::Failed)?;
                    Some((session.id, *failed))
                })
                .collect::<HashMap<_, _>>()
        };
        if !self
            .list()?
            .projects
            .iter()
            .any(|project| !blocked(project).is_empty())
        {
            return Ok(false);
        }
        self.change(|catalog| {
            let mut changed = false;
            for project in &mut catalog.projects {
                let failed = blocked(project);
                for session in &mut project.sessions {
                    if let Some(id) = failed.get(&session.id)
                        && let Some(job) = &mut session.job
                    {
                        job.status = Status::Blocked;
                        job.result = Some(JobResult {
                            text: format!("Задача заблокирована: шаг #{id} не завершился успешно."),
                            stop_reason: None,
                            truncated: false,
                            context: None,
                        });
                        changed = true;
                    }
                }
            }
            Ok(changed)
        })
    }

    fn complete_job(
        &self,
        key: &SessionKey,
        mut status: Status,
        mut result: JobResult,
        candidates: &[workflow::Candidate],
    ) -> Result<()> {
        self.change(|catalog| {
            let project = catalog
                .projects
                .iter_mut()
                .find(|project| project.id == key.project_id)
                .ok_or_else(|| missing("Project"))?;
            let session = project
                .sessions
                .iter_mut()
                .find(|session| session.id == key.project_session_id)
                .ok_or_else(|| missing("Task"))?;
            let job = session.job.as_mut().ok_or_else(|| missing("Task"))?;
            if job.status != Status::Running {
                return Ok(());
            }
            let next = if status == Status::Completed
                && let Some(workflow) = &job.workflow
            {
                match workflow.advance(&result, candidates, &session.folder) {
                    Ok(next) => next,
                    Err(reason) => {
                        status = Status::Failed;
                        result.text = format!("{reason}\n\n{}", result.text);
                        Vec::new()
                    }
                }
            } else {
                Vec::new()
            };
            job.status = status;
            job.result = Some(result);
            let folder = session.folder.clone();
            let folders = session.folders.clone();
            let mut ids = Vec::with_capacity(next.len());
            for mut step in next {
                catalog.next_id += 1;
                step.job.after = step.after.into_iter().map(|index| ids[index]).collect();
                ids.push(catalog.next_id);
                project.sessions.push(Session {
                    id: catalog.next_id,
                    name: step.name,
                    folder: folder.clone(),
                    folders: folders.clone(),
                    archived: false,
                    mode: super::ChatMode::Work,
                    job: Some(step.job),
                });
            }
            Ok(())
        })
    }

    fn enqueue(&self, input: Enqueue, origin: MessageOrigin) -> Result<Vec<TaskView>> {
        self.change(|catalog| {
            let project = catalog
                .projects
                .iter_mut()
                .find(|project| project.id == input.project_id)
                .ok_or_else(|| missing("Project"))?;
            project.ensure_active()?;
            if let Some(batch) = &input.batch_id {
                let existing = project
                    .sessions
                    .iter()
                    .filter(|session| {
                        session.job.as_ref().is_some_and(|job| {
                            job.batch_id.as_ref() == Some(batch)
                                && job.origin.as_str() == origin.as_str()
                        })
                    })
                    .filter_map(|session| TaskView::from_session(project.id, session))
                    .map(|mut task| {
                        task.job.omit_payload();
                        task
                    })
                    .collect::<Vec<_>>();
                if !existing.is_empty() {
                    return Ok(existing);
                }
            }
            let mut tasks = Vec::with_capacity(input.tasks.len());
            let mut keys = HashMap::new();
            for new in input.tasks {
                let name = checked_name(new.name)?;
                let folders = match new.folders {
                    Some(folders) => project.session_folders(&folders)?,
                    None => project.folders_with_primary(&project.workspace(None)?),
                };
                catalog.next_id += 1;
                let mut after = Vec::new();
                for dependency in new.after {
                    let id = match dependency {
                        Prerequisite::Key(key) => *keys.get(&key).ok_or_else(|| {
                            invalid(format!(
                                "Поставьте задачу с ключом {key} перед её зависимыми задачами."
                            ))
                        })?,
                        Prerequisite::Id(id)
                            if project
                                .session(id)
                                .is_ok_and(|session| session.job.is_some()) =>
                        {
                            id
                        }
                        Prerequisite::Id(id) => {
                            return Err(invalid(format!(
                                "Задача #{id} отсутствует в этом проекте."
                            )));
                        }
                    };
                    if !after.contains(&id) {
                        after.push(id);
                    }
                }
                if let Some(key) = &new.key
                    && keys.insert(key.clone(), catalog.next_id).is_some()
                {
                    return Err(invalid(format!("Ключ задачи {key} повторяется в пакете.")));
                }
                let write_paths = if let Some(paths) = new.write_paths {
                    write_scope::resolve_paths(&folders[0], &paths)
                        .map_err(|error| invalid(error.to_string()))?;
                    paths
                } else {
                    Vec::new()
                };
                let workflow = new
                    .follow_up
                    .map(|config| workflow::Workflow::new(catalog.next_id, config));
                let session = Session {
                    id: catalog.next_id,
                    name,
                    folder: folders[0].clone(),
                    folders,
                    archived: false,
                    mode: super::ChatMode::Work,
                    job: Some(Job {
                        model: new.model,
                        parallel: new.parallel,
                        write_paths,
                        role: new.role,
                        chat_session_id: None,
                        origin,
                        status: Status::Queued,
                        batch_id: input.batch_id.clone(),
                        input: Some(Prompt {
                            prompt: new.prompt,
                            max_output_tokens: new.max_output_tokens,
                            skills: new.skills,
                            context_tokens: new.context_tokens,
                            retry_prompt: None,
                        }),
                        result: None,
                        workflow,
                        key: new.key,
                        after,
                        external: new.external,
                        artifacts: Vec::new(),
                    }),
                };
                let mut task =
                    TaskView::from_session(project.id, &session).expect("new queue task");
                task.job.omit_payload();
                tasks.push(task);
                project.sessions.push(session);
            }
            Ok(tasks)
        })
    }

    fn query_tasks(&self, query: &Query) -> Result<Vec<TaskView>> {
        let catalog = self.list()?;
        let project = catalog
            .projects
            .iter()
            .find(|project| project.id == query.project_id)
            .ok_or_else(|| missing("Project"))?;
        let mut tasks = project
            .sessions
            .iter()
            .filter(|session| {
                query.task_ids.as_ref().is_none_or(|ids| {
                    ids.contains(&session.id)
                        || session
                            .job
                            .as_ref()
                            .and_then(|job| job.workflow.as_ref())
                            .is_some_and(|workflow| ids.contains(&workflow.root_id()))
                })
            })
            .filter_map(|session| TaskView::from_session(project.id, session))
            .collect::<Vec<_>>();
        if let Some(ids) = &query.task_ids
            && ids
                .iter()
                .any(|id| !tasks.iter().any(|task| task.project_session_id == *id))
        {
            return Err(missing("Task"));
        }
        for task in &mut tasks {
            task.job.input = None;
            task.job.batch_id = None;
            if let Some(workflow) = &mut task.job.workflow {
                workflow.omit_config();
            }
        }
        Ok(tasks)
    }

    fn ready_tasks(&self) -> Result<Vec<(TaskView, std::io::Result<Vec<PathBuf>>)>> {
        let catalog = self.list()?;
        let mut tasks = Vec::new();
        for project in &catalog.projects {
            for session in &project.sessions {
                let Some(job) = session.job.as_ref().filter(|job| {
                    job.status == Status::Queued
                        && !job.external
                        && job
                            .after
                            .iter()
                            .all(|id| project.dependency_state(*id) == Dependency::Ready)
                }) else {
                    continue;
                };
                let writes = if job.write_paths.is_empty() {
                    Ok(Vec::new())
                } else {
                    write_scope::resolve_paths(&session.folder, &job.write_paths)
                };
                tasks.push((
                    TaskView::from_session(project.id, session).expect("queued job"),
                    writes,
                ));
            }
        }
        tasks.sort_by_key(|(task, _)| task.project_session_id);
        Ok(tasks)
    }

    fn set_job(
        &self,
        key: &SessionKey,
        expected: &[Status],
        status: Status,
        result: Option<JobResult>,
    ) -> Result<bool> {
        self.change(|catalog| {
            let job = catalog
                .projects
                .iter_mut()
                .find(|project| project.id == key.project_id)
                .and_then(|project| {
                    project
                        .sessions
                        .iter_mut()
                        .find(|session| session.id == key.project_session_id)
                })
                .and_then(|session| session.job.as_mut())
                .ok_or_else(|| missing("Task"))?;
            if !expected.contains(&job.status) {
                return Ok(false);
            }
            job.status = status;
            job.result = result;
            Ok(true)
        })
    }

    pub(in crate::server) fn recover_tasks(&self) -> Result<()> {
        let has_running = self
            .list()?
            .projects
            .iter()
            .flat_map(|project| &project.sessions)
            .any(|session| {
                session
                    .job
                    .as_ref()
                    .is_some_and(|job| job.status == Status::Running)
            });
        if has_running {
            self.change(|catalog| {
                for job in catalog
                    .projects
                    .iter_mut()
                    .flat_map(|project| &mut project.sessions)
                    .filter_map(|session| session.job.as_mut())
                    .filter(|job| job.status == Status::Running)
                {
                    job.status = Status::Interrupted;
                }
                Ok(())
            })?;
        }
        Ok(())
    }
}

#[derive(PartialEq)]
enum Dependency {
    Ready,
    Waiting,
    Failed,
}

impl Project {
    fn dependency_state(&self, root: u64) -> Dependency {
        let Ok(session) = self.session(root) else {
            return Dependency::Failed;
        };
        let Some(root_job) = &session.job else {
            return Dependency::Failed;
        };
        let chain = self
            .sessions
            .iter()
            .filter_map(|session| session.job.as_ref())
            .filter(|job| {
                job.workflow
                    .as_ref()
                    .is_some_and(|flow| flow.root_id() == root)
            });
        let mut pending = false;
        for job in std::iter::once(root_job).chain(chain) {
            match job.status {
                Status::Completed => {}
                Status::Queued | Status::Running | Status::ContextFull => pending = true,
                _ => return Dependency::Failed,
            }
        }
        if pending {
            Dependency::Waiting
        } else {
            Dependency::Ready
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn fresh_retry_keeps_the_task_identity_scope_and_waiting_dependents() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        let store = Store::new(data.clone());
        let project = store
            .save(super::super::SaveProject {
                id: None,
                name: "Test project".into(),
                folders: vec![dir.path().to_path_buf()],
                search_skill: None,
            })
            .unwrap();
        let id = project.id;
        let tasks = store.enqueue(serde_json::from_value(json!({
            "project_id": id,
            "tasks": [
                {"key": "edit", "name": "Edit", "model": "local", "prompt": "Edit",
                    "parallel": true, "write_paths": ["src/test.rs"], "context_tokens": 4096},
                {"name": "Check", "model": "local", "prompt": "Check", "after": ["edit"]}
            ]
        })).unwrap(), MessageOrigin::Lyra).unwrap();
        let original = tasks[0].key();
        assert!(
            store
                .set_job(&original, &[Status::Queued], Status::Running, None)
                .unwrap()
        );
        let overflow = serde_json::from_value(json!({
            "text": "Full", "stop_reason": "context_full", "truncated": true,
            "context": {"context_tokens": 4096, "notice": "full"}
        }))
        .unwrap();
        store
            .complete_job(&original, Status::ContextFull, overflow, &[])
            .unwrap();

        let store = Store::new(data);
        assert!(!store.block_failed_dependencies().unwrap());
        assert!(store.ready_tasks().unwrap().is_empty());
        let retry = |fresh_chat| Retry {
            project_id: id,
            project_session_id: original.project_session_id,
            context_tokens: None,
            fresh_chat,
            prompt: "Continue with a compact handoff.".into(),
        };
        assert!(store.retry_task(retry(false)).is_err());
        let retried = store.retry_task(retry(true)).unwrap();
        assert_eq!(retried.project_session_id, original.project_session_id);
        let new_chat = retried.job.chat_session_id.unwrap();
        assert_ne!(new_chat, original.project_session_id);
        let catalog = store.list().unwrap();
        let project = &catalog.projects[0];
        assert!(project.has_pending_task(new_chat));
        assert_eq!(
            project.session(new_chat).unwrap().folder,
            project.session(original.project_session_id).unwrap().folder
        );
        assert!(!store.session_file(new_chat).exists());
        let ready = store.ready_tasks().unwrap();
        assert_eq!(ready.len(), 1);
        let job = &ready[0].0.job;
        assert_eq!(job.write_paths, [PathBuf::from("src/test.rs")]);
        assert_eq!(
            job.input.as_ref().unwrap().context_tokens,
            NonZeroU32::new(4096)
        );

        assert!(
            store
                .set_job(&original, &[Status::Queued], Status::Running, None)
                .unwrap()
        );
        assert!(
            store
                .set_job(&original, &[Status::Running], Status::Completed, None)
                .unwrap()
        );
        assert_eq!(
            store.ready_tasks().unwrap()[0].0.project_session_id,
            tasks[1].project_session_id
        );
    }

    #[test]
    fn batch_fixes_keep_dependencies_and_gate_the_next_review() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path().join("data"));
        let project = store
            .save(super::super::SaveProject {
                id: None,
                name: "Test project".into(),
                folders: vec![dir.path().to_path_buf()],
                search_skill: None,
            })
            .unwrap();
        let id = project.id;
        let [root]: [TaskView; 1] = store
            .enqueue(
                serde_json::from_value(json!({
                    "project_id": id,
                    "tasks": [{
                        "name": "Implementation", "model": "local", "prompt": "Implement",
                        "follow_up": {"review": {"model": "local", "prompt": "Review"}}
                    }],
                }))
                .unwrap(),
                MessageOrigin::Lyra,
            )
            .unwrap()
            .try_into()
            .ok()
            .unwrap();
        let finish = |task: &TaskView, text: &str, candidates: &[workflow::Candidate]| {
            let key = task.key();
            assert!(
                store
                    .set_job(&key, &[Status::Queued], Status::Running, None)
                    .unwrap()
            );
            store
                .complete_job(
                    &key,
                    Status::Completed,
                    JobResult {
                        text: text.into(),
                        stop_reason: None,
                        truncated: false,
                        context: None,
                    },
                    candidates,
                )
                .unwrap();
        };
        let ready = || {
            store
                .ready_tasks()
                .unwrap()
                .into_iter()
                .map(|(task, _)| task)
                .collect::<Vec<_>>()
        };
        finish(&root, "implemented", &[]);
        let review = ready().pop().unwrap();
        let candidate = workflow::test_candidate("local");
        finish(
            &review,
            r#"local
{"status":"fixes","findings":[
    {"location":"a","issue":"first","check":"check a","model":"local","write_paths":["a"]},
    {"location":"b","issue":"second","check":"check b","model":"local","write_paths":["b"]},
    {"location":"a again","issue":"third","check":"check a again","model":"local","write_paths":["a"],"after":[2]}
]}"#,
            &[candidate],
        );
        let [first, second]: [TaskView; 2] = ready().try_into().ok().unwrap();
        assert!(first.job.parallel && second.job.parallel);
        assert!(first.job.after.is_empty() && second.job.after.is_empty());

        let pass = "local\n{\"status\":\"pass\",\"summary\":\"fixed\"}";
        finish(&second, pass, &[]);
        assert_eq!(ready()[0].project_session_id, first.project_session_id);
        finish(&first, pass, &[]);
        let [third]: [TaskView; 1] = ready().try_into().ok().unwrap();
        assert_eq!(
            third.job.after,
            [first.project_session_id, second.project_session_id]
        );
        finish(&third, pass, &[]);
        let [next_review]: [TaskView; 1] = ready().try_into().ok().unwrap();
        assert_eq!(next_review.job.after.len(), 3);
        assert!(next_review.job.workflow.unwrap().is_review());
    }
}
