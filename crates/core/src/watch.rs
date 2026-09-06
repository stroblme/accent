//! Debounced vault watcher: turns raw inotify noise into a handful of vault-level facts.
//!
//! Two Syncthing realities shape this: a pulled file appears as `.syncthing.X.tmp` and is then
//! renamed into place (so the temp name must never reach the index), and a losing edit lands as
//! `X.sync-conflict-….md` (so the UI wants to know the moment one shows up).
//!
//! The watch set is exactly the directory list the walk kept — one non-recursive watch each,
//! never a recursive watch on the root. Two kernel limits make that matter: `max_user_watches`
//! (135 768 on the author's machine) is charged per directory, and `max_queued_events` (16 384)
//! is filled by our own walk, because inotify reports an open on every watched directory. A vault
//! with more watched directories than the queue holds therefore overflows it on each reconcile,
//! and an overflow asks for another reconcile — measured as a permanent loop on a 16 536-directory
//! vault. Whatever the walk skips must be skipped here too, or neither limit improves.

use std::path::{Path, PathBuf};
use std::time::Duration;

use notify::RecursiveMode;
use notify::event::{EventKind, ModifyKind, RenameMode};
use notify_debouncer_full::{
    DebounceEventResult, Debouncer, FileIdCache, RecommendedCache, new_debouncer, new_debouncer_opt,
};

use crate::fs::{is_sync_conflict, is_syncthing_temp};

/// What the vault did, with absolute paths.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum VaultEvent {
    Changed(PathBuf),
    Removed(PathBuf),
    Renamed {
        from: PathBuf,
        to: PathBuf,
    },
    /// A `*.sync-conflict-*` file appeared; the UI can offer a merge.
    ConflictAppeared(PathBuf),
    /// Something moved inside a repository's own directory: a commit, a checkout, a stage.
    ///
    /// Never a vault file — `.git` is the one tree the walk refuses to enter — so this exists to
    /// keep the two apart. Only the git directories the caller asked to watch produce it.
    Git(PathBuf),
    /// Events were dropped (queue overflow or watcher error): re-walk the vault.
    Rescan,
}

/// `fs.inotify.max_user_watches`, or `None` where the sysctl is unreadable (Android, containers).
pub fn inotify_budget() -> Option<u64> {
    std::fs::read_to_string("/proc/sys/fs/inotify/max_user_watches")
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// Owns the debouncer; dropping it stops watching.
pub struct Watcher {
    _backend: Backend,
}

/// Held only so the watcher keeps running; stopping happens in `Debouncer::drop`.
#[allow(dead_code)]
enum Backend {
    Native(Debouncer<notify::RecommendedWatcher, RecommendedCache>),
    Poll(Debouncer<notify::PollWatcher, RecommendedCache>),
}

impl Watcher {
    /// Watch `root` and each of `dirs`, one non-recursive watch per directory.
    ///
    /// `dirs` is what the walk kept, as vault paths (a directory reached through a symlink is
    /// watched through that same path — `inotify_add_watch` resolves the link, so the events come
    /// back under the name the vault knows). One watch per directory rather than a recursive
    /// watch on the root is what makes the skipped trees actually free: a recursive watch walks
    /// the disk itself and would re-add every `.venv` directory the walk refused. inotify charges
    /// per directory, so the watch set is exactly the walk's directory count.
    ///
    /// If that count is close to the kernel's inotify budget the watcher falls back to polling
    /// instead of silently missing changes (inotify fails per-directory once the budget is gone).
    ///
    /// A directory created after this call is not watched until the next walk rebuilds the
    /// watcher; the caller is responsible for triggering one.
    ///
    /// `on_event` runs on the debouncer's own thread, so it must not block: hand the event to a
    /// channel or the UI's main context and return.
    pub fn new(
        root: &Path,
        dirs: &[PathBuf],
        on_event: impl Fn(VaultEvent) + Send + 'static,
    ) -> anyhow::Result<Watcher> {
        let dir_count = dirs.len() + 1;
        let handler = move |result: DebounceEventResult| match result {
            Ok(events) => {
                for event in events {
                    for mapped in classify(&event.event) {
                        on_event(mapped);
                    }
                }
            }
            Err(errors) => {
                for e in &errors {
                    tracing::warn!(error = %e, "watch error; asking for a rescan");
                }
                on_event(VaultEvent::Rescan);
            }
        };

        let debounce = Duration::from_millis(300);
        let over_budget = inotify_budget().is_some_and(|b| dir_count as f64 > 0.8 * b as f64);

        let backend = if over_budget {
            tracing::warn!(
                dir_count,
                budget = inotify_budget(),
                "vault uses more than 80% of the inotify watch budget; falling back to 2s polling. \
                 Move dependency trees out of the vault, or raise the budget with: \
                 sudo sysctl -w fs.inotify.max_user_watches=524288"
            );
            let config = notify::Config::default().with_poll_interval(Duration::from_secs(2));
            let mut d = new_debouncer_opt::<_, notify::PollWatcher, RecommendedCache>(
                debounce,
                None,
                handler,
                RecommendedCache::new(),
                config,
            )?;
            watch_all(&mut d, root, dirs)?;
            Backend::Poll(d)
        } else {
            let mut d = new_debouncer(debounce, None, handler)?;
            watch_all(&mut d, root, dirs)?;
            Backend::Native(d)
        };

        Ok(Watcher { _backend: backend })
    }
}

fn watch_all<T: notify::Watcher, C: FileIdCache>(
    d: &mut Debouncer<T, C>,
    root: &Path,
    dirs: &[PathBuf],
) -> notify::Result<()> {
    d.watch(root, RecursiveMode::NonRecursive)?;
    for dir in dirs {
        // A directory the walk saw can be gone by the time we get here — a synced vault moves
        // under us — and one unwatchable folder is no reason to leave the whole vault unwatched.
        if let Err(e) = d.watch(dir, RecursiveMode::NonRecursive) {
            tracing::debug!(dir = %dir.display(), error = %e, "not watching");
        }
    }
    Ok(())
}

/// A path inside a repository's own directory.
fn in_git(path: &Path) -> bool {
    path.components().any(|c| c.as_os_str() == ".git")
}

/// Paths we never report as vault files: git internals, Syncthing's in-flight downloads, and the temporaries
/// [`crate::fs::write_note`] renames into place: a save of ours must reach the UI as one event
/// for the note, never as a stray `.accent-` file.
fn ignored(path: &Path) -> bool {
    in_git(path)
        || path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| is_syncthing_temp(n) || n.starts_with(".accent-"))
}

/// A path that just showed up: a conflict copy is worth its own event.
fn appeared(path: &Path) -> VaultEvent {
    if path
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(is_sync_conflict)
    {
        VaultEvent::ConflictAppeared(path.to_path_buf())
    } else {
        VaultEvent::Changed(path.to_path_buf())
    }
}

/// Map one debounced notify event to zero or more vault events.
fn classify(ev: &notify::Event) -> Vec<VaultEvent> {
    if ev.need_rescan() {
        return vec![VaultEvent::Rescan];
    }
    // A repository's innards, before the vault rules get a chance to drop them. What happened in
    // there does not matter — a commit touches a dozen files under `.git` and the answer to all
    // of them is the same one refresh — so the kind of change is not carried.
    if let Some(path) = ev.paths.iter().find(|p| in_git(p)) {
        return vec![VaultEvent::Git(path.clone())];
    }
    let live: Vec<&PathBuf> = ev.paths.iter().filter(|p| !ignored(p)).collect();

    match ev.kind {
        EventKind::Create(_) | EventKind::Modify(ModifyKind::Name(RenameMode::To)) => {
            live.iter().map(|p| appeared(p)).collect()
        }
        EventKind::Modify(ModifyKind::Name(RenameMode::Both)) if ev.paths.len() == 2 => {
            let (from, to) = (&ev.paths[0], &ev.paths[1]);
            match (ignored(from), ignored(to)) {
                (false, false) => vec![VaultEvent::Renamed {
                    from: from.clone(),
                    to: to.clone(),
                }],
                // Syncthing finishing a download: the destination is just new content.
                (true, false) => vec![appeared(to)],
                (false, true) => vec![VaultEvent::Removed(from.clone())],
                (true, true) => vec![],
            }
        }
        EventKind::Modify(ModifyKind::Name(RenameMode::From)) | EventKind::Remove(_) => live
            .iter()
            .map(|p| VaultEvent::Removed((*p).clone()))
            .collect(),
        // Backends that cannot tell the two halves apart (poll, kqueue): ask the filesystem.
        EventKind::Modify(ModifyKind::Name(_)) => live
            .iter()
            .map(|p| {
                if p.exists() {
                    appeared(p)
                } else {
                    VaultEvent::Removed((*p).clone())
                }
            })
            .collect(),
        EventKind::Modify(ModifyKind::Data(_) | ModifyKind::Metadata(_) | ModifyKind::Any) => live
            .iter()
            .map(|p| VaultEvent::Changed((*p).clone()))
            .collect(),
        // Access/Other: nothing the index cares about.
        _ => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;
    use std::sync::mpsc::{Receiver, RecvTimeoutError};
    use std::time::Instant;

    /// Collect events until the vault goes quiet, or `budget` runs out.
    fn drain(rx: &Receiver<VaultEvent>, budget: Duration) -> Vec<VaultEvent> {
        let deadline = Instant::now() + budget;
        let mut out = Vec::new();
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match rx.recv_timeout(left.min(Duration::from_millis(700))) {
                Ok(e) => out.push(e),
                Err(RecvTimeoutError::Disconnected) => break,
                // A quiet stretch after we already have something means the burst is over.
                Err(RecvTimeoutError::Timeout) if !out.is_empty() => break,
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
        out
    }

    fn touches(events: &[VaultEvent], name: &str) -> bool {
        let name = OsStr::new(name);
        events.iter().any(|e| match e {
            VaultEvent::Changed(p)
            | VaultEvent::Removed(p)
            | VaultEvent::ConflictAppeared(p)
            | VaultEvent::Renamed { to: p, .. }
            | VaultEvent::Git(p) => p.file_name() == Some(name),
            VaultEvent::Rescan => false,
        })
    }

    fn start(root: &Path) -> (Watcher, Receiver<VaultEvent>) {
        start_with(root, &[])
    }

    /// `.git` is never a vault file, but it is not nothing either: watched on purpose, it is the
    /// only way a commit made in a terminal reaches the app.
    #[test]
    fn a_watched_git_directory_reports_as_git_and_not_as_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let git = dir.path().join(".git");
        std::fs::create_dir_all(&git).unwrap();
        let (_w, rx) = start_with(dir.path(), std::slice::from_ref(&git));

        std::fs::write(git.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        let events = drain(&rx, Duration::from_secs(5));
        assert!(
            events.iter().any(|e| matches!(e, VaultEvent::Git(_))),
            "{events:?}"
        );
        assert!(
            !events.iter().any(|e| matches!(e, VaultEvent::Changed(_))),
            "a git write must never look like a note: {events:?}"
        );
    }

    fn start_with(root: &Path, dirs: &[PathBuf]) -> (Watcher, Receiver<VaultEvent>) {
        let (tx, rx) = std::sync::mpsc::channel();
        let w = Watcher::new(root, dirs, move |e| {
            let _ = tx.send(e);
        })
        .unwrap();
        // Let the watcher settle before the test perturbs the directory.
        std::thread::sleep(Duration::from_millis(200));
        (w, rx)
    }

    #[test]
    fn reports_a_write() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let (_w, rx) = start(&root);

        std::fs::write(root.join("Note.md"), "hello").unwrap();

        let events = drain(&rx, Duration::from_secs(5));
        assert!(
            touches(&events, "Note.md"),
            "expected an event for Note.md, got {events:?}"
        );
    }

    #[test]
    fn reports_a_conflict_copy() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let (_w, rx) = start(&root);

        let conflict = root.join("Note.sync-conflict-20260903-101500-ABCDEFG.md");
        std::fs::write(&conflict, "theirs").unwrap();

        let events = drain(&rx, Duration::from_secs(5));
        assert!(
            events
                .iter()
                .any(|e| matches!(e, VaultEvent::ConflictAppeared(p) if *p == conflict)),
            "expected ConflictAppeared, got {events:?}"
        );
    }

    #[test]
    fn syncthing_temp_is_invisible_until_renamed() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let (_w, rx) = start(&root);

        let tmp = root.join(".syncthing.Note.md.tmp");
        std::fs::write(&tmp, "pulled").unwrap();
        std::thread::sleep(Duration::from_millis(50));
        std::fs::rename(&tmp, root.join("Note.md")).unwrap();

        let events = drain(&rx, Duration::from_secs(5));
        assert!(
            touches(&events, "Note.md"),
            "expected an event for Note.md, got {events:?}"
        );
        assert!(
            !touches(&events, ".syncthing.Note.md.tmp"),
            "temp name leaked into the vault events: {events:?}"
        );
    }

    /// A save writes `.accent-XXXX` next to the note and renames it into place; the UI must see
    /// the note and never the temporary.
    ///
    /// The read before the write is what a real editor does, and it matters: once the debouncer
    /// has the note's inode cached it reports the rename over it as `Remove` + `Create` rather
    /// than as a modification. Nothing here can tell the two apart, which is why the façade
    /// re-stats a removed path before believing it.
    #[test]
    fn accent_temp_rename_reports_only_the_final_path() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let note = root.join("Note.md");
        std::fs::write(&note, "one").unwrap();
        let (_w, rx) = start(&root);

        let _ = std::fs::read_to_string(&note).unwrap();
        crate::fs::write_note(&note, "two", None).unwrap();

        let events = drain(&rx, Duration::from_secs(5));
        assert!(
            touches(&events, "Note.md"),
            "no event for the note: {events:?}"
        );
        assert!(
            events.iter().all(
                |e| matches!(e, VaultEvent::Changed(p) | VaultEvent::Removed(p) if *p == note)
            ),
            "expected only events for {note:?}, got {events:?}"
        );
        assert!(
            note.exists(),
            "the note the watcher reported on is still there"
        );
    }

    /// The watch set is a list, not a tree: a directory the walk skipped gets no watch, so its
    /// churn never reaches the queue. Without this the walk's skips buy nothing at the kernel.
    #[test]
    fn only_the_listed_directories_are_watched() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::create_dir(root.join("Notes")).unwrap();
        std::fs::create_dir(root.join(".venv")).unwrap();
        let (_w, rx) = start_with(&root, &[root.join("Notes")]);

        std::fs::write(root.join(".venv/site.py"), "skipped").unwrap();
        std::fs::write(root.join("Notes/Note.md"), "watched").unwrap();

        let events = drain(&rx, Duration::from_secs(5));
        assert!(touches(&events, "Note.md"), "{events:?}");
        assert!(!touches(&events, "site.py"), "{events:?}");
    }

    #[test]
    fn budget_is_readable_or_absent() {
        // Linux exposes it; Android and some containers do not. Either is fine, zero is not.
        if let Some(b) = inotify_budget() {
            assert!(b > 0);
        }
    }

    #[test]
    fn classify_maps_the_interesting_kinds() {
        use notify::event::{CreateKind, DataChange, RemoveKind};
        let p = |s: &str| PathBuf::from(s);

        let ev = |kind, paths: Vec<PathBuf>| notify::Event {
            kind,
            paths,
            attrs: Default::default(),
        };

        assert_eq!(
            classify(&ev(EventKind::Create(CreateKind::File), vec![p("/v/N.md")])),
            vec![VaultEvent::Changed(p("/v/N.md"))]
        );
        assert_eq!(
            classify(&ev(
                EventKind::Create(CreateKind::File),
                vec![p("/v/N.sync-conflict-20260903-101500-ABCDEFG.md")]
            )),
            vec![VaultEvent::ConflictAppeared(p(
                "/v/N.sync-conflict-20260903-101500-ABCDEFG.md"
            ))]
        );
        assert_eq!(
            classify(&ev(EventKind::Remove(RemoveKind::File), vec![p("/v/N.md")])),
            vec![VaultEvent::Removed(p("/v/N.md"))]
        );
        assert_eq!(
            classify(&ev(
                EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
                vec![p("/v/A.md"), p("/v/B.md")]
            )),
            vec![VaultEvent::Renamed {
                from: p("/v/A.md"),
                to: p("/v/B.md")
            }]
        );
        // Temp -> real is a plain content change, not a rename the index should track.
        assert_eq!(
            classify(&ev(
                EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
                vec![p("/v/.syncthing.N.md.tmp"), p("/v/N.md")]
            )),
            vec![VaultEvent::Changed(p("/v/N.md"))]
        );
        // Git's own files are their own kind, never a vault file: `.git` is watched on purpose
        // now, and one refresh is the answer to everything that happens in there.
        assert_eq!(
            classify(&ev(
                EventKind::Modify(ModifyKind::Data(DataChange::Content)),
                vec![p("/v/.git/index")]
            )),
            vec![VaultEvent::Git(p("/v/.git/index"))]
        );
        assert!(
            classify(&ev(
                EventKind::Modify(ModifyKind::Data(DataChange::Content)),
                vec![p("/v/.syncthing.N.md.tmp")]
            ))
            .is_empty()
        );
    }
}
