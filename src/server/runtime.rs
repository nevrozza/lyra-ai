use std::{convert::Infallible, io::SeekFrom, sync::Arc, time::Duration};

use axum::{
    Json, Router,
    extract::State,
    http::{StatusCode, header},
    response::{
        IntoResponse, Response, Sse,
        sse::{Event, KeepAlive},
    },
    routing::{get, post},
};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::{ram, run, telemetry};

use super::{App, assets};

pub(super) fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/events", get(events))
        .route("/startup-log/text", get(startup_log))
        .route("/metrics", get(metrics))
        .route("/ram", get(ram_usage))
        .route("/shutdown", post(notify_shutdown))
}

async fn notify_shutdown(State(app): State<Arc<App>>) -> StatusCode {
    app.task.send_modify(run::Snapshot::close);
    StatusCode::NO_CONTENT
}

async fn events(State(app): State<Arc<App>>) -> impl IntoResponse {
    #[derive(serde::Serialize)]
    struct Update<'a> {
        #[serde(flatten)]
        snapshot: &'a run::Snapshot,
        ui_revision: &'a str,
    }
    // Each connection receives the latest snapshot, including after a reconnect.
    let snapshots = futures_util::stream::unfold(
        (app.task.subscribe(), true),
        |(mut rx, initial)| async move {
            if !initial {
                rx.changed().await.ok()?;
                if !rx.borrow().closed() {
                    tokio::time::sleep(Duration::from_millis(180)).await;
                }
            }
            let data = serde_json::to_string(&Update {
                snapshot: &rx.borrow_and_update(),
                ui_revision: assets::revision(),
            })
            .ok()?;
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

pub(super) async fn register_benchmarks(app: Arc<App>, names: Vec<String>) {
    if let Ok(Err(error)) =
        tokio::task::spawn_blocking(move || app.benchmarks.register(&names)).await
    {
        eprintln!("Could not register model benchmarks: {error}");
    }
}

async fn ram_usage(State(app): State<Arc<App>>) -> Json<ram::Usage> {
    Json(app.ram.lock().await.sample().await)
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
