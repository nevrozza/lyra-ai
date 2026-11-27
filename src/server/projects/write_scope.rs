use std::{
    fs, io,
    io::Write,
    path::{Component, Path, PathBuf},
};

use serde::Deserialize;

pub(super) const MAX_PATHS: usize = 32;

pub(in crate::server) fn resolve_paths(root: &Path, paths: &[PathBuf]) -> io::Result<Vec<PathBuf>> {
    if paths.is_empty() || paths.len() > MAX_PATHS {
        return Err(invalid("Укажите от 1 до 32 файлов в write_paths."));
    }
    paths.iter().map(|path| resolve(root, path)).collect()
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

fn resolve(root: &Path, path: &Path) -> io::Result<PathBuf> {
    let invalid_path = || invalid(format!("Недопустимый путь для записи: {}", path.display()));
    if path.as_os_str().is_empty()
        || path.components().any(|part| match part {
            Component::Normal(name) => {
                let name = name.to_string_lossy();
                name.contains([':', '\\', '\n', '\r', '\0'])
                    || (cfg!(windows)
                        && (name.ends_with(['.', ' '])
                            || name.split('.').next().is_some_and(windows_device_name)))
            }
            _ => true,
        })
    {
        return Err(invalid_path());
    }

    let mut target = root.canonicalize()?;
    let mut parts = path.components().peekable();
    while let Some(part) = parts.next() {
        target.push(part);
        let last = parts.peek().is_none();
        match fs::symlink_metadata(&target) {
            Ok(metadata) if (last && metadata.is_file()) || (!last && metadata.is_dir()) => {
                let resolved = target.canonicalize()?;
                // Windows also exposes junctions and reparse points as directories.
                if resolved.parent() != target.parent()
                    || !resolved
                        .file_name()
                        .zip(target.file_name())
                        .is_some_and(|(a, b)| {
                            crate::run::path_key(Path::new(a)) == crate::run::path_key(Path::new(b))
                        })
                {
                    return Err(invalid_path());
                }
                target = resolved;
            }
            Ok(_) => return Err(invalid_path()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                target.extend(parts);
                break;
            }
            Err(error) => return Err(error),
        }
    }
    Ok(target)
}

fn windows_device_name(name: &str) -> bool {
    let name = name.trim_end().to_ascii_uppercase();
    matches!(
        name.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || name
        .strip_prefix("COM")
        .or_else(|| name.strip_prefix("LPT"))
        .is_some_and(|number| {
            matches!(
                number,
                "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
}

#[derive(Deserialize)]
struct Replacement {
    #[serde(rename = "oldText")]
    old: String,
    #[serde(rename = "newText")]
    new: String,
}

#[derive(Deserialize)]
#[serde(tag = "tool", rename_all = "snake_case")]
enum Mutation {
    Write {
        path: PathBuf,
        content: String,
    },
    Edit {
        path: PathBuf,
        edits: Vec<Replacement>,
    },
}

impl Mutation {
    fn path(&self) -> &Path {
        match self {
            Self::Write { path, .. } | Self::Edit { path, .. } => path,
        }
    }
}

pub(in crate::server) fn mutate(root: &Path, allowed: &[PathBuf], request: &str) -> io::Result<()> {
    let change: Mutation = serde_json::from_str(request)?;
    let canonical_root = root.canonicalize()?;
    let path = change.path();
    // Pi may return an absolute path from its read tool. Accept it only
    // when it is lexically under the project root, then validate every component.
    let relative = if path.is_absolute() {
        path.strip_prefix(root)
            .or_else(|_| path.strip_prefix(&canonical_root))
            .map_err(|_| invalid("Absolute path is outside the project root."))?
    } else {
        path
    };
    let target = resolve(root, relative)?;
    if !allowed.iter().any(|path| crate::run::path_key(path) == crate::run::path_key(&target)) {
        return Err(invalid(format!(
            "Этот файл не зарезервирован для задачи: {}",
            change.path().display()
        )));
    }
    let content = match change {
        Mutation::Write { content, .. } => content,
        Mutation::Edit { edits, .. } => edited_content(&fs::read_to_string(&target)?, edits)?,
    };
    let parent = target.parent().expect("file parent");
    fs::create_dir_all(parent)?;
    let permissions = match fs::metadata(&target) {
        Ok(metadata) => Some(metadata.permissions()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    // Replace the directory entry instead of mutating an inode shared by hard links.
    let mut file = {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tempfile::Builder::new()
                .permissions(fs::Permissions::from_mode(0o666))
                .tempfile_in(parent)?
        }
        #[cfg(not(unix))]
        tempfile::NamedTempFile::new_in(parent)?
    };
    file.write_all(content.as_bytes())?;
    if let Some(permissions) = permissions {
        file.as_file().set_permissions(permissions)?;
    }
    file.persist(target).map_err(|error| error.error)?;
    Ok(())
}

fn edited_content(source: &str, edits: Vec<Replacement>) -> io::Result<String> {
    if edits.is_empty() {
        return Err(invalid("Укажите хотя бы одну замену."));
    }
    let mut matches = Vec::with_capacity(edits.len());
    for Replacement { old, new } in edits {
        if old.is_empty() {
            return Err(invalid("oldText не должен быть пустым."));
        }
        let uses_crlf = source.contains("\r\n") && !old.contains('\r');
        let old = if uses_crlf && !source.contains(&old) {
            old.replace('\n', "\r\n")
        } else {
            old
        };
        let new = if uses_crlf && !new.contains('\r') {
            new.replace('\n', "\r\n")
        } else {
            new
        };
        let start = source
            .find(&old)
            .ok_or_else(|| invalid("oldText не найден в файле."))?;
        if source[start + old.chars().next().expect("nonempty text").len_utf8()..].contains(&old) {
            return Err(invalid("oldText встречается в файле несколько раз."));
        }
        matches.push((start, start + old.len(), new));
    }
    matches.sort_by_key(|(start, _, _)| *start);
    if matches.windows(2).any(|pair| pair[0].1 > pair[1].0) {
        return Err(invalid("Замены пересекаются."));
    }
    let mut result = source.to_owned();
    for (start, end, new) in matches.into_iter().rev() {
        result.replace_range(start..end, &new);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_changes_reserved_files_and_applies_edits_together() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let allowed = resolve_paths(root, &["src/new.txt".into()]).unwrap();
        assert!(
            mutate(
                root,
                &allowed,
                r#"{"tool":"write","path":"other.txt","content":"no"}"#
            )
            .is_err()
        );
        assert!(
            mutate(
                root,
                &allowed,
                r#"{"tool":"write","path":"../new.txt","content":"no"}"#
            )
            .is_err()
        );
        assert!(!root.join("other.txt").exists());
        mutate(
            root,
            &allowed,
            r#"{"tool":"write","path":"src/new.txt","content":"first\r\nsecond\r\nthird\r\n"}"#,
        )
        .unwrap();
        let path = root.join("src/new.txt");
        mutate(root, &allowed, r#"{"tool":"edit","path":"src/new.txt","edits":[{"oldText":"first\nsecond","newText":"one\ntwo"},{"oldText":"third","newText":"three"}]}"#).unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "one\r\ntwo\r\nthree\r\n"
        );
        assert!(mutate(root, &allowed, r#"{"tool":"edit","path":"src/new.txt","edits":[{"oldText":"one","newText":"changed"},{"oldText":"absent","newText":"bad"}]}"#).is_err());
        assert_eq!(fs::read_to_string(path).unwrap(), "one\r\ntwo\r\nthree\r\n");
    }

    #[test]
    fn accepts_absolute_reserved_paths_but_not_other_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let allowed = resolve_paths(root, &["new.txt".into()]).unwrap();
        let request = serde_json::json!({"tool":"write","path":root.join("new.txt"),"content":"old"});
        mutate(root, &allowed, &request.to_string()).unwrap();
        let request = serde_json::json!({"tool":"edit","path":root.join("new.txt"),"edits":[{"oldText":"old","newText":"new"}]});
        mutate(root, &allowed, &request.to_string()).unwrap();
        assert_eq!(fs::read_to_string(root.join("new.txt")).unwrap(), "new");
        let request = serde_json::json!({"tool":"write","path":root.join("other.txt"),"content":"no"});
        assert!(mutate(root, &allowed, &request.to_string()).is_err());
    }

    #[test]
    fn replacing_a_reserved_hard_link_does_not_change_its_other_name() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("original"), "old").unwrap();
        fs::hard_link(dir.path().join("original"), dir.path().join("alias")).unwrap();
        let allowed = resolve_paths(dir.path(), &["alias".into()]).unwrap();
        mutate(
            dir.path(),
            &allowed,
            r#"{"tool":"write","path":"alias","content":"new"}"#,
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join("original")).unwrap(),
            "old"
        );
        assert_eq!(fs::read_to_string(dir.path().join("alias")).unwrap(), "new");
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinks_even_if_created_after_dispatch() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("project");
        fs::create_dir(&root).unwrap();
        let target = dir.path().join("outside");
        fs::write(&target, "old").unwrap();
        let allowed = resolve_paths(&root, &["alias".into()]).unwrap();
        std::os::unix::fs::symlink(&target, root.join("alias")).unwrap();
        assert!(resolve_paths(&root, &["alias".into()]).is_err());
        assert!(
            mutate(
                &root,
                &allowed,
                r#"{"tool":"write","path":"alias","content":"new"}"#
            )
            .is_err()
        );
        assert_eq!(fs::read_to_string(target).unwrap(), "old");
    }
}
