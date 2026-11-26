use std::{fmt::Write, num::NonZeroU32, path::PathBuf, sync::Arc, time::Duration};

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
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
    pi, projects,
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
                "capabilities": {"tools": {}},
                "instructions": AI_SETTINGS.orchestrator_instructions
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
                    | "set_project_session_folders"
                    | "read_project_session"
                    | "archive_project"
                    | "enqueue_project_tasks"
                    | "get_project_tasks"
                    | "cancel_project_task"
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
        "description": "Report the result of a ready external step you executed with Codex or a connected tool. On success you can register existing files relative to the step's primary folder. This releases dependent tasks.",
        "inputSchema": {"type": "object", "properties": {
            "project_id": {"type": "integer", "minimum": 1},
            "project_session_id": {"type": "integer", "minimum": 1},
            "succeeded": {"type": "boolean"},
            "summary": {"type": "string"},
            "artifacts": {"type": "array", "maxItems": 16, "items": {"type": "string"}}
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
        "description": "Pick a review model once. It reviews the full task diff, plans small fixes with a model for each, and reviews the full diff again after each batch. Up to three reviews. Follow-up stages run sequentially.",
        "properties": {
            "review": stage,
            "launch": stage,
            "max_fixes": {"type": "integer", "minimum": 0, "maximum": 8, "description": "Optional total fix limit across review rounds."}
        },
        "required": ["review"],
        "additionalProperties": false
    });
    let mut tools = json!([
        {
            "name": "inspect",
            "description": "System load and local model shortlist. Pass model for benchmark, memory, thinking controls, the Pi benchmark result (benchmark_and_settings.pi_check), and Ollama tool capabilities (ollama_agent_tools). Pass system=true for detailed system and RAM usage.",
            "inputSchema": {"type": "object", "properties": {
                "model": {"type": "string"},
                "context_tokens": {"type": "integer", "minimum": 1, "maximum": u32::MAX, "description": "Estimate memory for this context size. Requires model."},
                "system": {"type": "boolean"}
            }, "additionalProperties": false},
            "annotations": {"readOnlyHint": true}
        },
        {
            "name": "set_description",
            "description": "Replace one model's description only when the user explicitly requests the edit.",
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
            "description": "Send a bounded text task to one installed Ollama model. Use the user's chosen model or choose from inspect by task fit, memory and speed.",
            "inputSchema": {"type": "object", "properties": {
                "model": {"type": "string", "description": "Model name from Lyra inspect"},
                "prompt": {"type": "string", "minLength": 1, "description": "Task text. Put any Thinking override in the separate think argument. Do not insert /think, /no_think, or other Thinking directives."},
                "context_tokens": {"type": "integer", "minimum": 1, "maximum": u32::MAX, "description": "Context window for this call. Saved to the model when Ollama finishes. Omit for the saved setting or Ollama default."},
                "max_output_tokens": {"type": "integer", "minimum": AI_SETTINGS.minimum_output_tokens, "maximum": AI_SETTINGS.maximum_output_tokens, "default": AI_SETTINGS.default_output_tokens, "description": "Generation budget. On reasoning models it can include thinking tokens and run out before any visible text."},
                "think": {"anyOf": [{"type": "boolean"}, {"type": "string", "minLength": 1}], "description": "Optional Thinking override. Pass false for Off, true for On, or a model-supported level. Omit to use the Lyra model setting or the Ollama default."},
                "require_loaded": {"type": "boolean", "description": "Use only a model already loaded with the requested or configured context"}
            }, "required": ["model", "prompt"], "additionalProperties": false},
            "annotations": {"destructiveHint": false}
        },
        {
            "name": "list_projects",
            "description": "List Lyra projects and saved Pi chats with their IDs and archive status. Only active projects and chats can be used with run_pi.",
            "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false},
            "annotations": {"readOnlyHint": true}
        },
        {
            "name": "archive_project",
            "description": "On explicit user request, archive or restore a Lyra project or one of its chats. Archived history remains readable.",
            "inputSchema": {"type": "object", "properties": {
                "project_id": {"type": "integer", "minimum": 1},
                "project_session_id": {"type": "integer", "minimum": 1, "description": "Omit to change the entire project."},
                "archived": {"type": "boolean"}
            }, "required": ["project_id", "archived"], "additionalProperties": false},
            "annotations": {"idempotentHint": true, "destructiveHint": false}
        },
        {
            "name": "save_project",
            "description": "Create a Lyra project or update it by id. Register only the local folders the user wants in the project.",
            "inputSchema": {"type": "object", "properties": {
                "id": {"type": "integer", "minimum": 1},
                "name": {"type": "string", "minLength": 1, "maxLength": 120},
                "folders": {"type": "array", "minItems": 1, "maxItems": 16, "items": {"type": "string"}, "description": "Absolute directory paths. The first is the default."},
                "search_skill": {"type": ["string", "null"], "description": "Installed Lyra skill to prefer for project searches, such as ast-index."}
            }, "required": ["name", "folders"], "additionalProperties": false}
        },
        {
            "name": "create_project_session",
            "description": "Create a persistent Pi chat in a Lyra project. Continue the chat using project_id and the returned id as project_session_id in run_pi.",
            "inputSchema": {"type": "object", "properties": {
                "project_id": {"type": "integer", "minimum": 1},
                "name": {"type": "string", "minLength": 1, "maxLength": 120},
                "folders": {"type": "array", "minItems": 1, "maxItems": 16, "items": {"type": "string"}, "description": "Registered working folders for this chat. The first is the working directory. Omit to use all project folders."},
                "folder": {"type": "string", "description": "Legacy option to set only the working directory. Use folders for new clients."}
            }, "required": ["project_id", "name"], "additionalProperties": false}
        },
        {
            "name": "set_project_session_folders",
            "description": "Change a saved chat's working folders. Keep its existing primary folder first.",
            "inputSchema": {"type": "object", "properties": {
                "project_id": {"type": "integer", "minimum": 1},
                "project_session_id": {"type": "integer", "minimum": 1},
                "folders": {"type": "array", "minItems": 1, "maxItems": 16, "items": {"type": "string"}}
            }, "required": ["project_id", "project_session_id", "folders"], "additionalProperties": false}
        },
        {
            "name": "enqueue_project_tasks",
            "description": "Queue tasks or build a project pipeline. Each gets a numbered saved chat. Dependencies must finish successfully before their consumers run. External steps wait for complete_project_step. Ready local tasks run in order unless they explicitly allow parallel execution.",
            "inputSchema": {"type": "object", "properties": {
                "project_id": {"type": "integer", "minimum": 1},
                "batch_id": {"type": "string", "description": "Optional unique ID for this batch. Reuse it only to retry the same submission."},
                "tasks": {"type": "array", "minItems": 1, "maxItems": 16, "items": {"type": "object", "properties": {
                    "name": {"type": "string"}, "prompt": {"type": "string", "description": "Task text. Thinking for local tasks follows the model configuration. Do not insert Thinking directives into the prompt."}, "model": {"type": "string", "description": "Installed model for a local task. Optional tool or model label for an external step."},
                    "key": {"type": "string", "description": "Unique key for this task within the batch."},
                    "after": {"type": "array", "maxItems": 16, "items": {"anyOf": [{"type": "string"}, {"type": "integer", "minimum": 1}]}, "description": "Earlier task keys in this batch or existing task chat IDs in the same project."},
                    "external": {"type": "boolean", "default": false, "description": "Codex or a connected specialist tool executes this step and reports completion."},
                    "parallel": {"type": "boolean", "default": false, "description": "Allow overlapping work. Choose this for independent tasks when there is enough memory."},
                    "folders": {"type": "array", "items": {"type": "string"}, "description": "Optional registered project folders, primary first. Defaults to the project's folders."},
                    "skills": {"type": "array", "items": {"type": "string"}},
                    "follow_up": follow_up,
                    "max_output_tokens": {"type": "integer", "minimum": AI_SETTINGS.minimum_output_tokens, "maximum": AI_SETTINGS.maximum_output_tokens}
                }, "required": ["name", "prompt"], "additionalProperties": false}}
            }, "required": ["project_id", "tasks"], "additionalProperties": false}
        },
        {
            "name": "get_project_tasks",
            "description": "Read queue status and results. Use task_ids from enqueue_project_tasks and wait_seconds to wait for those tasks and their automatic follow-up stages to finish or until the timeout.",
            "inputSchema": {"type": "object", "properties": {
                "project_id": {"type": "integer", "minimum": 1},
                "task_ids": {"type": "array", "items": {"type": "integer", "minimum": 1}},
                "wait_seconds": {"type": "integer", "minimum": 0, "maximum": 25, "default": 0}
            }, "required": ["project_id"], "additionalProperties": false},
            "annotations": {"readOnlyHint": true}
        },
        {
            "name": "cancel_project_task",
            "description": "Cancel a task waiting in the queue and keep its chat. For running work use the Stop control in its chat.",
            "inputSchema": {"type": "object", "properties": {
                "project_id": {"type": "integer", "minimum": 1},
                "project_session_id": {"type": "integer", "minimum": 1}
            }, "required": ["project_id", "project_session_id"], "additionalProperties": false}
        },
        {
            "name": "read_project_session",
            "description": "Read recent user and assistant messages from a saved Pi chat.",
            "inputSchema": {"type": "object", "properties": {
                "project_id": {"type": "integer", "minimum": 1},
                "project_session_id": {"type": "integer", "minimum": 1}
            }, "required": ["project_id", "project_session_id"], "additionalProperties": false},
            "annotations": {"readOnlyHint": true}
        },
        {
            "name": "run_pi",
            "description": "Work with files in a Pi workspace and request shell commands under Lyra's global Pi command mode. Before choosing or loading a model, use inspect to check benchmark_and_settings.pi_check and ollama_agent_tools.",
            "inputSchema": {"type": "object", "properties": {
                "model": {"type": "string"},
                "workspace": {"type": "string", "description": "Absolute folder path. Required for standalone runs. With project_id, choose a registered folder or omit for the default. With project_session_id, the folder is set by the chat."},
                "project_id": {"type": "integer", "minimum": 1, "description": "Lyra project to use. Loads instructions from its registered folders."},
                "project_session_id": {"type": "integer", "minimum": 1, "description": "Saved project chat to continue. Requires project_id."},
                "prompt": {"type": "string", "minLength": 1, "description": "Task text. Thinking follows the model configuration. Do not insert Thinking directives into the prompt."},
                "allow_commands": {"type": "boolean", "deprecated": true, "description": "Accepted for older clients. Lyra's global command mode controls approval."},
                "skills": {"type": "array", "items": {"type": "string"}, "description": "Limit this run to the given installed Lyra skill names. Omit to make all installed Lyra skills available, or pass [] to run without skills."},
                "max_output_tokens": {"type": "integer", "minimum": AI_SETTINGS.minimum_output_tokens, "maximum": AI_SETTINGS.maximum_output_tokens, "default": AI_SETTINGS.default_output_tokens}
            }, "required": ["model", "prompt"], "additionalProperties": false},
            "annotations": {"destructiveHint": false}
        }
    ]);
    tools
        .as_array_mut()
        .expect("tool definitions")
        .push(complete_step);
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
                    "max_output_tokens": {"type": "integer"}
                },
                "required": ["stop_reason", "truncated", "max_output_tokens"]
            });
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
    prompt: String,
    max_output_tokens: Option<u32>,
    skills: Option<Vec<String>>,
    #[serde(rename = "allow_commands")]
    _allow_commands: Option<bool>,
    #[serde(skip)]
    role: Option<pi::Role>,
}

#[derive(Clone, Copy, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum ChatMode {
    Chat,
    Work,
}

#[derive(Serialize)]
struct GenerationInfo {
    stop_reason: Option<String>,
    truncated: bool,
    max_output_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    text_only: Option<bool>,
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
        "set_project_session_folders" => {
            let input = arguments(args)?;
            project_result(
                projects::work(app, |app| projects::set_session_folders(app, input)).await,
            )
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
        "get_project_tasks" => project_result(projects::queue::query(app, arguments(args)?).await),
        "complete_project_step" => {
            project_result(projects::queue::complete_external(app, arguments(args)?).await)
        }
        "cancel_project_task" => {
            projects::queue::cancel(app, arguments(args)?)
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

pub(super) fn output_limit(prompt: &str, requested: Option<u32>) -> Result<u32, String> {
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
        max_output_tokens: limit,
        text_only: None,
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
    let limit = output_limit(&request.prompt, request.max_output_tokens)?;
    let session_id = request.project_session_id;
    let (project, workspace) = if let Some(id) = request.project_id {
        let workspace = request.workspace;
        let project = projects::work(app.clone(), move |app| {
            app.projects.task(id, session_id, workspace.as_deref())
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
    let model = installed_model(&request.model).await?;
    let dashboard = matches!(origin, projects::MessageOrigin::Dashboard);
    let mode = if dashboard {
        request.mode.unwrap_or(ChatMode::Chat)
    } else {
        ChatMode::Work
    };
    let store = app.clone();
    let source = model.clone();
    let text_only = mode == ChatMode::Chat;
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
        let context_window = session_id.map(|_| {
            record.context_tokens.map(u64::from)
                .or_else(|| {
                    ollama::running_models()?.into_iter()
                        .find(|entry| ollama::same_model(&entry.name, &source))?.context_length
                })
                .or_else(|| estimate.and_then(|info| info.configured_context_tokens))
                .or_else(|| record.latest.as_ref().and_then(benchmark::Measurement::auto_context_tokens))
                .and_then(|tokens| u32::try_from(tokens).ok())
                .filter(|&tokens| tokens > 0)
                .unwrap_or(4096)
        });
        Ok((record.context_tokens, context_window, record.think))
    })
    .await
    .map_err(|error| error.to_string())??;
    let search_skill = project
        .as_ref()
        .and_then(|project| project.project.search_skill.as_deref())
        .filter(|skill| {
            !text_only
                && request
                    .skills
                    .as_ref()
                    .is_none_or(|skills| skills.iter().any(|name| name == skill))
        });
    let skill_store = app.clone();
    let selected = if text_only {
        Some(Vec::new())
    } else {
        request.skills
    };
    let skills = tokio::task::spawn_blocking(move || skill_store.skills.paths(selected.as_deref()))
        .await
        .map_err(|_| "Could not read installed Pi skills")??;
    let search_skill = search_skill
        .map(|name| {
            skills
                .iter()
                .find(|path| {
                    path.parent()
                        .and_then(|folder| folder.file_name())
                        .is_some_and(|folder| folder == name)
                })
                .map(|manifest| (name, manifest.as_path()))
                .ok_or_else(|| format!("Project search skill is not installed: {name}"))
        })
        .transpose()?;
    let scope = if project.is_some() {
        DelegationScope::Project { session_id }
    } else {
        DelegationScope::Standalone
    };
    let delegation = match reserved {
        Some(id) => id,
        None => begin_delegation(&app, &model, scope)?,
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
            search_skill,
            origin,
            text_only,
            model: &model,
            delegation,
            context_window,
            think: think.as_ref(),
            role: request.role,
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
                pi::run(&workspace, &model, &request.prompt, &config, tokens, limit, Some(task)),
            ) => Some(result.unwrap_or_else(|_| Err("Pi timed out".into()))),
        }
    };
    let Some(result) = result else {
        finish_delegation(&app, delegation, false, false).await;
        let text = "Генерация остановлена.";
        trace.event(Kind::Pi, "Pi", text, Some("ok"));
        return Ok(ToolOutput {
            text: text.into(),
            is_error: false,
            generation: Some(GenerationInfo {
                stop_reason: Some("aborted".into()),
                truncated: false,
                max_output_tokens: limit,
                text_only: None,
            }),
        });
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
    if context_full && session_id.is_some() {
        app.task.send_if_modified(|snapshot| {
            snapshot.delegation_context(delegation, ContextNotice::Full)
        });
    }
    finish_delegation(&app, delegation, error.is_none(), false).await;
    if context_full && session_id.is_some() {
        return Err("Контекст модели заполнен. Увеличь размер контекста в настройках модели, сократи запрос или начни новый чат.".into());
    }
    let reply = result?;
    let is_error = reply.error.is_some();
    let status = if reply.stop_reason.as_deref() == Some("length") {
        " (output limit reached)"
    } else if reply.truncated {
        " (partial answer)"
    } else {
        ""
    };
    Ok(ToolOutput {
        text: reply
            .error
            .unwrap_or_else(|| format!("{model}{status}\n{}", reply.text)),
        is_error,
        generation: Some(GenerationInfo {
            stop_reason: reply.stop_reason,
            truncated: reply.truncated,
            max_output_tokens: limit,
            text_only: text_only.then_some(true),
        }),
    })
}

pub(super) fn queued_request(
    project_id: u64,
    project_session_id: u64,
    model: String,
    prompt: String,
    max_output_tokens: Option<u32>,
    skills: Option<Vec<String>>,
    role: Option<pi::Role>,
) -> RunPi {
    RunPi {
        model,
        prompt,
        max_output_tokens,
        skills,
        workspace: None,
        project_id: Some(project_id),
        project_session_id: Some(project_session_id),
        request_id: Some(format!("queue-{project_session_id}")),
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
    let pending = {
        let snapshot = app.task.borrow();
        snapshot
            .has_delegation(delegation)
            .then(|| snapshot.delegation_stopping(delegation))
    };
    if pending.is_some() {
        finish_delegation(&app, delegation, false, false).await;
    }
    match result {
        Ok(reply) => {
            let (stop_reason, truncated) = reply.generation.map_or((None, false), |details| {
                (details.stop_reason, details.truncated)
            });
            let status = if stop_reason.as_deref() == Some("aborted") {
                Status::Cancelled
            } else if reply.is_error {
                Status::Failed
            } else {
                Status::Completed
            };
            (
                status,
                JobResult {
                    text: reply.text,
                    stop_reason,
                    truncated,
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
                text,
                stop_reason: None,
                truncated: false,
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

async fn finish_delegation(app: &App, id: u64, succeeded: bool, context_changed: bool) {
    app.metrics.lock().await.invalidate();
    app.task.send_modify(|snapshot| {
        snapshot.finish_delegation(id, succeeded);
        if context_changed {
            snapshot.model_record_changed();
        }
    });
}

async fn inspect(app: Arc<App>, request: Inspect) -> Result<String, String> {
    if request.context_tokens.is_some() && request.model.is_none() {
        return Err("context_tokens requires model".into());
    }
    let metrics = app.metrics.lock().await.sample().await;
    let mut output = format!("busy={}", app.task.borrow().busy());
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
                .or_else(|| entries.first())
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
