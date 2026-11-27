use std::{
    collections::HashMap,
    fs, io,
    hash::{DefaultHasher, Hash, Hasher},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};

use serde::Deserialize;

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

// Fingerprint actual file bytes, not timestamps. Limit version tracking to
// ordinary files of a reasonable size; untracked existing files cannot be overwritten.
const MAX_TRACKED_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Clone, Copy, PartialEq, Eq)]
struct Version {
    hash: u64,
    len: u64,
}

fn fingerprint(path: &Path) -> io::Result<Option<Version>> {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if !file.metadata()?.is_file() {
        return Err(invalid("Only regular files may be versioned."));
    }
    let mut bytes = Vec::new();
    file.take(MAX_TRACKED_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_TRACKED_BYTES {
        return Err(invalid("File exceeds the 32 MiB version tracking limit."));
    }
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    Ok(Some(Version { hash: hasher.finish(), len: bytes.len() as u64 }))
}

fn target_in_roots(workspace: &Path, roots: &[PathBuf], path: &Path) -> io::Result<PathBuf> {
    if path.is_absolute() {
        for root in roots {
            let canonical = root.canonicalize()?;
            if let Ok(relative) = path.strip_prefix(root).or_else(|_| path.strip_prefix(&canonical)) {
                return resolve(root, relative);
            }
        }
        return Err(invalid("Absolute path is outside the registered working folders."));
    }
    resolve(workspace, path)
}

#[derive(Default)]
pub(in crate::server) struct Versions {
    observed: HashMap<PathBuf, Version>,
    pending: HashMap<String, (PathBuf, Version)>,
}

impl Versions {
    // Called before Pi's normal read executes. Reads of skill files, CONTEXT.md,
    // etc. remain available but never authorize writes outside working folders.
    pub(in crate::server) fn begin_read(&mut self, workspace: &Path, roots: &[PathBuf], id: String, path: &Path) {
        self.pending.remove(&id);
        let Ok(target) = target_in_roots(workspace, roots, path) else { return; };
        let key = crate::run::path_key(&target);
        self.observed.remove(&key);
        if let Ok(Some(version)) = fingerprint(&target) {
            self.pending.insert(id, (target, version));
        }
    }

    pub(in crate::server) fn finish_read(&mut self, id: &str, succeeded: bool) {
        let Some((target, before)) = self.pending.remove(id) else { return; };
        let key = crate::run::path_key(&target);
        if succeeded && fingerprint(&target).is_ok_and(|after| after == Some(before)) {
            self.observed.insert(key, before);
        } else {
            self.observed.remove(&key);
        }
    }

    pub(in crate::server) fn mutate(
        &mut self, workspace: &Path, roots: &[PathBuf], request: &str,
        claim: impl FnOnce(&Path) -> std::result::Result<(), String>,
    ) -> io::Result<()> {
        let change: Mutation = serde_json::from_str(request)?;
        let target = target_in_roots(workspace, roots, change.path())?;
        let key = crate::run::path_key(&target);
        let current = fingerprint(&target)?;
        // A previously observed file that vanished is also a stale version.
        match (self.observed.get(&key), current) {
            (Some(previous), Some(current)) if *previous == current => {}
            (Some(_), _) => return Err(invalid(format!(
                "FILE_CHANGED: {} changed since your last read. Read the current file and adapt your edits; never overwrite old contents.", target.display()
            ))),
            (None, Some(_)) => return Err(invalid(format!(
                "READ_REQUIRED: {} already exists. Read it before writing or editing.", target.display()
            ))),
            (None, None) => {}
        }
        // Validate edit before claiming, so a malformed edit does not lock a file.
        let content = match change {
            Mutation::Write { content, .. } => content,
            Mutation::Edit { edits, .. } => edited_content(&fs::read_to_string(&target)?, edits)?,
        };
        if content.len() as u64 > MAX_TRACKED_BYTES {
            return Err(invalid("File exceeds the 32 MiB version tracking limit."));
        }
        claim(&target).map_err(invalid)?;
        // No await between version check, claim, and atomic replacement.
        // Recheck after constructing content, before replacing the entry.
        if fingerprint(&target)? != current {
            return Err(invalid("FILE_CHANGED: file changed during the edit. Read it again."));
        }
        replace_file(&target, &content, current.is_none())?;
        // Keep the version of the bytes we wrote, not a later external edit.
        let mut hasher = DefaultHasher::new();
        content.as_bytes().hash(&mut hasher);
        self.observed.insert(key, Version { hash: hasher.finish(), len: content.len() as u64 });
        Ok(())
    }
}

fn replace_file(target: &Path, content: &str, new: bool) -> io::Result<()> {
    let parent = target.parent().expect("file parent");
    fs::create_dir_all(parent)?;
    let permissions = match fs::metadata(target) {
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
    if new {
        file.persist_noclobber(target).map_err(|error| error.error)?;
    } else {
        file.persist(target).map_err(|error| error.error)?;
    }
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

    fn read(versions: &mut Versions, root: &Path, path: &str) {
        versions.begin_read(root, &[root.to_path_buf()], "read".into(), Path::new(path));
        versions.finish_read("read", true);
    }
    fn mutate(versions: &mut Versions, root: &Path, request: &str) -> io::Result<()> {
        versions.mutate(root, &[root.to_path_buf()], request, |_| Ok(()))
    }

    #[test]
    fn stale_writer_cannot_overwrite_other_agents_change() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("file.txt"), "old").unwrap();
        let (mut a, mut b) = (Versions::default(), Versions::default());
        read(&mut a, root, "file.txt");
        read(&mut b, root, "file.txt");
        mutate(&mut b, root, r#"{"tool":"write","path":"file.txt","content":"new"}"#).unwrap();
        let error = mutate(&mut a, root, r#"{"tool":"write","path":"file.txt","content":"stale"}"#).unwrap_err();
        assert!(error.to_string().contains("FILE_CHANGED"));
        assert_eq!(fs::read_to_string(root.join("file.txt")).unwrap(), "new");
        read(&mut a, root, "file.txt");
        mutate(&mut a, root, r#"{"tool":"edit","path":"file.txt","edits":[{"oldText":"new","newText":"merged"}]}"#).unwrap();
        assert_eq!(fs::read_to_string(root.join("file.txt")).unwrap(), "merged");
    }

    #[test]
    fn new_files_need_no_reservation_but_existing_files_require_read() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let (mut a, mut b) = (Versions::default(), Versions::default());
        mutate(&mut a, root, r#"{"tool":"write","path":"src/new.txt","content":"first\r\nsecond\r\n"}"#).unwrap();
        let error = mutate(&mut b, root, r#"{"tool":"write","path":"src/new.txt","content":"bad"}"#).unwrap_err();
        assert!(error.to_string().contains("READ_REQUIRED"));
        mutate(&mut a, root, r#"{"tool":"edit","path":"src/new.txt","edits":[{"oldText":"first\nsecond","newText":"one\ntwo"}]}"#).unwrap();
        assert_eq!(fs::read_to_string(root.join("src/new.txt")).unwrap(), "one\r\ntwo\r\n");
        assert!(mutate(&mut a, root, r#"{"tool":"write","path":"../escape.txt","content":"bad"}"#).is_err());
        assert!(mutate(&mut a, root, r#"{"tool":"edit","path":"src/new.txt","edits":[{"oldText":"one","newText":"changed"},{"oldText":"absent","newText":"bad"}]}"#).is_err());
        assert_eq!(fs::read_to_string(root.join("src/new.txt")).unwrap(), "one\r\ntwo\r\n");
    }

    #[test]
    fn unsuccessful_or_changed_reads_do_not_authorize_a_write() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let mut versions = Versions::default();
        fs::write(root.join("file"), "old").unwrap();
        versions.begin_read(root, &[root.to_path_buf()], "read".into(), Path::new("file"));
        fs::write(root.join("file"), "new").unwrap();
        versions.finish_read("read", true);
        assert!(mutate(&mut versions, root, r#"{"tool":"write","path":"file","content":"bad"}"#).is_err());
        versions.begin_read(root, &[root.to_path_buf()], "read".into(), Path::new("file"));
        versions.finish_read("read", false);
        assert!(mutate(&mut versions, root, r#"{"tool":"write","path":"file","content":"bad"}"#).is_err());
    }

    #[test]
    fn conflict_does_not_modify_file_and_absolute_other_roots_work() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("one");
        let other = dir.path().join("two");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&other).unwrap();
        let roots = [root.clone(), other.clone()];
        let mut versions = Versions::default();
        let path = other.join("new.txt");
        let request = serde_json::json!({"tool":"write","path":path,"content":"new"}).to_string();
        let error = versions.mutate(&root, &roots, &request, |_| Err("FILE_LOCKED".into())).unwrap_err();
        assert!(error.to_string().contains("FILE_LOCKED"));
        assert!(!path.exists());
        versions.mutate(&root, &roots, &request, |_| Ok(())).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "new");
    }

    #[test]
    fn replacement_of_hard_link_preserves_other_name() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("original"), "old").unwrap();
        fs::hard_link(dir.path().join("original"), dir.path().join("alias")).unwrap();
        let mut versions = Versions::default();
        read(&mut versions, dir.path(), "alias");
        mutate(&mut versions, dir.path(), r#"{"tool":"write","path":"alias","content":"new"}"#).unwrap();
        assert_eq!(fs::read_to_string(dir.path().join("original")).unwrap(), "old");
        assert_eq!(fs::read_to_string(dir.path().join("alias")).unwrap(), "new");
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("project");
        fs::create_dir(&root).unwrap();
        let target = dir.path().join("outside");
        fs::write(&target, "old").unwrap();
        std::os::unix::fs::symlink(&target, root.join("alias")).unwrap();
        let mut versions = Versions::default();
        assert!(mutate(&mut versions, &root, r#"{"tool":"write","path":"alias","content":"new"}"#).is_err());
        assert_eq!(fs::read_to_string(target).unwrap(), "old");
    }
}
