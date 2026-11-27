use std::{
    fs, io,
    path::PathBuf,
    sync::{Arc, LazyLock},
};

use axum::{Json, Router, extract::State, http::StatusCode, routing::get};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use super::{App, mcp};
use crate::{model_name, ollama};

#[derive(Deserialize)]
pub(super) struct AiSettings {
    pub(super) minimum_output_tokens: u32,
    pub(super) default_output_tokens: u32,
    pub(super) maximum_output_tokens: u32,
    pub(super) parallel_project_tasks: usize,
    pub(super) parallel_project_models: usize,
    #[serde(default)]
    pub(super) project_models: ProjectModels,
}

#[derive(Clone, Default, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ProjectModels {
    pub(super) implement: Option<String>,
    pub(super) fix: Option<String>,
    pub(super) review: Option<String>,
}

pub(super) static AI_SETTINGS: LazyLock<AiSettings> = LazyLock::new(|| {
    let settings: AiSettings = serde_saphyr::from_str(include_str!("../../ai-settings.yaml"))
        .expect("Некорректный ai-settings.yaml");
    assert!(
        settings.minimum_output_tokens > 0
            && (settings.minimum_output_tokens..=settings.maximum_output_tokens)
                .contains(&settings.default_output_tokens),
        "В ai-settings.yaml требуется 0 < minimum_output_tokens ≤ default_output_tokens ≤ maximum_output_tokens"
    );
    assert!(
        (1..=16).contains(&settings.parallel_project_tasks)
            && (1..=settings.parallel_project_tasks).contains(&settings.parallel_project_models),
        "В ai-settings.yaml требуется 1 ≤ parallel_project_models ≤ parallel_project_tasks ≤ 16"
    );
    settings
});

pub(super) struct ProjectModelStore {
    path: PathBuf,
    current: Mutex<ProjectModels>,
}

impl ProjectModelStore {
    pub(super) fn load(path: PathBuf) -> io::Result<Self> {
        let current = match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(io::Error::other)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                AI_SETTINGS.project_models.clone()
            }
            Err(error) => return Err(error),
        };
        Ok(Self {
            path,
            current: Mutex::new(current),
        })
    }

    pub(super) async fn get(&self) -> ProjectModels {
        self.current.lock().await.clone()
    }

    async fn save(&self, requested: ProjectModels) -> Result<ProjectModels, (StatusCode, String)> {
        let mut current = self.current.lock().await;
        if *current == requested {
            return Ok(current.clone());
        }
        let previous = current.clone();
        let path = self.path.clone();
        let saved = tokio::task::spawn_blocking(move || {
            let mut requested = requested;
            let changes = [
                (&previous.implement, &mut requested.implement),
                (&previous.fix, &mut requested.fix),
                (&previous.review, &mut requested.review),
            ];
            let mut installed = None;
            for (old, new) in changes {
                if let Some(requested_tag) = new.as_deref().filter(|_| old != new) {
                    let name = model_name(requested_tag)
                        .map_err(|error| (StatusCode::BAD_REQUEST, error))?;
                    if installed.is_none() {
                        installed = Some(ollama::installed_models().ok_or((
                            StatusCode::SERVICE_UNAVAILABLE,
                            "Ollama недоступна.".into(),
                        ))?);
                    }
                    *new = Some(
                        mcp::resolve_model(installed.as_deref().unwrap_or_default(), &name)
                            .map_err(|error| (StatusCode::BAD_REQUEST, error))?,
                    );
                }
            }
            let directory = path.parent().expect("model roles parent");
            fs::create_dir_all(directory).map_err(storage_error)?;
            let file = tempfile::NamedTempFile::new_in(directory).map_err(storage_error)?;
            serde_json::to_writer(file.as_file(), &requested).map_err(storage_error)?;
            file.persist(path).map_err(storage_error)?;
            Ok(requested)
        })
        .await
        .map_err(storage_error)??;
        *current = saved.clone();
        Ok(saved)
    }
}

fn storage_error(error: impl std::fmt::Display) -> (StatusCode, String) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        format!("Не удалось сохранить роли моделей. {error}"),
    )
}

pub(super) fn routes() -> Router<Arc<App>> {
    Router::new().route("/model/roles", get(get_roles).post(set_roles))
}

async fn get_roles(State(app): State<Arc<App>>) -> Json<ProjectModels> {
    Json(app.project_models.get().await)
}

async fn set_roles(
    State(app): State<Arc<App>>,
    Json(roles): Json<ProjectModels>,
) -> Result<Json<ProjectModels>, (StatusCode, String)> {
    tokio::spawn(async move { app.project_models.save(roles).await })
        .await
        .map_err(storage_error)?
        .map(Json)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn persisted_roles_override_defaults_and_can_be_reset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("project-models.json");
        fs::write(
            &path,
            r#"{"implement":"worker:latest","fix":null,"review":"reviewer:latest"}"#,
        )
        .unwrap();
        let store = ProjectModelStore::load(path.clone()).unwrap();
        assert_eq!(store.get().await.review.as_deref(), Some("reviewer:latest"));
        store
            .save(ProjectModels {
                implement: Some("worker:latest".into()),
                fix: None,
                review: None,
            })
            .await
            .unwrap();
        let reopened = ProjectModelStore::load(path).unwrap().get().await;
        assert_eq!(reopened.implement.as_deref(), Some("worker:latest"));
        assert!(reopened.review.is_none());
    }
}
