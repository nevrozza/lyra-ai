use std::{
    collections::BTreeSet,
    fs, io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
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
pub(super) const IMPLEMENT: &str = include_str!("../../defaults/skills/lyra-implement/SKILL.md");
pub(super) const FIX: &str = include_str!("../../defaults/skills/lyra-fix/SKILL.md");
pub(super) const REVIEW: &str = include_str!("../../defaults/skills/lyra-review/SKILL.md");
const BUILT_IN_SKILLS: &[(&str, &str)] = &[
    (
        "ast-index",
        include_str!("../../defaults/skills/ast-index/SKILL.md"),
    ),
    (
        "post-code-review",
        include_str!("../../defaults/skills/post-code-review/SKILL.md"),
    ),
    ("lyra-implement", IMPLEMENT),
    ("lyra-fix", FIX),
    ("lyra-review", REVIEW),
];

fn is_builtin(name: &str) -> bool {
    BUILT_IN_SKILLS.iter().any(|(builtin, _)| *builtin == name)
}

pub(super) fn is_role_skill(name: &str) -> bool {
    matches!(name, "lyra-implement" | "lyra-fix" | "lyra-review")
}
const UPLOAD_LIMIT: usize = 16 * 1024 * 1024;

pub(super) struct Store {
    directory: PathBuf,
    lock: Mutex<()>,
}

#[derive(Deserialize, Serialize)]
pub(super) struct Skill {
    name: String,
    description: String,
    #[serde(default = "enabled_by_default")]
    enabled: bool,
    #[serde(default)]
    role_template: bool,
    #[serde(default)]
    builtin: bool,
}

fn enabled_by_default() -> bool { true }

#[derive(Default, Deserialize, Serialize)]
struct SkillState {
    #[serde(default)]
    disabled: BTreeSet<String>,
    #[serde(default)]
    removed: BTreeSet<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetEnabled {
    name: String,
    enabled: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoveSkill {
    name: String,
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
        .route("/skills/enabled", post(set_enabled))
        .route("/skills/remove", post(remove))
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
            .map(|skill| {
                app.task.send_modify(crate::run::Snapshot::skills_changed);
                (StatusCode::CREATED, Json(skill))
            })
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
            .map(|skill| {
                app.task.send_modify(crate::run::Snapshot::skills_changed);
                (StatusCode::CREATED, Json(skill))
            })
    })
    .await
    .map_err(storage_error)?
}

async fn set_enabled(
    State(app): State<Arc<App>>,
    Json(input): Json<SetEnabled>,
) -> Result<Json<Skill>, HttpError> {
    tokio::task::spawn_blocking(move || {
        let skill = app.skills.set_enabled(&input.name, input.enabled)?;
        app.task.send_modify(crate::run::Snapshot::skills_changed);
        Ok(Json(skill))
    })
        .await
        .map_err(storage_error)?
}

async fn remove(
    State(app): State<Arc<App>>,
    Json(input): Json<RemoveSkill>,
) -> Result<StatusCode, HttpError> {
    tokio::task::spawn_blocking(move || {
        let snapshot = app.task.borrow();
        if snapshot.busy() {
            return Err((StatusCode::CONFLICT, "Дождитесь завершения активных запусков перед удалением скилла.".into()));
        }
        app.skills.remove(&input.name)?;
        drop(snapshot);
        app.task.send_modify(crate::run::Snapshot::skills_changed);
        Ok(StatusCode::NO_CONTENT)
    }).await.map_err(storage_error)?
}

impl Store {
    pub(super) fn new(directory: PathBuf) -> Self {
        Self { directory, lock: Mutex::new(()) }
    }

    pub(super) fn install_defaults(&self) -> io::Result<()> {
        let _guard = self.lock.lock().expect("skill store lock");
        fs::create_dir_all(&self.directory)?;
        let mut state = self.state()?;
        // Restore built-ins removed by an earlier version, but respect the user's
        // intent not to use them by restoring them in the disabled state.
        let removed = std::mem::take(&mut state.removed);
        let migrated = !removed.is_empty();
        state.disabled.extend(removed.into_iter().filter(|name| is_builtin(name)));
        for &(name, manifest) in BUILT_IN_SKILLS {
            let directory = self.directory.join(name);
            match fs::create_dir(&directory) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    let metadata = fs::symlink_metadata(&directory)?;
                    if is_link(&metadata) || !metadata.is_dir() {
                        return Err(invalid(format!(
                            "Недопустимая папка встроенного скилла: {}",
                            directory.display()
                        )));
                    }
                }
                Err(error) => return Err(error),
            }
            let mut file = tempfile::NamedTempFile::new_in(&directory)?;
            io::Write::write_all(&mut file, manifest.as_bytes())?;
            file.persist(directory.join(MANIFEST))?;
        }
        if migrated { self.save_state(&state)?; }
        Ok(())
    }

    fn install(&self, path: &Path) -> Result<Skill, HttpError> {
        let _guard = self.lock.lock().expect("skill store lock");
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
        self.publish(&root, &package, copied_skill)
    }

    fn upload(&self, files: Vec<UploadedFile>) -> Result<Skill, HttpError> {
        let _guard = self.lock.lock().expect("skill store lock");
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
        self.publish(&root, &package, skill)
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

    fn publish(&self, root: &Path, package: &Path, mut skill: Skill) -> Result<Skill, HttpError> {
        let mut state = self.state().map_err(storage_error)?;
        let target = Self::install_target(root, &skill)?;
        if let Err(error) = fs::rename(package, &target) {
            return Err(if fs::symlink_metadata(&target).is_ok() {
                duplicate(&skill)
            } else {
                storage_error(error)
            });
        }
        let changed = state.removed.remove(&skill.name) | state.disabled.remove(&skill.name);
        if changed {
            if let Err(error) = self.save_state(&state) {
                fs::rename(&target, package).map_err(storage_error)?;
                return Err(storage_error(error));
            }
        }
        skill.enabled = true;
        skill.role_template = is_role_skill(&skill.name);
        skill.builtin = is_builtin(&skill.name);
        Ok(skill)
    }

    fn state(&self) -> io::Result<SkillState> {
        match fs::read(self.directory.join(".skill-state.json")) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(io::Error::other),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(SkillState::default()),
            Err(error) => Err(error),
        }
    }

    fn save_state(&self, state: &SkillState) -> io::Result<()> {
        let mut file = tempfile::NamedTempFile::new_in(&self.directory)?;
        serde_json::to_writer(&mut file, state).map_err(io::Error::other)?;
        file.as_file().sync_all()?;
        file.persist(self.directory.join(".skill-state.json"))?;
        Ok(())
    }

    fn installed_skill(&self, name: &str) -> Result<(PathBuf, Skill), HttpError> {
        if !valid_name(name) {
            return Err((StatusCode::BAD_REQUEST, "Некорректное имя скилла.".into()));
        }
        let target = self.directory.join(name);
        let metadata = fs::symlink_metadata(&target).map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                (StatusCode::NOT_FOUND, format!("Скилл «{name}» не установлен."))
            } else { storage_error(error) }
        })?;
        if is_link(&metadata) || !metadata.is_dir() {
            return Err((StatusCode::BAD_REQUEST, "Недопустимая папка скилла.".into()));
        }
        let skill = manifest(&target).map_err(storage_error)?;
        if skill.name != name {
            return Err((StatusCode::CONFLICT, "Имя скилла не совпадает с папкой.".into()));
        }
        Ok((target, skill))
    }

    fn set_enabled(&self, name: &str, enabled: bool) -> Result<Skill, HttpError> {
        let _guard = self.lock.lock().expect("skill store lock");
        let (_, mut skill) = self.installed_skill(name)?;
        let mut state = self.state().map_err(storage_error)?;
        if state.removed.contains(name) {
            return Err((StatusCode::NOT_FOUND, format!("Скилл «{name}» удалён.")));
        }
        let changed = if enabled { state.disabled.remove(name) } else { state.disabled.insert(name.into()) };
        if changed { self.save_state(&state).map_err(storage_error)?; }
        skill.enabled = enabled;
        skill.role_template = is_role_skill(name);
        skill.builtin = is_builtin(name);
        Ok(skill)
    }

    fn remove(&self, name: &str) -> Result<(), HttpError> {
        let _guard = self.lock.lock().expect("skill store lock");
        if is_builtin(name) {
            return Err((StatusCode::FORBIDDEN, "Встроенный скилл нельзя удалить. Его можно отключить.".into()));
        }
        let (target, _) = self.installed_skill(name)?;
        let mut state = self.state().map_err(storage_error)?;
        state.disabled.remove(name);
        let trash = staging(&self.directory)?;
        let package = trash.path().join("package");
        fs::rename(&target, &package).map_err(storage_error)?;
        if let Err(error) = self.save_state(&state) {
            fs::rename(&package, &target).map_err(storage_error)?;
            return Err(storage_error(error));
        }
        // TempDir cleans up all package files; no original import folder is touched.
        trash.close().map_err(storage_error)?;
        Ok(())
    }

    pub(super) fn list(&self) -> io::Result<Vec<Skill>> {
        let _guard = self.lock.lock().expect("skill store lock");
        self.list_unlocked()
    }

    fn list_unlocked(&self) -> io::Result<Vec<Skill>> {
        let state = self.state()?;
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
            if state.removed.contains(name) { continue; }
            let mut skill = manifest(&entry.path())
                .map_err(|error| io::Error::new(error.kind(), format!("{name}: {error}")))?;
            if skill.name != name {
                return Err(invalid(format!(
                    "Имя скилла «{}» не совпадает с папкой «{name}».",
                    skill.name
                )));
            }
            skill.enabled = !state.disabled.contains(name);
            skill.role_template = is_role_skill(name);
            skill.builtin = is_builtin(name);
            skills.push(skill);
        }
        skills.sort_unstable_by(|left, right| left.name.cmp(&right.name));
        Ok(skills)
    }

    pub(super) fn enabled(&self, name: &str) -> Result<bool, String> {
        self.list().map(|skills| skills.iter().any(|skill| skill.name == name && skill.enabled))
            .map_err(|error| storage_error(error).1)
    }

    pub(super) fn paths(&self, selected: Option<&[String]>) -> Result<Vec<PathBuf>, String> {
        if selected.is_some_and(<[String]>::is_empty) {
            return Ok(Vec::new());
        }
        let installed = self.list().map_err(|error| storage_error(error).1)?;
        if let Some(selected) = selected {
            for name in selected {
                match installed.iter().find(|skill| &skill.name == name) {
                    None => return Err(format!("Pi skill is not installed: {name}")),
                    Some(skill) if !skill.enabled => return Err(format!("Pi skill is disabled: {name}")),
                    Some(_) => {}
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
            .filter(|skill| {
                skill.enabled && selected.map_or_else(
                    || !is_role_skill(&skill.name),
                    |names| names.contains(&skill.name),
                )
            })
            .map(|skill| directory.join(skill.name).join(MANIFEST))
            .collect())
    }
}

// The selected manifest paths, not Pi's discovery rules, define this run's catalog.
pub(super) fn prompt_catalog(paths: &[PathBuf]) -> io::Result<String> {
    use std::fmt::Write;
    let mut text = String::from("\n\n# Available Lyra skills\nUse load_skill with the exact skill name when it matches the task or the user requests it. Only the names below are available to this run. The tool resolves the actual SKILL.md location; do not guess a path. Its response includes the package directory for any referenced resources. If a requested skill is absent, report that fact. Do not claim a skill was loaded unless the tool succeeded.\n");
    if paths.is_empty() {
        text.push_str("No additional skills selected.\n");
    }
    for path in paths {
        let directory = path.parent().ok_or_else(|| invalid("Missing skill directory"))?;
        let skill = manifest(directory)?;
        let description = skill.description.replace(['\r', '\n'], " ");
        let _ = writeln!(text, "- {}: {}", skill.name, description);
    }
    Ok(text)
}

pub(super) fn selected_manifest<'a>(paths: &'a [PathBuf], name: &str) -> Result<&'a Path, String> {
    paths.iter().find(|path|
        path.parent().and_then(Path::file_name).and_then(|part| part.to_str()) == Some(name)
    ).map(PathBuf::as_path).ok_or_else(|| {
        let available = paths.iter().filter_map(|path|
            path.parent()?.file_name()?.to_str()
        ).collect::<Vec<_>>().join(", ");
        format!("SKILL_NOT_AVAILABLE: {name}. Available skills: {}",
            if available.is_empty() { "(none)" } else { &available })
    })
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
    let (yaml, _) = split_frontmatter(&text)?;
    let mut skill: Skill = serde_saphyr::from_str(yaml)
        .map_err(|error| invalid(format!("Некорректный YAML в SKILL.md: {error}")))?;
    validate(&skill)?;
    // Catalog metadata comes from the store, never from imported frontmatter.
    skill.enabled = true;
    skill.role_template = is_role_skill(&skill.name);
    skill.builtin = is_builtin(&skill.name);
    Ok(skill)
}

pub(super) fn split_frontmatter(text: &str) -> io::Result<(&str, &str)> {
    let (_, remainder) = text
        .trim_start_matches('\u{feff}')
        .split_once('\n')
        .filter(|(line, _)| line.trim_end_matches('\r') == "---")
        .ok_or_else(|| invalid("В SKILL.md нужен блок YAML между строками «---»."))?;
    let mut offset = 0;
    for line in remainder.split_inclusive('\n') {
        if line.trim_end_matches(['\r', '\n']) == "---" {
            return Ok((&remainder[..offset], &remainder[offset + line.len()..]));
        }
        offset += line.len();
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
    fn reads_bundled_role_skill_bodies_with_windows_line_endings() {
        for manifest in [IMPLEMENT, FIX, REVIEW] {
            let normalized = manifest.replace("\r\n", "\n");
            let (_, body) = split_frontmatter(&normalized).unwrap();
            let windows = format!("\u{feff}{}", normalized.replace('\n', "\r\n"));
            let (_, windows_body) = split_frontmatter(&windows).unwrap();
            assert!(!body.trim().is_empty());
            assert_eq!(windows_body.replace("\r\n", "\n"), body);
        }
        assert!(split_frontmatter("---\r\nname: broken").is_err());
        assert!(split_frontmatter("No manifest\n---\n").is_err());
    }

    #[test]
    fn refreshes_built_in_skills_alongside_imported_skills() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::new(temp.path().join("installed"));
        let ast_index = store.directory.join("ast-index");
        fs::create_dir_all(&ast_index).unwrap();
        let custom = "---\nname: ast-index\ndescription: Local version\n---\n";
        fs::write(ast_index.join(MANIFEST), custom).unwrap();

        let imported = store.directory.join("custom");
        fs::create_dir(&imported).unwrap();
        let imported_manifest = "---\nname: custom\ndescription: Imported skill\n---\n";
        fs::write(imported.join(MANIFEST), imported_manifest).unwrap();

        store.install_defaults().unwrap();
        store.install_defaults().unwrap();

        let skills = store.list().unwrap();
        assert_eq!(skills.len(), BUILT_IN_SKILLS.len() + 1);
        assert_eq!(
            fs::read_to_string(ast_index.join(MANIFEST)).unwrap(),
            BUILT_IN_SKILLS[0].1
        );
        assert_eq!(
            fs::read_to_string(imported.join(MANIFEST)).unwrap(),
            imported_manifest
        );
        assert!(skills.iter().any(|skill| skill.name == "post-code-review"));
        assert!(store.paths(None).unwrap().iter().all(|path| {
            !is_role_skill(
                path.parent()
                    .unwrap()
                    .file_name()
                    .unwrap()
                    .to_str()
                    .unwrap(),
            )
        }));
        assert_eq!(
            store.paths(Some(&["post-code-review".into()])).unwrap(),
            [fs::canonicalize(&store.directory)
                .unwrap()
                .join("post-code-review")
                .join(MANIFEST)]
        );
    }

    #[test]
    fn builtins_are_protected_but_can_be_disabled_and_reenabled() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("installed");
        let store = Store::new(directory.clone());
        store.install_defaults().unwrap();
        for &(name, _) in BUILT_IN_SKILLS {
            assert!(store.list().unwrap().iter().any(|skill| skill.name == name && skill.builtin));
            assert!(matches!(store.remove(name), Err((StatusCode::FORBIDDEN, _))));
            assert!(directory.join(name).join(MANIFEST).is_file());
            store.set_enabled(name, false).unwrap();
            assert!(!store.enabled(name).unwrap());
            assert!(store.paths(Some(&[name.into()])).unwrap_err().contains("disabled"));
        }
        let store = Store::new(directory.clone());
        store.install_defaults().unwrap();
        assert!(store.paths(None).unwrap().is_empty());
        for &(name, _) in BUILT_IN_SKILLS {
            assert!(!store.enabled(name).unwrap());
            store.set_enabled(name, true).unwrap();
            assert_eq!(store.paths(Some(&[name.into()])).unwrap().len(), 1);
        }
        assert!(store.remove("../outside").is_err());
    }

    #[test]
    fn imported_skills_can_be_removed_and_reinstalled() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("installed");
        let store = Store::new(directory.clone());
        store.install_defaults().unwrap();
        let upload = || vec![UploadedFile {
            path: MANIFEST.into(),
            data: STANDARD.encode("---\nname: custom\ndescription: Test\n---\n"),
        }];
        let skill = store.upload(upload()).unwrap();
        assert!(!skill.builtin);
        store.set_enabled("custom", false).unwrap();
        store.remove("custom").unwrap();
        assert!(!directory.join("custom").exists());
        let store = Store::new(directory);
        store.install_defaults().unwrap();
        assert!(!store.list().unwrap().iter().any(|skill| skill.name == "custom"));
        assert!(store.upload(upload()).unwrap().enabled);
    }

    #[test]
    fn previously_removed_builtins_are_restored_disabled() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("installed");
        let store = Store::new(directory.clone());
        store.install_defaults().unwrap();
        fs::remove_dir_all(directory.join("ast-index")).unwrap();
        store.save_state(&SkillState {
            removed: BTreeSet::from(["ast-index".into()]),
            ..SkillState::default()
        }).unwrap();
        store.install_defaults().unwrap();
        assert!(directory.join("ast-index").join(MANIFEST).is_file());
        assert!(!store.enabled("ast-index").unwrap());
        assert!(store.state().unwrap().removed.is_empty());
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

#[cfg(test)]
mod selected_skill_tests {
    use super::*;

    #[test]
    fn catalog_and_loader_use_only_selected_names() {
        let temp = tempfile::tempdir().unwrap();
        let package = temp.path().join("canvas-design");
        fs::create_dir(&package).unwrap();
        let path = package.join("SKILL.md");
        fs::write(&path, "---\nname: canvas-design\ndescription: Compose original visual designs\n---\nUse an original composition.").unwrap();
        let paths = [path.clone()];
        let catalog = prompt_catalog(&paths).unwrap();
        assert!(catalog.contains("canvas-design: Compose original visual designs"));
        assert_eq!(selected_manifest(&paths, "canvas-design").unwrap(), path.as_path());
        assert!(selected_manifest(&paths, "../canvas-design").is_err());
        assert!(selected_manifest(&[], "canvas-design").unwrap_err().contains("SKILL_NOT_AVAILABLE"));
    }
}
