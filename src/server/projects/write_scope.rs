use std::{
    collections::HashMap,
    fs, io,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};

use serde::Deserialize;

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

pub(super) fn resolve(root: &Path, path: &Path, directory: bool) -> io::Result<PathBuf> {
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
            Ok(metadata) if (last && !directory && metadata.is_file()) || ((!last || directory) && metadata.is_dir()) => {
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
    Mkdir { path: PathBuf },
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
            Self::Write { path, .. } | Self::Edit { path, .. } | Self::Mkdir { path } => path,
        }
    }
}

const FILE_LIMIT: usize = 4 * 1024 * 1024;
const READ_LIMIT: usize = 48 * 1024;
const VERSION_BUDGET: usize = 32 * 1024 * 1024;

#[derive(Default)]
pub(in crate::server) struct FileVersions {
    // Compare the actual bytes, not mtimes or a potentially colliding hash.
    seen: HashMap<PathBuf, Version>,
    conflicts: std::collections::BTreeMap<PathBuf, String>,
    bytes: usize,
}

struct Version {
    bytes: Vec<u8>,
    full: bool,
}

#[derive(Deserialize)]
struct ReadRequest {
    path: PathBuf,
    offset: Option<usize>,
    limit: Option<usize>,
}

impl FileVersions {
    pub(in crate::server) fn unresolved_conflicts(&self) -> Option<String> {
        if self.conflicts.is_empty() { return None; }
        let mut text = format!("Unresolved file conflicts ({}):", self.conflicts.len());
        for (path, error) in self.conflicts.iter().take(8) {
            text.push_str(&format!("\n- {}: {}", path.display(), error));
        }
        if self.conflicts.len() > 8 { text.push_str("\nAdditional conflicts omitted."); }
        Some(text)
    }

    fn conflict(&mut self, target: &Path, error: &io::Error) {
        let message = error.to_string();
        if ["FILE_LOCKED:", "FILE_CHANGED:", "READ_REQUIRED:"].iter().any(|prefix| message.starts_with(*prefix)) {
            self.conflicts.insert(crate::run::path_key(target), message);
        }
    }

    fn remember(&mut self, path: &Path, bytes: Vec<u8>, full: bool) {
        let key = crate::run::path_key(path);
        if let Some(old) = self.seen.remove(&key) {
            self.bytes -= old.bytes.len();
        }
        if self.bytes + bytes.len() > VERSION_BUDGET || self.seen.len() >= 4096 {
            // Forgotten versions fail closed: another read is needed before writing.
            self.seen.clear();
            self.bytes = 0;
        }
        self.bytes += bytes.len();
        self.seen.insert(key, Version { bytes, full });
    }

    pub(in crate::server) fn read(&mut self, root: &Path, request: &str) -> io::Result<serde_json::Value> {
        use base64::{Engine, engine::general_purpose::STANDARD};
        let input: ReadRequest = serde_json::from_str(request)?;
        let path = if input.path.is_absolute() { input.path } else { root.join(input.path) };
        let target = path.canonicalize()?;
        let bytes = read_bytes(&target)?;
        let key = crate::run::path_key(&target);
        let old_full = self.seen.get(&key).is_some_and(|old| old.full && old.bytes == bytes);
        // Keep ordinary image reads available; file edits themselves remain text-only.
        let mime = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") { Some("image/png") }
            else if bytes.starts_with(&[0xff, 0xd8, 0xff]) { Some("image/jpeg") }
            else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") { Some("image/gif") }
            else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") { Some("image/webp") }
            else { None };
        if let Some(mime) = mime {
            return Ok(serde_json::json!({"image": STANDARD.encode(&bytes), "mime": mime}));
        }
        let source = std::str::from_utf8(&bytes).map_err(|_| invalid("File is not UTF-8 text or a supported image"))?;
        let start = input.offset.unwrap_or(1).max(1) - 1;
        let max_lines = input.limit.unwrap_or(2000).clamp(1, 2000);
        let mut output = String::new();
        let mut lines = source.split_inclusive('\n');
        for _ in 0..start {
            if lines.next().is_none() { return Err(invalid("Read offset is beyond end of file")); }
        }
        let mut count = 0;
        let mut partial_line = false;
        for line in lines.by_ref().take(max_lines) {
            if output.len() + line.len() > READ_LIMIT {
                if count == 0 {
                    output.push_str(&line[..line.floor_char_boundary(READ_LIMIT)]);
                    partial_line = true;
                }
                // The unreturned line must be included on the next read.
                break;
            }
            output.push_str(line);
            count += 1;
        }
        let full = start == 0 && output.len() == source.len();
        // Compare by byte count to account for a consumed-but-not-returned line.
        let prefix_bytes: usize = source.split_inclusive('\n').take(start).map(str::len).sum();
        let more = prefix_bytes + output.len() < source.len();
        if partial_line {
            output.push_str("\n[Read clipped within a long line. Use targeted search/edit; full replacement requires a complete read.]");
        } else if more {
            output.push_str(&format!("\n[More text available: read with offset={}.]", start + count + 1));
        }
        self.remember(&target, bytes, full || old_full);
        Ok(serde_json::json!({"text": output}))
    }

    fn validate(&self, target: &Path, current: Option<&[u8]>, replacing: bool) -> io::Result<()> {
        let seen = self.seen.get(&crate::run::path_key(target));
        match (seen, current) {
            (Some(old), Some(now)) if old.bytes.as_slice() != now =>
                Err(invalid("FILE_CHANGED: file changed since your last read. Read it again, reconcile all changes, then retry.")),
            (Some(_), None) => Err(invalid("FILE_CHANGED: file disappeared since your last read. Inspect the current state and report if needed.")),
            (None, Some(_)) => Err(invalid("READ_REQUIRED: read the existing file before changing it; do not overwrite unseen content.")),
            (Some(old), Some(_)) if replacing && !old.full =>
                Err(invalid("READ_REQUIRED: full replacement needs a complete read of the existing file. Use targeted edit after reading the relevant lines instead.")),
            (None, None) if !replacing => Err(invalid("File does not exist. Use write to create it.")),
            _ => Ok(()),
        }
    }
}

fn read_bytes(path: &Path) -> io::Result<Vec<u8>> {
    if !fs::metadata(path)?.is_file() {
        return Err(invalid("Only regular files can be read"));
    }
    let mut bytes = Vec::new();
    fs::File::open(path)?.take((FILE_LIMIT + 1) as u64).read_to_end(&mut bytes)?;
    if bytes.len() > FILE_LIMIT {
        return Err(invalid("File exceeds the 4 MiB guarded file limit. Report the limitation instead of bypassing it with shell writes."));
    }
    Ok(bytes)
}

fn target_path(roots: &[PathBuf], root: &Path, path: &Path, directory: bool) -> io::Result<PathBuf> {
    if !path.is_absolute() {
        return resolve(root, path, directory);
    }
    for folder in std::iter::once(root).chain(roots.iter().map(PathBuf::as_path)) {
        let canonical = folder.canonicalize()?;
        if let Ok(relative) = path.strip_prefix(folder).or_else(|_| path.strip_prefix(&canonical)) {
            return resolve(folder, relative, directory);
        }
    }
    Err(invalid("Absolute path is outside the registered project folders."))
}

pub(in crate::server) fn mutate(
    root: &Path,
    roots: &[PathBuf],
    versions: &mut FileVersions,
    request: &str,
    claim: impl FnOnce(&Path) -> io::Result<bool>,
    release: impl FnOnce(&Path),
) -> io::Result<()> {
    let change: Mutation = serde_json::from_str(request)?;
    let directory = matches!(&change, Mutation::Mkdir { .. });
    let target = target_path(roots, root, change.path(), directory)?;
    // An existing directory is a no-op, not an exclusive reservation of its subtree.
    if directory && target.is_dir() {
        versions.conflicts.remove(&crate::run::path_key(&target));
        return Ok(());
    }
    // All cooperating Lyra writers acquire this lock before the version check.
    let newly_claimed = match claim(&target) {
        Ok(claimed) => claimed,
        Err(error) => { versions.conflict(&target, &error); return Err(error); }
    };
    if directory {
        // Coordinate creation with file mutations, but release this transient
        // directory claim immediately; unrelated children remain independent.
        let result = fs::create_dir_all(&target);
        if newly_claimed { release(&target); }
        if let Err(error) = &result { versions.conflict(&target, error); }
        else { versions.conflicts.remove(&crate::run::path_key(&target)); }
        return result;
    }
    let result = (|| {
        let current = match read_bytes(&target) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        let replacing = matches!(&change, Mutation::Write { .. });
        versions.validate(&target, current.as_deref(), replacing)?;
        let full = replacing || versions.seen.get(&crate::run::path_key(&target)).is_some_and(|old| old.full);
        let content = match change {
            Mutation::Mkdir { .. } => unreachable!("directory handled above"),
            Mutation::Write { content, .. } => content,
            Mutation::Edit { edits, .. } => edited_content(
                std::str::from_utf8(current.as_deref().ok_or_else(|| invalid("File does not exist"))?)
                    .map_err(|_| invalid("Only UTF-8 text can be edited"))?, edits)?,
        };
        if content.len() > FILE_LIMIT { return Err(invalid("File exceeds the 4 MiB guarded file limit")); }
        replace_file(&target, &content, current.as_deref())?;
        versions.remember(&target, content.into_bytes(), full);
        Ok(())
    })();
    // A failed first mutation did not earn ownership. Existing locks remain.
    if let Err(error) = &result {
        if newly_claimed { release(&target); }
        versions.conflict(&target, error);
    } else {
        versions.conflicts.remove(&crate::run::path_key(&target));
    }
    result
}

fn replace_file(target: &Path, content: &str, expected: Option<&[u8]>) -> io::Result<()> {
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
    if let Some(expected) = expected {
        // Also catch external edits during preparation, although arbitrary external
        // writers are not coordinated by Lyra and can still race the final rename.
        match read_bytes(target) {
            Ok(current) if current.as_slice() == expected => {}
            Ok(_) => return Err(invalid("FILE_CHANGED: file changed during the edit. Read it again.")),
            Err(error) if error.kind() == io::ErrorKind::NotFound =>
                return Err(invalid("FILE_CHANGED: file disappeared during the edit. Read it again.")),
            Err(error) => return Err(error),
        }
        file.persist(target).map_err(|error| error.error)?;
    } else {
        // A file created by another process after the existence check must
        // never be silently overwritten by a create operation.
        file.persist_noclobber(target).map_err(|error| {
            if error.error.kind() == io::ErrorKind::AlreadyExists {
                invalid("FILE_CHANGED: a file appeared during creation. Read it before modifying.")
            } else { error.error }
        })?;
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

    fn change(root: &Path, versions: &mut FileVersions, request: &str) -> io::Result<()> {
        mutate(root, &[], versions, request, |_| Ok(true), |_| {})
    }

    #[test]
    fn new_files_and_targeted_edits_work_without_a_predeclared_list() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let mut versions = FileVersions::default();
        change(root, &mut versions, r#"{"tool":"write","path":"src/new.txt","content":"first\r\nsecond\r\nthird\r\n"}"#).unwrap();
        change(root, &mut versions, r#"{"tool":"edit","path":"src/new.txt","edits":[{"oldText":"first\nsecond","newText":"one\ntwo"},{"oldText":"third","newText":"three"}]}"#).unwrap();
        assert_eq!(fs::read_to_string(root.join("src/new.txt")).unwrap(), "one\r\ntwo\r\nthree\r\n");
        assert!(change(root, &mut versions, r#"{"tool":"write","path":"../outside","content":"no"}"#).is_err());
    }

    #[test]
    fn mkdir_creates_nested_empty_directories_without_retaining_a_claim() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let mut versions = FileVersions::default();
        let released = std::cell::Cell::new(false);
        mutate(root, &[], &mut versions, r#"{"tool":"mkdir","path":"src/assets/empty"}"#,
            |_| Ok(true), |_| released.set(true)).unwrap();
        assert!(root.join("src/assets/empty").is_dir());
        assert!(released.get());
        mutate(root, &[], &mut versions, r#"{"tool":"mkdir","path":"src/assets/empty"}"#,
            |_| panic!("existing directory needs no claim"), |_| {}).unwrap();
        assert!(change(root, &mut versions, r#"{"tool":"mkdir","path":"../escape"}"#).is_err());
        fs::write(root.join("file"), "keep").unwrap();
        assert!(change(root, &mut versions, r#"{"tool":"mkdir","path":"file"}"#).is_err());
        assert_eq!(fs::read_to_string(root.join("file")).unwrap(), "keep");
    }

    #[cfg(unix)]
    #[test]
    fn mkdir_rejects_symlink_ancestors() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        let outside = dir.path().join("outside");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
        assert!(change(&root, &mut FileVersions::default(), r#"{"tool":"mkdir","path":"link/escape"}"#).is_err());
        assert!(!outside.join("escape").exists());
    }

    #[test]
    fn atomic_creation_never_overwrites_an_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.txt");
        fs::write(&path, "created elsewhere").unwrap();
        assert!(replace_file(&path, "new", None).unwrap_err().to_string().contains("FILE_CHANGED"));
        assert_eq!(fs::read_to_string(&path).unwrap(), "created elsewhere");
        assert!(replace_file(&path, "new", Some(b"old")).unwrap_err().to_string().contains("FILE_CHANGED"));
        assert_eq!(fs::read_to_string(&path).unwrap(), "created elsewhere");
    }

    #[test]
    fn stale_reader_cannot_overwrite_a_finished_writer() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("shared.txt"), "old").unwrap();
        let mut a = FileVersions::default();
        let mut b = FileVersions::default();
        let read = r#"{"path":"shared.txt"}"#;
        a.read(root, read).unwrap();
        b.read(root, read).unwrap();
        change(root, &mut b, r#"{"tool":"write","path":"shared.txt","content":"new from B"}"#).unwrap();
        let write = r#"{"tool":"write","path":"shared.txt","content":"A"}"#;
        assert!(change(root, &mut a, write).unwrap_err().to_string().contains("FILE_CHANGED"));
        assert!(change(root, &mut a, r#"{"tool":"edit","path":"shared.txt","edits":[{"oldText":"old","newText":"A"}]}"#).unwrap_err().to_string().contains("FILE_CHANGED"));
        assert_eq!(fs::read_to_string(root.join("shared.txt")).unwrap(), "new from B");
        assert!(a.unresolved_conflicts().unwrap().contains("FILE_CHANGED"));
        a.read(root, read).unwrap();
        change(root, &mut a, r#"{"tool":"edit","path":"shared.txt","edits":[{"oldText":"new from B","newText":"new from B and A"}]}"#).unwrap();
        assert_eq!(fs::read_to_string(root.join("shared.txt")).unwrap(), "new from B and A");
        assert!(a.unresolved_conflicts().is_none());
    }

    #[test]
    fn parallel_runs_keep_new_files_locked_and_reject_stale_reads_after_release() {
        use crate::run::{DelegationScope, Snapshot};
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let state = std::cell::RefCell::new(Snapshot::new(None));
        let scope = |id| DelegationScope::Queued {
            session_id: id, parallel: true, context_tokens: None, max_tasks: 2, max_models: 1,
        };
        let a = state.borrow_mut().begin_delegation("local", scope(1)).unwrap();
        let b = state.borrow_mut().begin_delegation("local", scope(2)).unwrap();
        let mut va = FileVersions::default();
        let mut vb = FileVersions::default();
        let apply = |id, versions: &mut FileVersions, request: &str| {
            mutate(root, &[], versions, request,
                |path| state.borrow_mut().claim_file(id, path).map_err(io::Error::other),
                |path| { state.borrow_mut().release_file(id, path); })
        };
        let read = r#"{"path":"new.txt"}"#;
        apply(a, &mut va, r#"{"tool":"write","path":"new.txt","content":"first"}"#).unwrap();
        vb.read(root, read).unwrap();
        let b_write = r#"{"tool":"write","path":"new.txt","content":"second"}"#;
        assert!(apply(b, &mut vb, b_write).unwrap_err().to_string().contains("FILE_LOCKED"));
        apply(a, &mut va, r#"{"tool":"write","path":"new.txt","content":"first updated"}"#).unwrap();
        state.borrow_mut().finish_delegation(a, true);
        assert!(apply(b, &mut vb, b_write).unwrap_err().to_string().contains("FILE_CHANGED"));
        assert_eq!(fs::read_to_string(root.join("new.txt")).unwrap(), "first updated");
        vb.read(root, read).unwrap();
        apply(b, &mut vb, r#"{"tool":"edit","path":"new.txt","edits":[{"oldText":"first updated","newText":"first updated + second"}]}"#).unwrap();
        assert!(vb.unresolved_conflicts().is_none());
        assert_eq!(fs::read_to_string(root.join("new.txt")).unwrap(), "first updated + second");
    }

    #[test]
    fn existing_file_requires_read_and_partial_read_cannot_replace() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("a"), "first\nsecond\n").unwrap();
        let mut v = FileVersions::default();
        let write = r#"{"tool":"write","path":"a","content":"replacement"}"#;
        assert!(change(root, &mut v, write).unwrap_err().to_string().contains("READ_REQUIRED"));
        v.read(root, r#"{"path":"a","limit":1}"#).unwrap();
        assert!(change(root, &mut v, write).unwrap_err().to_string().contains("READ_REQUIRED"));
        change(root, &mut v, r#"{"tool":"edit","path":"a","edits":[{"oldText":"first","newText":"changed"}]}"#).unwrap();
        assert_eq!(fs::read_to_string(root.join("a")).unwrap(), "changed\nsecond\n");
        assert!(change(root, &mut v, write).is_err());
    }

    #[test]
    fn conflict_never_changes_file_and_failed_first_claim_is_released() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("a"), "old").unwrap();
        let mut v = FileVersions::default();
        v.read(root, r#"{"path":"a"}"#).unwrap();
        let request = r#"{"tool":"write","path":"a","content":"new"}"#;
        let denied = mutate(root, &[], &mut v, request, |_| Err(invalid("FILE_LOCKED")), |_| panic!("no claim"));
        assert!(denied.unwrap_err().to_string().contains("FILE_LOCKED"));
        fs::write(root.join("a"), "other").unwrap();
        let released = std::cell::Cell::new(false);
        assert!(mutate(root, &[], &mut v, request, |_| Ok(true), |_| released.set(true)).is_err());
        assert!(released.get());
        assert_eq!(fs::read_to_string(root.join("a")).unwrap(), "other");
    }

    #[cfg(unix)]
    #[test]
    fn symlink_mutation_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("project");
        fs::create_dir(&root).unwrap();
        let outside = dir.path().join("outside");
        fs::write(&outside, "old").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("alias")).unwrap();
        assert!(change(&root, &mut FileVersions::default(), r#"{"tool":"write","path":"alias","content":"new"}"#).is_err());
        assert_eq!(fs::read_to_string(outside).unwrap(), "old");
    }

    #[test]
    fn replacing_hard_link_does_not_mutate_other_name() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("original"), "old").unwrap();
        fs::hard_link(root.join("original"), root.join("alias")).unwrap();
        let mut v = FileVersions::default();
        v.read(root, r#"{"path":"alias"}"#).unwrap();
        change(root, &mut v, r#"{"tool":"write","path":"alias","content":"new"}"#).unwrap();
        assert_eq!(fs::read_to_string(root.join("original")).unwrap(), "old");
    }
}
