use std::{io, path::Path, process::Stdio};

use serde_json::Value;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::Command,
};

use crate::context;

use super::sessions::{Call, Kind};

const ANSWER_LIMIT: usize = 8_000;

pub(super) struct Reply {
    pub(super) text: String,
    pub(super) stop_reason: Option<String>,
    pub(super) truncated: bool,
    pub(super) error: Option<String>,
}

pub(super) async fn run(
    workspace: &Path,
    model: &str,
    prompt: &str,
    config: &Path,
    context_tokens: Option<u32>,
    limit: u32,
    trace: &Call,
) -> Result<Reply, String> {
    let source = model.to_owned();
    let backend =
        tokio::task::spawn_blocking(move || context::backend_model(&source, context_tokens))
            .await
            .map_err(|error| error.to_string())??;
    configure_pi(config, &backend, model, context_tokens, limit)
        .await
        .map_err(|error| format!("Could not configure Pi: {error}"))?;
    let mut child = spawn_pi(workspace, &backend, config)
        .map_err(|error| format!("Could not start Pi: {error}"))?;
    trace.event(
        Kind::Pi,
        "Pi",
        &workspace.display().to_string(),
        Some("running"),
    );
    let mut stdin = child.stdin.take().expect("piped stdin");
    let prompt = prompt.to_owned();
    let input = tokio::spawn(async move { stdin.write_all(prompt.as_bytes()).await });
    let stderr = child.stderr.take().expect("piped stderr");
    let diagnostics = tokio::spawn(collect_diagnostics(stderr));
    let mut lines = BufReader::new(child.stdout.take().expect("piped stdout")).lines();
    let mut answer = String::new();
    let mut text_deltas = String::new();
    let mut delta_bytes = 0usize;
    let mut last_error = None;
    let mut stop_reason = None;
    let mut text_clipped = false;

    while let Some(line) = lines
        .next_line()
        .await
        .map_err(|error| format!("Could not read Pi output: {error}"))?
    {
        let Ok(event) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        match event["type"].as_str() {
            Some("message_start") if event["message"]["role"] == "assistant" => {
                text_deltas.clear();
                delta_bytes = 0;
            }
            Some("message_update") => {
                let update = &event["assistantMessageEvent"];
                if let Some(delta) = update["delta"].as_str() {
                    match update["type"].as_str() {
                        Some("thinking_delta") => trace.event(Kind::Thinking, "Pi", delta, None),
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
                let path = event["args"]["path"].as_str().unwrap_or_default();
                trace.event(Kind::Tool, name, path, Some("running"));
            }
            Some("tool_execution_end") => {
                let name = event["toolName"].as_str().unwrap_or("tool");
                let status = if event["isError"] == true {
                    "error"
                } else {
                    "ok"
                };
                trace.event(Kind::Tool, name, "", Some(status));
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
                    trace.event(
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
    if status.success() && last_error.is_none() {
        input
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| format!("Could not send task to Pi: {error}"))?;
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
            trace.event(Kind::Pi, "Pi", detail, Some("ok"));
        }
    }
    Ok(Reply {
        text: answer,
        stop_reason,
        truncated,
        error: last_error,
    })
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
) -> io::Result<tokio::process::Child> {
    let spawn = |program: &str| {
        let mut command = Command::new(program);
        command.env("PI_CODING_AGENT_DIR", pi_agent_dir);
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
                "read,grep,find,ls,write,edit",
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
