use std::{
    io,
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{ChildStdin, Command},
};

use crate::{context, ollama, run::ContextNotice};

use super::{
    commands, projects,
    sessions::{Call, Kind},
};

const ANSWER_LIMIT: usize = 8_000;
const COMMAND_EXTENSION: &str = include_str!("commands/pi-extension.ts");

mod review_diff;

#[derive(Clone, Copy, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Role {
    Implement,
    Fix,
    Review,
}

impl Role {
    fn instructions(self) -> io::Result<&'static str> {
        let manifest = match self {
            Self::Implement => super::skills::IMPLEMENT,
            Self::Fix => super::skills::FIX,
            Self::Review => super::skills::REVIEW,
        };
        super::skills::split_frontmatter(manifest)
            .map(|(_, body)| body)
            .map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!("Некорректный служебный скилл Pi: {error}"),
                )
            })
    }
}

struct BackgroundCommand(tokio::task::JoinHandle<commands::ResultBody>);

impl Drop for BackgroundCommand {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub(super) struct Task<'a> {
    pub(super) trace: &'a Call,
    pub(super) skills: &'a [PathBuf],
    pub(super) agents_file: &'a Path,
    pub(super) project: Option<&'a projects::Task>,
    pub(super) search_skill: Option<(&'a str, &'a Path)>,
    pub(super) origin: projects::MessageOrigin,
    pub(super) request_id: Option<&'a str>,
    pub(super) text_only: bool,
    pub(super) model: &'a str,
    pub(super) delegation: u64,
    pub(super) context_window: Option<u32>,
    pub(super) think: Option<&'a ollama::Think>,
    pub(super) role: Option<Role>,
    pub(super) writes: &'a [PathBuf],
}

pub(super) struct Reply {
    pub(super) text: String,
    pub(super) stop_reason: Option<String>,
    pub(super) truncated: bool,
    pub(super) error: Option<String>,
    pub(super) context_overflow: bool,
    successful_file_tools: [bool; 3],
}

pub(super) async fn check(model: &str, context_tokens: Option<u32>) -> Result<bool, String> {
    let nonce = nonce();
    let directory = TemporaryDirectory::new("check", nonce)
        .map_err(|error| format!("Не удалось создать папку проверки Pi: {error}"))?;
    let workspace = directory.0.join("workspace");
    tokio::fs::create_dir(&workspace)
        .await
        .map_err(|error| format!("Не удалось подготовить папку проверки Pi: {error}"))?;
    tokio::fs::write(
        workspace.join("input.txt"),
        format!("id={nonce}\nstatus=pending"),
    )
    .await
    .map_err(|error| format!("Не удалось создать тестовый файл Pi: {error}"))?;
    let config = directory.0.join("config");
    let reply = tokio::time::timeout(
        Duration::from_secs(120),
        run(
            &workspace,
            model,
            "Use the read tool to read input.txt. Use the write tool to copy its contents exactly into result.txt. Then use the edit tool on result.txt to replace status=pending with status=passed. Reply Done when finished.",
            &config,
            context_tokens,
            2048,
            None,
        ),
    )
    .await
    .map_err(|_| "Проверка Pi превысила 120 секунд".to_owned())??;
    if matches!(reply.stop_reason.as_deref(), Some("error" | "aborted")) {
        return Err(reply.error.unwrap_or_else(|| "Pi прервал проверку".into()));
    }
    if reply.error.is_some() || reply.truncated || reply.successful_file_tools != [true; 3] {
        return Ok(false);
    }
    let result = match tokio::fs::read_to_string(workspace.join("result.txt")).await {
        Ok(result) => result,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(format!(
                "Не удалось прочитать результат проверки Pi: {error}"
            ));
        }
    };
    Ok(result.trim_end_matches(['\r', '\n']).replace("\r\n", "\n")
        == format!("id={nonce}\nstatus=passed"))
}

fn nonce() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

struct TemporaryDirectory(PathBuf);

impl TemporaryDirectory {
    fn new(kind: &str, nonce: u128) -> io::Result<Self> {
        let path =
            std::env::temp_dir().join(format!("lyra-pi-{kind}-{}-{nonce}", std::process::id()));
        std::fs::create_dir(&path)?;
        Ok(Self(path))
    }
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub(super) async fn run(
    workspace: &Path,
    model: &str,
    prompt: &str,
    config: &Path,
    context_tokens: Option<u32>,
    limit: u32,
    task: Option<Task<'_>>,
) -> Result<Reply, String> {
    let project = task.as_ref().and_then(|task| task.project);
    let project_run = project
        .map(|_| TemporaryDirectory::new("project", nonce()))
        .transpose()
        .map_err(|error| format!("Could not prepare Pi project: {error}"))?;
    let runtime_config = match &project_run {
        Some(directory) => {
            let target = directory.0.join("config");
            tokio::fs::create_dir(&target)
                .await
                .map_err(|error| error.to_string())?;
            match tokio::fs::copy(config.join("settings.json"), target.join("settings.json")).await
            {
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(format!("Could not copy Lyra Pi settings: {error}")),
            }
            Some(target)
        }
        None => None,
    };
    let config = runtime_config.as_deref().unwrap_or(config);
    let source = model.to_owned();
    let backend =
        tokio::task::spawn_blocking(move || context::backend_model(&source, context_tokens))
            .await
            .map_err(|error| error.to_string())??;
    let context_window = task
        .as_ref()
        .and_then(|task| task.context_window)
        .or(context_tokens);
    configure_pi(config, &backend, model, context_window, limit)
        .await
        .map_err(|error| format!("Could not configure Pi: {error}"))?;
    if let Some(window) = task.as_ref().and_then(|task| {
        task.project?.session_file.as_ref()?;
        task.context_window
    }) {
        configure_chat_compaction(config, window)
            .await
            .map_err(|error| format!("Could not configure Pi history compaction: {error}"))?;
    }
    let trace = task.as_ref().map(|task| task.trace);
    let commands = trace;
    let text_only = task.as_ref().is_some_and(|task| task.text_only);
    let writes = task.as_ref().map_or(&[][..], |task| task.writes);
    let reviewing = task
        .as_ref()
        .is_some_and(|task| matches!(task.role, Some(Role::Review)));
    let skills = task.as_ref().map_or(&[][..], |task| task.skills);
    let project_instructions;
    let agents_file = if let (Some(project), Some(directory), Some(task)) =
        (project, &project_run, &task)
    {
        project_instructions = directory.0.join("AGENTS.md");
        write_project_instructions(&project_instructions, project, task)
            .await
            .map_err(|error| format!("Could not load project instructions: {error}"))?;
        Some(project_instructions.as_path())
    } else if let Some(task) = &task {
        match tokio::fs::metadata(task.agents_file).await {
            Ok(metadata) if metadata.is_file() => (metadata.len() > 0).then_some(task.agents_file),
            Ok(_) => return Err("Lyra AGENTS.md must be a file".into()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(format!("Could not access Lyra AGENTS.md: {error}")),
        }
    } else {
        None
    };
    if let Some(file) = project.and_then(|project| project.session_file.as_deref()) {
        tokio::fs::create_dir_all(file.parent().expect("Pi session directory"))
            .await
            .map_err(|error| format!("Could not prepare Pi session: {error}"))?;
    }
    let command_directory = commands
        .map(|_| TemporaryDirectory::new("commands", nonce()))
        .transpose()
        .map_err(|error| format!("Could not prepare Pi commands: {error}"))?;
    let extension = command_directory
        .as_ref()
        .map(|dir| dir.0.join("commands.ts"));
    if let Some(path) = &extension {
        tokio::fs::write(path, COMMAND_EXTENSION)
            .await
            .map_err(|error| format!("Could not prepare Pi command extension: {error}"))?;
    }
    let mut child = spawn_pi(
        workspace,
        &backend,
        config,
        extension.as_deref(),
        skills,
        agents_file,
        task.as_ref(),
    )
    .map_err(|error| format!("Could not start Pi: {error}"))?;
    let log = |kind, title: &str, text: &str, status| {
        if let Some(trace) = trace {
            trace.event(kind, title, text, status);
        }
    };
    log(
        Kind::Pi,
        "Pi",
        &workspace.display().to_string(),
        Some("running"),
    );
    let mut stdin = child.stdin.take();
    let input = if commands.is_none() {
        let mut stdin = stdin.take().expect("piped stdin");
        let prompt = prompt.to_owned();
        Some(tokio::spawn(async move {
            stdin.write_all(prompt.as_bytes()).await
        }))
    } else {
        None
    };
    let stderr = child.stderr.take().expect("piped stderr");
    let diagnostics = tokio::spawn(collect_diagnostics(stderr));
    let mut lines = BufReader::new(child.stdout.take().expect("piped stdout")).lines();
    let mut answer = String::new();
    let mut text_deltas = String::new();
    let mut delta_bytes = 0usize;
    let mut last_error = None;
    let mut stop_reason = None;
    let mut last_input_tokens = None;
    let mut text_clipped = false;
    let mut successful_file_tools = [false; 3];
    let mut ready = commands.is_none();
    let mut legacy_rpc = false;
    let mut background = None::<BackgroundCommand>;
    let mut first_thinking_delta = true;
    let startup_deadline = tokio::time::Instant::now() + Duration::from_secs(20);

    loop {
        let line = if ready {
            lines.next_line().await
        } else {
            tokio::time::timeout_at(startup_deadline, lines.next_line()).await
                .map_err(|_| "Pi command extension did not start. Check that Pi supports RPC extensions.")?
        }.map_err(|error| format!("Could not read Pi output: {error}"))?;
        let Some(line) = line else {
            break;
        };
        let Ok(event) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        match event["type"].as_str() {
            Some("auto_compaction_start") => {
                report_context(task.as_ref(), ContextNotice::Compacting)
            }
            Some("auto_compaction_end") if event["result"].is_object() => {
                report_context(task.as_ref(), ContextNotice::Compacted);
            }
            Some("auto_compaction_end") => report_context(task.as_ref(), ContextNotice::NearLimit),
            Some("extension_ui_request") if commands.is_some() => {
                let stdin = stdin.as_mut().expect("Pi RPC input");
                match event["method"].as_str() {
                    Some("notify") if event["message"] == "lyra:context-near-limit" => {
                        report_context(task.as_ref(), ContextNotice::NearLimit);
                    }
                    Some("notify") if event["message"] == "lyra:context-compacting" => {
                        report_context(task.as_ref(), ContextNotice::Compacting);
                    }
                    Some("notify") if event["message"] == "lyra:context-compacted" => {
                        report_context(task.as_ref(), ContextNotice::Compacted);
                    }
                    Some("notify")
                        if !ready
                            && matches!(
                                event["message"].as_str(),
                                Some("lyra:commands-ready" | "lyra:commands-ready-legacy")
                            ) =>
                    {
                        legacy_rpc = event["message"] == "lyra:commands-ready-legacy";
                        rpc_send(
                            stdin,
                            json!({"id": "task", "type": "prompt", "message": prompt}),
                        )
                        .await?;
                        ready = true;
                    }
                    Some("input")
                        if ready
                            && !writes.is_empty()
                            && event["title"] == "lyra:write-scoped-file" =>
                    {
                        let id = event["id"].as_str().ok_or("Missing Pi file request ID")?;
                        let request = event["placeholder"]
                            .as_str()
                            .ok_or("Missing Pi file request")?;
                        // Finish the filesystem mutation before yielding to cancellation.
                        let result = projects::write_scope::mutate(workspace, writes, request);
                        let result = match result {
                            Ok(()) => json!({"ok": true}),
                            Err(error) => json!({"ok": false, "message": error.to_string()}),
                        };
                        rpc_send(stdin, json!({
                            "type": "extension_ui_response", "id": id, "value": result.to_string()
                        })).await?;
                    }
                    Some("input")
                        if ready
                            && !text_only
                            && writes.is_empty()
                            && event["title"] == "lyra:run-command" =>
                    {
                        let id = event["id"]
                            .as_str()
                            .ok_or("Missing Pi command request ID")?;
                        let command = event["placeholder"].as_str().ok_or("Missing Pi command")?;
                        let result = if background.is_some() {
                            json!({"status": "busy", "message": "Collect the background command first, or use read_review_diff."})
                        } else {
                            serde_json::to_value(
                                commands::run(commands.expect("command trace"), workspace, command)
                                    .await,
                            )
                            .map_err(|error| error.to_string())?
                        };
                        rpc_send(
                            stdin,
                            json!({"type": "extension_ui_response", "id": id, "value": result.to_string()}),
                        )
                        .await?;
                    }
                    Some("input") if ready && reviewing => {
                        let id = event["id"]
                            .as_str()
                            .ok_or("Missing Pi review tool request ID")?;
                        let argument = event["placeholder"].as_str().unwrap_or_default();
                        let result = match event["title"].as_str() {
                            Some("lyra:review-resources") => {
                                let app = commands.expect("command trace").app();
                                let mut cache = app.metrics.lock().await;
                                cache.invalidate();
                                let metrics = cache.sample().await;
                                json!({
                                    "cpu_percent": metrics.cpu_percent, "ram": metrics.ram,
                                    "gpus": metrics.gpus, "loaded_models": metrics.models,
                                })
                            }
                            Some("lyra:start-background-command") if background.is_none() => {
                                let trace =
                                    Call::untracked(commands.expect("command trace").app_handle());
                                let workspace = workspace.to_path_buf();
                                let command = argument.to_owned();
                                background = Some(BackgroundCommand(tokio::spawn(async move {
                                    commands::run(&trace, &workspace, &command).await
                                })));
                                json!({"status": "pending", "message": "Command submitted through Lyra. Collect the result with wait_background_command."})
                            }
                            Some("lyra:start-background-command") => {
                                json!({"status": "busy", "message": "Collect the previous background result first."})
                            }
                            Some("lyra:wait-background-command") => {
                                if let Some(command) = &mut background {
                                    let result = (&mut command.0)
                                        .await
                                        .map_err(|error| error.to_string())?;
                                    background = None;
                                    serde_json::to_value(result)
                                        .map_err(|error| error.to_string())?
                                } else {
                                    json!({"status": "error", "message": "No background command to collect."})
                                }
                            }
                            Some("lyra:read-review-diff") => {
                                review_diff::read(workspace, argument).await
                            }
                            _ => continue,
                        };
                        rpc_send(stdin, json!({"type": "extension_ui_response", "id": id, "value": result.to_string()})).await?;
                    }
                    _ => {}
                }
            }
            Some("response") if event["id"] == "task" && event["success"] == false => {
                return Err(format!(
                    "Pi rejected the task: {}",
                    event["error"].as_str().unwrap_or("unknown error")
                ));
            }
            Some("agent_end") if legacy_rpc => {
                // Pi 0.x checks extension shutdown requests after handling an RPC command.
                rpc_send(
                    stdin.as_mut().expect("Pi RPC input"),
                    json!({"type": "get_state"}),
                )
                .await?;
            }
            Some("extension_error") if commands.is_some() => {
                return Err(format!(
                    "Pi command extension: {}",
                    event["error"].as_str().unwrap_or("unknown error")
                ));
            }
            Some("message_start") if event["message"]["role"] == "assistant" => {
                text_deltas.clear();
                delta_bytes = 0;
                first_thinking_delta = true;
            }
            Some("message_update") => {
                let update = &event["assistantMessageEvent"];
                if let Some(delta) = update["delta"].as_str() {
                    match update["type"].as_str() {
                        Some("thinking_delta") if !delta.is_empty() => {
                            if let Some(task) = &task {
                                task.trace.app().task.send_if_modified(|snapshot| {
                                    snapshot.append_chat_thinking(
                                        task.delegation,
                                        delta,
                                        first_thinking_delta,
                                    )
                                });
                            }
                            first_thinking_delta = false;
                            log(Kind::Thinking, "Pi", delta, None);
                        }
                        Some("text_delta") => {
                            delta_bytes = delta_bytes.saturating_add(delta.len());
                            append_tail(&mut text_deltas, delta, ANSWER_LIMIT);
                        }
                        _ => {}
                    }
                }
            }
            Some("tool_execution_start") => {
                let name = event["toolName"].as_str().unwrap_or("tool");
                if commands.is_some() && name == "run_command" {
                    continue;
                }
                let path = event["args"]["path"].as_str().unwrap_or_default();
                log(Kind::Tool, name, path, Some("running"));
            }
            Some("tool_execution_end") => {
                let name = event["toolName"].as_str().unwrap_or("tool");
                if commands.is_some() && name == "run_command" {
                    continue;
                }
                let status = if event["isError"] == true {
                    "error"
                } else {
                    if let Some(index) = ["read", "write", "edit"]
                        .iter()
                        .position(|tool| *tool == name)
                    {
                        successful_file_tools[index] = true;
                    }
                    "ok"
                };
                log(Kind::Tool, name, "", Some(status));
            }
            Some("message_end") if event["message"]["role"] == "assistant" => {
                let message = &event["message"];
                let reason = message["stopReason"].as_str();
                stop_reason = reason.map(str::to_owned);
                let usage = &message["usage"];
                last_input_tokens = usage["input"].as_u64().map(|input| {
                    input
                        .saturating_add(usage["cacheRead"].as_u64().unwrap_or_default())
                        .saturating_add(usage["cacheWrite"].as_u64().unwrap_or_default())
                });
                last_error = match reason {
                    Some("error" | "aborted") => Some(
                        message["errorMessage"]
                            .as_str()
                            .unwrap_or("Pi interrupted the reply")
                            .to_owned(),
                    ),
                    _ => None,
                };
                let content = message["content"].as_array().into_iter().flatten();
                let text = content
                    .filter(|block| block["type"] == "text")
                    .filter_map(|block| block["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
                let (text, text_bytes) = if text.is_empty() {
                    (text_deltas.as_str(), delta_bytes)
                } else {
                    (text.as_str(), text.len())
                };
                text_clipped = text_bytes > ANSWER_LIMIT;
                answer = text[..text.floor_char_boundary(ANSWER_LIMIT)].to_owned();
                if !answer.trim().is_empty() {
                    log(
                        Kind::Answer,
                        "Pi",
                        &answer,
                        (reason == Some("length") || text_clipped).then_some("partial"),
                    );
                }
            }
            _ => {}
        }
    }

    if background.is_some() {
        return Err("Проверка закончилась до получения результата фоновой команды.".into());
    }
    let status = child.wait().await.map_err(|error| error.to_string())?;
    let diagnostics = diagnostics.await.unwrap_or_default();
    if !ready {
        return Err(format!(
            "Pi command extension did not start: {}",
            diagnostics.trim()
        ));
    }
    if status.success() && last_error.is_none() {
        if let Some(input) = input {
            input
                .await
                .map_err(|error| error.to_string())?
                .map_err(|error| format!("Could not send task to Pi: {error}"))?;
        }
    } else {
        let reason = last_error
            .as_deref()
            .filter(|text| !text.is_empty())
            .unwrap_or(diagnostics.trim());
        last_error = Some(if reason.is_empty() {
            format!("Pi exited with {status}")
        } else {
            format!("Pi: {reason}")
        });
    }
    let truncated = text_clipped
        || matches!(
            stop_reason.as_deref(),
            Some("length" | "aborted" | "toolUse" | "deferred")
        );
    if last_error.is_none() {
        if answer.trim().is_empty() {
            last_error = Some(if stop_reason.as_deref() == Some("length") {
                format!(
                    "Pi reached its {limit}-token max_output_tokens budget before returning text"
                )
            } else {
                "Pi returned no text".into()
            });
        } else {
            let detail = if truncated {
                "Частичный ответ"
            } else {
                ""
            };
            log(Kind::Pi, "Pi", detail, Some("ok"));
        }
    }
    Ok(Reply {
        context_overflow: answer.trim().is_empty()
            && stop_reason.as_deref() == Some("length")
            && last_input_tokens
                .zip(context_window)
                .is_some_and(|(input, window)| input >= u64::from(window)),
        text: answer,
        stop_reason,
        truncated,
        error: last_error,
        successful_file_tools,
    })
}

fn report_context(task: Option<&Task<'_>>, notice: ContextNotice) {
    let Some(task) = task else { return };
    if task.context_window.is_some() {
        task.trace
            .app()
            .task
            .send_if_modified(|snapshot| snapshot.delegation_context(task.delegation, notice));
    }
}

pub(super) fn is_context_overflow(error: &str) -> bool {
    let error = error.to_ascii_lowercase();
    [
        "context_length_exceeded",
        "context_window_exceeded",
        "context length exceeded",
        "exceeds the context window",
        "exceeds the context length",
        "exceeds maximum context length",
        "maximum context length is",
        "context window exceeded",
        "exceeded max context length",
        "exceeds the maximum context length",
        "exceeds the available context size",
        "exceeds model's maximum context length",
        "prompt too long",
        "prompt is too long",
    ]
    .iter()
    .any(|phrase| error.contains(phrase))
}

async fn write_project_instructions(
    path: &Path,
    project: &projects::Task,
    task: &Task<'_>,
) -> io::Result<()> {
    use std::fmt::Write;

    async fn read(path: &Path) -> io::Result<String> {
        match tokio::fs::read_to_string(path).await {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(String::new()),
            result => result.map_err(|error| {
                io::Error::new(error.kind(), format!("{}: {error}", path.display()))
            }),
        }
    }

    let mut text = read(task.agents_file).await?;
    let _ = write!(
        text,
        "\n\n# Lyra project: {}\n\nPrimary working directory: {}\nWorking folders for this task (use absolute paths for files in other folders):\n",
        project.project.name,
        project.workspace.display()
    );
    for folder in &project.folders {
        let _ = writeln!(text, "- {}", folder.display());
    }
    for folder in &project.folders {
        let instructions = read(&folder.join("AGENTS.md")).await?;
        if !instructions.trim().is_empty() {
            let _ = write!(
                text,
                "\n## AGENTS.md for {}\nApply these instructions when working in this folder.\n\n{instructions}\n",
                folder.display()
            );
        }
    }
    if let Some((skill, manifest)) = task.search_skill {
        let instructions = tokio::fs::read_to_string(manifest).await?;
        let directory = manifest.parent().expect("search skill directory");
        let _ = write!(
            text,
            "\n## Project search: {skill}\n\nUse this skill's workflow for project search before using built-in search. Resolve skill resources relative to {}. If its commands are unavailable or denied, report that and use the available search tools where appropriate.\n\n{instructions}\n",
            directory.display()
        );
    }
    if !task.writes.is_empty() {
        let root = tokio::fs::canonicalize(&project.workspace).await?;
        let paths: Vec<_> = task
            .writes
            .iter()
            .filter_map(|path| path.strip_prefix(&root).ok())
            .map(|path| {
                path.to_string_lossy()
                    .replace(std::path::MAIN_SEPARATOR, "/")
            })
            .collect();
        if paths.len() != task.writes.len() {
            return Err(io::Error::other(
                "The primary working directory changed after reservation",
            ));
        }
        let _ = writeln!(
            text,
            "\nUse write and edit only on these reserved paths, relative to the primary working directory: {}. Shell commands are unavailable. The list can include new files. If another path is needed, report its exact relative path.",
            json!(paths),
        );
    }
    if task.text_only {
        text.push_str("\nAnswer normally using the conversation and instructions. File and shell tools are unavailable for this turn. Mention that only if needed for the user's request.\n");
    }
    if let Some(role) = task.role {
        text.push_str("\n\n");
        text.push_str(role.instructions()?);
    }
    if project.session_file.is_some() {
        text.push_str("\nEarlier turns and saved summaries, when present, belong to this same local Lyra chat even if the user switched models. Use the conversation you actually received to answer questions about earlier turns. Say if a specific detail is missing. Identify yourself by your model name or as Lyra.\n");
    }
    tokio::fs::write(path, text).await
}

async fn rpc_send(stdin: &mut ChildStdin, value: Value) -> Result<(), String> {
    stdin
        .write_all(format!("{value}\n").as_bytes())
        .await
        .map_err(|error| format!("Could not send RPC message to Pi: {error}"))
}

async fn configure_pi(
    directory: &Path,
    model: &str,
    name: &str,
    context_tokens: Option<u32>,
    limit: u32,
) -> io::Result<()> {
    let mut entry = serde_json::json!({ "id": model, "maxTokens": limit });
    if let Some(tokens) = context_tokens {
        entry["name"] = serde_json::json!(name);
        entry["contextWindow"] = serde_json::json!(tokens);
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

async fn configure_chat_compaction(directory: &Path, window: u32) -> io::Result<()> {
    let path = directory.join("settings.json");
    let mut settings: Value = match tokio::fs::read(&path).await {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(io::Error::other)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => json!({}),
        Err(error) => return Err(error),
    };
    let settings = settings
        .as_object_mut()
        .ok_or_else(|| io::Error::other("Pi settings must be an object"))?;
    let compaction = settings
        .entry("compaction")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| io::Error::other("Pi compaction settings must be an object"))?;
    // Pi's defaults target large windows. Scale defaults to leave room for a summary on small models.
    compaction
        .entry("reserveTokens")
        .or_insert_with(|| json!((window / 4).min(16_384)));
    compaction
        .entry("keepRecentTokens")
        .or_insert_with(|| json!((window / 2).min(20_000)));
    tokio::fs::write(
        path,
        serde_json::to_vec(settings).map_err(io::Error::other)?,
    )
    .await
}

fn spawn_pi(
    workspace: &Path,
    model: &str,
    pi_agent_dir: &Path,
    extension: Option<&Path>,
    skills: &[PathBuf],
    agents_file: Option<&Path>,
    task: Option<&Task<'_>>,
) -> io::Result<tokio::process::Child> {
    let session = task.and_then(|task| {
        task.project?
            .session_file
            .as_deref()
            .map(|path| (path, task))
    });
    let text_only = task.is_some_and(|task| task.text_only);
    let spawn = |program: &str| {
        let mut command = Command::new(program);
        command.env("PI_CODING_AGENT_DIR", pi_agent_dir);
        command.env_remove("LYRA_THINKING_EFFORT");
        if let Some(think) = task.and_then(|task| task.think) {
            let effort = match think {
                ollama::Think::Enabled(true) => "medium",
                ollama::Think::Enabled(false) => "none",
                ollama::Think::Level(level) => level,
            };
            command.env("LYRA_THINKING_EFFORT", effort);
        }
        command.args([
            "--provider",
            "ollama",
            "--model",
            model,
            "--mode",
            if extension.is_some() { "rpc" } else { "json" },
            "--no-extensions",
            "--no-skills",
            "--no-context-files",
        ]);
        command.env_remove("LYRA_PI_TEXT_ONLY");
        command.env_remove("LYRA_PI_REVIEW");
        command.env_remove("LYRA_PI_SCOPED_WRITE");
        let scoped_write = task.is_some_and(|task| !task.writes.is_empty());
        if scoped_write {
            command.env("LYRA_PI_SCOPED_WRITE", "1");
        }
        let reviewing = task.is_some_and(|task| matches!(task.role, Some(Role::Review)));
        if reviewing {
            command.env("LYRA_PI_REVIEW", "1");
        }
        if text_only {
            command.env("LYRA_PI_TEXT_ONLY", "1").arg("--no-tools");
        } else {
            let tools = if reviewing {
                "read,grep,find,ls,run_command,inspect_review_resources,start_background_command,wait_background_command,read_review_diff"
            } else if scoped_write {
                "read,grep,find,ls"
            } else if extension.is_some() {
                "read,grep,find,ls,write,edit,run_command"
            } else {
                "read,grep,find,ls,write,edit"
            };
            command.args(["--tools", tools]);
        }
        command.env_remove("LYRA_MESSAGE_REQUEST_ID");
        if let Some((path, task)) = session {
            command.env("LYRA_MESSAGE_ORIGIN", task.origin.as_str());
            command.env("LYRA_CHAT_MODEL", task.model);
            if let Some(request_id) = task.request_id {
                command.env("LYRA_MESSAGE_REQUEST_ID", request_id);
            }
            command.arg("--session").arg(path);
        } else {
            command.env_remove("LYRA_MESSAGE_ORIGIN");
            command.env_remove("LYRA_CHAT_MODEL");
            command.arg("--no-session");
        }
        if extension.is_some() || session.is_some() {
            command.arg("--no-prompt-templates");
        }
        if let Some(path) = extension {
            command.arg("--extension").arg(path);
        }
        if let Some(path) = agents_file {
            command.arg("--append-system-prompt").arg(path);
        }
        for skill in skills {
            command.arg("--skill").arg(skill);
        }
        command
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
