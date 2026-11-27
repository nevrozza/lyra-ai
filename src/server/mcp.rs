use std::panic::AssertUnwindSafe;
use std::{fmt::Write, num::NonZeroU32, path::PathBuf, sync::Arc, time::Duration};

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use futures_util::FutureExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    benchmark, context, model_name, ollama,
    run::{AiAction, ContextNotice, DelegationScope},
    telemetry,
};

use super::{
    App,
    ai_settings::AI_SETTINGS,
    models::{execute_model_action, model_settings, save_description},
    pi,
    projects::{self, ChatMode},
    sessions::{Call, Kind},
};

const VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26"];
pub(super) fn routes() -> Router<Arc<App>> {
    Router::new().route("/mcp", post(handle))
}

async fn handle(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Json(message): Json<Value>,
) -> Response {
    if headers.get("mcp-protocol-version").is_some_and(|version| {
        !version
            .to_str()
            .is_ok_and(|version| VERSIONS.contains(&version))
    }) {
        return StatusCode::BAD_REQUEST.into_response();
    }

    let id = message.get("id");
    let method = message["method"].as_str();
    if message["jsonrpc"] != "2.0"
        || method.is_none()
        || id.is_some_and(|id| !id.is_string() && !id.is_number())
    {
        return rpc_error(&Value::Null, -32600, "Invalid request");
    }
    let Some(id) = id else {
        return StatusCode::ACCEPTED.into_response();
    };
    let params = &message["params"];
    let result = match method.unwrap_or_default() {
        "initialize" => {
            let Some(version) = params["protocolVersion"].as_str() else {
                return rpc_error(id, -32602, "protocolVersion required");
            };
            json!({
                "protocolVersion": if VERSIONS.contains(&version) { version } else { VERSIONS[0] },
                "serverInfo": {"name": "lyra", "version": env!("CARGO_PKG_VERSION")},
                "capabilities": {"tools": {}}
            })
        }
        "ping" => json!({}),
        "tools/list" => json!({"tools": tools()}),
        "tools/call" => {
            let Some(name) = params["name"].as_str() else {
                return rpc_error(id, -32602, "Tool name required");
            };
            if !matches!(
                name,
                "inspect"
                    | "set_description"
                    | "manage_model"
                    | "run_model"
                    | "run_pi"
                    | "list_projects"
                    | "save_project"
                    | "create_project_session"
                    | "set_project_session_settings"
                    | "get_project_session_mode"
                    | "set_project_session_mode"
                    | "read_project_session"
                    | "get_group_context"
                    | "set_group_context"
                    | "archive_project"
                    | "enqueue_project_tasks"
                    | "get_project_tasks"
                    | "cancel_project_task"
                    | "retry_project_task"
                    | "complete_project_step"
            ) {
                return rpc_error(id, -32602, "Unknown tool");
            }
            let mut args = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let Some(fields) = args.as_object_mut() else {
                return rpc_error(id, -32602, "Tool arguments must be an object");
            };
            let group = match fields.remove("session_id") {
                None => None,
                Some(Value::String(value)) if is_session_id(&value) => Some(value),
                _ => return rpc_error(id, -32602, "Invalid session_id"),
            };
            let model = args["model"]
                .as_str()
                .filter(|model| model.len() <= 120 && model_name(model).is_ok());
            let mut detail = model.unwrap_or_default().to_owned();
            if name == "manage_model"
                && let Some(action @ ("load" | "unload" | "pin" | "unpin")) =
                    args["action"].as_str()
            {
                if !detail.is_empty() {
                    detail.push_str(" · ");
                }
                detail.push_str(action);
            }
            if matches!(name, "inspect" | "run_model")
                && let Some(tokens) = args["context_tokens"].as_u64()
                && (1..=u64::from(u32::MAX)).contains(&tokens)
            {
                if !detail.is_empty() {
                    detail.push_str(" · ");
                }
                let _ = write!(detail, "контекст {tokens} токенов");
            }
            let mut trace = Call::start(app.clone(), name, model, &detail, group);
            let name = name.to_owned();
            // Keep the operation and its timeline going if its MCP connection closes.
            let result = tokio::spawn(async move {
                let result = call(app, &name, args, &mut trace).await;
                trace.finish(match &result {
                    Ok(output) if output.is_error => Some(&output.text),
                    Err(error) => Some(error),
                    Ok(_) => None,
                });
                result
            })
            .await
            .unwrap_or_else(|_| Err("Lyra tool task failed".into()));
            match result {
                Ok(output) => output.into_result(),
                Err(text) => json!({"isError": true, "content": [{"type": "text", "text": text}]}),
            }
        }
        _ => return rpc_error(id, -32601, "Method not found"),
    };
    Json(json!({"jsonrpc": "2.0", "id": id, "result": result})).into_response()
}

fn rpc_error(id: &Value, code: i32, message: &str) -> Response {
    Json(json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}))
        .into_response()
}

fn tools() -> Value {
    let complete_step = json!({
        "name": "complete_project_step",
        "description": "Complete a ready external step and optionally register its output files.",
        "inputSchema": {"type": "object", "properties": {
            "project_id": {"type": "integer", "minimum": 1},
            "project_session_id": {"type": "integer", "minimum": 1},
            "succeeded": {"type": "boolean"},
            "summary": {"type": "string"},
            "artifacts": {"type": "array", "maxItems": 16, "items": {"type": "string"}, "description": "Produced files relative to the primary folder."}
        }, "required": ["project_id", "project_session_id", "succeeded", "summary"], "additionalProperties": false}
    });
    let stage = json!({
        "type": "object",
        "properties": {"model": {"type": "string"}, "prompt": {"type": "string"}},
        "required": ["model", "prompt"],
        "additionalProperties": false
    });
    let follow_up = json!({
        "type": "object",
        "description": "Automatic review, fixes, and optional startup verification.",
        "properties": {
            "review": stage,
            "launch": stage,
            "max_fixes": {"type": "integer", "minimum": 0, "maximum": 8, "description": "Optional total fix limit across review rounds."}
        },
        "required": ["review"],
        "additionalProperties": false
    });
    let pi_output_limit = json!({"type": "integer", "minimum": 1, "maximum": u32::MAX, "description": "Optional Pi generation cap. Omit to use Pi's default."});
    let worker_role = json!({"enum": ["implement", "fix", "review"], "description": "Local worker role. Review uses read-only file tools."});
    let mut queue_task = json!({"type": "object", "properties": {
        "name": {"type": "string"}, "prompt": {"type": "string", "description": "Actionable technical specification for the step: goal, verified code context, concrete implementation or review plan, constraints, dependencies, acceptance checks and handoff."}, "model": {"type": "string", "description": "Installed model for a local task. Optional tool or model label for an external step."},
        "key": {"type": "string", "description": "Unique key for this task within the batch."},
        "after": {"type": "array", "maxItems": 16, "items": {"anyOf": [{"type": "string"}, {"type": "integer", "minimum": 1}]}, "description": "Earlier task keys in this batch or existing task chat IDs in the same project."},
        "external": {"type": "boolean", "default": false, "description": "Step completed outside Lyra with complete_project_step."},
        "parallel": {"type": "boolean", "default": false, "description": "Allow independent steps to overlap; Lyra locks files on mutation and checks read versions. Declare dependencies via after."},
        "role": worker_role,
        "workspace": {"type": "string", "description": "Optional registered project folder to use as the step execution root. Defaults to the first project folder."},
        "skills": {"type": "array", "items": {"type": "string"}, "description": "Optional enabled skill names. Omit for enabled non-role skills; disabled or removed names are rejected."},
        "context_tokens": {"type": "integer", "minimum": 1, "maximum": u32::MAX, "description": "Ollama context window for this task only."},
        "follow_up": follow_up,
        "max_output_tokens": pi_output_limit
    }, "required": ["name", "prompt"], "additionalProperties": false});
    queue_task["properties"]["role"]["default"] = json!("implement");
    let retry_task = json!({
        "name": "retry_project_task",
        "description": "Resume a task paused on context_full or output_limit while preserving its task ID and dependencies. Inspect saved tool evidence and continue only remaining work; do not retry user cancellation.",
        "inputSchema": {"type": "object", "properties": {
            "project_id": {"type": "integer", "minimum": 1},
            "project_session_id": {"type": "integer", "minimum": 1, "description": "Original queued task ID."},
            "context_tokens": {"type": "integer", "minimum": 1, "maximum": u32::MAX, "description": "Ollama context for the retry. For context_full, a same-chat retry requires a strictly larger window. For output_limit or a fresh chat, omit to keep the last window."},
            "fresh_chat": {"type": "boolean", "default": false, "description": "Start a new saved chat with the same task workspace and no previous messages."},
            "prompt": {"type": "string", "minLength": 1, "maxLength": 32000, "description": "Continuation prompt for the retry."}
        }, "required": ["project_id", "project_session_id", "prompt"], "additionalProperties": false}
    });
    let get_session_mode = json!({
        "name": "get_project_session_mode",
        "description": "Read the saved Chat or Work mode of a project chat.",
        "inputSchema": {"type": "object", "properties": {
            "project_id": {"type": "integer", "minimum": 1},
            "project_session_id": {"type": "integer", "minimum": 1}
        }, "required": ["project_id", "project_session_id"], "additionalProperties": false},
        "annotations": {"readOnlyHint": true}
    });
    let set_session_mode = json!({
        "name": "set_project_session_mode",
        "description": "Set the saved Chat or Work mode for subsequent dashboard and MCP messages in a project chat.",
        "inputSchema": {"type": "object", "properties": {
            "project_id": {"type": "integer", "minimum": 1},
            "project_session_id": {"type": "integer", "minimum": 1},
            "mode": {"enum": ["chat", "work"]}
        }, "required": ["project_id", "project_session_id", "mode"], "additionalProperties": false},
        "annotations": {"idempotentHint": true, "destructiveHint": false}
    });
    let mut tools = json!([
        {
            "name": "inspect",
            "description": "Read system load, local models, benchmarks, memory estimates, and Lyra queue settings.",
            "inputSchema": {"type": "object", "properties": {
                "model": {"type": "string"},
                "context_tokens": {"type": "integer", "minimum": 1, "maximum": u32::MAX, "description": "Estimate memory for this context size. Requires model."},
                "system": {"type": "boolean"}
            }, "additionalProperties": false},
            "annotations": {"readOnlyHint": true}
        },
        {
            "name": "set_description",
            "description": "Replace the saved description of a model.",
            "inputSchema": {"type": "object", "properties": {
                "model": {"type": "string"},
                "description": {"type": "string", "maxLength": 2000}
            }, "required": ["model", "description"], "additionalProperties": false},
            "annotations": {"idempotentHint": true}
        },
        {
            "name": "manage_model",
            "description": "Load for 5 minutes, pin indefinitely, unpin for expiry after 5 minutes, or unload one installed Ollama model.",
            "inputSchema": {"type": "object", "properties": {
                "model": {"type": "string", "description": "Model name from Lyra inspect"},
                "action": {"enum": ["load", "pin", "unpin", "unload"]}
            }, "required": ["model", "action"], "additionalProperties": false},
            "annotations": {"destructiveHint": false}
        },
        {
            "name": "run_model",
            "description": "Generate a bounded text response with an installed Ollama model.",
            "inputSchema": {"type": "object", "properties": {
                "model": {"type": "string", "description": "Model name from Lyra inspect"},
                "prompt": {"type": "string", "minLength": 1, "description": "Text prompt."},
                "context_tokens": {"type": "integer", "minimum": 1, "maximum": u32::MAX, "description": "Context window for this call. Saved to the model when Ollama finishes. Omit for the saved setting or Ollama default."},
                "max_output_tokens": {"type": "integer", "minimum": AI_SETTINGS.minimum_output_tokens, "maximum": AI_SETTINGS.maximum_output_tokens, "default": AI_SETTINGS.default_output_tokens, "description": "Generation budget. On reasoning models it can include thinking tokens and run out before any visible text."},
                "think": {"anyOf": [{"type": "boolean"}, {"type": "string", "minLength": 1}], "description": "Optional Thinking override. Pass false for Off, true for On, or a model-supported level. Omit to use the Lyra model setting or the Ollama default."},
                "require_loaded": {"type": "boolean", "description": "Use only a model already loaded with the requested or configured context"}
            }, "required": ["model", "prompt"], "additionalProperties": false},
            "annotations": {"destructiveHint": false}
        },
        {
            "name": "list_projects",
            "description": "List projects and saved Pi chats with IDs and archive status.",
            "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false},
            "annotations": {"readOnlyHint": true}
        },
        {
            "name": "archive_project",
            "description": "Archive or restore a project, a group with all its chats, or one saved chat.",
            "inputSchema": {"type": "object", "properties": {
                "project_id": {"type": "integer", "minimum": 1},
                "project_session_id": {"type": "integer", "minimum": 1, "description": "Target one chat; omit both this and group_id to target the project."},
                "group_id": {"type": "integer", "minimum": 1, "description": "Target all chats in a group; mutually exclusive with project_session_id."},
                "archived": {"type": "boolean"}
            }, "required": ["project_id", "archived"], "additionalProperties": false},
            "annotations": {"idempotentHint": true, "destructiveHint": false}
        },
        {
            "name": "save_project",
            "description": "Create a project or update its registered folders and metadata.",
            "inputSchema": {"type": "object", "properties": {
                "id": {"type": "integer", "minimum": 1},
                "name": {"type": "string", "minLength": 1, "maxLength": 120},
                "folders": {"type": "array", "minItems": 1, "maxItems": 16, "items": {"type": "string"}, "description": "Absolute directory paths. The first is the default."}
            }, "required": ["name", "folders"], "additionalProperties": false}
        },
        {
            "name": "create_project_session",
            "description": "Create a saved Pi chat in a project, optionally in a named task group.",
            "inputSchema": {"type": "object", "properties": {
                "project_id": {"type": "integer", "minimum": 1},
                "name": {"type": "string", "minLength": 1, "maxLength": 120},
                "group_id": {"type": "integer", "minimum": 1, "description": "Join an existing group in this project."},
                "group_name": {"type": "string", "minLength": 1, "maxLength": 120, "description": "Create a named group; mutually exclusive with group_id."},
                "group_brief": {"type": "string", "maxLength": 8000, "description": "Initial shared Markdown goal and constraints for a new group; requires group_name."},
                "mode": {"enum": ["chat", "work"], "default": "work", "description": "Initial saved mode."}
            }, "required": ["project_id", "name"], "additionalProperties": false}
        },
        {
            "name": "set_project_session_settings",
            "description": "Rename a saved chat. Its working folders are inherited from the project.",
            "inputSchema": {"type": "object", "properties": {
                "project_id": {"type": "integer", "minimum": 1},
                "project_session_id": {"type": "integer", "minimum": 1},
                "name": {"type": "string", "minLength": 1, "maxLength": 120}
            }, "required": ["project_id", "project_session_id", "name"], "additionalProperties": false}
        },
        {
            "name": "enqueue_project_tasks",
            "description": "Queue project steps in an optional named group with dependencies and parallel file scopes. Multi-step batches and automatic workflows are grouped by default.",
            "inputSchema": {"type": "object", "properties": {
                "project_id": {"type": "integer", "minimum": 1},
                "batch_id": {"type": "string", "description": "Optional unique ID for this batch. Reuse it only to retry the same submission."},
                "group_id": {"type": "integer", "minimum": 1, "description": "Join an existing task group in this project across batches."},
                "group_name": {"type": "string", "minLength": 1, "maxLength": 120, "description": "Create a group named for the overall task. Mutually exclusive with group_id."},
                "group_brief": {"type": "string", "maxLength": 8000, "description": "Initial shared goal and constraints, saved atomically before any new group task starts. For an existing group use set_group_context instead."},
                "tasks": {"type": "array", "minItems": 1, "maxItems": 16, "items": queue_task}
            }, "required": ["project_id", "tasks"], "additionalProperties": false}
        },
        {
            "name": "get_project_tasks",
            "description": "Read project task status, results, follow-ups, and context notices.",
            "inputSchema": {"type": "object", "properties": {
                "project_id": {"type": "integer", "minimum": 1},
                "task_ids": {"type": "array", "items": {"type": "integer", "minimum": 1}},
                "group_id": {"type": "integer", "minimum": 1, "description": "Filter tasks by their group ID."},
                "wait_seconds": {"type": "integer", "minimum": 0, "maximum": 25, "default": 0, "description": "Maximum wait for completion or a context pause."}
            }, "required": ["project_id"], "additionalProperties": false},
            "annotations": {"readOnlyHint": true}
        },
        {
            "name": "cancel_project_task",
            "description": "Cancel a queued or context-paused task and keep its chat.",
            "inputSchema": {"type": "object", "properties": {
                "project_id": {"type": "integer", "minimum": 1},
                "project_session_id": {"type": "integer", "minimum": 1}
            }, "required": ["project_id", "project_session_id"], "additionalProperties": false}
        },
        {
            "name": "get_group_context",
            "description": "Read a group's generated CONTEXT.md, including the shared brief, committed chat/queue results and statuses. Read before editing the brief or planning dependent work.",
            "inputSchema": {"type": "object", "properties": {
                "project_id": {"type": "integer", "minimum": 1},
                "group_id": {"type": "integer", "minimum": 1}
            }, "required": ["project_id", "group_id"], "additionalProperties": false},
            "annotations": {"readOnlyHint": true}
        },
        {
            "name": "set_group_context",
            "description": "Replace only the shared Markdown goal and constraints; automatic worker results are retained. Use brief_revision from get_group_context; stale edits are rejected.",
            "inputSchema": {"type": "object", "properties": {
                "project_id": {"type": "integer", "minimum": 1},
                "group_id": {"type": "integer", "minimum": 1},
                "brief": {"type": "string", "maxLength": 8000},
                "brief_revision": {"type": "integer", "minimum": 0}
            }, "required": ["project_id", "group_id", "brief", "brief_revision"], "additionalProperties": false}
        },
        {
            "name": "read_project_session",
            "description": "Read recent saved chat messages and compact tool calls/results, including confirmed operations and intentional user-stop notices. Pending or missing tool results are not proof of success. Never automatically resume a user-stopped task.",
            "inputSchema": {"type": "object", "properties": {
                "project_id": {"type": "integer", "minimum": 1},
                "project_session_id": {"type": "integer", "minimum": 1}
            }, "required": ["project_id", "project_session_id"], "additionalProperties": false},
            "annotations": {"readOnlyHint": true}
        },
        {
            "name": "run_pi",
            "description": "Run Pi with an installed Ollama model in Chat or Work mode.",
            "inputSchema": {"type": "object", "properties": {
                "model": {"type": "string"},
                "workspace": {"type": "string", "description": "Absolute folder path. Required for standalone runs. With project_id, choose a registered folder or omit for the default. Ordinary saved chats also inherit this default. Queued steps retain their reserved execution root."},
                "project_id": {"type": "integer", "minimum": 1, "description": "Lyra project to use. Loads instructions from its registered folders."},
                "project_session_id": {"type": "integer", "minimum": 1, "description": "Saved project chat to continue. Requires project_id."},
                "mode": {"enum": ["chat", "work"], "description": "Expected saved mode for an existing chat. For runs without a saved chat, selects the mode and defaults to Work."},
                "prompt": {"type": "string", "minLength": 1, "description": "Task prompt."},
                "allow_commands": {"type": "boolean", "deprecated": true, "description": "Accepted for older clients. Lyra's global command mode controls approval."},
                "skills": {"type": "array", "items": {"type": "string"}, "description": "Limit this run to enabled Lyra skill names. Omit for enabled non-role skills, or pass [] to run without optional skills. Disabled or removed names are rejected."},
                "context_tokens": {"type": "integer", "minimum": 1, "maximum": u32::MAX, "description": "Ollama context window for this run. Uses a separate model profile of this size without changing the saved default."},
                "role": worker_role,
                "max_output_tokens": pi_output_limit
            }, "required": ["model", "prompt"], "additionalProperties": false},
            "annotations": {"destructiveHint": false}
        }
    ]);
    tools.as_array_mut().expect("tool definitions").extend([
        complete_step,
        retry_task,
        get_session_mode,
        set_session_mode,
    ]);
    for tool in tools.as_array_mut().expect("tool definitions") {
        tool["inputSchema"]["properties"]["session_id"] = json!({
            "type": "string", "description": "Shared ID for related calls in the dashboard",
            "minLength": 1, "maxLength": 80, "pattern": "^[A-Za-z0-9._-]+$"
        });
        if matches!(tool["name"].as_str(), Some("run_model" | "run_pi")) {
            tool["outputSchema"] = json!({
                "type": "object",
                "properties": {
                    "stop_reason": {"type": ["string", "null"]},
                    "truncated": {"type": "boolean"},
                    "work_progress": {"type": "object", "description": "Work execution evidence, worker confirmation and final handoff status; not proof of task acceptance.", "properties": {
                        "generation_stop_reason": {"type": ["string", "null"]},
                        "confirmation_status": {"enum": ["unconfirmed", "completed", "blocked"]},
                        "confirmation_summary": {"type": "string"},
                        "handoff_complete": {"type": "boolean"},
                        "continuation_attempts": {"type": "integer"},
                        "confirmed_mutation_count": {"type": "integer"},
                        "recent_mutations": {"type": "array", "items": {"type": "object", "properties": {"tool": {"type": "string"}, "path": {"type": "string"}}, "required": ["tool", "path"]}}
                    }, "required": ["generation_stop_reason", "confirmation_status", "handoff_complete", "continuation_attempts", "confirmed_mutation_count", "recent_mutations"]},
                    "max_output_tokens": {"type": "integer"},
                    "context": {"type": "object", "properties": {
                        "context_tokens": {"type": "integer"},
                        "notice": {"enum": ["near_limit", "compacting", "compacted", "full"]}
                    }, "required": ["context_tokens"]}
                },
                "required": ["stop_reason", "truncated"]
            });
            if tool["name"] == "run_model" {
                tool["outputSchema"]["required"]
                    .as_array_mut()
                    .expect("required output fields")
                    .push(json!("max_output_tokens"));
            }
        }
    }
    tools
}

pub(super) fn is_session_id(value: &str) -> bool {
    (1..=80).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Inspect {
    model: Option<String>,
    context_tokens: Option<NonZeroU32>,
    #[serde(default)]
    system: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetDescription {
    model: String,
    description: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ManageModel {
    model: String,
    action: ollama::ModelAction,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RunModel {
    model: String,
    prompt: String,
    context_tokens: Option<NonZeroU32>,
    max_output_tokens: Option<u32>,
    think: Option<ollama::Think>,
    #[serde(default)]
    require_loaded: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RunPi {
    model: String,
    workspace: Option<PathBuf>,
    project_id: Option<u64>,
    project_session_id: Option<u64>,
    request_id: Option<String>,
    mode: Option<ChatMode>,
    context_tokens: Option<NonZeroU32>,
    #[serde(skip)]
    chat_session_id: Option<u64>,
    prompt: String,
    max_output_tokens: Option<u32>,
    skills: Option<Vec<String>>,
    #[serde(rename = "allow_commands")]
    _allow_commands: Option<bool>,
    role: Option<pi::Role>,
}

#[derive(Clone, Deserialize, Serialize)]
pub(super) struct PiContext {
    pub(super) context_tokens: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    notice: Option<ContextNotice>,
}

#[derive(Serialize)]
struct GenerationInfo {
    stop_reason: Option<String>,
    truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_output_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    text_only: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    context: Option<PiContext>,
    #[serde(skip_serializing_if = "Option::is_none")]
    work_progress: Option<pi::WorkProgress>,
}

struct ToolOutput {
    text: String,
    is_error: bool,
    generation: Option<GenerationInfo>,
}

impl ToolOutput {
    fn into_result(self) -> Value {
        let mut result = json!({"content": [{"type": "text", "text": self.text}]});
        if self.is_error {
            result["isError"] = json!(true);
        }
        if let Some(generation) = self.generation {
            result["structuredContent"] = json!(generation);
        }
        result
    }
}

async fn call(
    app: Arc<App>,
    name: &str,
    args: Value,
    trace: &mut Call,
) -> Result<ToolOutput, String> {
    let text = match name {
        "inspect" => {
            let request: Inspect = arguments(args)?;
            inspect(app, request).await
        }
        "set_description" => {
            let request: SetDescription = arguments(args)?;
            let model = installed_model(&request.model).await?;
            if app.task.borrow().closed() {
                return Err("Lyra is stopped".into());
            }
            match save_description(
                app,
                model.clone(),
                request.description,
                Some(AiAction::Description),
            )
            .await
            {
                StatusCode::NO_CONTENT => Ok(format!("Description saved for {model}")),
                StatusCode::BAD_REQUEST => Err("Description exceeds 2000 characters".into()),
                _ => Err("Could not save description".into()),
            }
        }
        "manage_model" => manage_model(app, arguments(args)?).await,
        "run_model" => return run_model(app, arguments(args)?, trace).await,
        "run_pi" => {
            return run_pi(
                app,
                arguments(args)?,
                trace,
                projects::MessageOrigin::Orchestrator,
                None,
            )
            .await;
        }
        "list_projects" => {
            let _: NoArguments = arguments(args)?;
            project_result(projects::work(app, |app| app.projects.catalog_summary()).await)
        }
        "save_project" => {
            let input = arguments(args)?;
            project_result(projects::work(app, |app| projects::save_project(app, input)).await)
        }
        "create_project_session" => {
            let input = arguments(args)?;
            project_result(projects::work(app, |app| projects::add_session(app, input)).await)
        }
        "set_project_session_settings" => {
            let input = arguments(args)?;
            project_result(
                projects::work(app, |app| projects::set_session_settings(app, input)).await,
            )
        }
        "get_project_session_mode" => {
            let input = arguments(args)?;
            project_result(projects::work(app, |app| app.projects.session_mode(input)).await)
        }
        "set_project_session_mode" => {
            let input = arguments(args)?;
            project_result(projects::work(app, |app| projects::set_session_mode(app, input)).await)
        }
        "get_group_context" => {
            let input = arguments(args)?;
            project_result(projects::work(app, move |app| app.projects.group_context(input)).await)
        }
        "set_group_context" => {
            let input = arguments(args)?;
            project_result(projects::work(app, move |app| projects::set_group_context(app, input)).await)
        }
        "read_project_session" => {
            let input = arguments(args)?;
            project_result(
                projects::work(app, |app| {
                    app.projects.history(input, &app.benchmarks, false)
                })
                .await,
            )
        }
        "archive_project" => {
            let input = arguments(args)?;
            project_result(projects::work(app, |app| projects::set_archive(app, input)).await)
        }
        "enqueue_project_tasks" => project_result(
            projects::queue::enqueue(app, arguments(args)?, projects::MessageOrigin::Orchestrator)
                .await,
        ),
        "retry_project_task" => project_result(projects::queue::retry(app, arguments(args)?).await),
        "get_project_tasks" => project_result(projects::queue::query(app, arguments(args)?).await),
        "complete_project_step" => {
            project_result(projects::queue::complete_external(app, arguments(args)?).await)
        }
        "cancel_project_task" => {
            projects::queue::cancel(app, arguments(args)?, false)
                .await
                .map_err(|(_, error)| error)?;
            Ok("Task cancelled".into())
        }
        _ => unreachable!(),
    }?;
    Ok(ToolOutput {
        text,
        is_error: false,
        generation: None,
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NoArguments {}

fn project_result(value: Result<impl Serialize, (StatusCode, String)>) -> Result<String, String> {
    serde_json::to_string(&value.map_err(|(_, error)| error)?).map_err(|error| error.to_string())
}

fn arguments<T: serde::de::DeserializeOwned>(args: Value) -> Result<T, String> {
    serde_json::from_value(args).map_err(|error| format!("Invalid arguments: {error}"))
}

pub(super) fn resolve_model(installed: &[String], name: &str) -> Result<String, String> {
    installed
        .iter()
        .find(|model| model.as_str() == name)
        .or_else(|| {
            installed
                .iter()
                .find(|model| ollama::same_model(model, name))
        })
        .cloned()
        .ok_or_else(|| format!("Model is not installed: {name}"))
}

async fn installed_model(requested: &str) -> Result<String, String> {
    let name = model_name(requested)?;
    let installed = tokio::task::spawn_blocking(ollama::installed_models)
        .await
        .ok()
        .flatten()
        .ok_or("Ollama unavailable")?;
    resolve_model(&installed, &name)
}

async fn manage_model(app: Arc<App>, request: ManageModel) -> Result<String, String> {
    let model = installed_model(&request.model).await?;
    if !app
        .task
        .send_if_modified(|snapshot| snapshot.begin_model_action(model.clone(), request.action))
    {
        return Err("Lyra is busy or stopped".into());
    }
    execute_model_action(
        app,
        model.clone(),
        request.action,
        Some(AiAction::Model(request.action)),
    )
    .await
    .map_err(|_| "Lyra model operation failed")??;
    let action = match request.action {
        ollama::ModelAction::Load => "load",
        ollama::ModelAction::Pin => "pin",
        ollama::ModelAction::Unpin => "unpin",
        ollama::ModelAction::Unload => "unload",
    };
    Ok(format!("model={model} action={action} ok"))
}

fn output_limit(prompt: &str, requested: Option<u32>) -> Result<u32, String> {
    let limit = requested.unwrap_or(AI_SETTINGS.default_output_tokens);
    if !(AI_SETTINGS.minimum_output_tokens..=AI_SETTINGS.maximum_output_tokens).contains(&limit) {
        return Err(format!(
            "max_output_tokens must be between {} and {}",
            AI_SETTINGS.minimum_output_tokens, AI_SETTINGS.maximum_output_tokens
        ));
    }
    if prompt.trim().is_empty() {
        return Err("Prompt is empty".into());
    }
    Ok(limit)
}

pub(super) fn validate_pi_generation(prompt: &str, limit: Option<u32>) -> Result<(), String> {
    if prompt.trim().is_empty() {
        return Err("Prompt is empty".into());
    }
    if limit == Some(0) {
        return Err("max_output_tokens must be positive when supplied".into());
    }
    Ok(())
}

async fn run_model(
    app: Arc<App>,
    request: RunModel,
    trace: &mut Call,
) -> Result<ToolOutput, String> {
    let limit = output_limit(&request.prompt, request.max_output_tokens)?;
    if matches!(&request.think, Some(ollama::Think::Level(level)) if level.trim().is_empty()) {
        return Err("think must be a boolean or a non-empty supported level".into());
    }
    let model = installed_model(&request.model).await?;
    let delegation = begin_delegation(&app, &model, DelegationScope::Standalone)?;
    trace.record_token_usage(limit, None, None, None);
    let store = app.clone();
    let source = model.clone();
    let execution = tokio::task::spawn_blocking(move || {
        let settings = model_settings(&store, &source)?;
        let tokens = settings.context_tokens;
        let requested = request.context_tokens.map(NonZeroU32::get);
        let reply = ollama::chat(
            &source,
            &request.prompt,
            limit,
            tokens,
            requested,
            request.require_loaded,
            request.think.or(settings.think),
        )?;
        let saved = if requested.is_some() && requested != tokens && reply.completed() {
            store
                .benchmarks
                .set_context(source, requested)
                .map(|()| true)
        } else {
            Ok(false)
        };
        Ok((reply, saved))
    })
    .await
    .map_err(|_| "Ollama chat task failed".to_owned())
    .and_then(|result| result);
    let context_changed = execution
        .as_ref()
        .is_ok_and(|(_, saved)| matches!(saved, Ok(true)));
    finish_delegation(&app, delegation, execution.is_ok(), context_changed).await;
    let (reply, saved) = execution?;
    trace.record_token_usage(
        limit,
        reply.prompt_eval_count,
        reply.eval_count,
        reply.stop_reason(),
    );
    let save_warning = saved.err().map_or_else(String::new, |error| {
        format!("\nLyra could not save the context setting: {error}")
    });
    let had_thinking = reply.thinking().is_some();
    let stop_reason = reply.stop_reason().map(str::to_owned);
    let generation = Some(GenerationInfo {
        truncated: stop_reason.as_deref() == Some("length") || !reply.completed(),
        stop_reason,
        max_output_tokens: Some(limit),
        text_only: None,
        context: None,
        work_progress: None,
    });
    if let Some(thinking) = reply.thinking() {
        trace.event(Kind::Thinking, "Ollama", thinking, None);
    }
    let (text, truncated) = match reply.into_text(limit) {
        Ok(answer) => answer,
        Err(error) => {
            return Ok(ToolOutput {
                text: format!("{error}{save_warning}"),
                is_error: true,
                generation,
            });
        }
    };
    trace.event(Kind::Answer, "Ollama", &text, None);
    let status = if truncated && had_thinking {
        " (output limit reached; the budget includes thinking)"
    } else if truncated {
        " (output limit reached)"
    } else {
        ""
    };
    Ok(ToolOutput {
        text: format!("{model}{status}\n{text}{save_warning}"),
        is_error: false,
        generation,
    })
}

async fn run_pi(
    app: Arc<App>,
    request: RunPi,
    trace: &Call,
    origin: projects::MessageOrigin,
    reserved: Option<u64>,
) -> Result<ToolOutput, String> {
    let limit = request.max_output_tokens;
    validate_pi_generation(&request.prompt, limit)?;
    let session_id = request.project_session_id;
    let (project, workspace) = if let Some(id) = request.project_id {
        let workspace = request.workspace;
        let chat_session_id = request.chat_session_id.or(session_id);
        let project = projects::work(app.clone(), move |app| {
            app.projects.task(id, chat_session_id, workspace.as_deref())
        })
        .await
        .map_err(|(_, error)| error)?;
        if reserved.is_none() && session_id.is_some_and(|id| project.project.has_pending_task(id)) {
            return Err("Дождитесь завершения задачи в очереди или отмените её.".into());
        }
        let workspace = project.workspace.clone();
        (Some(project), workspace)
    } else {
        if session_id.is_some() {
            return Err("project_session_id requires project_id".into());
        }
        let workspace = request.workspace.ok_or("Provide workspace or project_id")?;
        if !workspace.is_absolute()
            || !tokio::fs::metadata(&workspace)
                .await
                .is_ok_and(|metadata| metadata.is_dir())
        {
            return Err("workspace must be an absolute path to an existing directory".into());
        }
        let workspace = tokio::fs::canonicalize(&workspace)
            .await
            .map_err(|error| format!("Could not resolve workspace: {error}"))?;
        (None, workspace)
    };
    let saved_mode = project.as_ref().and_then(|project| project.mode);
    if let Some((expected, current)) = request.mode.zip(saved_mode)
        && expected != current
    {
        return Err(format!(
            "Режим чата изменился: сейчас {}, ожидался {}.",
            current.as_str(),
            expected.as_str(),
        ));
    }
    let mode = saved_mode.or(request.mode).unwrap_or_default();
    let model = installed_model(&request.model).await?;
    let dashboard = matches!(origin, projects::MessageOrigin::Dashboard);
    let store = app.clone();
    let source = model.clone();
    let text_only = mode == ChatMode::Chat;
    if text_only && request.role.is_some() {
        return Err("role requires Work mode".into());
    }
    let requested_context = request.context_tokens.map(NonZeroU32::get);
    let (tokens, context_window, think) = tokio::task::spawn_blocking(move || {
        let estimate = context::estimate(&source);
        if text_only && estimate.as_ref().and_then(|info| info.capabilities.as_ref())
            .is_some_and(|capabilities| !capabilities.iter().any(|item| item == "completion"))
        {
            return Err(format!("Модель {source} не поддерживает текстовые ответы."));
        }
        let unsupported = estimate.as_ref().and_then(|info| info.supports_agent_tools()) == Some(false);
        if unsupported && !text_only {
            return Err(if dashboard {
                format!("Модель {source} не поддерживает инструменты Pi для режима Work. Выберите другую модель или переключитесь на Chat.")
            } else {
                format!("{source} lacks Ollama text generation or tool calling support required for agentic Pi tasks. Inspect another model.")
            });
        }
        let record = model_settings(&store, &source)?;
        let failed = record.pi_check == Some(false);
        if failed && !text_only {
            return Err(if dashboard {
                format!("Модель {source} не прошла проверку Pi для режима Work. Выберите другую модель, повторите проверку или переключитесь на Chat.")
            } else {
                format!("{source} failed Lyra's Pi benchmark. Choose another model or rerun the Pi benchmark in Lyra.")
            });
        }
        if let Some(tokens) = requested_context {
            context::validate_window(&source, tokens, estimate.as_ref())?;
        }
        let tokens = requested_context.or(record.context_tokens);
        let context_window = tokens.map(u64::from)
            .or_else(|| {
                ollama::running_models()?.into_iter()
                    .find(|entry| ollama::same_model(&entry.name, &source))?.context_length
            })
            .or_else(|| estimate.and_then(|info| info.configured_context_tokens))
            .or_else(|| record.latest.as_ref().and_then(benchmark::Measurement::auto_context_tokens))
            .and_then(|tokens| u32::try_from(tokens).ok())
            .filter(|&tokens| tokens > 0)
            .unwrap_or(4096);
        Ok((tokens, context_window, record.think))
    })
    .await
    .map_err(|error| error.to_string())??;
    let scope = if project.is_some() {
        DelegationScope::Project {
            session_id,
            work: !text_only,
        }
    } else {
        DelegationScope::Standalone
    };
    let delegation = match reserved {
        Some(id) => id,
        None => begin_delegation(&app, &model, scope)?,
    };
    let skill_store = app.clone();
    let selected = if text_only { Some(Vec::new()) } else { request.skills };
    let role = request.role;
    let (skills, role_skill_enabled) = match tokio::task::spawn_blocking(move || {
        let skills = skill_store.skills.paths(selected.as_deref())?;
        let role_skill_enabled = match role {
            Some(role) if !text_only => skill_store.skills.enabled(role.skill_name())?,
            _ => false,
        };
        Ok::<_, String>((skills, role_skill_enabled))
    }).await {
        Ok(Ok(selection)) => selection,
        result => {
            if reserved.is_none() {
                if finish_delegation(&app, delegation, false, false).await {
                    return Ok(user_cancelled_output(trace, context_window, limit));
                }
            }
            return Err(match result {
                Ok(Err(error)) => error,
                _ => "Could not read installed Pi skills".into(),
            });
        }
    };
    let config = app
        .pi_agent_dir
        .clone()
        .unwrap_or_else(|| std::env::temp_dir().join(format!("lyra-pi-{}", std::process::id())));
    let result = {
        let mut state = app.task.subscribe();
        let task = pi::Task {
            trace,
            skills: &skills,
            agents_file: &app.agents_file,
            project: project.as_ref(),
            origin,
            text_only,
            model: &model,
            delegation,
            context_window: Some(context_window),
            think: think.as_ref(),
            role: request.role,
            role_skill_enabled,
            request_id: (dashboard || reserved.is_some())
                .then_some(request.request_id.as_deref())
                .flatten(),
        };
        tokio::select! {
            biased;
            cancelled = async {
                // Drop the watch borrow before cancellation can drop a pending command.
                state.wait_for(|snapshot| snapshot.closed() || snapshot.delegation_stopping(delegation))
                    .await.is_ok_and(|snapshot| !snapshot.closed())
            } => {
                if cancelled { None } else { Some(Err("Lyra stopped".into())) }
            },
            result = tokio::time::timeout(
                Duration::from_secs(900),
                AssertUnwindSafe(pi::run(&workspace, &model, &request.prompt, &config, tokens, limit, Some(task)))
                    .catch_unwind(),
            ) => Some(match result {
                Ok(Ok(reply)) => reply,
                Ok(Err(_)) => Err("Pi failed with an internal Lyra error".into()),
                Err(_) => Err("Pi timed out".into()),
            }),
        }
    };
    let Some(result) = result else {
        finish_delegation(&app, delegation, false, false).await;
        return Ok(user_cancelled_output(trace, context_window, limit));
    };
    let error = match &result {
        Ok(reply) => reply.error.as_deref(),
        Err(error) => Some(error.as_str()),
    };
    if let Some(error) = error {
        trace.event(Kind::Pi, "Pi", error, Some("error"));
    }
    let context_full = error.is_some_and(pi::is_context_overflow)
        || result.as_ref().is_ok_and(|reply| reply.context_overflow);
    if context_full {
        app.task.send_if_modified(|snapshot| {
            snapshot.delegation_context(delegation, ContextNotice::Full)
        });
    }
    let context = Some(PiContext {
        context_tokens: context_window,
        notice: app.task.borrow().context_notice(delegation),
    });
    if finish_delegation(&app, delegation, !context_full && error.is_none(), false).await {
        return Ok(user_cancelled_output(trace, context_window, limit));
    }
    if context_full {
        let work_progress = result.as_ref().ok().and_then(|reply| reply.work_progress.clone());
        let evidence = work_progress.as_ref().map(pi::WorkProgress::recovery_note).unwrap_or_default();
        return Ok(ToolOutput {
            text: format!(
                "Контекст модели заполнен. Окно Pi: {context_window} токенов. Увеличьте окно или начните новый чат с кратким состоянием задачи.{evidence}"
            ),
            is_error: true,
            generation: Some(GenerationInfo {
                stop_reason: Some("context_full".into()),
                truncated: true,
                max_output_tokens: limit,
                text_only: text_only.then_some(true),
                context,
                work_progress,
            }),
        });
    }
    let reply = result?;
    let is_error = reply.error.is_some();
    if reserved.is_none() && !is_error && !reply.text.trim().is_empty()
        && !matches!(reply.stop_reason.as_deref(), Some("aborted" | "error"))
        && let Some(project) = &project
        && let Some(session_id) = project.session_id
    {
        let project_id = project.project.id;
        let text = reply.text.clone();
        let truncated = reply.truncated;
        let timestamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default().as_millis() as u64;
        match projects::work(app.clone(), move |app| app.projects.record_group_answer(
            project_id, session_id, &text, truncated, timestamp
        )).await {
            Ok(true) => app.task.send_modify(crate::run::Snapshot::projects_changed),
            Ok(false) => {},
            Err((_, error)) => trace.event(Kind::Pi, "Group context", &error, Some("error")),
        }
    }
    let status = if reply.stop_reason.as_deref() == Some("length") {
        " (output limit reached)"
    } else if reply.truncated {
        " (partial answer)"
    } else {
        ""
    };
    let evidence = reply.work_progress.as_ref().map(pi::WorkProgress::recovery_note).unwrap_or_default();
    Ok(ToolOutput {
        text: reply
            .error
            .map(|error| format!("{error}{evidence}"))
            .unwrap_or_else(|| format!("{model}{status}\n{}", reply.text)),
        is_error,
        generation: Some(GenerationInfo {
            stop_reason: reply.stop_reason,
            truncated: reply.truncated,
            max_output_tokens: limit,
            text_only: text_only.then_some(true),
            context,
            work_progress: reply.work_progress,
        }),
    })
}

pub(super) fn queued_request(
    key: &projects::SessionKey,
    model: String,
    input: projects::queue::Prompt,
    role: Option<pi::Role>,
    chat_session_id: Option<u64>,
) -> RunPi {
    RunPi {
        model,
        prompt: input.prompt,
        max_output_tokens: input.max_output_tokens,
        skills: input.skills,
        context_tokens: input.context_tokens,
        chat_session_id,
        workspace: None,
        project_id: Some(key.project_id),
        project_session_id: Some(key.project_session_id),
        request_id: Some(format!("queue-{}", key.project_session_id)),
        mode: Some(ChatMode::Work),
        _allow_commands: None,
        role,
    }
}

pub(super) async fn run_queued(
    app: Arc<App>,
    request: RunPi,
    origin: projects::MessageOrigin,
    delegation: u64,
) -> (projects::queue::Status, projects::queue::JobResult) {
    use projects::queue::{JobResult, Status};

    let trace = Call::untracked(app.clone());
    let result = run_pi(app.clone(), request, &trace, origin, Some(delegation)).await;
    let mut pending = {
        let snapshot = app.task.borrow();
        snapshot
            .has_delegation(delegation)
            .then(|| snapshot.delegation_stopping(delegation))
    };
    if pending.is_some() {
        pending = Some(finish_delegation(&app, delegation, false, false).await);
    }
    match result {
        Ok(reply) => {
            let (stop_reason, truncated, context, work_progress) =
                reply.generation.map_or((None, false, None, None), |details| {
                    (details.stop_reason, details.truncated, details.context, details.work_progress)
                });
            let status = match stop_reason.as_deref() {
                Some(projects::USER_STOP_REASON) => Status::Cancelled,
                Some("context_full") => Status::ContextFull,
                Some("length") => Status::OutputLimit,
                Some("file_conflict" | "task_blocked" | "task_incomplete") => Status::Blocked,
                _ if reply.is_error => Status::Failed,
                _ if truncated || stop_reason.as_deref() != Some("stop") => Status::Blocked,
                _ => Status::Completed,
            };
            (
                status,
                JobResult {
                    text: reply.text,
                    stop_reason,
                    truncated,
                    context,
                    work_progress,
                },
            )
        }
        Err(text) => (
            if pending == Some(true) {
                Status::Cancelled
            } else {
                Status::Failed
            },
            JobResult {
                text: if pending == Some(true) { projects::USER_STOP_TEXT.into() } else { text },
                stop_reason: (pending == Some(true)).then(|| projects::USER_STOP_REASON.into()),
                truncated: false,
                context: None,
                work_progress: None,
            },
        ),
    }
}

pub(super) async fn run_project_chat(
    State(app): State<Arc<App>>,
    Json(request): Json<RunPi>,
) -> Result<Json<Value>, (StatusCode, String)> {
    if request.project_id.is_none() || request.project_session_id.is_none() {
        return Err((StatusCode::BAD_REQUEST, "Choose a project session".into()));
    }
    if request
        .request_id
        .as_deref()
        .is_some_and(|id| !is_session_id(id))
    {
        return Err((StatusCode::BAD_REQUEST, "Invalid request_id".into()));
    }
    tokio::spawn(async move {
        let trace = Call::untracked(app.clone());
        match run_pi(
            app,
            request,
            &trace,
            projects::MessageOrigin::Dashboard,
            None,
        )
        .await
        {
            Ok(reply) if !reply.is_error => Ok(Json(reply.into_result())),
            Ok(reply) => Err((StatusCode::BAD_REQUEST, reply.text)),
            Err(error) => Err((StatusCode::BAD_REQUEST, error)),
        }
    })
    .await
    .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?
}

#[derive(Deserialize)]
pub(super) struct StopProjectChat {
    delegation_id: u64,
    project_session_id: u64,
}

pub(super) async fn stop_project_chat(
    State(app): State<Arc<App>>,
    Json(request): Json<StopProjectChat>,
) -> StatusCode {
    if app.task.send_if_modified(|snapshot| {
        snapshot.stop_project_delegation(request.delegation_id, request.project_session_id)
    }) {
        let session_id = request.project_session_id;
        if let Err((_, error)) = projects::work(app.clone(), move |app| {
            app.projects.record_user_stop(session_id)
        }).await {
            eprintln!("Could not persist user stop: {error}");
            return StatusCode::INTERNAL_SERVER_ERROR;
        }
        app.task.send_modify(crate::run::Snapshot::projects_changed);
        StatusCode::NO_CONTENT
    } else {
        StatusCode::NOT_FOUND
    }
}

fn begin_delegation(app: &App, model: &str, scope: DelegationScope) -> Result<u64, String> {
    let mut id = None;
    app.task.send_if_modified(|snapshot| {
        id = snapshot.begin_delegation(model, scope);
        id.is_some()
    });
    id.ok_or_else(|| match scope {
        DelegationScope::Project { .. } | DelegationScope::Queued { .. } => {
            "Lyra is busy, stopped, or the project chat is already active".into()
        }
        DelegationScope::Standalone => "Lyra is busy or stopped".into(),
    })
}

fn user_cancelled_output(trace: &Call, context_window: u32, limit: Option<u32>) -> ToolOutput {
    let text = projects::USER_STOP_TEXT;
    trace.event(Kind::Pi, "Остановлено пользователем", text, Some("ok"));
    ToolOutput {
        text: text.into(),
        is_error: false,
        generation: Some(GenerationInfo {
            stop_reason: Some(projects::USER_STOP_REASON.into()),
            truncated: false,
            max_output_tokens: limit,
            text_only: None,
            context: Some(PiContext { context_tokens: context_window, notice: None }),
            work_progress: None,
        }),
    }
}

async fn finish_delegation(app: &App, id: u64, succeeded: bool, context_changed: bool) -> bool {
    app.metrics.lock().await.invalidate();
    let mut stopped = false;
    app.task.send_modify(|snapshot| {
        stopped = snapshot.delegation_stopping(id);
        snapshot.finish_delegation(id, succeeded && !stopped);
        if context_changed {
            snapshot.model_record_changed();
        }
    });
    stopped
}

async fn inspect(app: Arc<App>, request: Inspect) -> Result<String, String> {
    if request.context_tokens.is_some() && request.model.is_none() {
        return Err("context_tokens requires model".into());
    }
    let metrics = app.metrics.lock().await.sample().await;
    let mut output = format!("busy={}", app.task.borrow().busy());
    if request.model.is_none() {
        let store = app.clone();
        let skills = tokio::task::spawn_blocking(move || store.skills.list()).await
            .map_err(|_| "Could not read skills")?
            .map_err(|error| error.to_string())?;
        write!(
            output,
            "\n{}",
            json!({
                "skills": skills,
                "model_intelligence": app.project_models.get().await,
                "model_selection_policy": "Low is only for trivial, fully specified mechanical fixes (typos, literal replacements, formatting); never delegate implementation or fixes requiring reasoning to Low. Medium is the minimum for writing code, debugging and refactoring. High is preferred for complex implementation, architectural or subtle behavioral fixes and review. Escalate after a failed attempt; never downgrade merely to save memory. Unclassified models require evidence of sufficient capability.",
                "parallel_project_tasks": AI_SETTINGS.parallel_project_tasks,
                "parallel_project_models": AI_SETTINGS.parallel_project_models,
            })
        )
        .expect("writing to String");
    }
    if let Some(requested) = request.model {
        let model = resolve_model(
            metrics.installed_models().ok_or("Ollama unavailable")?,
            &model_name(&requested)?,
        )?;
        let store = app.clone();
        let source = model.clone();
        let (record, estimate) = tokio::task::spawn_blocking(move || {
            let record = store.benchmarks.get(&source)?;
            Ok::<_, std::io::Error>((record, context::estimate(&source).unwrap_or_default()))
        })
        .await
        .map_err(|_| "Could not read model details")?
        .map_err(|_| "Could not read benchmark data")?;
        let loaded = metrics.models.as_ref().map(|models| {
            models
                .iter()
                .filter(|entry| entry.name == model)
                .collect::<Vec<_>>()
        });
        let requested_tokens = request.context_tokens.map(|tokens| u64::from(tokens.get()));
        let resident = loaded.as_ref().and_then(|entries| {
            entries
                .iter()
                .find(|entry| {
                    entry.context_length
                        == requested_tokens.or(record.context_tokens.map(u64::from))
                })
                .or_else(|| {
                    if requested_tokens.is_some() {
                        entries.iter().find(|entry| !entry.is_pinned())
                    } else {
                        entries.first()
                    }
                })
                .copied()
        });
        let memory = memory_estimate(
            &metrics,
            &model,
            &record,
            &estimate,
            resident,
            requested_tokens,
        );
        let thinking_supported = if !estimate.thinking_options.is_empty() {
            Some(true)
        } else if estimate.thinking["values"].is_array() || estimate.capabilities.is_some() {
            Some(false)
        } else {
            None
        };
        write!(
            output,
            "\n{}",
            json!({
                "model": model,
                "benchmark_and_settings": record,
                "loaded": loaded,
                "memory_estimate": memory,
                "max_context_tokens": estimate.max_context_tokens,
                "ollama_agent_tools": match estimate.supports_agent_tools() {
                    Some(true) => "advertised",
                    Some(false) => "unsupported",
                    None => "unknown",
                },
                "thinking": {
                    "supported": thinking_supported,
                    "controls": estimate.thinking,
                    "counts_toward_max_output_tokens": thinking_supported
                }
            })
        )
        .expect("writing to String");
    } else if !request.system {
        append_summary(&mut output, &app, &metrics).await?;
    }

    if request.system {
        let ram = app.ram.lock().await.sample().await;
        write!(
            output,
            "\n{}",
            json!({
                "cpu_percent": metrics.cpu_percent, "ram": metrics.ram, "gpus": metrics.gpus,
                "ram_breakdown": ram
            })
        )
        .expect("writing to String");
    }
    Ok(output)
}

async fn append_summary(
    output: &mut String,
    app: &Arc<App>,
    metrics: &telemetry::Metrics,
) -> Result<(), String> {
    write!(
        output,
        "\nCPU {}%; RAM free/total GiB {}",
        number(metrics.cpu_percent),
        memory_pair(metrics.ram.as_ref())
    )
    .expect("writing to String");
    for gpu in &metrics.gpus {
        write!(
            output,
            "\nGPU {} {}%; VRAM free/total GiB {}",
            gpu.name,
            number(gpu.load.map(f64::from)),
            memory_pair(gpu.vram.as_ref())
        )
        .expect("writing to String");
    }
    let Some(names) = metrics.installed_models() else {
        output.push_str("\nOllama unavailable");
        return Ok(());
    };
    if names.is_empty() {
        output.push_str("\nNo local models");
        return Ok(());
    }
    let store = app.clone();
    let models = names.to_vec();
    let summaries = tokio::task::spawn_blocking(move || store.benchmarks.summaries(&models))
        .await
        .map_err(|_| "Could not read benchmark data")?
        .map_err(|_| "Could not read benchmark data")?;
    output.push_str("\nmodel | weights GiB | loaded | cold ms avg | last tok/s");
    for (model, (launch, speed)) in names.iter().zip(summaries) {
        let weights = metrics
            .model_sizes
            .as_ref()
            .and_then(|sizes| sizes.get(model))
            .copied();
        let loaded = metrics.models.as_ref().map_or("?", |entries| {
            let mut matches = entries
                .iter()
                .filter(|entry| entry.name == *model)
                .peekable();
            if matches.peek().is_none() {
                return "no";
            }
            if matches.any(ollama::Model::is_pinned) {
                "pinned"
            } else {
                "yes"
            }
        });
        write!(
            output,
            "\n{model} | {} | {loaded} | {} | {}",
            number(weights.map(gib)),
            number(launch),
            number(speed)
        )
        .expect("writing to String");
    }
    Ok(())
}

fn number(value: Option<f64>) -> String {
    value.map_or_else(|| "?".into(), |value| format!("{value:.1}"))
}

fn gib(bytes: u64) -> f64 {
    bytes as f64 / (1_u64 << 30) as f64
}

fn memory_pair(memory: Option<&telemetry::Memory>) -> String {
    memory.map_or_else(
        || "?".into(),
        |m| {
            format!(
                "{:.1}/{:.1}",
                gib(m.total.saturating_sub(m.used)),
                gib(m.total)
            )
        },
    )
}

pub(super) fn memory_estimate(
    metrics: &telemetry::Metrics,
    model: &str,
    record: &benchmark::Record,
    estimate: &context::Estimate,
    resident: Option<&ollama::Model>,
    requested_tokens: Option<u64>,
) -> Value {
    let tokens = requested_tokens
        .or(record.context_tokens.map(u64::from))
        .or(estimate.configured_context_tokens)
        .or_else(|| {
            resident
                .filter(|entry| !entry.context_profile)
                .and_then(|entry| entry.context_length)
        })
        .or_else(|| {
            record
                .latest
                .as_ref()
                .and_then(|latest| latest.auto_context_tokens())
        });
    let weights = metrics
        .model_sizes
        .as_ref()
        .and_then(|sizes| sizes.get(model))
        .copied();
    let kv = estimate
        .bytes_per_token_f16
        .zip(tokens)
        .and_then(|(bytes, tokens)| bytes.checked_mul(tokens));
    let base = weights.map(|weights| weights.saturating_add(kv.unwrap_or(0)));
    let compute = base.map(|bytes| (512 << 20).max(bytes.div_ceil(10)));
    let required = base
        .zip(compute)
        .map(|(base, compute)| base.saturating_add(compute));
    let ram = metrics
        .ram
        .as_ref()
        .map(|ram| room(ram, resident.and_then(ollama::Model::ram_bytes)));
    let mut gpus = metrics
        .gpus
        .iter()
        .filter_map(|gpu| gpu.vram.as_ref())
        .peekable();
    let first = gpus.next();
    let vram = first.map(|first| {
        if gpus.peek().is_none() {
            room(first, resident.and_then(|entry| entry.size_vram))
        } else {
            std::iter::once(first)
                .chain(gpus)
                .map(|vram| room(vram, None))
                .max()
                .unwrap_or(0)
        }
    });
    let ram_reserve = 1 << 30;
    let fallback_ram = required.map(|bytes| bytes.saturating_sub(vram.unwrap_or(0)));
    json!({
        "context_tokens": tokens, "kv_fp16_bytes_per_token": estimate.bytes_per_token_f16,
        "weights_bytes": weights, "kv_fp16_bytes": kv, "compute_buffer_bytes": compute,
        "total_bytes_excluding_unknown_kv": required, "kv_included": kv.is_some(),
        "ram_available_bytes": ram, "ram_reserve_bytes": ram_reserve,
        "best_gpu_vram_available_bytes": vram, "fallback_ram_bytes": fallback_ram,
        "ram_shortfall_bytes": ram.zip(fallback_ram).map(|(ram, needed)| needed.saturating_add(ram_reserve).saturating_sub(ram))
    })
}

fn room(memory: &telemetry::Memory, releasable: Option<u64>) -> u64 {
    memory
        .total
        .saturating_sub(memory.used.saturating_sub(releasable.unwrap_or(0)))
}
