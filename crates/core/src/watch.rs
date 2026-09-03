//! Debounced vault watcher: turns raw inotify noise into a handful of vault-level facts.
//!
//! Two Syncthing realities shape this: a pulled file appears as `.syncthing.X.tmp` and is then
//! renamed into place (so the temp name must never reach the index), and a losing edit lands as
//! `X.sync-conflict-….md` (so the UI wants to know the moment one shows up).

use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
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
    /// Watch `root` and each of `extra_dirs` recursively.
    ///
    /// `extra_dirs` are symlinked directories the caller resolved during the walk: inotify does
    /// not follow symlinks, so a linked-in folder needs its own watch.
    ///
    /// `dir_count` is how many directories the caller's walk found. If that is close to the
    /// kernel's inotify budget the watcher falls back to polling instead of silently missing
    /// changes (inotify fails per-directory once the budget is gone).
    pub fn new(
        root: &Path,
        extra_dirs: &[PathBuf],
        dir_count: usize,
        tx: Sender<VaultEvent>,
    ) -> anyhow::Result<Watcher> {
        let handler = move |result: DebounceEventResult| match result {
            Ok(events) => {
                for event in events {
                    for mapped in classify(&event.event) {
                        if tx.send(mapped).is_err() {
                            return; // receiver gone; the watcher will be dropped shortly
                        }
                    }
                }
            }
            Err(errors) => {
                for e in &errors {
                    tracing::warn!(error = %e, "watch error; asking for a rescan");
                }
                let _ = tx.send(VaultEvent::Rescan);
            }
        };

        let debounce = Duration::from_millis(300);
        let over_budget = inotify_budget().is_some_and(|b| dir_count as f64 > 0.8 * b as f64);

        let backend = if over_budget {
            tracing::warn!(
                dir_count,
                budget = inotify_budget(),
                "vault uses more than 80% of the inotify watch budget; falling back to 2s polling. \
                 Raise it with: sudo sysctl -w fs.inotify.max_user_watches=524288"
            );
            let config = notify::Config::default().with_poll_interval(Duration::from_secs(2));
            let mut d = new_debouncer_opt::<_, notify::PollWatcher, RecommendedCache>(
                debounce,
                None,
                handler,
                RecommendedCache::new(),
                config,
            )?;
            watch_all(&mut d, root, extra_dirs)?;
            Backend::Poll(d)
        } else {
            let mut d = new_debouncer(debounce, None, handler)?;
            watch_all(&mut d, root, extra_dirs)?;
            Backend::Native(d)
        };

        Ok(Watcher { _backend: backend })
    }
}

fn watch_all<T: notify::Watcher, C: FileIdCache>(
    d: &mut Debouncer<T, C>,
    root: &Path,
    extra_dirs: &[PathBuf],
) -> notify::Result<()> {
    d.watch(root, RecursiveMode::Recursive)?;
    for dir in extra_dirs {
        d.watch(dir, RecursiveMode::Recursive)?;
    }
    Ok(())
}

/// Paths we never report: git internals and Syncthing's in-flight downloads.
fn ignored(path: &Path) -> bool {
    path.components().any(|c| c.as_os_str() == ".git")
        || path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(is_syncthing_temp)
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
            | VaultEvent::Renamed { to: p, .. } => p.file_name() == Some(name),
            VaultEvent::Rescan => false,
        })
    }

    fn start(root: &Path) -> (Watcher, Receiver<VaultEvent>) {
        let (tx, rx) = std::sync::mpsc::channel();
        let w = Watcher::new(root, &[], 0, tx).unwrap();
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
        assert!(
            classify(&ev(
                EventKind::Modify(ModifyKind::Data(DataChange::Content)),
                vec![p("/v/.git/index")]
            ))
            .is_empty()
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
