//! Sync conflicts, where Keep Theirs writes the conflict copy over the original once the original
//! is backed up beside it.

use std::io;

use accent_core::path::{basename, parent_dir};

use super::copy::{copy_chunks, open_file};
use super::{Local, Msg, named};
use crate::paths::accent_conflict_name;
use crate::{Etag, Result, fs};

impl Local {
    /// Keep theirs by writing the conflict copy over the original after UI
    /// comparison and choice. First preserve the original in a sibling
    /// `*.sync-conflict-<timestamp>-accent.md` so the dialog can offer it back.
    /// Leave the winning copy for system-trash cleanup. Keeping mine only
    /// trashes the conflict copy and needs no call here.
    pub fn adopt_conflict(&self, original: &str, conflict: &str) -> Result<Etag> {
        self.adopt_conflict_at(original, conflict, chrono::Local::now().naive_local())
    }

    /// [`adopt_conflict`](Self::adopt_conflict), its backup named for the time `now`.
    pub(super) fn adopt_conflict_at(
        &self,
        original: &str,
        conflict: &str,
        now: chrono::NaiveDateTime,
    ) -> Result<Etag> {
        let (text, _) = fs::read_note(&self.resolve(conflict)?).map_err(named(conflict))?;
        let target = self.resolve(original)?;
        let failed = |e| accent_core::Error::io(original, e);
        // The backup and the write's gate are one version of the original: the etag comes from
        // the handle the backup is copied from, so an edit landing meanwhile refuses the write.
        let (src, _) = open_file(&target).map_err(failed)?;
        let seen = src.metadata().map_err(failed)?;
        let (name, backup) =
            claim_backup(&target, basename(original), now, &seen).map_err(failed)?;
        let written = copy_chunks(&src, &backup, &|| false)
            .and_then(|()| backup.sync_all())
            .map_err(failed)
            .and_then(|()| {
                #[cfg(test)]
                if let Some(edit) = BACKED_UP.take() {
                    edit(&target);
                }
                fs::write_note(&target, &text, Some(Etag::from_meta(&seen)))
            })
            // Whatever failed, the note is as it was, so the backup goes: it may hold a version
            // the note no longer does.
            .inspect_err(|_| {
                let _ = std::fs::remove_file(target.with_file_name(&name));
            })?;
        // The kept version too, so the banner offering it back is up at once, not a watcher
        // debounce later.
        let kept = match parent_dir(original) {
            "" => name,
            dir => format!("{dir}/{name}"),
        };
        for rel in [original.to_string(), kept] {
            self.post(Msg::Update { rel, own: true });
        }
        Ok(written)
    }
}

/// Create the backup [`Local::adopt_conflict`] makes at `now` beside `target`, the note called
/// `note`, with `seen`'s mode. It takes the first of [`accent_conflict_name`]'s names nothing
/// holds, `create_new` claiming it in the step that finds it free, so a second adopt within one
/// second keeps its backup beside the first one's.
fn claim_backup(
    target: &std::path::Path,
    note: &str,
    now: chrono::NaiveDateTime,
    seen: &std::fs::Metadata,
) -> io::Result<(String, std::fs::File)> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let mut n = 1;
    loop {
        let kept = accent_conflict_name(note, now, n);
        let claimed = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(seen.permissions().mode() & 0o777)
            .open(target.with_file_name(&kept));
        match claimed {
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => n += 1,
            claimed => return claimed.map(|file| (kept, file)),
        }
    }
}

#[cfg(test)]
thread_local! {
    /// A test's edit of the note, made once [`Local::adopt_conflict`] has copied its backup.
    pub(super) static BACKED_UP: std::cell::Cell<Option<fn(&std::path::Path)>> =
        const { std::cell::Cell::new(None) };
}
