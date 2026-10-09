//! Safe read/write for Syncthing-synced, symlinked vaults. See plan "Save" row.
//!
//! Save model is VS Code's: the editor holds an etag from the last read, and a save that
//! finds a different etag on disk is refused so the UI can offer a diff instead of clobbering
//! a change that Syncthing (or another device) pulled in behind our back.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::{self, Read as _, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

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
    pub fn of(path: &Path) -> Result<Etag> {
        let meta = std::fs::metadata(path).map_err(|e| Error::io(path.display(), e))?;
        Ok(Etag::from_meta(&meta))
    }

    /// The etag of a file already open, which a path could have been renamed away from since.
    pub fn from_meta(m: &std::fs::Metadata) -> Etag {
        Etag {
            mtime_ns: m.mtime() * 1_000_000_000 + m.mtime_nsec(),
            size: m.size(),
            ino: m.ino(),
        }
    }
}

/// What a file holds, as a digest of its bytes: the etag says a file was written, this says
/// whether what it holds changed. Syncthing setting a file's mtime, or the same bytes written
/// again, moves the etag alone.
pub type Digest = blake3::Hash;

pub fn digest(bytes: &str) -> Digest {
    blake3::hash(bytes.as_bytes())
}

/// The file at `path`, opened, and its etag, taken from the handle before anything is read: a
/// writer renaming a file into place leaves the bytes and the etag both of the file opened, and
/// one writing in place moves the mtime past the etag, so a writer that raced the read costs a
/// refused save rather than a silent overwrite. A stat of the path after the read paired the
/// bytes of the file opened with the etag of the one renamed over it.
///
/// Only a regular file is opened: a FIFO or a device has no end to read to. The open does not
/// block, so a FIFO is refused rather than waited on until something writes to it; a regular
/// file ignores the flag.
fn open_stamped(path: &Path) -> io::Result<(std::fs::File, std::fs::Metadata)> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    Ok((file, meta))
}

/// The bytes of the file at `path` and its etag, taken as [`open_stamped`] takes it, or the
/// file's size as the error when it holds more than [`MAX_TEXT`]. The cap holds while reading, so
/// a file that grows past it mid-read is too large too, never read to wherever it ends.
fn read_capped(path: &Path) -> io::Result<Result<(Vec<u8>, Etag), u64>> {
    let (file, meta) = open_stamped(path)?;
    if meta.size() > MAX_TEXT {
        return Ok(Err(meta.size()));
    }
    let mut bytes = Vec::with_capacity(meta.size() as usize);
    (&file).take(MAX_TEXT + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_TEXT {
        return Ok(Err(file.metadata()?.size()));
    }
    Ok(Ok((bytes, Etag::from_meta(&meta))))
}

/// Read a note and the etag to hand back to [`write_note`]. A file over [`MAX_TEXT`], or one that
/// is not UTF-8, is [`Error::Invalid`].
pub fn read_note(path: &Path) -> Result<(String, Etag)> {
    let read = read_capped(path).map_err(|e| Error::io(path.display(), e))?;
    let (bytes, etag) = read.map_err(|_| {
        Error::Invalid(format!(
            "the file is larger than {} MiB",
            MAX_TEXT / (1024 * 1024)
        ))
    })?;
    let text = String::from_utf8(bytes)
        .map_err(|e| Error::Invalid(format!("the file is not UTF-8: {e}")))?;
    Ok((text, etag))
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

/// Read any file as text, saying so when it is binary or too big to hold. The etag is taken as
/// [`read_note`] takes it.
pub fn read_text(path: &Path) -> Result<Read> {
    let (bytes, etag) = match read_capped(path).map_err(|e| Error::io(path.display(), e))? {
        Ok(read) => read,
        Err(size) => return Ok(Read::TooLarge { size }),
    };
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
        etag,
        crlf,
        lossy,
    }))
}

impl Text {
    /// The [`digest`] of the file as an editor writes it back, CRLF put back on every line: the
    /// bytes read, but for a file of mixed line endings, and what a write of this text leaves.
    pub fn digest(&self) -> Digest {
        match self.crlf {
            true => digest(&for_disk(&self.text, true, false)),
            false => digest(&self.text),
        }
    }
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
    /// The vault is on another machine that is not answering: nothing was written, nothing is
    /// wrong with the file, and the buffer is still the only copy of the edits.
    ///
    /// Never produced here — a save on this machine has a disk to fail against — but it is in
    /// this enum because there is one save path above it and the difference matters at the top of
    /// it: a link that is down is a state the window already shows, not a disk error to report.
    #[error("the vault is not connected")]
    Offline,
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

/// The same atomic save for bytes: a PDF an annotation was written into, a new drawing, an image
/// pasted into a note.
///
/// Split out of [`write_note`] rather than duplicated, so a PDF gets the etag gate, the symlink
/// resolution and the preserved ownership a note has always had.
///
/// The etag is checked once the bytes are written and synced, right before the rename, and the
/// saves of one file in this process take turns at the two ([`turn`]): of saves holding the same
/// etag, the first lands and the others are refused. Another process (Syncthing, a second accent)
/// can still replace the file between that check and the rename, and its write is then lost:
/// Linux has no rename that replaces a file only while it is still the one checked.
pub fn write_bytes(path: &Path, bytes: &[u8], expected: Option<Etag>) -> Result<Etag, SaveError> {
    // Resolve symlinks first: writing through a link must replace the link *target*, otherwise the
    // rename below would silently turn a symlinked note into a regular file in the vault.
    let canonical = canonical_target(path)?;
    let parent = canonical
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no parent"))?;

    let existing = std::fs::metadata(&canonical).ok();
    let mut tmp = temp_file(parent, existing.as_ref())?;
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

    let _turn = turn(&canonical);
    if let Some(exp) = expected {
        match std::fs::metadata(&canonical) {
            Ok(m) if Etag::from_meta(&m) != exp => {
                return Err(SaveError::ChangedOnDisk {
                    current: Etag::from_meta(&m),
                });
            }
            Ok(_) => {}
            Err(_) => {
                return Err(
                    io::Error::new(io::ErrorKind::NotFound, "file vanished before save").into(),
                );
            }
        }
    }

    // ponytail: no directory fsync after the rename. The rename itself is atomic, so a crash can
    // only lose the whole save, never half of it. Add `File::open(parent)?.sync_all()` here if
    // crash-consistency (as opposed to torn-write safety) ever matters.
    let file = tmp
        .persist(&canonical)
        .map_err(|e| SaveError::Io(e.error))?;
    Ok(Etag::from_meta(&file.metadata()?))
}

/// A temporary file in `parent` for a save to write into, no more open to others than the file it
/// replaces (`existing`) while it holds the bytes.
///
/// Created with that file's mode, or with 0666 for a new one as `File::create` would, the umask
/// applying to both: a 0600 file's bytes never wait in a 0644 temp file, and a new file gets the
/// mode a new note gets (0644 under the usual 022) rather than tempfile's own 0600, an image
/// nobody else on a shared vault could open. What the umask took from an existing file's mode is
/// put back before the rename.
fn temp_file(
    parent: &Path,
    existing: Option<&std::fs::Metadata>,
) -> io::Result<tempfile::NamedTempFile> {
    let mode = existing.map_or(0o666, |m| m.mode() & 0o777);
    tempfile::Builder::new()
        .prefix(".accent-")
        .permissions(std::fs::Permissions::from_mode(mode))
        .tempfile_in(parent)
}

/// The turn a save of `canonical` takes at its etag check and its rename, so two saves of one file
/// in this process cannot both pass the check before either renames. It is held for a `stat` and a
/// `rename`, never the write or the fsync. One lock per stripe of paths rather than per path, so
/// there is no table of paths to clear: two files sharing a stripe only wait a rename for each
/// other.
fn turn(canonical: &Path) -> MutexGuard<'static, ()> {
    static TURNS: [Mutex<()>; 64] = [const { Mutex::new(()) }; 64];
    let mut hash = DefaultHasher::new();
    canonical.hash(&mut hash);
    // The lock guards no data, so a save that panicked holding it left nothing to distrust.
    TURNS[hash.finish() as usize % TURNS.len()]
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// Create a new note, refusing to clobber an existing file. Missing parent directories are created.
///
/// `File::create_new` claims the name in one syscall, so two writers racing for the same new note
/// cannot both believe they won it. The content then goes through [`write_note`], which is what
/// gives a brand new note the same atomic-rename guarantees as every later save.
pub fn create_note(path: &Path, text: &str) -> Result<Etag> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|e| Error::io(parent.display(), e))?;
    }
    // AlreadyExists if the name is taken.
    std::fs::File::create_new(path).map_err(|e| Error::io(path.display(), e))?;
    write_note(path, text, None).map_err(|e| Error::Io(format!("{}: {e}", path.display())))
}

/// Move a file or directory, refusing to overwrite an existing target.
///
/// `std::fs::rename` moves a symlink itself rather than what it points at, which is what a vault
/// wants: renaming a linked-in note must not copy the target into the vault.
///
/// ponytail: the existence check races a concurrent create, and a move across mount points still
/// fails with `EXDEV`. `renameat2(RENAME_NOREPLACE)` closes the first, copy-then-delete the second.
pub fn rename(from: &Path, to: &Path) -> Result<()> {
    if to.symlink_metadata().is_ok() {
        return Err(Error::AlreadyExists(to.display().to_string()));
    }
    std::fs::rename(from, to).map_err(|e| Error::io(from.display(), e))
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
    fn reads_hold_the_cap_and_refuse_special_files() {
        let dir = tempfile::tempdir().unwrap();
        let big = dir.path().join("big.md");
        // Sparse, so the test writes nothing to disk.
        std::fs::File::create(&big)
            .unwrap()
            .set_len(MAX_TEXT + 1)
            .unwrap();
        assert!(matches!(read_note(&big), Err(Error::Invalid(_))));
        assert!(
            matches!(read_text(&big).unwrap(), Read::TooLarge { size } if size == MAX_TEXT + 1)
        );

        // Opening a FIFO nobody writes to for reading would wait forever.
        let fifo = dir.path().join("fifo.md");
        let name = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(matches!(read_note(&fifo), Err(Error::Invalid(_))));
        assert!(read_text(&fifo).is_err());
    }

    #[test]
    fn a_read_digests_the_bytes_a_write_of_it_leaves() {
        let dir = tempfile::tempdir().unwrap();
        let note = dir.path().join("Note.md");
        for bytes in ["a\nb\n", "a\r\nb\r\n"] {
            std::fs::write(&note, bytes).unwrap();
            let Read::Text(text) = read_text(&note).unwrap() else {
                panic!("expected Text");
            };
            assert_eq!(text.digest(), digest(bytes));
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
    fn of_saves_holding_one_etag_only_the_first_lands() {
        let dir = tempfile::tempdir().unwrap();
        let note = dir.path().join("Note.md");
        std::fs::write(&note, "zero").unwrap();
        let (_, etag) = read_note(&note).unwrap();

        let start = std::sync::Barrier::new(8);
        let landed = std::thread::scope(|s| {
            let saves: Vec<_> = (0..8)
                .map(|i| {
                    let (note, start) = (&note, &start);
                    s.spawn(move || {
                        start.wait();
                        write_note(note, &i.to_string(), Some(etag))
                    })
                })
                .collect();
            saves
                .into_iter()
                .map(|save| match save.join().unwrap() {
                    Ok(_) => 1,
                    Err(SaveError::ChangedOnDisk { .. }) => 0,
                    Err(e) => panic!("{e}"),
                })
                .sum::<usize>()
        });
        assert_eq!(landed, 1);
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
    fn a_private_file_is_written_through_a_private_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let note = dir.path().join("Note.md");
        std::fs::write(&note, "one").unwrap();
        std::fs::set_permissions(&note, std::fs::Permissions::from_mode(0o600)).unwrap();

        let tmp = temp_file(dir.path(), Some(&std::fs::metadata(&note).unwrap())).unwrap();
        let mode = tmp.as_file().metadata().unwrap().permissions().mode();
        assert_eq!(
            mode & 0o077,
            0,
            "the bytes are readable to others: {mode:o}"
        );
    }

    #[test]
    fn a_new_file_gets_the_mode_a_new_note_gets() {
        let dir = tempfile::tempdir().unwrap();
        let (note, image) = (dir.path().join("New.md"), dir.path().join("New.png"));
        create_note(&note, "hello").unwrap();
        write_bytes(&image, b"png", None).unwrap();

        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&image), mode(&note));
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
            Err(e) => assert!(matches!(e, Error::AlreadyExists(_)), "{e}"),
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
            Err(e) => assert!(matches!(e, Error::AlreadyExists(_)), "{e}"),
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
}
