use std::{collections::BTreeSet, sync::Arc, time::Duration};

use axum::{
    Json, Router,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};

use crate::{benchmark, context, model_name, ollama, run};

use super::{App, mcp, pi};

pub(super) fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/model/pin", post(pin_model))
        .route("/model/unload", post(unload_model))
        .route("/model/select", post(select_model))
        .route("/model/benchmark", get(get_benchmark).post(start_benchmark))
        .route("/model/benchmark/profile", post(start_profile))
        .route("/model/description", post(describe_model))
        .route("/model/benchmark/pi", post(start_pi_benchmark))
        .route("/model/context", post(set_model_context))
        .route("/model/context/estimate", get(context_estimate))
        .route(
            "/model/thinking",
            get(model_thinking).post(set_model_thinking),
        )
}

#[derive(serde::Deserialize)]
struct PinRequest {
    pinned: bool,
}

async fn pin_model(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Json(request): Json<PinRequest>,
) -> StatusCode {
    let action = if request.pinned {
        ollama::ModelAction::Pin
    } else {
        ollama::ModelAction::Unpin
    };
    change_model(app, &headers, action).await
}

async fn unload_model(State(app): State<Arc<App>>, headers: HeaderMap) -> StatusCode {
    change_model(app, &headers, ollama::ModelAction::Unload).await
}

#[derive(serde::Deserialize)]
struct SelectRequest {
    model: String,
}

async fn get_benchmark(
    State(app): State<Arc<App>>,
    Query(request): Query<SelectRequest>,
) -> Result<Json<benchmark::Record>, StatusCode> {
    model_name(&request.model).map_err(|_| StatusCode::BAD_REQUEST)?;
    match tokio::task::spawn_blocking(move || app.benchmarks.get(&request.model)).await {
        Ok(Ok(record)) => Ok(Json(record)),
        _ => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

#[derive(serde::Deserialize)]
struct DescriptionRequest {
    description: String,
}

async fn start_pi_benchmark(State(app): State<Arc<App>>, headers: HeaderMap) -> StatusCode {
    let model = match control_model(&app, &headers).await {
        Ok(model) => model,
        Err(status) => return status,
    };
    let admission = app.context_admission.lock().await;
    if !app.task.send_if_modified(|snapshot| {
        if !matches_selection(snapshot, &headers) || !snapshot.begin_benchmark(model.clone()) {
            return false;
        }
        snapshot.set_benchmark_stage(benchmark::Stage::CheckingPi);
        true
    }) {
        return StatusCode::CONFLICT;
    }
    drop(admission);
    tokio::spawn(async move {
        let result = run_pi_benchmark(&app, model).await;
        app.metrics.lock().await.invalidate();
        app.task.send_modify(|snapshot| snapshot.finish_benchmark(result));
    });
    StatusCode::ACCEPTED
}

async fn run_pi_benchmark(app: &Arc<App>, model: String) -> Result<(), String> {
    let store = app.clone();
    let source = model.clone();
    let (tokens, advertised) = tokio::task::spawn_blocking(move || {
        let tokens = model_context(&store, &source)?;
        let advertised =
            context::estimate(&source).and_then(|details| details.supports_agent_tools());
        Ok::<_, String>((tokens, advertised))
    })
    .await
    .map_err(|error| error.to_string())??;
    let passed = if advertised == Some(false) {
        false
    } else {
        let tokens = if tokens.is_some() {
            mcp::release_idle_profiles(&model, tokens, false).await?;
            tokens
        } else {
            mcp::select_idle_auto(app, &model, 1, 1).await?.tokens
        };
        pi::check(&model, tokens).await?
    };
    let store = app.clone();
    tokio::task::spawn_blocking(move || store.benchmarks.save_pi_check(model, passed))
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| format!("Не удалось сохранить результат проверки Pi: {error}"))
}

async fn describe_model(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Json(request): Json<DescriptionRequest>,
) -> StatusCode {
    let model = match control_model(&app, &headers).await {
        Ok(model) => model,
        Err(status) => return status,
    };
    save_description(app, model, request.description, None).await
}

pub(super) async fn save_description(
    app: Arc<App>,
    model: String,
    description: String,
    ai_action: Option<run::AiAction>,
) -> StatusCode {
    if description.chars().count() > 2_000 {
        return StatusCode::BAD_REQUEST;
    }
    let store = app.clone();
    let saved_model = model.clone();
    match tokio::task::spawn_blocking(move || store.benchmarks.describe(saved_model, description))
        .await
    {
        Ok(Ok(())) => {
            app.task.send_modify(|snapshot| {
                snapshot.record_model_action(model, ai_action);
                snapshot.model_record_changed();
            });
            StatusCode::NO_CONTENT
        }
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

#[derive(serde::Deserialize)]
struct ContextRequest {
    context_tokens: Option<u32>,
}

async fn context_estimate(
    State(app): State<Arc<App>>,
    Query(request): Query<SelectRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let model = model_name(&request.model).map_err(|_| StatusCode::BAD_REQUEST)?;
    let source = model.clone();
    let estimate = tokio::task::spawn_blocking(move || context::estimate_auto(&source))
        .await.ok().flatten().unwrap_or_default();
    let metrics = app.metrics.lock().await.sample().await;
    let record = app.benchmarks.get(&model).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let reclaimable = if app.task.borrow().busy() { 0 } else {
        mcp::preview_reclaimable(&app, &metrics, &model, &record)
    };
    let auto = mcp::auto_context_estimate(&metrics, &model, &record, &estimate, 1, 1, reclaimable);
    let preview = |tokens| {
        let resident = metrics.models.as_deref().unwrap_or_default().iter()
            .find(|entry| ollama::same_model(&entry.name, &model) && entry.context_length == Some(tokens));
        mcp::memory_estimate(&metrics, &model, &record, &estimate, resident, Some(tokens), reclaimable)
    };
    let memory_by_context: std::collections::BTreeMap<_, _> = context::PRESETS.into_iter()
        .chain(record.context_tokens)
        .map(|tokens| (tokens, preview(u64::from(tokens)))).collect();
    let mut result = serde_json::to_value(&estimate).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    result["auto_context_tokens"] = serde_json::json!(auto.tokens);
    result["auto_context_source"] = serde_json::json!(auto.source);
    result["auto_context_reason"] = serde_json::json!(auto.reason);
    result["auto_memory"] = serde_json::json!(auto.tokens.map(|tokens| preview(u64::from(tokens))));
    result["memory_by_context"] = serde_json::json!(memory_by_context);
    Ok(Json(result))
}
async fn set_model_context(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Json(request): Json<ContextRequest>,
) -> StatusCode {
    if request
        .context_tokens
        .is_some_and(|tokens| !context::PRESETS.contains(&tokens))
    {
        return StatusCode::BAD_REQUEST;
    }
    let model = match control_model(&app, &headers).await {
        Ok(model) => model,
        Err(status) => return status,
    };
    if app.task.borrow().busy() {
        return StatusCode::CONFLICT;
    }
    let store = app.clone();
    let saved_model = model.clone();
    match tokio::task::spawn_blocking(move || {
        store.benchmarks.set_context(saved_model, request.context_tokens)
    })
    .await
    {
        Ok(Ok(())) => {
            app.task.send_modify(|snapshot| {
                snapshot.record_model_action(model, None);
                snapshot.model_record_changed();
            });
            StatusCode::NO_CONTENT
        }
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

pub(super) fn model_context(app: &App, model: &str) -> Result<Option<u32>, String> {
    Ok(model_settings(app, model)?.context_tokens)
}

pub(super) fn model_settings(app: &App, model: &str) -> Result<benchmark::Record, String> {
    app.benchmarks
        .get(model)
        .map_err(|error| format!("Не удалось прочитать настройки модели: {error}"))
}

#[derive(serde::Deserialize)]
struct ThinkingRequest {
    think: Option<ollama::Think>,
}

#[derive(serde::Serialize)]
struct ThinkingSettings {
    options: Vec<ollama::Think>,
    think: Option<ollama::Think>,
    default: serde_json::Value,
    off_uses_lowest: bool,
}

async fn model_thinking(
    State(app): State<Arc<App>>,
    Query(request): Query<SelectRequest>,
) -> Result<Json<ThinkingSettings>, StatusCode> {
    model_name(&request.model).map_err(|_| StatusCode::BAD_REQUEST)?;
    tokio::task::spawn_blocking(move || {
        let estimate = context::estimate(&request.model).ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
        let think = app
            .benchmarks
            .get(&request.model)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
            .think;
        let off_uses_lowest = estimate.thinking["values"]
            .as_array()
            .is_some_and(|values| !values.iter().any(|value| value == &false));
        Ok(Json(ThinkingSettings {
            options: estimate.thinking_options,
            think,
            default: estimate.thinking["default"].clone(),
            off_uses_lowest,
        }))
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
}

async fn set_model_thinking(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Json(request): Json<ThinkingRequest>,
) -> StatusCode {
    let model = match control_model(&app, &headers).await {
        Ok(model) => model,
        Err(status) => return status,
    };
    let store = app.clone();
    let result = tokio::task::spawn_blocking(move || {
        if let Some(think) = &request.think {
            let estimate = context::estimate(&model).ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
            if !estimate.thinking_options.contains(think) {
                return Err(StatusCode::BAD_REQUEST);
            }
        }
        store
            .benchmarks
            .set_thinking(model, request.think)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
    })
    .await;
    match result {
        Ok(Ok(())) => {
            app.task.send_modify(run::Snapshot::model_record_changed);
            StatusCode::NO_CONTENT
        }
        Ok(Err(status)) => status,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

async fn start_benchmark(State(app): State<Arc<App>>, headers: HeaderMap) -> StatusCode {
    start_benchmark_mode(app, headers, false).await
}

async fn start_profile(State(app): State<Arc<App>>, headers: HeaderMap) -> StatusCode {
    start_benchmark_mode(app, headers, true).await
}

async fn start_benchmark_mode(app: Arc<App>, headers: HeaderMap, full: bool) -> StatusCode {
    let model = match control_model(&app, &headers).await {
        Ok(model) => model,
        Err(status) => return status,
    };
    let admission = app.context_admission.lock().await;
    if !app.task.send_if_modified(|snapshot| {
        matches_selection(snapshot, &headers) && snapshot.begin_benchmark(model.clone())
    }) {
        return StatusCode::CONFLICT;
    }
    drop(admission);
    tokio::spawn(async move {
        let store = app.clone();
        let result = tokio::task::spawn_blocking(move || {
            if full {
                let profile = benchmark::measure_profile(&model, |stage| {
                    store.task.send_modify(|snapshot| snapshot.set_benchmark_stage(stage));
                })?;
                store.task.send_modify(|snapshot| snapshot.set_benchmark_stage(benchmark::Stage::Saving));
                return store.benchmarks.save_profile(model, profile)
                    .map_err(|error| format!("Не удалось сохранить профиль: {error}"));
            }
            let tokens = model_context(&store, &model)?;
            let measurement = benchmark::measure(&model, tokens, |stage| {
                store
                    .task
                    .send_modify(|snapshot| snapshot.set_benchmark_stage(stage));
            })?;
            store
                .task
                .send_modify(|snapshot| snapshot.set_benchmark_stage(benchmark::Stage::Saving));
            store
                .benchmarks
                .save(model, measurement)
                .map_err(|error| format!("Не удалось сохранить бенчмарк: {error}"))
        })
        .await
        .unwrap_or_else(|error| Err(error.to_string()));
        app.metrics.lock().await.invalidate();
        app.task
            .send_modify(|snapshot| snapshot.finish_benchmark(result));
    });
    StatusCode::ACCEPTED
}

async fn select_model(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Json(request): Json<SelectRequest>,
) -> StatusCode {
    let Some(names) = tokio::task::spawn_blocking(ollama::installed_models)
        .await
        .ok()
        .flatten()
    else {
        return StatusCode::SERVICE_UNAVAILABLE;
    };
    if !names.contains(&request.model) {
        return StatusCode::NOT_FOUND;
    }
    if !app.task.send_if_modified(|snapshot| {
        matches_selection(snapshot, &headers) && snapshot.select_model(request.model)
    }) {
        return StatusCode::CONFLICT;
    }
    StatusCode::NO_CONTENT
}

pub(super) fn matches_selection(snapshot: &run::Snapshot, headers: &HeaderMap) -> bool {
    headers.get("x-lyra-model").is_none_or(|requested| {
        requested.as_bytes() == snapshot.model().unwrap_or_default().as_bytes()
    })
}

pub(super) async fn control_model(app: &App, headers: &HeaderMap) -> Result<String, StatusCode> {
    let explicit = if let Some(value) = headers.get("x-lyra-target-model") {
        let model = model_name(value.to_str().map_err(|_| StatusCode::BAD_REQUEST)?)
            .map_err(|_| StatusCode::BAD_REQUEST)?;
        let installed = tokio::task::spawn_blocking(ollama::installed_models)
            .await
            .ok()
            .flatten()
            .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
        if !installed.contains(&model) {
            return Err(StatusCode::NOT_FOUND);
        }
        Some(model)
    } else {
        None
    };
    let snapshot = app.task.borrow();
    if !matches_selection(&snapshot, headers) || snapshot.closed() {
        return Err(StatusCode::CONFLICT);
    }
    explicit
        .or_else(|| snapshot.model().map(str::to_owned))
        .ok_or(StatusCode::CONFLICT)
}

async fn change_model(
    app: Arc<App>,
    headers: &HeaderMap,
    action: ollama::ModelAction,
) -> StatusCode {
    let model = match control_model(&app, headers).await {
        Ok(model) => model,
        Err(status) => return status,
    };
    let admission = app.context_admission.lock().await;
    if !app.task.send_if_modified(|snapshot| {
        matches_selection(snapshot, headers) && snapshot.begin_model_action(model.clone(), action)
    }) {
        return StatusCode::CONFLICT;
    }

    drop(admission);
    execute_model_action(app, model, action, None);
    StatusCode::ACCEPTED
}

pub(super) fn execute_model_action(
    app: Arc<App>,
    model: String,
    action: ollama::ModelAction,
    ai_action: Option<run::AiAction>,
) -> tokio::task::JoinHandle<Result<bool, String>> {
    tokio::spawn(async move {
        let result = async {
            let tokens = if action.uses_context() {
                let record = model_settings(&app, &model)?;
                if record.context_tokens.is_some() {
                    mcp::release_idle_profiles(&model, record.context_tokens, false).await?;
                    record.context_tokens
                } else {
                    let selected = mcp::select_idle_auto(&app, &model, 1, 1).await?;
                    if selected.tokens.is_none() { eprintln!("Auto context for {model}: {}. Using Ollama default for load/pin.", selected.reason); }
                    selected.tokens
                }
            } else {
                None
            };
            tokio::task::spawn_blocking(move || ollama::change_model(&model, action, tokens))
                .await.map_err(|error| error.to_string())?
        }.await;
        app.metrics.lock().await.invalidate();
        app.task.send_modify(|snapshot| {
            snapshot.finish_model_action(action, &result, ai_action);
        });
        result
    })
}

pub(super) async fn clean_context_profiles(app: Arc<App>) {
    let mut ticker = tokio::time::interval(Duration::from_secs(15));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut seen_orphans = BTreeSet::new();
    loop {
        ticker.tick().await;
        if app.task.borrow().busy() {
            continue;
        }
        let store = app.clone();
        seen_orphans = tokio::task::spawn_blocking(move || {
            context::clean_orphans(&store.benchmarks, seen_orphans)
        })
        .await
        .unwrap_or_default();
    }
}
