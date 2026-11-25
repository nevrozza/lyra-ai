mod run;
mod telemetry;

use std::{convert::Infallible, net::Ipv4Addr, sync::Arc, time::Duration};

use axum::{
    Json, Router,
    extract::{Request, State},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{
        Html, IntoResponse, Response, Sse,
        sse::{Event, KeepAlive},
    },
    routing::{get, post},
};
use clap::Parser;
use tokio::{
    net::TcpListener,
    sync::{Mutex, watch},
};

#[derive(Parser)]
#[command(about = "Local dashboard for a Pi agent running on Ollama")]
struct Options {
    /// Name of the local model, as shown by `ollama list`.
    #[arg(long, value_parser = model_name)]
    model: String,
    /// Local dashboard port.
    #[arg(long, default_value_t = 8787)]
    port: u16,
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
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, options.port)).await?;
    let host = format!("127.0.0.1:{}", listener.local_addr()?.port());
    let app = Arc::new(App {
        origin: format!("http://{host}"),
        host,
        task: watch::channel(run::Snapshot::new(options.model)).0,
        metrics: Mutex::default(),
    });
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
        .route("/metrics", get(metrics))
        .route("/run", post(start))
        .route("/model/pin", post(pin_model))
        .route("/model/unload", post(unload_model))
        .layer(middleware::from_fn_with_state(app.clone(), local_only))
        .with_state(app.clone());

    println!("Dashboard: {}", app.origin);
    tokio::select! {
        result = axum::serve(listener, router) => result,
        result = tokio::signal::ctrl_c() => result,
    }
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

async fn start(State(app): State<Arc<App>>) -> StatusCode {
    if !app.task.send_if_modified(run::Snapshot::start) {
        return StatusCode::CONFLICT;
    }
    tokio::spawn(run::execute(app.task.clone()));
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
    Json(app.metrics.lock().await.sample().await)
}

#[derive(serde::Deserialize)]
struct PinRequest {
    pinned: bool,
}

async fn pin_model(State(app): State<Arc<App>>, Json(request): Json<PinRequest>) -> StatusCode {
    let action = if request.pinned {
        telemetry::ModelAction::Pin
    } else {
        telemetry::ModelAction::Unpin
    };
    change_model(app, action)
}

async fn unload_model(State(app): State<Arc<App>>) -> StatusCode {
    change_model(app, telemetry::ModelAction::Unload)
}

fn change_model(app: Arc<App>, action: telemetry::ModelAction) -> StatusCode {
    if !app
        .task
        .send_if_modified(|snapshot| snapshot.begin_model_action(action))
    {
        return StatusCode::CONFLICT;
    }

    tokio::spawn(async move {
        let model = app.task.borrow().model().to_owned();
        let result = tokio::task::spawn_blocking(move || telemetry::change_model(&model, action))
            .await
            .unwrap_or_else(|error| Err(error.to_string()));
        app.metrics.lock().await.invalidate();
        app.task
            .send_modify(|snapshot| snapshot.finish_model_action(action, result));
    });
    StatusCode::ACCEPTED
}
