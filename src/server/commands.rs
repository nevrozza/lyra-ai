use std::{
    fs, io,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};

use axum::{
    Json, Router,
    extract::{Path as RoutePath, State as AppState},
    http::StatusCode,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    sync::watch,
    time::timeout,
};

use super::{
    App,
    sessions::{Call, Kind},
};

mod process_tree;

const APPROVAL_TIMEOUT: Duration = Duration::from_secs(120);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(300);
const OUTPUT_LIMIT: usize = 3500;

pub(super) struct State {
    next_id: u64,
    active: Option<Active>,
    mode: Mode,
    mode_path: PathBuf,
}

impl State {
    pub(super) fn new(mode_path: PathBuf) -> Self {
        let mode = match fs::read(&mode_path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
            Err(_) => Mode::default(),
        };
        Self {
            next_id: 0,
            active: None,
            mode,
            mode_path,
        }
    }

    fn set_mode(&mut self, mode: Mode) -> io::Result<()> {
        if self.mode == mode {
            return Ok(());
        }
        let directory = self.mode_path.parent().expect("command mode directory");
        fs::create_dir_all(directory)?;
        let mut file = tempfile::NamedTempFile::new_in(directory)?;
        serde_json::to_writer(&mut file, &mode)?;
        file.as_file().sync_all()?;
        file.persist(&self.mode_path)?;
        self.mode = mode;
        Ok(())
    }
}

#[derive(Clone, Copy, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Mode {
    #[default]
    Ask,
    AllowAll,
}

#[derive(Serialize)]
struct SettingsView {
    mode: Mode,
    command: Option<View>,
}

struct Active {
    view: View,
    actions: watch::Sender<Option<Action>>,
}

#[derive(Clone, Serialize)]
struct View {
    id: u64,
    workspace: String,
    command: String,
    shell: &'static str,
    running: bool,
    stopping: bool,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Action {
    Allow,
    Deny,
    Stop,
}

pub(super) fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/commands", get(view))
        .route("/commands/mode", post(set_mode))
        .route("/commands/{id}", post(decide))
}

async fn view(AppState(app): AppState<Arc<App>>) -> Json<SettingsView> {
    let state = app.commands.lock().expect("command state lock");
    Json(SettingsView {
        mode: state.mode,
        command: state.active.as_ref().map(|active| active.view.clone()),
    })
}

async fn set_mode(AppState(app): AppState<Arc<App>>, Json(mode): Json<Mode>) -> StatusCode {
    match tokio::task::spawn_blocking(move || {
        app.commands
            .lock()
            .expect("command state lock")
            .set_mode(mode)?;
        app.task.send_modify(crate::run::Snapshot::commands_changed);
        Ok::<_, io::Error>(())
    })
    .await
    {
        Ok(Ok(())) => StatusCode::NO_CONTENT,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

async fn decide(
    AppState(app): AppState<Arc<App>>,
    RoutePath(id): RoutePath<u64>,
    Json(action): Json<Action>,
) -> StatusCode {
    let mut state = app.commands.lock().expect("command state lock");
    let Some(active) = state.active.as_mut().filter(|active| active.view.id == id) else {
        return StatusCode::NOT_FOUND;
    };
    if active.view.running != matches!(action, Action::Stop) || active.actions.borrow().is_some() {
        return StatusCode::CONFLICT;
    }
    active.actions.send_replace(Some(action));
    if matches!(action, Action::Stop) {
        active.view.stopping = true;
        app.task.send_modify(crate::run::Snapshot::commands_changed);
    }
    StatusCode::NO_CONTENT
}

struct Request<'a> {
    trace: &'a Call,
    app: &'a App,
    finished: bool,
}

impl Drop for Request<'_> {
    fn drop(&mut self) {
        self.app.commands.lock().expect("command state lock").active = None;
        self.app
            .task
            .send_modify(crate::run::Snapshot::commands_changed);
        if !self.finished {
            self.trace.event(
                Kind::Tool,
                "run_command",
                "Задача Pi прервана",
                Some("error"),
            );
        }
    }
}

#[derive(Serialize)]
pub(super) struct ResultBody {
    status: &'static str,
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
    output_truncated: bool,
    message: Option<String>,
}

impl ResultBody {
    fn error(status: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            exit_code: None,
            stdout: String::new(),
            stderr: String::new(),
            output_truncated: false,
            message: Some(message.into()),
        }
    }
}

pub(super) async fn run(trace: &Call, workspace: &Path, command: &str) -> ResultBody {
    if command.trim().is_empty() || command.len() > 4000 || command.contains('\0') {
        return ResultBody::error(
            "invalid",
            "Expected a nonempty command of at most 4000 bytes",
        );
    }
    let app = trace.app();
    let (preapproved, mut actions) = {
        let mut state = app.commands.lock().expect("command state lock");
        if state.active.is_some() {
            return ResultBody::error("busy", "Another command is active");
        }
        let preapproved = state.mode == Mode::AllowAll;
        state.next_id += 1;
        let (actions, receiver) = watch::channel(None);
        state.active = Some(Active {
            view: View {
                id: state.next_id,
                workspace: workspace.display().to_string(),
                command: command.to_owned(),
                shell: process_tree::SHELL,
                running: preapproved,
                stopping: false,
            },
            actions,
        });
        (preapproved, receiver)
    };
    let mut request = Request {
        trace,
        app,
        finished: false,
    };
    app.task.send_modify(crate::run::Snapshot::commands_changed);
    trace.event(
        Kind::Tool,
        "run_command",
        &format!("{} · {}", process_tree::SHELL, workspace.display()),
        None,
    );
    trace.event(
        Kind::Tool,
        "run_command",
        command,
        Some(if preapproved { "running" } else { "waiting" }),
    );

    let permitted = preapproved
        || matches!(
            timeout(APPROVAL_TIMEOUT, actions.wait_for(Option::is_some)).await,
            Ok(Ok(action)) if matches!(*action, Some(Action::Allow))
        );
    let result = if permitted {
        if !preapproved {
            let mut state = app.commands.lock().expect("command state lock");
            let active = state.active.as_mut().expect("pending command");
            active.actions.send_replace(None);
            active.view.running = true;
            app.task.send_modify(crate::run::Snapshot::commands_changed);
            trace.event(
                Kind::Tool,
                "run_command",
                "Разрешено пользователем",
                Some("running"),
            );
        }
        execute(workspace, command, &mut actions)
            .await
            .unwrap_or_else(|error| {
                ResultBody::error("error", format!("Could not execute command: {error}"))
            })
    } else {
        ResultBody::error("denied", "Command approval was declined or expired")
    };
    for (label, text) in [("stdout", &result.stdout), ("stderr", &result.stderr)] {
        if !text.is_empty() {
            trace.event(Kind::Tool, label, text, None);
        }
    }
    let mut detail = result
        .exit_code
        .map_or_else(String::new, |code| format!("Код завершения: {code}"));
    if let Some(message) = &result.message {
        if !detail.is_empty() {
            detail.push('\n');
        }
        detail.push_str(message);
    }
    if result.output_truncated {
        detail.push_str("\nВывод сокращён");
    }
    trace.event(
        Kind::Tool,
        "run_command",
        detail.trim(),
        Some(
            if result.status == "exited" && result.exit_code == Some(0) {
                "ok"
            } else {
                "error"
            },
        ),
    );
    request.finished = true;
    result
}

async fn execute(
    workspace: &Path,
    script: &str,
    actions: &mut watch::Receiver<Option<Action>>,
) -> io::Result<ResultBody> {
    let mut command = process_tree::shell(script)?;
    command
        .current_dir(workspace)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn()?;
    let tree = process_tree::Tree::attach(&child)?;
    process_tree::start(&mut child, script).await?;
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let mut out = Tail::default();
    let mut err = Tail::default();

    let wait = async {
        tokio::select! {
            biased;
            _ = actions.wait_for(|action| matches!(action, Some(Action::Stop))) => Ok(("stopped", None)),
            result = timeout(COMMAND_TIMEOUT, child.wait()) => match result {
                Ok(status) => status.map(|status| ("exited", status.code())),
                Err(_) => Ok(("timeout", None)),
            }
        }
    };
    let ((status, exit_code), drain) = {
        let drain =
            async { tokio::try_join!(out.read(&mut stdout), err.read(&mut stderr)).map(|_| ()) };
        tokio::pin!(wait, drain);
        let (status, drained) = tokio::select! {
            result = &mut wait => (result?, false),
            result = &mut drain => {
                result?;
                (wait.await?, true)
            }
        };
        drop(tree);
        if !drained {
            (
                status,
                timeout(Duration::from_secs(1), drain)
                    .await
                    .is_ok_and(|result| result.is_ok()),
            )
        } else {
            (status, true)
        }
    };
    let message = match status {
        "stopped" => Some("Command stopped by the user".into()),
        "timeout" => Some("Command timed out after 300 seconds".into()),
        _ if exit_code.is_none() => Some("Command terminated without an exit code".into()),
        _ => None,
    };
    let (stdout, stdout_truncated) = out.into_text();
    let (stderr, stderr_truncated) = err.into_text();
    Ok(ResultBody {
        status,
        exit_code,
        stdout,
        stderr,
        output_truncated: stdout_truncated || stderr_truncated || !drain,
        message,
    })
}

#[derive(Default)]
struct Tail {
    bytes: Vec<u8>,
    truncated: bool,
}

impl Tail {
    fn into_text(self) -> (String, bool) {
        let text = String::from_utf8_lossy(&self.bytes);
        let start = text.ceil_char_boundary(text.len().saturating_sub(OUTPUT_LIMIT));
        (text[start..].to_owned(), self.truncated || start != 0)
    }

    async fn read(&mut self, stream: &mut (impl AsyncRead + Unpin)) -> io::Result<()> {
        let mut chunk = [0; 4096];
        loop {
            let count = stream.read(&mut chunk).await?;
            if count == 0 {
                return Ok(());
            }
            self.bytes.extend_from_slice(&chunk[..count]);
            let remove = self.bytes.len().saturating_sub(OUTPUT_LIMIT);
            self.truncated |= remove != 0;
            self.bytes.drain(..remove);
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn asks_for_each_command_and_persists_automatic_approval() {
        let workspace = tempfile::tempdir().unwrap();
        let mode_path = workspace.path().join("command-mode.json");
        let app = Arc::new(App {
            host: String::new(),
            origin: String::new(),
            task: watch::channel(crate::run::Snapshot::new(None)).0,
            metrics: Default::default(),
            ram: Default::default(),
            startup_log: None,
            pi_agent_dir: None,
            benchmarks: crate::benchmark::Store::new(Some(
                workspace.path().join("benchmarks.json"),
            )),
            sessions: Default::default(),
            commands: std::sync::Mutex::new(State::new(mode_path.clone())),
            skills: super::super::skills::Store::new(workspace.path().join("skills")),
        });
        let trace = Call::start(app.clone(), "run_pi", None, "", None);
        let shell = "printf approved";
        for action in [Action::Allow, Action::Deny] {
            let check = async {
                let command = view(AppState(app.clone())).await.0.command.unwrap();
                assert!(!command.running);
                assert_eq!(
                    decide(AppState(app.clone()), RoutePath(command.id), Json(action)).await,
                    StatusCode::NO_CONTENT,
                );
            };
            let (result, ()) = tokio::join!(biased; run(&trace, workspace.path(), shell), check);
            match action {
                Action::Allow => assert_eq!(result.stdout, "approved"),
                Action::Deny => assert_eq!(result.status, "denied"),
                Action::Stop => unreachable!(),
            }
        }
        assert_eq!(
            set_mode(AppState(app.clone()), Json(Mode::AllowAll)).await,
            StatusCode::NO_CONTENT,
        );
        assert!(State::new(mode_path.clone()).mode == Mode::AllowAll);
        let result = timeout(Duration::from_secs(3), run(&trace, workspace.path(), shell))
            .await
            .unwrap();
        assert_eq!(result.stdout, "approved");
        assert_eq!(
            set_mode(AppState(app), Json(Mode::Ask)).await,
            StatusCode::NO_CONTENT,
        );
        assert!(State::new(mode_path).mode == Mode::Ask);
    }

    #[tokio::test]
    async fn preserves_exit_code_and_bounds_both_output_streams() {
        let (_sender, mut actions) = watch::channel(None);
        let result = execute(
            &std::env::temp_dir(),
            "i=0; while [ \"$i\" -lt 4000 ]; do printf é; i=$((i + 1)); done; printf out; printf err >&2; exit 7",
            &mut actions,
        )
        .await
        .unwrap();
        assert_eq!(result.status, "exited");
        assert_eq!(result.exit_code, Some(7));
        assert!(result.output_truncated);
        assert!(result.stdout.len() <= OUTPUT_LIMIT && result.stdout.ends_with("out"));
        assert_eq!(result.stderr, "err");
    }
}
