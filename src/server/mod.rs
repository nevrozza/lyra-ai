mod assets;
mod models;
mod runtime;

use std::{net::Ipv4Addr, path::PathBuf, sync::Arc};

use axum::{
    extract::{Request, State},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use tokio::{
    net::TcpListener,
    sync::{Mutex, watch},
};

use crate::{Options, benchmark, ollama, ram, run, telemetry};

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

pub(super) async fn serve(options: Options) -> std::io::Result<()> {
    let catalog = tokio::task::spawn_blocking(ollama::installed_models)
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
        runtime::register_benchmarks(app.clone(), names).await;
    }
    let router = assets::routes()
        .merge(runtime::routes())
        .merge(models::routes())
        .layer(middleware::from_fn_with_state(app.clone(), local_only))
        .with_state(app.clone());

    println!("Dashboard: {}", app.origin);
    let cleanup = tokio::spawn(models::clean_context_profiles(app));
    let result = tokio::select! {
        result = axum::serve(listener, router) => result,
        result = tokio::signal::ctrl_c() => result,
    };
    cleanup.abort();
    result
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
    if request.method() == axum::http::Method::POST
        && request.uri().path() != "/shutdown"
        && app.task.borrow().closed()
    {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let mut response = next.run(request).await;
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response
}
