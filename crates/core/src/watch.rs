//! Debounce filesystem events into vault changes, suppressing Syncthing temporaries and promptly
//! reporting conflict copies.
//!
//! Watch each indexed directory non-recursively. This keeps skipped dependency trees out of
//! inotify and prevents directory-open events from overflowing the queue and causing scan loops.
//! macOS has one FSEvents stream over the root that reports the same ([`fsevents`]).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use notify::RecursiveMode;
use notify::event::{EventKind, ModifyKind, RenameMode};
use notify_debouncer_full::{
    DebounceEventResult, Debouncer, FileIdCache, RecommendedCache, new_debouncer_opt,
};

use crate::fs::{is_sync_conflict, is_syncthing_temp};

#[cfg(target_os = "macos")]
mod fsevents;

/// The kernel's own watcher: inotify on Linux, and on macOS FSEvents behind [`fsevents::Native`].
#[cfg(not(target_os = "macos"))]
type Native = notify::RecommendedWatcher;
#[cfg(target_os = "macos")]
type Native = fsevents::Native;

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
fn inotify_budget() -> Option<u64> {
    std::fs::read_to_string("/proc/sys/fs/inotify/max_user_watches")
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// Owns the debouncer; dropping it stops watching.
pub struct Watcher {
    backend: Backend,
    /// What is watched besides the root, so a new set is a diff against it. See
    /// [`set_dirs`](Self::set_dirs).
    dirs: HashSet<PathBuf>,
    /// Why the vault is polled rather than watched, when it is.
    polled: Option<Polled>,
}

/// Why a [`Watcher`] polls.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Polled {
    /// The vault's folders would take more than 80% of the inotify watch budget.
    OverBudget,
    /// inotify refused a watch: the budget is spent, other programs' watches counted too. Polled
    /// for the watcher's life, since a watch refused again would leave a folder unheard.
    Refused,
}

impl Polled {
    /// What the reader is told, once: what happened, and the way out.
    fn notice(self) -> String {
        let why = match self {
            Polled::OverBudget => "vault uses more than 80% of the inotify watch budget",
            Polled::Refused => "inotify refused to watch a folder: its watch budget is spent",
        };
        format!(
            "{why}; falling back to 2s polling. Move dependency trees out of the vault, or raise \
             the budget with: sudo sysctl -w fs.inotify.max_user_watches=524288"
        )
    }
}

/// Stopping happens in `Debouncer::drop`.
enum Backend {
    Native(Debouncer<Native, RecommendedCache>),
    Poll(Debouncer<notify::PollWatcher, RecommendedCache>),
}

/// Whether watching `dir_count` directories would take more than 80% of the inotify budget, which
/// is where polling takes over.
fn over_budget(dir_count: usize) -> bool {
    inotify_budget().is_some_and(|b| dir_count as f64 > 0.8 * b as f64)
}

impl Watcher {
    /// Watch `root` and `dirs` non-recursively so skipped trees do not consume inotify watches.
    /// Symlink paths retain their vault names in events. Fall back to polling near the watch
    /// budget, or once inotify refuses a watch for want of it ([`polling`](Self::polling)).
    ///
    /// New directories require [`set_dirs`](Self::set_dirs). `on_event` runs on the debouncer
    /// thread and must return promptly, handing work to a channel or main context.
    pub fn new(
        root: &Path,
        dirs: &[PathBuf],
        on_event: impl Fn(VaultEvent) + Send + Sync + 'static,
    ) -> crate::Result<Watcher> {
        let refused =
            |e: notify::Error| crate::Error::Io(format!("watching {}: {e}", root.display()));
        // Shared, as an inotify debouncer refused a watch hands over to a polling one.
        let on_event = Arc::new(on_event);
        let handler = || {
            let on_event = on_event.clone();
            move |result: DebounceEventResult| match result {
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
            }
        };

        let debounce = Duration::from_millis(300);
        // How often the debouncer looks for events that have stood that long, waking whether or
        // not it holds any: its default, a quarter of the debounce, was 13 wakeups a second of an
        // idle window. Half of it reports a change 300 to 450 ms after it, not 300 to 375.
        let tick = Some(debounce / 2);
        let polled = match over_budget(dirs.len() + 1) {
            true => Polled::OverBudget,
            false => {
                let mut d = new_debouncer_opt::<_, Native, RecommendedCache>(
                    debounce,
                    tick,
                    handler(),
                    RecommendedCache::new(),
                    notify::Config::default(),
                )
                .map_err(refused)?;
                if !watch_all(&mut d, root, dirs).map_err(refused)? {
                    return Ok(Watcher {
                        backend: Backend::Native(d),
                        dirs: dirs.iter().cloned().collect(),
                        polled: None,
                    });
                }
                Polled::Refused
            }
        };
        let config = notify::Config::default().with_poll_interval(Duration::from_secs(2));
        let mut d = new_debouncer_opt::<_, notify::PollWatcher, RecommendedCache>(
            debounce,
            tick,
            handler(),
            RecommendedCache::new(),
            config,
        )
        .map_err(refused)?;
        watch_all(&mut d, root, dirs).map_err(refused)?;
        Ok(Watcher {
            backend: Backend::Poll(d),
            dirs: dirs.iter().cloned().collect(),
            polled: Some(polled),
        })
    }

    /// What to tell the reader when the vault is polled rather than watched, naming the budget.
    pub fn polling(&self) -> Option<String> {
        self.polled.map(Polled::notice)
    }

    /// Update directory watches on the existing debouncer, retaining pending events that
    /// replacing it would lose. Returns `false` if a new watcher has to switch between polling
    /// and inotify: the budget crossed either way, or a watch refused for want of it.
    pub fn set_dirs(&mut self, dirs: &[PathBuf]) -> bool {
        let poll = over_budget(dirs.len() + 1) || self.polled == Some(Polled::Refused);
        if poll != matches!(self.backend, Backend::Poll(_)) {
            return false;
        }
        let next: HashSet<PathBuf> = dirs.iter().cloned().collect();
        for dir in self.dirs.difference(&next) {
            // A deleted directory took its watch with it, and the kernel said so already.
            if let Err(e) = self.backend.unwatch(dir) {
                tracing::debug!(dir = %dir.display(), error = %e, "not unwatching");
            }
        }
        for dir in next.difference(&self.dirs) {
            match self.backend.watch(dir) {
                Ok(()) => {}
                Err(e) if starved(&e) => return false,
                Err(e) => tracing::debug!(dir = %dir.display(), error = %e, "not watching"),
            }
        }
        self.dirs = next;
        true
    }

    /// Take `dir`, and everything under it, out of the set without unwatching: it was removed,
    /// and the kernel dropped its watches with it. A directory made again under that name is a new
    /// one, which the next [`set_dirs`](Self::set_dirs) naming it watches rather than takes for
    /// the one already watched.
    pub fn forget(&mut self, dir: &Path) {
        if self.dirs.remove(dir) {
            self.dirs.retain(|d| !d.starts_with(dir));
        }
    }
}

impl Backend {
    fn watch(&mut self, dir: &Path) -> notify::Result<()> {
        match self {
            Backend::Native(d) => d.watch(dir, RecursiveMode::NonRecursive),
            Backend::Poll(d) => d.watch(dir, RecursiveMode::NonRecursive),
        }
    }

    fn unwatch(&mut self, dir: &Path) -> notify::Result<()> {
        match self {
            Backend::Native(d) => d.unwatch(dir),
            Backend::Poll(d) => d.unwatch(dir),
        }
    }
}

/// Watch `root` and `dirs`, returning `true`, and stopping there, once inotify refuses a watch
/// for want of budget ([`starved`]).
fn watch_all<T: notify::Watcher, C: FileIdCache>(
    d: &mut Debouncer<T, C>,
    root: &Path,
    dirs: &[PathBuf],
) -> notify::Result<bool> {
    match d.watch(root, RecursiveMode::NonRecursive) {
        Err(e) if starved(&e) => return Ok(true),
        other => other?,
    }
    for dir in dirs {
        // A directory the walk saw can be gone by the time we get here — a synced vault moves
        // under us — and one unwatchable folder is no reason to leave the whole vault unwatched.
        match d.watch(dir, RecursiveMode::NonRecursive) {
            Ok(()) => {}
            Err(e) if starved(&e) => return Ok(true),
            Err(e) => tracing::debug!(dir = %dir.display(), error = %e, "not watching"),
        }
    }
    Ok(false)
}

/// inotify refusing a watch because the user's budget (`fs.inotify.max_user_watches`) is spent.
fn starved(e: &notify::Error) -> bool {
    matches!(e.kind, notify::ErrorKind::MaxFilesWatch)
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
    // of them is the same one refresh — so the kind of change is not carried. Reads are the one
    // thing dropped: inotify's mask carries `IN_OPEN`, so the `git status` the pane runs opens
    // `.git/HEAD` and came back as news for the pane, which asked git again — a refresh a second
    // for as long as the window was open.
    if let Some(path) = ev.paths.iter().find(|p| in_git(p)) {
        return match ev.kind {
            EventKind::Access(_) => vec![],
            _ => vec![VaultEvent::Git(path.clone())],
        };
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

    /// A watch the kernel refuses for want of budget, which other programs spend as well as ours,
    /// turns the vault over to polling: a folder left unwatched would never be heard from. Run
    /// again in a user namespace of its own whose budget is 8 watches; where none can be made
    /// (no `unshare`, user namespaces refused) there is nothing to run it in.
    #[test]
    fn a_refused_watch_turns_the_vault_over_to_polling() {
        const INNER: &str = "ACCENT_TEST_STARVED";
        if std::env::var_os(INNER).is_none() {
            let script = "echo 8 > /proc/sys/user/max_inotify_watches || exit 77; \
                exec \"$0\" --exact watch::tests::a_refused_watch_turns_the_vault_over_to_polling";
            let ran = std::process::Command::new("unshare")
                .args(["-Ur", "sh", "-c", script])
                .arg(std::env::current_exe().unwrap())
                .env(INNER, "1")
                .status();
            match ran.map(|s| s.code()) {
                Ok(Some(0)) => {}
                // What the test harness exits with when the test failed.
                Ok(Some(101)) => panic!("failed in its own user namespace"),
                other => eprintln!("no user namespace to spend the watch budget in ({other:?})"),
            }
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let dirs: Vec<PathBuf> = (0..20).map(|i| root.join(format!("d{i:02}"))).collect();
        for dir in &dirs {
            std::fs::create_dir(dir).unwrap();
        }
        let (w, rx) = start_with(&root, &dirs);
        let said = w.polling().unwrap_or_default();
        assert!(said.contains("fs.inotify.max_user_watches"), "{said:?}");

        std::fs::write(dirs[19].join("late.md"), "late").unwrap();
        let events = drain(&rx, Duration::from_secs(8));
        assert!(
            touches(&events, "late.md"),
            "the last folder's write went unheard: {events:?}"
        );
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

    /// Opening git files must not trigger refreshes; reporting `IN_OPEN` from `git status` would
    /// create an endless status loop.
    #[test]
    fn a_read_inside_a_git_directory_is_not_a_change() {
        use notify::event::{AccessKind, AccessMode, DataChange};
        let p = |s: &str| PathBuf::from(s);
        let ev = |kind, paths: Vec<PathBuf>| notify::Event {
            kind,
            paths,
            attrs: Default::default(),
        };

        for kind in [
            EventKind::Access(AccessKind::Open(AccessMode::Read)),
            EventKind::Access(AccessKind::Close(AccessMode::Read)),
            EventKind::Access(AccessKind::Read),
        ] {
            assert!(
                classify(&ev(kind, vec![p("/v/.git/HEAD")])).is_empty(),
                "{kind:?} is a read"
            );
        }
        assert_eq!(
            classify(&ev(
                EventKind::Modify(ModifyKind::Data(DataChange::Content)),
                vec![p("/v/.git/HEAD")]
            )),
            vec![VaultEvent::Git(p("/v/.git/HEAD"))],
            "a write to the same file still is"
        );
    }
}
