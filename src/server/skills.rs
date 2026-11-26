use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::Arc,
};

use axum::{Json, Router, extract::State, http::StatusCode, routing::get};
use serde::{Deserialize, Serialize};

use super::App;

const MANIFEST: &str = "SKILL.md";

pub(super) struct Store {
    directory: PathBuf,
}

#[derive(Deserialize, Serialize)]
struct Skill {
    name: String,
    description: String,
}

#[derive(Deserialize)]
struct Import {
    path: PathBuf,
}

type HttpError = (StatusCode, String);

pub(super) fn routes() -> Router<Arc<App>> {
    Router::new().route("/skills", get(list).post(install))
}

async fn list(State(app): State<Arc<App>>) -> Result<Json<Vec<Skill>>, HttpError> {
    tokio::task::spawn_blocking(move || app.skills.list().map(Json))
        .await
        .map_err(storage_error)?
        .map_err(storage_error)
}

async fn install(
    State(app): State<Arc<App>>,
    Json(import): Json<Import>,
) -> Result<(StatusCode, Json<Skill>), HttpError> {
    tokio::task::spawn_blocking(move || {
        app.skills
            .install(&import.path)
            .map(|skill| (StatusCode::CREATED, Json(skill)))
    })
    .await
    .map_err(storage_error)?
}

impl Store {
    pub(super) fn new(directory: PathBuf) -> Self {
        Self { directory }
    }

    fn install(&self, path: &Path) -> Result<Skill, HttpError> {
        if !path.is_absolute() {
            return Err((
                StatusCode::BAD_REQUEST,
                "Укажите абсолютный путь к папке скилла.".into(),
            ));
        }
        let source = fs::canonicalize(path).map_err(source_error)?;
        let skill = manifest(&source).map_err(source_error)?;
        fs::create_dir_all(&self.directory).map_err(storage_error)?;
        let root = fs::canonicalize(&self.directory).map_err(storage_error)?;
        if root.starts_with(&source) {
            return Err((
                StatusCode::BAD_REQUEST,
                "Папка скилла содержит хранилище Lyra.".into(),
            ));
        }
        let target = root.join(&skill.name);
        let duplicate = || {
            (
                StatusCode::CONFLICT,
                format!("Скилл «{}» уже установлен.", skill.name),
            )
        };
        match fs::symlink_metadata(&target) {
            Ok(_) => return Err(duplicate()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(storage_error(error)),
        }
        let staging = tempfile::Builder::new()
            .prefix(".install-")
            .tempdir_in(&root)
            .map_err(storage_error)?;
        let package = staging.path().join(&skill.name);
        copy_directory(&source, &package).map_err(source_error)?;
        let copied_skill = manifest(&package).map_err(source_error)?;
        if copied_skill.name != skill.name {
            return Err((
                StatusCode::CONFLICT,
                "Имя скилла изменилось во время установки.".into(),
            ));
        }
        if let Err(error) = fs::rename(package, &target) {
            return Err(if fs::symlink_metadata(&target).is_ok() {
                duplicate()
            } else {
                storage_error(error)
            });
        }
        Ok(copied_skill)
    }

    fn list(&self) -> io::Result<Vec<Skill>> {
        let entries = match fs::read_dir(&self.directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        let mut skills = Vec::new();
        for entry in entries {
            let entry = entry?;
            let folder = entry.file_name();
            let Some(name) = folder.to_str().filter(|name| valid_name(name)) else {
                continue;
            };
            let metadata = fs::symlink_metadata(entry.path())?;
            if is_link(&metadata) || !metadata.is_dir() {
                continue;
            }
            let skill = manifest(&entry.path())
                .map_err(|error| io::Error::new(error.kind(), format!("{name}: {error}")))?;
            if skill.name != name {
                return Err(invalid(format!(
                    "Имя скилла «{}» не совпадает с папкой «{name}».",
                    skill.name
                )));
            }
            skills.push(skill);
        }
        skills.sort_unstable_by(|left, right| left.name.cmp(&right.name));
        Ok(skills)
    }

    pub(super) fn paths(&self, selected: Option<&[String]>) -> Result<Vec<PathBuf>, String> {
        if selected.is_some_and(<[String]>::is_empty) {
            return Ok(Vec::new());
        }
        let installed = self.list().map_err(|error| storage_error(error).1)?;
        if let Some(selected) = selected {
            for name in selected {
                if !installed.iter().any(|skill| &skill.name == name) {
                    return Err(format!("Pi skill is not installed: {name}"));
                }
            }
        }
        if installed.is_empty() {
            return Ok(Vec::new());
        }
        let directory =
            fs::canonicalize(&self.directory).map_err(|error| storage_error(error).1)?;
        Ok(installed
            .into_iter()
            .filter(|skill| selected.is_none_or(|names| names.contains(&skill.name)))
            .map(|skill| directory.join(skill.name).join(MANIFEST))
            .collect())
    }
}

fn manifest(directory: &Path) -> io::Result<Skill> {
    let path = directory.join(MANIFEST);
    let metadata = fs::symlink_metadata(&path).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("Не удалось открыть {MANIFEST}: {error}"),
        )
    })?;
    if is_link(&metadata) || !metadata.is_file() {
        return Err(invalid(format!("{MANIFEST} должен быть обычным файлом.")));
    }
    let text = fs::read_to_string(path)?;
    let mut lines = text.trim_start_matches('\u{feff}').lines();
    if lines.next() != Some("---") {
        return Err(invalid("В SKILL.md нужен блок YAML между строками «---»."));
    }
    let mut yaml = String::new();
    for line in lines {
        if line == "---" {
            let skill: Skill = serde_saphyr::from_str(&yaml)
                .map_err(|error| invalid(format!("Некорректный YAML в SKILL.md: {error}")))?;
            validate(&skill)?;
            return Ok(skill);
        }
        yaml.push_str(line);
        yaml.push('\n');
    }
    Err(invalid("В SKILL.md нет закрывающей строки «---»."))
}

fn validate(skill: &Skill) -> io::Result<()> {
    if !valid_name(&skill.name) {
        return Err(invalid(
            "Имя скилла должно содержать до 64 строчных латинских букв, цифр и одиночных дефисов между ними.",
        ));
    }
    if skill.description.trim().is_empty() || skill.description.chars().count() > 1024 {
        return Err(invalid(
            "Укажите описание скилла длиной от 1 до 1024 символов.",
        ));
    }
    Ok(())
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.split('-').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        })
}

fn copy_directory(source: &Path, target: &Path) -> io::Result<()> {
    fs::create_dir(target)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let from = entry.path();
        let to = target.join(entry.file_name());
        let metadata = fs::symlink_metadata(&from)?;
        if is_link(&metadata) {
            return Err(invalid(format!("Ссылка внутри скилла: {}", from.display())));
        }
        if metadata.is_dir() {
            copy_directory(&from, &to)?;
        } else if metadata.is_file() {
            fs::copy(from, to)?;
        } else {
            return Err(invalid(format!(
                "Неподдерживаемый тип файла: {}",
                from.display()
            )));
        }
    }
    Ok(())
}

fn is_link(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const REPARSE_POINT: u32 = 0x400;
        metadata.file_attributes() & REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        metadata.is_symlink()
    }
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn source_error(error: io::Error) -> HttpError {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        format!("Не удалось импортировать скилл. {error}"),
    )
}

fn storage_error(error: impl std::fmt::Display) -> HttpError {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        format!("Не удалось открыть хранилище скиллов Lyra. {error}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installs_a_snapshot_with_supporting_files_and_rejects_duplicates() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source folder");
        fs::create_dir_all(source.join("references")).unwrap();
        fs::write(
            source.join(MANIFEST),
            "---\r\nname: project-check\r\ndescription: >-\r\n  Check the project\r\n  before release.\r\n---\r\n# Guide\r\n",
        ).unwrap();
        fs::write(source.join("references/checks.md"), "checks").unwrap();

        let store = Store::new(temp.path().join("installed"));
        assert!(store.paths(None).unwrap().is_empty());
        let installed = store.install(&source).unwrap();
        assert_eq!(installed.description, "Check the project before release.");
        assert!(matches!(
            store.install(&source),
            Err((StatusCode::CONFLICT, _))
        ));
        fs::remove_dir_all(source).unwrap();

        let paths = store.paths(Some(&["project-check".into()])).unwrap();
        assert_eq!(paths, store.paths(None).unwrap());
        assert!(paths[0].is_file());
        let checks = paths[0].parent().unwrap().join("references/checks.md");
        assert_eq!(fs::read_to_string(checks).unwrap(), "checks");
        assert!(store.paths(Some(&[])).unwrap().is_empty());
        assert!(store.paths(Some(&["unknown".into()])).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_copy_does_not_leave_a_partial_installation() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(
            source.join(MANIFEST),
            "---\nname: sample\ndescription: Example\n---\n",
        )
        .unwrap();
        std::os::unix::fs::symlink(temp.path(), source.join("link")).unwrap();
        let store = Store::new(temp.path().join("installed"));
        assert!(store.install(&source).is_err());
        assert_eq!(fs::read_dir(&store.directory).unwrap().count(), 0);
    }
}
