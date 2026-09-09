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

/// Biggest file we will pull into memory as text; anything larger stays closed.
pub const MAX_TEXT: u64 = 16 * 1024 * 1024;

/// What a file turned out to be when we tried to open it as text.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Read {
    Text(Text),
    Binary { size: u64 },
    TooLarge { size: u64 },
}

/// A file that decoded as text, together with what had to be changed to get there.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Text {
    pub text: String,
    pub etag: Etag,
    /// The file uses CRLF on disk; `text` holds it normalised to `\n`.
    pub crlf: bool,
    /// The bytes were not valid UTF-8 and were decoded with replacement characters, so what is
    /// in `text` no longer round-trips to the original file.
    pub lossy: bool,
}

/// Read any file as text, saying so when it is binary or too big to hold.
///
/// As in [`read_note`], the stat happens *after* the read, so a writer that raced us costs a
/// refused save rather than a silent overwrite.
pub fn read_text(path: &Path) -> io::Result<Read> {
    let size = std::fs::metadata(path)?.size();
    if size > MAX_TEXT {
        return Ok(Read::TooLarge { size });
    }
    let bytes = std::fs::read(path)?;
    // A NUL byte is the same "this is not text" test `grep` and `git` use.
    if bytes.contains(&0) {
        return Ok(Read::Binary {
            size: bytes.len() as u64,
        });
    }
    let (text, lossy) = match String::from_utf8(bytes) {
        Ok(text) => (text, false),
        Err(e) => (String::from_utf8_lossy(&e.into_bytes()).into_owned(), true),
    };
    let crlf = text.contains("\r\n");
    let text = if crlf {
        text.replace("\r\n", "\n")
    } else {
        text
    };
    Ok(Read::Text(Text {
        text,
        etag: Etag::of(path)?,
        crlf,
        lossy,
    }))
}

/// The inverse of [`read_text`]: put an editor buffer back into the shape the file had.
///
/// Stripping runs first so the CRLF pass cannot re-insert a `\r` that stripping would then keep.
pub fn for_disk(text: &str, crlf: bool, strip_trailing: bool) -> String {
    let stripped = if strip_trailing {
        // `split_inclusive` keeps each line's own newline, so a missing final one stays missing.
        text.split_inclusive('\n')
            .map(|line| match line.strip_suffix('\n') {
                Some(body) => format!("{}\n", body.trim_end_matches([' ', '\t'])),
                None => line.trim_end_matches([' ', '\t']).to_string(),
            })
            .collect()
    } else {
        text.to_string()
    };
    if crlf {
        stripped.replace('\n', "\r\n")
    } else {
        stripped
    }
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
    write_bytes(path, text.as_bytes(), expected)
}

/// The same atomic save for bytes: a PDF an annotation was written into, and nothing else so far.
///
/// Split out of [`write_note`] rather than duplicated, so a PDF gets the etag gate, the symlink
/// resolution and the preserved ownership a note has always had.
pub fn write_bytes(path: &Path, bytes: &[u8], expected: Option<Etag>) -> Result<Etag, SaveError> {
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
                return Err(SaveError::ChangedOnDisk {
                    current: Etag::from_meta(m),
                });
            }
            None => {
                return Err(
                    io::Error::new(io::ErrorKind::NotFound, "file vanished before save").into(),
                );
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
    tmp.write_all(bytes)?;
    tmp.as_file().sync_all()?;

    if let Some(m) = &existing {
        // Neither is allowed to fail the save. exFAT, SMB and Android's FUSE `/sdcard` have no
        // POSIX mode to set and answer `chmod` with EPERM or ENOTSUP; a note the user could open
        // there must still be one they can save. chown likewise only works as root or when we
        // already own the file, which on a single-user vault we do.
        if let Err(e) = std::fs::set_permissions(tmp.path(), m.permissions()) {
            tracing::debug!("{}: keeping the temp file's mode: {e}", canonical.display());
        }
        let _ = std::os::unix::fs::chown(tmp.path(), Some(m.uid()), Some(m.gid()));
    }

    // ponytail: no directory fsync after the rename. The rename itself is atomic, so a crash can
    // only lose the whole save, never half of it. Add `File::open(parent)?.sync_all()` here if
    // crash-consistency (as opposed to torn-write safety) ever matters.
    let file = tmp
        .persist(&canonical)
        .map_err(|e| SaveError::Io(e.error))?;
    Ok(Etag::from_meta(&file.metadata()?))
}

/// Create a new note, refusing to clobber an existing file. Missing parent directories are created.
///
/// `File::create_new` claims the name in one syscall, so two writers racing for the same new note
/// cannot both believe they won it. The content then goes through [`write_note`], which is what
/// gives a brand new note the same atomic-rename guarantees as every later save.
pub fn create_note(path: &Path, text: &str) -> io::Result<Etag> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::File::create_new(path)?; // AlreadyExists if the name is taken
    write_note(path, text, None).map_err(io::Error::other)
}

/// Move a file or directory, refusing to overwrite an existing target.
///
/// `std::fs::rename` moves a symlink itself rather than what it points at, which is what a vault
/// wants: renaming a linked-in note must not copy the target into the vault.
///
/// ponytail: the existence check races a concurrent create, and a move across mount points still
/// fails with `EXDEV`. `renameat2(RENAME_NOREPLACE)` closes the first, copy-then-delete the second.
pub fn rename(from: &Path, to: &Path) -> io::Result<()> {
    if to.symlink_metadata().is_ok() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{} already exists", to.display()),
        ));
    }
    std::fs::rename(from, to)
}

/// `canonicalize()`, but tolerating a file that does not exist yet by resolving its parent.
fn canonical_target(path: &Path) -> io::Result<PathBuf> {
    match path.canonicalize() {
        Ok(p) => Ok(p),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let name = path.file_name().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "path has no file name")
            })?;
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
    fn read_text_classifies_binary_and_normalises_crlf() {
        let dir = tempfile::tempdir().unwrap();

        let bin = dir.path().join("bin");
        std::fs::write(&bin, b"a\0b").unwrap();
        match read_text(&bin).unwrap() {
            Read::Binary { size } => assert_eq!(size, 3),
            other => panic!("expected Binary, got {other:?}"),
        }

        let dos = dir.path().join("dos.md");
        std::fs::write(&dos, "x\r\ny").unwrap();
        match read_text(&dos).unwrap() {
            Read::Text(t) => {
                assert!(t.crlf);
                assert!(!t.lossy);
                assert_eq!(t.text, "x\ny");
            }
            other => panic!("expected Text, got {other:?}"),
        }

        let broken = dir.path().join("broken.md");
        std::fs::write(&broken, [0xff, b'a']).unwrap();
        match read_text(&broken).unwrap() {
            Read::Text(t) => {
                assert!(t.lossy);
                assert!(!t.crlf);
                assert!(t.text.ends_with('a'));
            }
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[test]
    fn for_disk_strips_and_restores_crlf() {
        assert_eq!(for_disk("a  \nb\t\n", true, true), "a\r\nb\r\n");
        assert_eq!(for_disk("a  \nb\t\n", false, false), "a  \nb\t\n");
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

        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
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
    fn create_note_refuses_existing_and_creates_parents() {
        let dir = tempfile::tempdir().unwrap();
        let note = dir.path().join("sub/deep/New.md");

        let etag = create_note(&note, "hello").unwrap();
        assert_eq!(std::fs::read_to_string(&note).unwrap(), "hello");
        assert_eq!(etag, Etag::of(&note).unwrap());

        match create_note(&note, "other") {
            Err(e) => assert_eq!(e.kind(), io::ErrorKind::AlreadyExists),
            Ok(_) => panic!("clobbered an existing note"),
        }
        assert_eq!(std::fs::read_to_string(&note).unwrap(), "hello");

        let leftovers: Vec<_> = std::fs::read_dir(note.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(leftovers, vec!["New.md".to_string()]);
    }

    #[test]
    fn rename_refuses_overwrite_and_moves_the_symlink_not_its_target() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("real.md");
        let link = dir.path().join("link.md");
        std::fs::write(&target, "one").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();

        match rename(&link, &target) {
            Err(e) => assert_eq!(e.kind(), io::ErrorKind::AlreadyExists),
            Ok(()) => panic!("overwrote an existing file"),
        }

        let moved = dir.path().join("moved.md");
        rename(&link, &moved).unwrap();
        assert!(
            std::fs::symlink_metadata(&moved)
                .unwrap()
                .file_type()
                .is_symlink(),
            "the link itself must move, not its target"
        );
        assert!(std::fs::symlink_metadata(&link).is_err());
        assert!(target.exists(), "the target stayed where it was");
        assert_eq!(std::fs::read_to_string(&moved).unwrap(), "one");
    }

    #[test]
    fn expected_etag_on_missing_file_is_an_io_error() {
        let dir = tempfile::tempdir().unwrap();
        let ghost = Etag {
            mtime_ns: 1,
            size: 1,
            ino: 1,
        };
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

        assert!(is_sync_conflict(
            "Note.sync-conflict-20260903-101500-ABCDEFG.md"
        ));
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
        let a = dir
            .path()
            .join("Note.sync-conflict-20260903-101500-AAAAAAA.md");
        let b = dir
            .path()
            .join("Note.sync-conflict-20260903-101600-BBBBBBB.md");
        for p in [&note, &a, &b] {
            std::fs::write(p, "x").unwrap();
        }
        std::fs::write(
            dir.path()
                .join("Other.sync-conflict-20260903-101500-CCCCCCC.md"),
            "x",
        )
        .unwrap();

        assert_eq!(
            conflict_siblings(&note).unwrap(),
            vec![a.clone(), b.clone()]
        );
        // Asking with a conflict copy in hand finds the whole set, including itself.
        assert_eq!(conflict_siblings(&a).unwrap(), vec![a, b]);
    }
}
