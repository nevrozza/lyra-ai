use std::{collections::HashSet, io, path::Path, process::Stdio, sync::Arc, time::Duration};

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

#[derive(Default)]
pub(super) struct State {
    next_id: u64,
    active: Option<Active>,
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
    AllowRepeat,
    Deny,
    Stop,
}

pub(super) fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/commands", get(view))
        .route("/commands/{id}", post(decide))
}

async fn view(AppState(app): AppState<Arc<App>>) -> Json<Option<View>> {
    let state = app.commands.lock().expect("command state lock");
    Json(state.active.as_ref().map(|active| active.view.clone()))
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

pub(super) async fn run(
    trace: &Call,
    workspace: &Path,
    command: &str,
    repeat: &mut HashSet<String>,
) -> ResultBody {
    if command.trim().is_empty() || command.len() > 4000 || command.contains('\0') {
        return ResultBody::error(
            "invalid",
            "Expected a nonempty command of at most 4000 bytes",
        );
    }
    let app = trace.app();
    let preapproved = repeat.contains(command);
    let mut actions = {
        let mut state = app.commands.lock().expect("command state lock");
        if state.active.is_some() {
            return ResultBody::error("busy", "Another command is active");
        }
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
        receiver
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

    let permitted = if preapproved {
        true
    } else {
        match timeout(APPROVAL_TIMEOUT, actions.wait_for(Option::is_some)).await {
            Ok(Ok(action)) => match *action {
                Some(Action::AllowRepeat) => {
                    repeat.insert(command.to_owned());
                    true
                }
                Some(Action::Allow) => true,
                _ => false,
            },
            _ => false,
        }
    };
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
    if matches!(result.status, "stopped" | "timeout") {
        repeat.remove(command);
    }
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
