//! Safe read/write for Syncthing-synced, symlinked vaults. See plan "Save" row.
//!
//! Save model is VS Code's: the editor holds an etag from the last read, and a save that
//! finds a different etag on disk is refused so the UI can offer a diff instead of clobbering
//! a change that Syncthing (or another device) pulled in behind our back.

use std::io::{self, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Cheap identity of a file's content, taken from `stat(2)`.
///
/// `ino` is part of it on purpose: every save replaces the file via rename, so a save by us or
/// by Syncthing changes the inode even when mtime granularity would hide the write.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Etag {
    pub mtime_ns: i64,
    pub size: u64,
    pub ino: u64,
}

impl Etag {
    /// Stat `path`, following symlinks (the vault may link notes in from elsewhere).
    pub fn of(path: &Path) -> io::Result<Etag> {
        Ok(Etag::from_meta(&std::fs::metadata(path)?))
    }

    fn from_meta(m: &std::fs::Metadata) -> Etag {
        Etag {
            mtime_ns: m.mtime() * 1_000_000_000 + m.mtime_nsec(),
            size: m.size(),
            ino: m.ino(),
        }
    }
}

/// Read a note and the etag to hand back to [`write_note`].
///
/// The stat happens *after* the read: if a writer raced us we may pair new bytes with an older
/// etag, and the next save then reports [`SaveError::ChangedOnDisk`] instead of overwriting.
pub fn read_note(path: &Path) -> io::Result<(String, Etag)> {
    let text = std::fs::read_to_string(path)?;
    Ok((text, Etag::of(path)?))
}

#[derive(Debug, thiserror::Error)]
pub enum SaveError {
    #[error("file changed on disk since it was read")]
    ChangedOnDisk { current: Etag },
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Write `text` to `path` atomically, refusing the save if the file changed since `expected`.
///
/// Passing `expected: None` forces the write (the "overwrite anyway" branch in the UI).
/// Returns the etag of the file just written, ready for the next save.
pub fn write_note(path: &Path, text: &str, expected: Option<Etag>) -> Result<Etag, SaveError> {
    // Resolve symlinks first: writing through a link must replace the link *target*, otherwise the
    // rename below would silently turn a symlinked note into a regular file in the vault.
    let canonical = canonical_target(path)?;
    let parent = canonical
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no parent"))?;

    let existing = std::fs::metadata(&canonical).ok();
    if let Some(exp) = expected {
        match &existing {
            Some(m) if Etag::from_meta(m) != exp => {
                return Err(SaveError::ChangedOnDisk { current: Etag::from_meta(m) });
            }
            None => {
                return Err(io::Error::new(io::ErrorKind::NotFound, "file vanished before save").into());
            }
            _ => {}
        }
    }

    // ponytail: new files inherit tempfile's 0600 instead of 0666 & !umask. Fine for a private
    // vault; if notes ever need to be group-readable, set the mode explicitly here.
    let mut tmp = tempfile::Builder::new()
        .prefix(".accent-")
        .tempfile_in(parent)
        .map_err(SaveError::Io)?;
    tmp.write_all(text.as_bytes())?;
    tmp.as_file().sync_all()?;

    if let Some(m) = &existing {
        std::fs::set_permissions(tmp.path(), m.permissions())?;
        // ponytail: chown only works as root or when we already own the file; EPERM is the
        // normal case on a single-user vault and is ignored rather than failing the save.
        let _ = std::os::unix::fs::chown(tmp.path(), Some(m.uid()), Some(m.gid()));
    }

    // ponytail: no directory fsync after the rename. The rename itself is atomic, so a crash can
    // only lose the whole save, never half of it. Add `File::open(parent)?.sync_all()` here if
    // crash-consistency (as opposed to torn-write safety) ever matters.
    let file = tmp.persist(&canonical).map_err(|e| SaveError::Io(e.error))?;
    Ok(Etag::from_meta(&file.metadata()?))
}

/// `canonicalize()`, but tolerating a file that does not exist yet by resolving its parent.
fn canonical_target(path: &Path) -> io::Result<PathBuf> {
    match path.canonicalize() {
        Ok(p) => Ok(p),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let name = path
                .file_name()
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no file name"))?;
            let parent = match path.parent() {
                Some(p) if !p.as_os_str().is_empty() => p,
                _ => Path::new("."),
            };
            Ok(parent.canonicalize()?.join(name))
        }
        Err(e) => Err(e),
    }
}

/// `.syncthing.Note.md.tmp` / `~syncthing~Note.md.tmp`: a partial download, never a note.
pub fn is_syncthing_temp(name: &str) -> bool {
    (name.starts_with(".syncthing.") && name.ends_with(".tmp")) || name.starts_with("~syncthing~")
}

/// `Note.sync-conflict-20260903-101500-ABCDEFG.md`: a real file, but not a note of its own.
pub fn is_sync_conflict(name: &str) -> bool {
    name.contains(".sync-conflict-")
}

/// `Note.sync-conflict-20260903-101500-ABCDEFG.md` -> `Note.md`.
pub fn conflict_original(name: &str) -> Option<String> {
    let (stem, rest) = name.split_once(".sync-conflict-")?;
    // Syncthing appends its marker before the extension, so whatever follows the last dot of the
    // marker is the original extension.
    Some(match rest.rfind('.') {
        Some(dot) => format!("{stem}{}", &rest[dot..]),
        None => stem.to_string(),
    })
}

/// Conflict copies sitting next to `path`. Accepts either the original or one of the copies.
pub fn conflict_siblings(path: &Path) -> io::Result<Vec<PathBuf>> {
    let dir = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return Ok(Vec::new());
    };
    let original = conflict_original(name).unwrap_or_else(|| name.to_string());

    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let Some(entry_name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if conflict_original(&entry_name).as_deref() == Some(original.as_str()) {
            out.push(entry.path());
        }
    }
    out.sort();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn roundtrip_returns_fresh_etag() {
        let dir = tempfile::tempdir().unwrap();
        let note = dir.path().join("Note.md");
        std::fs::write(&note, "one").unwrap();

        let (text, etag) = read_note(&note).unwrap();
        assert_eq!(text, "one");

        let new_etag = write_note(&note, "two", Some(etag)).unwrap();
        assert_ne!(new_etag, etag, "atomic replace must produce a new inode");

        let (text, read_back) = read_note(&note).unwrap();
        assert_eq!(text, "two");
        assert_eq!(read_back, new_etag);
    }

    #[test]
    fn stale_etag_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let note = dir.path().join("Note.md");
        std::fs::write(&note, "one").unwrap();

        let (_, stale) = read_note(&note).unwrap();
        write_note(&note, "someone else", None).unwrap();

        match write_note(&note, "mine", Some(stale)) {
            Err(SaveError::ChangedOnDisk { current }) => assert_ne!(current, stale),
            other => panic!("expected ChangedOnDisk, got {other:?}"),
        }
        assert_eq!(std::fs::read_to_string(&note).unwrap(), "someone else");
    }

    #[test]
    fn write_through_symlink_replaces_the_target() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("real.md");
        let link = dir.path().join("link.md");
        std::fs::write(&target, "one").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let (_, etag) = read_note(&link).unwrap();
        write_note(&link, "two", Some(etag)).unwrap();

        assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "two");
        assert_eq!(std::fs::read_to_string(&link).unwrap(), "two");
    }

    #[test]
    fn mode_is_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let note = dir.path().join("Note.md");
        std::fs::write(&note, "one").unwrap();
        std::fs::set_permissions(&note, std::fs::Permissions::from_mode(0o600)).unwrap();

        write_note(&note, "two", None).unwrap();

        let mode = std::fs::metadata(&note).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "saved file kept the original mode");
    }

    #[test]
    fn creates_new_file() {
        let dir = tempfile::tempdir().unwrap();
        let note = dir.path().join("New.md");

        let etag = write_note(&note, "hello", None).unwrap();
        assert_eq!(std::fs::read_to_string(&note).unwrap(), "hello");
        assert_eq!(etag, Etag::of(&note).unwrap());

        // No temp files left behind.
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(leftovers, vec!["New.md".to_string()]);
    }

    #[test]
    fn expected_etag_on_missing_file_is_an_io_error() {
        let dir = tempfile::tempdir().unwrap();
        let ghost = Etag { mtime_ns: 1, size: 1, ino: 1 };
        match write_note(&dir.path().join("Gone.md"), "x", Some(ghost)) {
            Err(SaveError::Io(e)) => assert_eq!(e.kind(), io::ErrorKind::NotFound),
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[test]
    fn name_helpers() {
        assert!(is_syncthing_temp(".syncthing.Note.md.tmp"));
        assert!(is_syncthing_temp("~syncthing~Note.md.tmp"));
        assert!(!is_syncthing_temp("Note.md"));
        assert!(!is_syncthing_temp(".syncthing.Note.md"));

        assert!(is_sync_conflict("Note.sync-conflict-20260903-101500-ABCDEFG.md"));
        assert!(!is_sync_conflict("Note.md"));

        assert_eq!(
            conflict_original("Note.sync-conflict-20260903-101500-ABCDEFG.md").as_deref(),
            Some("Note.md")
        );
        assert_eq!(
            conflict_original("a.b.sync-conflict-20260903-101500-ABCDEFG.md").as_deref(),
            Some("a.b.md")
        );
        assert_eq!(
            conflict_original("README.sync-conflict-20260903-101500-ABCDEFG").as_deref(),
            Some("README")
        );
        assert_eq!(conflict_original("Note.md"), None);
    }

    #[test]
    fn conflict_siblings_finds_copies() {
        let dir = tempfile::tempdir().unwrap();
        let note = dir.path().join("Note.md");
        let a = dir.path().join("Note.sync-conflict-20260903-101500-AAAAAAA.md");
        let b = dir.path().join("Note.sync-conflict-20260903-101600-BBBBBBB.md");
        for p in [&note, &a, &b] {
            std::fs::write(p, "x").unwrap();
        }
        std::fs::write(dir.path().join("Other.sync-conflict-20260903-101500-CCCCCCC.md"), "x").unwrap();

        assert_eq!(conflict_siblings(&note).unwrap(), vec![a.clone(), b.clone()]);
        // Asking with a conflict copy in hand finds the whole set, including itself.
        assert_eq!(conflict_siblings(&a).unwrap(), vec![a, b]);
    }
}
