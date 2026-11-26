use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::Arc,
};

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::StatusCode,
    routing::{get, post},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};

use super::App;

const MANIFEST: &str = "SKILL.md";
const BUILT_IN_SKILLS: &[(&str, &str)] = &[
    (
        "ast-index",
        include_str!("../../defaults/skills/ast-index/SKILL.md"),
    ),
    (
        "post-code-review",
        include_str!("../../defaults/skills/post-code-review/SKILL.md"),
    ),
];
const UPLOAD_LIMIT: usize = 16 * 1024 * 1024;

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

#[derive(Deserialize)]
struct UploadedFile {
    path: String,
    data: String,
}

type HttpError = (StatusCode, String);

pub(super) fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/skills", get(list).post(install))
        .route(
            "/skills/upload",
            post(upload).layer(DefaultBodyLimit::max(24 * 1024 * 1024)),
        )
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

async fn upload(
    State(app): State<Arc<App>>,
    Json(files): Json<Vec<UploadedFile>>,
) -> Result<(StatusCode, Json<Skill>), HttpError> {
    tokio::task::spawn_blocking(move || {
        app.skills
            .upload(files)
            .map(|skill| (StatusCode::CREATED, Json(skill)))
    })
    .await
    .map_err(storage_error)?
}

impl Store {
    pub(super) fn new(directory: PathBuf) -> Self {
        Self { directory }
    }

    pub(super) fn install_defaults(&self) -> io::Result<()> {
        for &(name, manifest) in BUILT_IN_SKILLS {
            match fs::symlink_metadata(self.directory.join(name)) {
                Ok(_) => continue,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            match self.upload(vec![UploadedFile {
                path: MANIFEST.into(),
                data: STANDARD.encode(manifest),
            }]) {
                Ok(_) | Err((StatusCode::CONFLICT, _)) => {}
                Err((_, message)) => return Err(io::Error::other(message)),
            }
        }
        Ok(())
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
        let root = self.root()?;
        if root.starts_with(&source) {
            return Err((
                StatusCode::BAD_REQUEST,
                "Папка скилла содержит хранилище Lyra.".into(),
            ));
        }
        Self::install_target(&root, &skill)?;
        let staging = staging(&root)?;
        let package = staging.path().join("package");
        copy_directory(&source, &package).map_err(source_error)?;
        let copied_skill = manifest(&package).map_err(source_error)?;
        if copied_skill.name != skill.name {
            return Err((
                StatusCode::CONFLICT,
                "Имя скилла изменилось во время установки.".into(),
            ));
        }
        Self::publish(&root, &package, copied_skill)
    }

    fn upload(&self, files: Vec<UploadedFile>) -> Result<Skill, HttpError> {
        if files.is_empty() || files.len() > 2048 {
            return Err(source_error(invalid("Выберите от 1 до 2048 файлов.")));
        }
        let root = self.root()?;
        let staging = staging(&root)?;
        let package = staging.path().join("package");
        fs::create_dir(&package).map_err(storage_error)?;
        let mut remaining = UPLOAD_LIMIT;
        for file in files {
            let path = package.join(upload_path(&file.path).map_err(source_error)?);
            let data = STANDARD
                .decode(file.data)
                .map_err(|_| source_error(invalid("Повреждены данные файла.")))?;
            remaining = remaining.checked_sub(data.len()).ok_or_else(|| {
                (
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "Скилл превышает 16 МБ.".into(),
                )
            })?;
            fs::create_dir_all(path.parent().expect("upload parent")).map_err(source_error)?;
            let mut target = fs::File::create_new(path).map_err(source_error)?;
            io::Write::write_all(&mut target, &data).map_err(storage_error)?;
        }
        let skill = manifest(&package).map_err(source_error)?;
        Self::publish(&root, &package, skill)
    }

    fn root(&self) -> Result<PathBuf, HttpError> {
        fs::create_dir_all(&self.directory).map_err(storage_error)?;
        fs::canonicalize(&self.directory).map_err(storage_error)
    }

    fn install_target(root: &Path, skill: &Skill) -> Result<PathBuf, HttpError> {
        let target = root.join(&skill.name);
        match fs::symlink_metadata(&target) {
            Ok(_) => Err(duplicate(skill)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(target),
            Err(error) => Err(storage_error(error)),
        }
    }

    fn publish(root: &Path, package: &Path, skill: Skill) -> Result<Skill, HttpError> {
        let target = Self::install_target(root, &skill)?;
        if let Err(error) = fs::rename(package, &target) {
            return Err(if fs::symlink_metadata(&target).is_ok() {
                duplicate(&skill)
            } else {
                storage_error(error)
            });
        }
        Ok(skill)
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

fn staging(root: &Path) -> Result<tempfile::TempDir, HttpError> {
    tempfile::Builder::new()
        .prefix(".install-")
        .tempdir_in(root)
        .map_err(storage_error)
}

fn duplicate(skill: &Skill) -> HttpError {
    (
        StatusCode::CONFLICT,
        format!("Скилл «{}» уже установлен.", skill.name),
    )
}

fn upload_path(path: &str) -> io::Result<PathBuf> {
    let mut relative = PathBuf::new();
    let mut parts = path.split('/');
    if path.len() > 4096 || parts.clone().count() > 64 {
        return Err(invalid("Слишком длинный путь внутри скилла."));
    }
    for part in &mut parts {
        let stem = part
            .split('.')
            .next()
            .unwrap_or_default()
            .trim_end()
            .to_ascii_uppercase();
        let device = matches!(
            stem.as_str(),
            "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
        ) || ["COM", "LPT"].iter().any(|prefix| {
            stem.strip_prefix(prefix).is_some_and(|suffix| {
                matches!(
                    suffix,
                    "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                )
            })
        });
        if part.is_empty()
            || part.ends_with(['.', ' '])
            || device
            || part
                .chars()
                .any(|ch| ch.is_control() || "\\:\"<>|?*".contains(ch))
        {
            return Err(invalid(format!("Недопустимый путь внутри скилла: {path}")));
        }
        relative.push(part);
    }
    Ok(relative)
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
    fn installs_new_built_in_skills_with_existing_local_versions() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::new(temp.path().join("installed"));
        let ast_index = store.directory.join("ast-index");
        fs::create_dir_all(&ast_index).unwrap();
        let custom = "---\nname: ast-index\ndescription: Local version\n---\n";
        fs::write(ast_index.join(MANIFEST), custom).unwrap();

        store.install_defaults().unwrap();
        store.install_defaults().unwrap();

        let skills = store.list().unwrap();
        assert_eq!(skills.len(), 2);
        assert_eq!(skills[0].name, "ast-index");
        assert_eq!(skills[0].description, "Local version");
        assert_eq!(skills[1].name, "post-code-review");
        assert_eq!(
            store.paths(Some(&["post-code-review".into()])).unwrap(),
            [fs::canonicalize(&store.directory)
                .unwrap()
                .join("post-code-review")
                .join(MANIFEST)]
        );
    }

    #[test]
    fn uploads_nested_binary_files_and_rejects_paths_outside_the_package() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::new(temp.path().join("installed"));
        let file = |path: &str, bytes: &[u8]| UploadedFile {
            path: path.into(),
            data: STANDARD.encode(bytes),
        };
        let manifest = || file(MANIFEST, b"---\nname: uploaded\ndescription: Test\n---\n");
        for path in [
            "../escape",
            "assets/../../escape",
            "/absolute",
            "C:/absolute",
            "assets\\escape",
            "NUL.txt",
        ] {
            assert!(store.upload(vec![manifest(), file(path, b"data")]).is_err());
            assert_eq!(fs::read_dir(&store.directory).unwrap().count(), 0);
        }
        let bytes = [0, 1, 128, 255];
        let skill = store
            .upload(vec![manifest(), file("assets/nested/data.bin", &bytes)])
            .unwrap();
        assert_eq!(skill.name, "uploaded");
        let paths = store.paths(None).unwrap();
        assert_eq!(
            fs::read(paths[0].parent().unwrap().join("assets/nested/data.bin")).unwrap(),
            bytes,
        );
        assert!(matches!(
            store.upload(vec![manifest()]),
            Err((StatusCode::CONFLICT, _))
        ));
    }

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
