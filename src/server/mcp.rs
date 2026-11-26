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
    run::{AiAction, DelegationScope},
    telemetry,
};

use super::{
    App,
    ai_settings::AI_SETTINGS,
    models::{execute_model_action, model_context, save_description},
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
                    | "read_project_session"
                    | "archive_project"
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
                "prompt": {"type": "string", "minLength": 1},
                "context_tokens": {"type": "integer", "minimum": 1, "maximum": u32::MAX, "description": "Context window for this call. Saved to the model when Ollama finishes. Omit for the saved setting or Ollama default."},
                "max_output_tokens": {"type": "integer", "minimum": AI_SETTINGS.minimum_output_tokens, "maximum": AI_SETTINGS.maximum_output_tokens, "default": AI_SETTINGS.default_output_tokens, "description": "Generation budget. On reasoning models it can include thinking tokens and run out before any visible text."},
                "think": {"anyOf": [{"type": "boolean"}, {"type": "string", "minLength": 1}], "description": "Optional thinking switch or a supported model-specific level from inspect. Omit for the model default."},
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
                "folder": {"type": "string", "description": "Registered folder for the chat. Defaults to the project's first folder."}
            }, "required": ["project_id", "name"], "additionalProperties": false}
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
            "description": "Work with files in a Pi workspace using read, grep, find, ls, write and edit. With allow_commands, Pi can also run shell commands according to the command mode in Lyra. Before choosing or loading a model, use inspect to check benchmark_and_settings.pi_check and ollama_agent_tools.",
            "inputSchema": {"type": "object", "properties": {
                "model": {"type": "string"},
                "workspace": {"type": "string", "description": "Absolute folder path. Required for standalone runs. With project_id, choose a registered folder or omit for the default. With project_session_id, the folder is set by the chat."},
                "project_id": {"type": "integer", "minimum": 1, "description": "Lyra project to use. Loads instructions from its registered folders."},
                "project_session_id": {"type": "integer", "minimum": 1, "description": "Saved project chat to continue. Requires project_id."},
                "prompt": {"type": "string", "minLength": 1},
                "allow_commands": {"type": "boolean", "default": false, "description": "Enable the shell command tool for this run. Individual approval follows the Lyra dashboard command mode."},
                "skills": {"type": "array", "items": {"type": "string"}, "description": "Limit this run to the given installed Lyra skill names. Omit to make all installed Lyra skills available, or pass [] to run without skills."},
                "max_output_tokens": {"type": "integer", "minimum": AI_SETTINGS.minimum_output_tokens, "maximum": AI_SETTINGS.maximum_output_tokens, "default": AI_SETTINGS.default_output_tokens}
            }, "required": ["model", "prompt"], "additionalProperties": false},
            "annotations": {"destructiveHint": false}
        }
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
                    "max_output_tokens": {"type": "integer"}
                },
                "required": ["stop_reason", "truncated", "max_output_tokens"]
            });
        }
    }
    tools
}

fn is_session_id(value: &str) -> bool {
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
    prompt: String,
    max_output_tokens: Option<u32>,
    skills: Option<Vec<String>>,
    #[serde(default)]
    allow_commands: bool,
}

#[derive(Serialize)]
struct GenerationInfo {
    stop_reason: Option<String>,
    truncated: bool,
    max_output_tokens: u32,
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
        "run_pi" => return run_pi(app, arguments(args)?, trace).await,
        "list_projects" => {
            let _: NoArguments = arguments(args)?;
            project_result(projects::work(app, |app| app.projects.list()).await)
        }
        "save_project" => {
            let input = arguments(args)?;
            project_result(projects::work(app, |app| projects::save_project(app, input)).await)
        }
        "create_project_session" => {
            let input = arguments(args)?;
            project_result(projects::work(app, |app| projects::add_session(app, input)).await)
        }
        "read_project_session" => {
            let input = arguments(args)?;
            project_result(projects::work(app, |app| app.projects.history(input)).await)
        }
        "archive_project" => {
            let input = arguments(args)?;
            project_result(projects::work(app, |app| projects::set_archive(app, input)).await)
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

fn resolve_model(installed: &[String], name: &str) -> Result<String, String> {
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
        let tokens = model_context(&store, &source)?;
        let requested = request.context_tokens.map(NonZeroU32::get);
        let reply = ollama::chat(
            &source,
            &request.prompt,
            limit,
            tokens,
            requested,
            request.require_loaded,
            request.think,
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

async fn run_pi(app: Arc<App>, request: RunPi, trace: &Call) -> Result<ToolOutput, String> {
    let limit = output_limit(&request.prompt, request.max_output_tokens)?;
    let session_id = request.project_session_id;
    let (project, workspace) = if let Some(id) = request.project_id {
        let workspace = request.workspace;
        let project = projects::work(app.clone(), move |app| {
            app.projects.task(id, session_id, workspace.as_deref())
        })
        .await
        .map_err(|(_, error)| error)?;
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
        let workspace = if request.allow_commands {
            tokio::fs::canonicalize(&workspace)
                .await
                .map_err(|error| format!("Could not resolve workspace: {error}"))?
        } else {
            workspace
        };
        (None, workspace)
    };
    let search_skill = project
        .as_ref()
        .and_then(|project| project.project.search_skill.as_deref())
        .filter(|skill| {
            request
                .skills
                .as_ref()
                .is_none_or(|skills| skills.iter().any(|name| name == skill))
        });
    let skill_store = app.clone();
    let selected = request.skills;
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
    let model = installed_model(&request.model).await?;
    let candidate = model.clone();
    let supported = tokio::task::spawn_blocking(move || {
        context::estimate(&candidate).and_then(|details| details.supports_agent_tools())
    })
    .await
    .map_err(|_| "Could not check Ollama capabilities")?;
    if supported == Some(false) {
        return Err(format!(
            "{model} lacks Ollama text generation or tool calling support required for agentic Pi tasks. Inspect another model."
        ));
    }
    let scope = if project.is_some() {
        DelegationScope::Project { session_id }
    } else {
        DelegationScope::Standalone
    };
    let delegation = begin_delegation(&app, &model, scope)?;
    let store = app.clone();
    let source = model.clone();
    let tokens = tokio::task::spawn_blocking(move || {
        let record = store
            .benchmarks
            .get(&source)
            .map_err(|error| format!("Не удалось прочитать настройки модели: {error}"))?;
        if record.pi_check == Some(false) {
            return Err(format!(
                "{source} failed Lyra's Pi benchmark. Choose another model or rerun the Pi benchmark in Lyra."
            ));
        }
        Ok(record.context_tokens)
    })
    .await;
    let config = app
        .pi_agent_dir
        .clone()
        .unwrap_or_else(|| std::env::temp_dir().join(format!("lyra-pi-{}", std::process::id())));
    let result = match tokens {
        Ok(Ok(tokens)) => {
            let mut state = app.task.subscribe();
            let max_seconds = if request.allow_commands { 900 } else { 120 };
            let task = pi::Task {
                trace,
                allow_commands: request.allow_commands,
                skills: &skills,
                agents_file: &app.agents_file,
                project: project.as_ref(),
                search_skill,
            };
            tokio::select! {
                biased;
                _ = async { let _ = state.wait_for(crate::run::Snapshot::closed).await; } => {
                    Err("Lyra stopped".into())
                },
                result = tokio::time::timeout(
                    Duration::from_secs(max_seconds),
                    pi::run(&workspace, &model, &request.prompt, &config, tokens, limit, Some(task)),
                ) => result.unwrap_or_else(|_| Err("Pi timed out".into())),
            }
        }
        Ok(Err(error)) => Err(error),
        Err(error) => Err(error.to_string()),
    };
    let error = match &result {
        Ok(reply) => reply.error.as_deref(),
        Err(error) => Some(error.as_str()),
    };
    if let Some(error) = error {
        trace.event(Kind::Pi, "Pi", error, Some("error"));
    }
    finish_delegation(&app, delegation, error.is_none(), false).await;
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
        }),
    })
}

pub(super) async fn run_project_chat(
    State(app): State<Arc<App>>,
    Json(request): Json<RunPi>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let (Some(project), Some(session)) = (request.project_id, request.project_session_id) else {
        return Err((StatusCode::BAD_REQUEST, "Choose a project session".into()));
    };
    tokio::spawn(async move {
        let trace = Call::start(
            app.clone(),
            "run_pi",
            Some(&request.model),
            &request.model,
            Some(format!("project-{project}-chat-{session}")),
        );
        let result = run_pi(app, request, &trace).await;
        trace.finish(match &result {
            Ok(reply) if reply.is_error => Some(&reply.text),
            Ok(_) => None,
            Err(error) => Some(error),
        });
        match result {
            Ok(reply) if !reply.is_error => Ok(Json(reply.into_result())),
            Ok(reply) => Err((StatusCode::BAD_REQUEST, reply.text)),
            Err(error) => Err((StatusCode::BAD_REQUEST, error)),
        }
    })
    .await
    .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?
}

fn begin_delegation(app: &App, model: &str, scope: DelegationScope) -> Result<u64, String> {
    let mut id = None;
    app.task.send_if_modified(|snapshot| {
        id = snapshot.begin_delegation(model, scope);
        id.is_some()
    });
    id.ok_or_else(|| match scope {
        DelegationScope::Project { .. } => {
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
        let thinking_supported = estimate.thinking["values"]
            .as_array()
            .map(|values| {
                values
                    .iter()
                    .any(|value| value == true || value.is_string())
            })
            .or_else(|| {
                estimate
                    .capabilities
                    .as_ref()
                    .map(|items| items.iter().any(|item| item == "thinking"))
            });
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

fn memory_estimate(
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
