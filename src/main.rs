mod benchmark;
mod context;
mod ram;
mod run;
mod telemetry;

use std::{
    convert::Infallible, io::SeekFrom, net::Ipv4Addr, path::PathBuf, sync::Arc, time::Duration,
};

use axum::{
    Json, Router,
    extract::{Query, Request, State},
    http::{HeaderMap, StatusCode, header},
    middleware::{self, Next},
    response::{
        Html, IntoResponse, Response, Sse,
        sse::{Event, KeepAlive},
    },
    routing::{get, post},
};
use clap::Parser;
use tokio::{
    io::{AsyncReadExt, AsyncSeekExt},
    net::TcpListener,
    sync::{Mutex, watch},
};

#[derive(Parser)]
#[command(about = "Local dashboard for a Pi agent running on Ollama")]
struct Options {
    /// Preferred local model, as shown by `ollama list`.
    #[arg(long, value_parser = model_name)]
    model: Option<String>,
    /// Pi configuration directory managed by the Lyra launcher.
    #[arg(long)]
    pi_agent_dir: Option<PathBuf>,
    /// Local dashboard port.
    #[arg(long, default_value_t = 8787)]
    port: u16,
    /// Startup log supplied by the Windows launcher.
    #[arg(long)]
    startup_log: Option<PathBuf>,
    /// Path to the JSON file for model descriptions and benchmark results.
    #[arg(long)]
    benchmark_file: Option<PathBuf>,
}

fn model_name(name: &str) -> Result<String, String> {
    if name
        .as_bytes()
        .first()
        .is_some_and(u8::is_ascii_alphanumeric)
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-._:/".contains(&byte))
    {
        return Ok(name.to_owned());
    }
    Err("use the name from `ollama list`, for example qwen3:8b".into())
}

struct App {
    host: String,
    origin: String,
    task: watch::Sender<run::Snapshot>,
    metrics: Mutex<telemetry::Cache>,
    ram: Mutex<ram::Cache>,
    startup_log: Option<PathBuf>,
    pi_agent_dir: Option<PathBuf>,
    benchmarks: benchmark::Store,
}

fn main() -> std::io::Result<()> {
    let options = Options::parse();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(serve(options));
    runtime.shutdown_timeout(Duration::from_secs(1));
    result
}

async fn serve(options: Options) -> std::io::Result<()> {
    let catalog = tokio::task::spawn_blocking(telemetry::installed_models)
        .await
        .ok()
        .flatten();
    let model = match catalog.as_ref() {
        Some(names) => names
            .iter()
            .find(|name| {
                options.model.as_deref().is_some_and(|preferred| {
                    name.eq_ignore_ascii_case(preferred)
                        || name.eq_ignore_ascii_case(&format!("{preferred}:latest"))
                })
            })
            .or_else(|| names.first())
            .cloned(),
        None => options.model,
    };
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, options.port)).await?;
    let host = format!("127.0.0.1:{}", listener.local_addr()?.port());
    let app = Arc::new(App {
        origin: format!("http://{host}"),
        host,
        task: watch::channel(run::Snapshot::new(model)).0,
        metrics: Mutex::default(),
        ram: Mutex::default(),
        startup_log: options.startup_log,
        pi_agent_dir: options.pi_agent_dir,
        benchmarks: benchmark::Store::new(options.benchmark_file),
    });
    if let Some(names) = catalog {
        register_benchmarks(app.clone(), names).await;
    }
    let router = Router::new()
        .route(
            "/",
            get(|| async { Html(include_str!("../web/index.html")) }),
        )
        .route(
            "/app.css",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
                    include_str!("../web/app.css"),
                )
            }),
        )
        .route(
            "/app.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../web/app.js"),
                )
            }),
        )
        .route("/events", get(events))
        .route(
            "/startup-log",
            get(|| async { Html(include_str!("../web/startup-log.html")) }),
        )
        .route("/startup-log/text", get(startup_log))
        .route("/metrics", get(metrics))
        .route("/ram", get(ram_usage))
        .route("/run", post(start))
        .route("/model/pin", post(pin_model))
        .route("/model/unload", post(unload_model))
        .route("/model/select", post(select_model))
        .route("/model/benchmark", get(get_benchmark).post(start_benchmark))
        .route("/model/description", post(describe_model))
        .route("/model/context", post(set_model_context))
        .route("/model/context/estimate", get(context_estimate))
        .layer(middleware::from_fn_with_state(app.clone(), local_only))
        .with_state(app.clone());

    println!("Dashboard: {}", app.origin);
    tokio::select! {
        result = axum::serve(listener, router) => result,
        result = tokio::signal::ctrl_c() => result,
    }
}

async fn startup_log(State(app): State<Arc<App>>) -> Response {
    let Some(path) = &app.startup_log else {
        return "Журнал доступен при запуске через лаунчер Windows.".into_response();
    };
    match read_startup_log(path).await {
        Ok(log) => ([(header::X_CONTENT_TYPE_OPTIONS, "nosniff")], log).into_response(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            (StatusCode::NOT_FOUND, "Журнал запуска ещё недоступен.").into_response()
        }
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Не удалось прочитать журнал запуска.",
        )
            .into_response(),
    }
}

async fn read_startup_log(path: &std::path::Path) -> std::io::Result<String> {
    const MAX_BYTES: u64 = 256 * 1024;
    let mut file = tokio::fs::File::open(path).await?;
    let start = file.metadata().await?.len().saturating_sub(MAX_BYTES);
    file.seek(SeekFrom::Start(start)).await?;
    let mut bytes = Vec::new();
    file.take(MAX_BYTES).read_to_end(&mut bytes).await?;
    if start == 0 {
        return Ok(String::from_utf8_lossy(&bytes)
            .trim_start_matches('\u{feff}')
            .to_owned());
    }
    let offset = bytes
        .iter()
        .position(|&byte| byte == b'\n')
        .map_or(0, |index| index + 1);
    Ok(format!(
        "Показаны последние 256 КБ журнала.\n\n{}",
        String::from_utf8_lossy(&bytes[offset..])
    ))
}

async fn local_only(State(app): State<Arc<App>>, request: Request, next: Next) -> Response {
    let valid_host = request
        .headers()
        .get(header::HOST)
        .is_some_and(|value| value == app.host.as_str());
    let valid_origin = request
        .headers()
        .get(header::ORIGIN)
        .is_some_and(|value| value == app.origin.as_str());
    if !valid_host || (request.method() == axum::http::Method::POST && !valid_origin) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let mut response = next.run(request).await;
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response
}

async fn start(State(app): State<Arc<App>>, headers: HeaderMap) -> StatusCode {
    if !app
        .task
        .send_if_modified(|snapshot| matches_selection(snapshot, &headers) && snapshot.start())
    {
        return StatusCode::CONFLICT;
    }
    tokio::spawn(async move {
        let model = current_model(&app);
        let store = app.clone();
        let tokens = tokio::task::spawn_blocking(move || model_context(&store, &model))
            .await
            .unwrap_or_else(|error| Err(error.to_string()));
        run::execute(app.task.clone(), app.pi_agent_dir.clone(), tokens).await;
    });
    StatusCode::ACCEPTED
}

async fn events(State(app): State<Arc<App>>) -> impl IntoResponse {
    // Each connection receives the latest snapshot, including after a reconnect.
    let snapshots = futures_util::stream::unfold(
        (app.task.subscribe(), true),
        |(mut rx, initial)| async move {
            if !initial {
                rx.changed().await.ok()?;
                tokio::time::sleep(Duration::from_millis(180)).await;
            }
            let data = serde_json::to_string(&*rx.borrow_and_update()).ok()?;
            Some((
                Ok::<_, Infallible>(Event::default().data(data)),
                (rx, false),
            ))
        },
    );
    Sse::new(snapshots).keep_alive(KeepAlive::default())
}

async fn metrics(State(app): State<Arc<App>>) -> Json<telemetry::Metrics> {
    let metrics = app.metrics.lock().await.sample().await;
    if let Some(names) = metrics.installed_models() {
        register_benchmarks(app.clone(), names.to_vec()).await;
    }
    Json(metrics)
}

async fn register_benchmarks(app: Arc<App>, names: Vec<String>) {
    if let Ok(Err(error)) =
        tokio::task::spawn_blocking(move || app.benchmarks.register(&names)).await
    {
        eprintln!("Could not register model benchmarks: {error}");
    }
}

async fn ram_usage(State(app): State<Arc<App>>) -> Json<ram::Usage> {
    Json(app.ram.lock().await.sample().await)
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
        telemetry::ModelAction::Pin
    } else {
        telemetry::ModelAction::Unpin
    };
    change_model(app, &headers, action)
}

async fn unload_model(State(app): State<Arc<App>>, headers: HeaderMap) -> StatusCode {
    change_model(app, &headers, telemetry::ModelAction::Unload)
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
    if request.description.chars().count() > 2_000 {
        return StatusCode::BAD_REQUEST;
    }
    let model = {
        let snapshot = app.task.borrow();
        if !matches_selection(&snapshot, &headers) {
            return StatusCode::CONFLICT;
        }
        let Some(model) = snapshot.model() else {
            return StatusCode::CONFLICT;
        };
        model.to_owned()
    };
    match tokio::task::spawn_blocking(move || app.benchmarks.describe(model, request.description))
        .await
    {
        Ok(Ok(())) => StatusCode::NO_CONTENT,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

#[derive(serde::Deserialize)]
struct ContextRequest {
    context_tokens: Option<u32>,
}

#[derive(serde::Serialize)]
struct ContextEstimate {
    bytes_per_token_f16: Option<u64>,
}

async fn context_estimate(
    Query(request): Query<SelectRequest>,
) -> Result<Json<ContextEstimate>, StatusCode> {
    model_name(&request.model).map_err(|_| StatusCode::BAD_REQUEST)?;
    let bytes_per_token_f16 =
        tokio::task::spawn_blocking(move || context::cache_bytes_per_token(&request.model))
            .await
            .ok()
            .flatten();
    Ok(Json(ContextEstimate {
        bytes_per_token_f16,
    }))
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
    let model = {
        let snapshot = app.task.borrow();
        if !matches_selection(&snapshot, &headers) || snapshot.busy() {
            return StatusCode::CONFLICT;
        }
        let Some(model) = snapshot.model() else {
            return StatusCode::CONFLICT;
        };
        model.to_owned()
    };
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

fn current_model(app: &App) -> String {
    app.task
        .borrow()
        .model()
        .expect("running operation has a model")
        .to_owned()
}

fn model_context(app: &App, model: &str) -> Result<Option<u32>, String> {
    app.benchmarks
        .get(model)
        .map(|record| record.context_tokens)
        .map_err(|error| format!("Не удалось прочитать настройки модели: {error}"))
}

async fn start_benchmark(State(app): State<Arc<App>>, headers: HeaderMap) -> StatusCode {
    if !app.task.send_if_modified(|snapshot| {
        matches_selection(snapshot, &headers) && snapshot.begin_benchmark()
    }) {
        return StatusCode::CONFLICT;
    }
    tokio::spawn(async move {
        let store = app.clone();
        let model = current_model(&app);
        let result = tokio::task::spawn_blocking(move || {
            let tokens = model_context(&store, &model)?;
            let measurement = benchmark::measure(&model, tokens)?;
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
    let Some(names) = tokio::task::spawn_blocking(telemetry::installed_models)
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

fn matches_selection(snapshot: &run::Snapshot, headers: &HeaderMap) -> bool {
    headers.get("x-lyra-model").is_none_or(|requested| {
        requested.as_bytes() == snapshot.model().unwrap_or_default().as_bytes()
    })
}

fn change_model(app: Arc<App>, headers: &HeaderMap, action: telemetry::ModelAction) -> StatusCode {
    if !app.task.send_if_modified(|snapshot| {
        matches_selection(snapshot, headers) && snapshot.begin_model_action(action)
    }) {
        return StatusCode::CONFLICT;
    }

    tokio::spawn(async move {
        let model = current_model(&app);
        let store = app.clone();
        let result = tokio::task::spawn_blocking(move || {
            let tokens = if matches!(action, telemetry::ModelAction::Pin) {
                model_context(&store, &model)?
            } else {
                None
            };
            telemetry::change_model(&model, action, tokens)
        })
        .await
        .unwrap_or_else(|error| Err(error.to_string()));
        app.metrics.lock().await.invalidate();
        app.task
            .send_modify(|snapshot| snapshot.finish_model_action(action, result));
    });
    StatusCode::ACCEPTED
}
