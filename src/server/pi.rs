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

use crate::context;

use super::{
    commands, projects,
    sessions::{Call, Kind},
};

const ANSWER_LIMIT: usize = 8_000;
const COMMAND_EXTENSION: &str = include_str!("commands/pi-extension.ts");

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
}

pub(super) struct Reply {
    pub(super) text: String,
    pub(super) stop_reason: Option<String>,
    pub(super) truncated: bool,
    pub(super) error: Option<String>,
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
    configure_pi(config, &backend, model, context_tokens, limit)
        .await
        .map_err(|error| format!("Could not configure Pi: {error}"))?;
    let trace = task.as_ref().map(|task| task.trace);
    let commands = trace;
    let text_only = task.as_ref().is_some_and(|task| task.text_only);
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
    let mut text_clipped = false;
    let mut successful_file_tools = [false; 3];
    let mut ready = commands.is_none();
    let mut legacy_rpc = false;
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
            Some("extension_ui_request") if commands.is_some() => {
                let stdin = stdin.as_mut().expect("Pi RPC input");
                match event["method"].as_str() {
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
                        if ready && !text_only && event["title"] == "lyra:run-command" =>
                    {
                        let id = event["id"]
                            .as_str()
                            .ok_or("Missing Pi command request ID")?;
                        let command = event["placeholder"].as_str().ok_or("Missing Pi command")?;
                        let result =
                            commands::run(commands.expect("command trace"), workspace, command)
                                .await;
                        let value =
                            serde_json::to_string(&result).map_err(|error| error.to_string())?;
                        rpc_send(
                            stdin,
                            json!({"type": "extension_ui_response", "id": id, "value": value}),
                        )
                        .await?;
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
            }
            Some("message_update") => {
                let update = &event["assistantMessageEvent"];
                if let Some(delta) = update["delta"].as_str() {
                    match update["type"].as_str() {
                        Some("thinking_delta") => log(Kind::Thinking, "Pi", delta, None),
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
        text: answer,
        stop_reason,
        truncated,
        error: last_error,
        successful_file_tools,
    })
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
    if task.text_only {
        text.push_str("\nThis turn is text-only. Answer from the available conversation and instructions. File and shell tools are unavailable. If the task needs them, say so.\n");
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
        if text_only {
            command.env("LYRA_PI_TEXT_ONLY", "1").arg("--no-tools");
        } else {
            let tools = if extension.is_some() {
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
