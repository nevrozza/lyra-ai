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
    write_agents(path, DEFAULT_AGENTS)
}

fn write_agents(path: &Path, content: &str) -> io::Result<()> {
    let directory = path.parent().expect("AGENTS.md parent");
    fs::create_dir_all(directory)?;
    let mut file = tempfile::NamedTempFile::new_in(directory)?;
    file.write_all(content.as_bytes())?;
    file.persist(path)?;
    Ok(())
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
    tokio::task::spawn_blocking(move || write_agents(&app.agents_file, &content))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_replaces_the_global_agents_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("AGENTS.md");
        write_agents(&path, "Previous local instructions").unwrap();
        install_default(&path).unwrap();
        assert_eq!(fs::read_to_string(path).unwrap(), DEFAULT_AGENTS);
    }
}
