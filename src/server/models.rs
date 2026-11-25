use std::{collections::BTreeSet, sync::Arc, time::Duration};

use axum::{
    Json, Router,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};

use crate::{benchmark, context, model_name, ollama, run};

use super::App;

pub(super) fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/model/pin", post(pin_model))
        .route("/model/unload", post(unload_model))
        .route("/model/select", post(select_model))
        .route("/model/benchmark", get(get_benchmark).post(start_benchmark))
        .route("/model/description", post(describe_model))
        .route("/model/context", post(set_model_context))
        .route("/model/context/estimate", get(context_estimate))
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

async fn describe_model(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Json(request): Json<DescriptionRequest>,
) -> StatusCode {
    let model = match control_model(&app, &headers).await {
        Ok(model) => model,
        Err(status) => return status,
    };
    save_description(app, model, request.description).await
}

pub(super) async fn save_description(app: Arc<App>, model: String, description: String) -> StatusCode {
    if description.chars().count() > 2_000 {
        return StatusCode::BAD_REQUEST;
    }
    let store = app.clone();
    match tokio::task::spawn_blocking(move || store.benchmarks.describe(model, description)).await {
        Ok(Ok(())) => {
            app.task.send_modify(run::Snapshot::model_record_changed);
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
    Query(request): Query<SelectRequest>,
) -> Result<Json<context::Estimate>, StatusCode> {
    model_name(&request.model).map_err(|_| StatusCode::BAD_REQUEST)?;
    let estimate = tokio::task::spawn_blocking(move || context::estimate(&request.model))
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
    Ok(Json(estimate))
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
    match tokio::task::spawn_blocking(move || {
        store.benchmarks.set_context(model, request.context_tokens)
    })
    .await
    {
        Ok(Ok(())) => {
            app.task.send_modify(run::Snapshot::model_record_changed);
            StatusCode::NO_CONTENT
        }
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

pub(super) fn model_context(app: &App, model: &str) -> Result<Option<u32>, String> {
    app.benchmarks
        .get(model)
        .map(|record| record.context_tokens)
        .map_err(|error| format!("Не удалось прочитать настройки модели: {error}"))
}

async fn start_benchmark(State(app): State<Arc<App>>, headers: HeaderMap) -> StatusCode {
    let model = match control_model(&app, &headers).await {
        Ok(model) => model,
        Err(status) => return status,
    };
    if !app.task.send_if_modified(|snapshot| {
        matches_selection(snapshot, &headers) && snapshot.begin_benchmark(model.clone())
    }) {
        return StatusCode::CONFLICT;
    }
    tokio::spawn(async move {
        let store = app.clone();
        let result = tokio::task::spawn_blocking(move || {
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

async fn control_model(app: &App, headers: &HeaderMap) -> Result<String, StatusCode> {
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
    if !matches_selection(&snapshot, headers) || snapshot.closed() || snapshot.mode() != run::Mode::Manual {
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
    if !app.task.send_if_modified(|snapshot| {
        matches_selection(snapshot, headers) && snapshot.begin_model_action(model.clone(), action)
    }) {
        return StatusCode::CONFLICT;
    }

    tokio::spawn(async move {
        let store = app.clone();
        let result = tokio::task::spawn_blocking(move || {
            let tokens = if matches!(action, ollama::ModelAction::Pin) {
                model_context(&store, &model)?
            } else {
                None
            };
            ollama::change_model(&model, action, tokens)
        })
        .await
        .unwrap_or_else(|error| Err(error.to_string()));
        app.metrics.lock().await.invalidate();
        app.task
            .send_modify(|snapshot| snapshot.finish_model_action(action, result));
    });
    StatusCode::ACCEPTED
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
