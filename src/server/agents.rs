use std::{
    fs,
    io::{self, Write},
    path::Path,
    sync::Arc,
};

use axum::{Json, Router, extract::State, http::StatusCode, routing::get};

use super::App;

type HttpError = (StatusCode, String);
const DEFAULT_AGENTS: &str = include_str!("../../defaults/AGENTS.md");

pub(super) fn install_default(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(_) => return Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let directory = path.parent().expect("AGENTS.md parent");
    fs::create_dir_all(directory)?;
    let mut file = tempfile::NamedTempFile::new_in(directory)?;
    file.write_all(DEFAULT_AGENTS.as_bytes())?;
    match file.persist_noclobber(path) {
        Ok(_) => Ok(()),
        Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error.error),
    }
}

pub(super) fn routes() -> Router<Arc<App>> {
    Router::new().route("/skills/agents", get(load).post(save))
}

async fn load(State(app): State<Arc<App>>) -> Result<Json<String>, HttpError> {
    tokio::fs::read_to_string(&app.agents_file)
        .await
        .or_else(|error| match error.kind() {
            io::ErrorKind::NotFound => Ok(String::new()),
            _ => Err(error),
        })
        .map(Json)
        .map_err(|error| storage_error("прочитать", error))
}

async fn save(
    State(app): State<Arc<App>>,
    Json(content): Json<String>,
) -> Result<StatusCode, HttpError> {
    tokio::task::spawn_blocking(move || -> io::Result<()> {
        let directory = app.agents_file.parent().expect("AGENTS.md parent");
        fs::create_dir_all(directory)?;
        let mut file = tempfile::NamedTempFile::new_in(directory)?;
        file.write_all(content.as_bytes())?;
        file.persist(&app.agents_file)?;
        Ok(())
    })
    .await
    .map_err(|error| storage_error("сохранить", error))?
    .map_err(|error| storage_error("сохранить", error))?;
    Ok(StatusCode::NO_CONTENT)
}

fn storage_error(action: &str, error: impl std::fmt::Display) -> HttpError {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        format!("Не удалось {action} AGENTS.md. {error}"),
    )
}
