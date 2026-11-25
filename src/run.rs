use std::{
    collections::VecDeque,
    io,
    path::{Path, PathBuf},
    process::Stdio,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use serde::Serialize;
use serde_json::Value;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::Command,
    sync::watch,
};

use crate::{benchmark, context, ollama::ModelAction};

const PROMPT: &str = "Create smoke.txt in the current directory with exactly this line: Pi работает\nThen use the read tool to read the file and report its content. Only access this file.";
const EXPECTED: &str = "Pi работает";
const TRACE_LIMIT: usize = 6_000;
const EVENT_LIMIT: usize = 32;

#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    #[default]
    Manual,
    Orchestrator,
}

#[derive(Serialize)]
pub struct Snapshot {
    model: Option<String>,
    mode: Mode,
    closed: bool,
    model_working: bool,
    delegated_model: Option<String>,
    pinned: Option<bool>,
    model_action: Option<ModelAction>,
    operation_model: Option<String>,
    model_error: Option<String>,
    benchmark_running: bool,
    benchmark_stage: Option<benchmark::Stage>,
    benchmark_revision: u64,
    status: &'static str,
    phase: String,
    started_at: Option<u128>,
    elapsed_ms: Option<u128>,
    workspace: Option<String>,
    timeline: VecDeque<Activity>,
    thinking: String,
    result: Option<String>,
}

#[derive(Serialize)]
struct Activity {
    at_ms: u128,
    kind: &'static str,
    text: String,
}

impl Snapshot {
    pub fn new(model: Option<String>) -> Self {
        Self {
            model,
            mode: Mode::default(),
            closed: false,
            model_working: false,
            delegated_model: None,
            pinned: None,
            model_action: None,
            operation_model: None,
            model_error: None,
            benchmark_running: false,
            benchmark_stage: None,
            benchmark_revision: 0,
            status: "idle",
            phase: "Готов к запуску".into(),
            started_at: None,
            elapsed_ms: None,
            workspace: None,
            timeline: VecDeque::new(),
            thinking: String::new(),
            result: None,
        }
    }

    pub fn start(&mut self) -> bool {
        if self.mode != Mode::Manual || self.model.is_none() || self.busy() {
            return false;
        }
        *self = Self {
            mode: self.mode,
            pinned: self.pinned,
            benchmark_revision: self.benchmark_revision,
            ..Self::new(self.model.clone())
        };
        self.status = "running";
        self.phase = "Запускаем Pi".into();
        self.started_at = Some(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
        );
        self.log(0, "info", "Запуск Pi".into());
        true
    }

    pub fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn set_mode(&mut self, mode: Mode) -> bool {
        if self.closed || self.mode == mode {
            return false;
        }
        self.mode = mode;
        true
    }

    pub fn closed(&self) -> bool {
        self.closed
    }

    pub fn close(&mut self) {
        self.closed = true;
    }

    pub fn busy(&self) -> bool {
        self.closed
            || self.status == "running"
            || self.delegated_model.is_some()
            || self.model_action.is_some()
            || self.benchmark_running
    }

    pub fn select_model(&mut self, model: String) -> bool {
        if self.mode != Mode::Manual || self.busy() {
            return false;
        }
        self.model = Some(model);
        self.pinned = None;
        self.model_error = None;
        true
    }

    pub fn begin_delegation(&mut self, model: String) -> bool {
        if self.busy() {
            return false;
        }
        self.delegated_model = Some(model);
        true
    }

    pub fn finish_delegation(&mut self) {
        self.delegated_model = None;
    }

    pub fn begin_model_action(&mut self, model: String, action: ModelAction) -> bool {
        if self.busy() {
            return false;
        }
        self.model_action = Some(action);
        self.operation_model = Some(model);
        self.model_error = None;
        true
    }

    pub fn finish_model_action(&mut self, action: ModelAction, result: Result<(), String>) {
        self.model_action = None;
        match result {
            Ok(())
                if self.operation_model == self.model && !matches!(action, ModelAction::Load) =>
            {
                self.pinned = Some(matches!(action, ModelAction::Pin))
            }
            Ok(()) => {}
            Err(error) => self.model_error = Some(error),
        }
    }

    pub fn begin_benchmark(&mut self, model: String) -> bool {
        if self.mode != Mode::Manual || self.busy() {
            return false;
        }
        self.benchmark_running = true;
        self.benchmark_stage = Some(benchmark::Stage::Preparing);
        self.operation_model = Some(model);
        self.model_error = None;
        true
    }

    pub fn set_benchmark_stage(&mut self, stage: benchmark::Stage) {
        self.benchmark_stage = Some(stage);
    }

    pub fn finish_benchmark(&mut self, result: Result<(), String>) {
        self.benchmark_running = false;
        self.benchmark_stage = None;
        match result {
            Ok(()) => self.model_record_changed(),
            Err(error) => self.model_error = Some(error),
        }
    }

    pub fn model_record_changed(&mut self) {
        self.benchmark_revision += 1;
    }

    fn log(&mut self, at_ms: u128, kind: &'static str, mut text: String) {
        text.truncate(text.floor_char_boundary(280));
        if self.timeline.len() == EVENT_LIMIT {
            self.timeline.pop_front();
        }
        self.timeline.push_back(Activity { at_ms, kind, text });
    }
}

pub async fn execute(
    state: watch::Sender<Snapshot>,
    pi_agent_dir: Option<PathBuf>,
    context_tokens: Result<Option<u32>, String>,
    before_finish: impl Future<Output = ()>,
) {
    let started = Instant::now();
    let model = state
        .borrow()
        .model
        .clone()
        .expect("running task has a model");
    let result = match context_tokens {
        Ok(tokens) => run_pi(&state, &model, pi_agent_dir.as_deref(), tokens, started).await,
        Err(error) => Err(error),
    };
    let elapsed_ms = started.elapsed().as_millis();
    before_finish.await;
    state.send_modify(|state| {
        state.elapsed_ms = Some(elapsed_ms);
        state.model_working = false;
        let (status, phase, mut message) = match result {
            Ok(()) => (
                "success",
                "Проверка пройдена",
                "Pi записал и прочитал smoke.txt. Содержимое файла проверено.".into(),
            ),
            Err(error) => ("error", "Проверка не пройдена", error),
        };
        state.status = status;
        state.phase = phase.into();
        message.truncate(message.floor_char_boundary(1_600));
        state.result = Some(message);
    });
}

async fn run_pi(
    state: &watch::Sender<Snapshot>,
    model: &str,
    pi_agent_dir: Option<&Path>,
    context_tokens: Option<u32>,
    started: Instant,
) -> Result<(), String> {
    let workspace = tokio::task::spawn_blocking(create_workspace)
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| format!("Не удалось создать тестовую папку: {error}"))?;
    state.send_modify(|state| state.workspace = Some(workspace.display().to_string()));

    let temporary_config = workspace.join("pi-agent");
    let config = pi_agent_dir.or(context_tokens.map(|_| temporary_config.as_path()));
    let source = model.to_owned();
    let backend =
        tokio::task::spawn_blocking(move || context::backend_model(&source, context_tokens))
            .await
            .map_err(|error| error.to_string())??;
    if let Some(directory) = config {
        configure_pi(directory, &backend, model, context_tokens)
            .await
            .map_err(|error| format!("Не удалось настроить модель для Pi: {error}"))?;
    }
    let mut child = spawn_pi(&workspace, &backend, config).map_err(|error| {
        format!("Не удалось запустить Pi. Проверьте `pi --version` в терминале: {error}")
    })?;
    let mut stdin = child.stdin.take().expect("piped stdin");
    let input = tokio::spawn(async move { stdin.write_all(PROMPT.as_bytes()).await });
    let stderr = child.stderr.take().expect("piped stderr");
    let diagnostics = tokio::spawn(collect_diagnostics(stderr));
    let mut lines = BufReader::new(child.stdout.take().expect("piped stdout")).lines();
    let mut last_error = None;
    let mut read_ok = false;
    let mut write_ok = false;

    while let Some(line) = lines
        .next_line()
        .await
        .map_err(|error| format!("Не удалось прочитать вывод Pi: {error}"))?
    {
        let Ok(event) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let at_ms = started.elapsed().as_millis();
        match event["type"].as_str() {
            Some("message_start") if event["message"]["role"] == "assistant" => {
                state.send_modify(|state| state.phase = "Модель генерирует ответ".into());
            }
            Some("message_update") => {
                let update = &event["assistantMessageEvent"];
                if update["type"] == "thinking_delta" {
                    if let Some(delta) = update["delta"].as_str() {
                        state.send_modify(|state| {
                            state.model_working = true;
                            state.phase = "Модель рассуждает".into();
                            append_tail(&mut state.thinking, delta, TRACE_LIMIT);
                        });
                    }
                } else if update["type"] == "text_start" || update["type"] == "toolcall_start" {
                    state.send_modify(|state| {
                        state.model_working = true;
                        state.phase = "Модель генерирует ответ".into();
                    });
                }
            }
            Some("tool_execution_start") => {
                let name = event["toolName"].as_str().unwrap_or("tool");
                let title = match name {
                    "write" => "Pi записывает файл".to_owned(),
                    "read" => "Pi читает файл".to_owned(),
                    _ => format!("Pi запускает {name}"),
                };
                let path = event["args"]["path"].as_str().unwrap_or_default();
                let text = if path.is_empty() {
                    title.clone()
                } else {
                    format!("{title} · {path}")
                };
                state.send_modify(|state| {
                    state.model_working = false;
                    state.phase = title;
                    state.log(at_ms, "info", text);
                });
            }
            Some("tool_execution_end") => {
                let name = event["toolName"].as_str().unwrap_or("tool");
                let failed = event["isError"] == true;
                read_ok |= name == "read" && !failed;
                write_ok |= name == "write" && !failed;
                let (kind, label) = if failed {
                    ("error", "ошибка")
                } else {
                    ("ok", "выполнено")
                };
                state.send_modify(|state| state.log(at_ms, kind, format!("{name} · {label}")));
            }
            Some("message_end") if event["message"]["role"] == "assistant" => {
                state.send_if_modified(|state| std::mem::replace(&mut state.model_working, false));
                let message = &event["message"];
                last_error = match message["stopReason"].as_str() {
                    Some("error" | "aborted") => Some(
                        message["errorMessage"]
                            .as_str()
                            .unwrap_or("Модель прервала ответ")
                            .to_owned(),
                    ),
                    _ => None,
                };
            }
            Some("agent_settled") => {
                state.send_modify(|state| state.phase = "Проверяем файл".into());
            }
            _ => {}
        }
    }

    let status = child.wait().await.map_err(|error| error.to_string())?;
    let diagnostics = diagnostics.await.unwrap_or_default();
    if !status.success() {
        let reason = last_error
            .as_deref()
            .filter(|text| !text.is_empty())
            .unwrap_or(diagnostics.trim());
        return Err(if reason.is_empty() {
            format!("Pi завершился с ошибкой ({status})")
        } else {
            format!("Pi: {reason}")
        });
    }
    input
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| format!("Не удалось передать задачу Pi: {error}"))?;
    if let Some(error) = last_error {
        return Err(format!("Модель: {error}"));
    }
    let contents = tokio::fs::read_to_string(workspace.join("smoke.txt"))
        .await
        .ok();
    if !read_ok
        || !write_ok
        || contents
            .as_deref()
            .map(|text| text.trim_end_matches(['\r', '\n']))
            != Some(EXPECTED)
    {
        return Err("Pi завершился, но проверка записи и чтения smoke.txt не прошла. Проверьте события и поддержку инструментов у модели.".into());
    }
    Ok(())
}

fn create_workspace() -> io::Result<std::path::PathBuf> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let path = std::env::temp_dir().join(format!("local-agent-{}-{timestamp}", std::process::id()));
    std::fs::create_dir(&path)?;
    Ok(path)
}

async fn configure_pi(
    directory: &Path,
    model: &str,
    name: &str,
    context_tokens: Option<u32>,
) -> io::Result<()> {
    let mut entry = serde_json::json!({ "id": model });
    if let Some(tokens) = context_tokens {
        entry["name"] = serde_json::json!(name);
        entry["contextWindow"] = serde_json::json!(tokens);
        entry["maxTokens"] = serde_json::json!((tokens / 2).min(16_384));
    }
    let config = serde_json::json!({
        "providers": {
            "ollama": {
                "baseUrl": "http://127.0.0.1:11434/v1",
                "api": "openai-completions",
                "apiKey": "ollama",
                "models": [entry]
            }
        }
    });
    tokio::fs::create_dir_all(directory).await?;
    tokio::fs::write(directory.join("models.json"), config.to_string()).await
}

fn spawn_pi(
    workspace: &Path,
    model: &str,
    pi_agent_dir: Option<&Path>,
) -> io::Result<tokio::process::Child> {
    let spawn = |program: &str| {
        let mut command = Command::new(program);
        if let Some(directory) = pi_agent_dir {
            command.env("PI_CODING_AGENT_DIR", directory);
        }
        command
            .args([
                "--provider",
                "ollama",
                "--model",
                model,
                "--mode",
                "json",
                "--no-session",
                "--no-extensions",
                "--no-skills",
                "--no-context-files",
                "--tools",
                "read,write",
            ])
            .current_dir(workspace)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
    };
    let result = spawn("pi");
    #[cfg(windows)]
    if result
        .as_ref()
        .is_err_and(|error| error.kind() == io::ErrorKind::NotFound)
    {
        // The npm installation uses a CMD shim. Task text always travels via stdin.
        return spawn("pi.cmd");
    }
    result
}

async fn collect_diagnostics(stderr: tokio::process::ChildStderr) -> String {
    let mut lines = BufReader::new(stderr).lines();
    let mut tail = String::new();
    while let Ok(Some(line)) = lines.next_line().await {
        if !line.trim().is_empty() {
            append_tail(&mut tail, &line, 1_200);
            append_tail(&mut tail, "\n", 1_200);
        }
    }
    tail
}

fn append_tail(buffer: &mut String, chunk: &str, limit: usize) {
    buffer.push_str(chunk);
    let remove = buffer.ceil_char_boundary(buffer.len().saturating_sub(limit));
    buffer.drain(..remove);
}
