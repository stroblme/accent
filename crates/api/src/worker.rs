//! The one thread that writes the index and owns the watcher.
//!
//! Everything here runs on it, so the index has exactly one writer and the UI never waits for
//! one. The inbox is drained in bursts rather than one message at a time: a Syncthing pull of
//! 500 files is one batch, and therefore one [`Event`] for the window.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::thread::JoinHandle;

use anyhow::{Context, Result};

use accent_core::index::{Change, Index};
use accent_core::path::parent_dir;
use accent_core::walk::{self, FileKind, ScanOptions};
use accent_core::watch::{VaultEvent, Watcher};

use crate::Event;
use crate::paths::{conflict_original_rel, conflict_pairs};

/// Start the worker for a vault, and hand back the handle its `Drop` joins.
///
/// `watch` is false on Android, where inotify over emulated storage drops events and the app
/// asks for a [`Msg::Rescan`] when it comes back to the foreground instead.
///
/// `stop` is the one thing that reaches this thread while it is walking: the inbox is only read
/// between batches, and a walk of 40 000 files is not one message-loop turn. It holds [`RUN`],
/// [`PAUSE`] or [`CLOSE`]. See [`Worker::reconcile`].
pub(crate) fn spawn(
    root: PathBuf,
    index: Index,
    rx: Receiver<Msg>,
    tx: Sender<Msg>,
    events: Sender<Event>,
    watch: bool,
    stop: Arc<AtomicU8>,
) -> Result<JoinHandle<()>> {
    let worker = Worker {
        root,
        index,
        rx,
        tx,
        events,
        watch,
        watcher: None,
        seen_conflicts: BTreeSet::new(),
        reported: BTreeSet::new(),
        git_dirs: Vec::new(),
        held: Vec::new(),
        stop,
        paused: false,
    };
    std::thread::Builder::new()
        .name("accent-vault".to_string())
        .spawn(move || worker.run())
        .context("spawning the vault worker")
}

/// What `stop` says: walk on.
pub(crate) const RUN: u8 = 0;
/// The user stopped the walk: see [`Worker::paused`].
pub(crate) const PAUSE: u8 = 1;
/// The vault is closing, for good: a walk stops as for a pause, and nothing is set up after it.
pub(crate) const CLOSE: u8 = 2;

/// The worker's inbox. The watcher pushes `Fs`, the public API pushes the rest.
pub(crate) enum Msg {
    Fs(VaultEvent),
    Update {
        rel: String,
        own: bool,
    },
    /// The git directories to watch, as `repos()` last found them. The walk hard-skips `.git`,
    /// so these are never in the index's directory list and the watcher has to be told.
    WatchGit(Vec<PathBuf>),
    /// What search leaves out, and where to say it has been written. See
    /// [`Local::set_excluded`]: the worker holds the only writing connection.
    SetExcluded(Vec<String>, Sender<Result<()>>),
    /// Answered once everything posted before it has reached the index. The one thing the
    /// worker's batching costs a caller is that a write it has just made is not yet readable;
    /// this is how [`Local::settle_index`] waits for it instead of guessing at a delay.
    Settled(Sender<()>),
    Rescan,
    /// Walk again after a [`Local::stop_indexing`](crate::local::Local::stop_indexing), and only
    /// then: while a vault is paused every other reason to rescan is ignored, or the walk the
    /// user just stopped would start again on the next thing the watcher saw.
    Resume,
    Shutdown,
}

/// Owns the writing connection and the watcher. Everything here runs on one thread, so the
/// index has exactly one writer and the UI never waits for it.
struct Worker {
    root: PathBuf,
    index: Index,
    rx: Receiver<Msg>,
    tx: Sender<Msg>,
    events: Sender<Event>,
    /// Whether this vault is watched at all. See [`spawn`].
    watch: bool,
    watcher: Option<Watcher>,
    /// Conflict copies the UI has already been offered, so a rescan never repeats one.
    seen_conflicts: BTreeSet<String>,
    /// Failures the UI has already been told about, so a recurring one is said once. See
    /// [`Worker::fail`].
    reported: BTreeSet<String>,
    /// Every watched repository's git directory, absolute. A change under one of these is news
    /// for the git pane and nothing else: `.git` is not indexed and must never be.
    git_dirs: Vec<PathBuf>,
    /// What the inbox held that a walk read between its batches and could not answer there. See
    /// [`Worker::reconcile`].
    held: Vec<Msg>,
    /// Set by [`Local::stop_indexing`](crate::local::Local::stop_indexing) and by the vault's
    /// `Drop` from another thread, and read inside the walk: the inbox cannot be reached from there.
    stop: Arc<AtomicU8>,
    /// A walk stopped, and nothing but [`Msg::Resume`] may start another. Not persisted, and it
    /// must not be: the index is a diff, so *opening the vault again* is the resume. What a
    /// pause has to survive is this session, where the vault stays open and half-indexed.
    paused: bool,
}

/// What one batch has accumulated: the directories whose children changed, and the paths it took
/// out of the index.
#[derive(Default)]
struct Batch {
    dirs: BTreeSet<String>,
    removed: BTreeSet<String>,
    /// A directory was added, so the watch set — one watch per directory — is short one entry.
    rewatch: bool,
}

impl Worker {
    fn run(mut self) {
        // Watch before walking, so a change made during the first reconcile is not lost between
        // the two.
        //
        // ponytail: on a cold cache the index lists no directories yet, so this first watcher
        // covers the vault root alone and a change made deeper during the initial reconcile is
        // missed until the rebuild below. Build the watch set from the scan result instead of
        // the cache if that ever bites.
        self.rebuild_watcher();
        self.reconcile();

        loop {
            // One `recv` plus the rest of the burst: a Syncthing pull of 500 files is one batch,
            // and therefore one event for the UI. What the last walk held back came first.
            let mut batch = std::mem::take(&mut self.held);
            if batch.is_empty() {
                let Ok(first) = self.rx.recv() else { break };
                batch.push(first);
            }
            batch.extend(self.rx.try_iter());
            let stop = batch.iter().any(|m| matches!(m, Msg::Shutdown));
            self.process(batch);
            if stop {
                break;
            }
        }
    }

    fn process(&mut self, batch: Vec<Msg>) {
        // Taken out first and answered last, whichever way the batch goes below — a rescan is
        // the index brought up to date too, only wholesale. Everything posted before one of
        // these is in the same batch or an earlier one, so answering here means it has landed.
        let (settled, batch): (Vec<Msg>, Vec<Msg>) = batch
            .into_iter()
            .partition(|m| matches!(m, Msg::Settled(_)));
        self.process_batch(batch);
        for msg in settled {
            if let Msg::Settled(reply) = msg {
                let _ = reply.send(());
            }
        }
    }

    fn process_batch(&mut self, batch: Vec<Msg>) {
        // Git first, and before anything else looks at these paths. A `.git` directory is full of
        // children, so `needs_rescan` would read a commit as a whole tree moved in and walk the
        // vault; and `rel` cannot place a submodule's git directory, which lives outside the
        // vault entirely. Taking them out here leaves the rest of the worker exactly as it was.
        let (git, batch): (Vec<Msg>, Vec<Msg>) = batch
            .into_iter()
            .partition(|m| matches!(m, Msg::Fs(VaultEvent::Git(_))));
        if !git.is_empty() {
            self.emit(Event::GitChanged);
        }
        for msg in &batch {
            if let Msg::WatchGit(dirs) = msg
                && *dirs != self.git_dirs
            {
                self.git_dirs = dirs.clone();
                self.rebuild_watcher();
            }
        }
        // Before the rescan test below, which returns early: a caller is waiting for this answer
        // and would otherwise be told the worker had gone. A walk in the same batch costs the
        // order nothing: the rows it adds take their parent directory's flag (`upsert`).
        for msg in &batch {
            if let Msg::SetExcluded(entries, reply) = msg {
                let _ = reply.send(self.index.set_excluded(entries));
            }
        }
        // Before the test below, which refuses every other reason to walk while paused.
        if batch.iter().any(|m| matches!(m, Msg::Resume)) {
            self.paused = false;
        }
        if batch.iter().any(|m| self.needs_rescan(m)) {
            // The walk replaces the index wholesale, but the moves in this batch are still news:
            // a tab open on a path that was renamed under it has to follow.
            for msg in &batch {
                if let Msg::Fs(VaultEvent::Renamed { from, to }) = msg
                    && let (Some(from), Some(to)) = (self.rel(from), self.rel(to))
                {
                    self.emit(Event::FileRenamed { from, to });
                }
            }
            self.reconcile();
            return;
        }
        let mut batched = Batch::default();
        for msg in batch {
            match msg {
                // `Settled` is answered by the caller of this one and never reaches here.
                Msg::Rescan
                | Msg::Resume
                | Msg::Shutdown
                | Msg::WatchGit(_)
                | Msg::SetExcluded(..)
                | Msg::Settled(_) => {}
                Msg::Update { rel, own } => self.update(&rel, own, &mut batched),
                Msg::Fs(ev) => self.apply(ev, &mut batched),
            }
        }
        // A new directory is watched from now on, not from the next reconcile: watches are
        // per-directory, so `mkdir Ideas` followed by a write into it would otherwise be silent.
        if batched.rewatch {
            self.rebuild_watcher();
        }
        if !batched.dirs.is_empty() {
            self.emit(Event::DirsChanged(batched.dirs.into_iter().collect()));
        }
    }

    fn needs_rescan(&self, msg: &Msg) -> bool {
        // A paused vault walks again when the user says so and at no other prompting: a folder
        // moved in, a resumed Android app, a reconnect — every one of them would otherwise
        // restart the walk that was just stopped. What the watcher says about single files is
        // still applied, so the partial index keeps up with what is edited in it.
        if self.paused {
            return false;
        }
        if touches_gitignore(msg) {
            return true;
        }
        match msg {
            Msg::Resume | Msg::Rescan | Msg::Fs(VaultEvent::Rescan) => true,
            // A directory that shows up with children was moved in whole, and inotify reports
            // nothing about what is inside it: only a walk can find those files. A directory that
            // was renamed — by us or in a terminal — is the same story, and worse: the removal of
            // the old name drops the subtree, and indexing the new one adds back the directory row
            // alone, so every note under it would vanish until the next restart. An *empty* new
            // directory needs no walk, only a watch: see `Batch::rewatch`.
            Msg::Fs(VaultEvent::Changed(p)) => *p != self.root && has_children(p),
            Msg::Fs(VaultEvent::Renamed { to, .. }) => *to != self.root && has_children(to),
            Msg::Update { rel, .. } => !rel.is_empty() && has_children(&self.root.join(rel)),
            _ => false,
        }
    }

    /// A full walk, then a fresh watcher because symlinked directories may have come or gone.
    /// [`Event::Reconciled`] is emitted once both are done, so receiving it means the vault is
    /// indexed *and* watched.
    ///
    /// A first walk of a large vault takes seconds, so the inbox is read between its batches.
    /// What does not touch the walk is answered there — git news, the git directories to watch,
    /// the exclusion set — and everything else is held for [`Worker::run`], in the order it came.
    fn reconcile(&mut self) {
        let mut excluded = None;
        let Worker {
            root,
            index,
            rx,
            tx,
            events,
            watcher,
            git_dirs,
            held,
            stop,
            ..
        } = self;
        let stats = index.reconcile_with(
            root,
            &ScanOptions::default(),
            &|| stop.load(Ordering::Relaxed) != RUN,
            |index, p| {
                let _ = events.send(Event::Progress(p));
                let mut git = false;
                for msg in rx.try_iter() {
                    match msg {
                        Msg::Fs(VaultEvent::Git(_)) => git = true,
                        Msg::WatchGit(dirs) => {
                            if dirs != *git_dirs {
                                *git_dirs = dirs;
                                // A failure is left to the rebuild after the walk, which reports it.
                                if let Err(e) = watch(watcher, index, root, git_dirs, tx) {
                                    tracing::warn!("watching the vault: {e:#}");
                                }
                            }
                        }
                        Msg::SetExcluded(entries, reply) => {
                            let _ = reply.send(index.set_excluded(&entries));
                            excluded = Some(entries);
                        }
                        other => held.push(other),
                    }
                }
                if git {
                    let _ = events.send(Event::GitChanged);
                }
            },
        );
        // A pause has done its work either way, and one that arrived as the walk ended must not
        // be waiting for the next one. A close stays: nothing walks again.
        let _ = self
            .stop
            .compare_exchange(PAUSE, RUN, Ordering::Relaxed, Ordering::Relaxed);
        self.paused = matches!(&stats, Ok(s) if s.stopped);
        // A set written mid-walk marked the rows that were there, and what the walk added after
        // it inherited its parent's flag — but a directory the set itself names, added after it,
        // had no marked parent to inherit from, so the set is applied once more.
        if let Some(entries) = excluded
            && let Err(e) = self.index.set_excluded(&entries)
        {
            self.fail("recording the exclusion set", e);
        }
        // A closing vault needs no watcher, and a walk it stopped is no pause to report: the next
        // open carries on by itself.
        if self.stop.load(Ordering::Relaxed) == CLOSE {
            return;
        }
        self.rebuild_watcher();
        match stats {
            Ok(stats) => {
                // Emitted for a stopped walk too, carrying `stopped`: it is what tells the window
                // it is looking at a partial index rather than a finished one.
                self.emit(Event::Reconciled(stats));
                self.emit_conflicts();
            }
            Err(e) => self.fail("reconciling the vault", e),
        }
    }

    /// Conflicts the walk found. Syncthing usually leaves them while accent is closed, so no
    /// watcher event will ever announce them and the resolve dialog would never be offered.
    fn emit_conflicts(&mut self) {
        match conflict_pairs(&self.index) {
            Ok(pairs) => {
                for (original, conflict) in pairs {
                    if self.seen_conflicts.insert(conflict.clone()) {
                        self.emit(Event::Conflict { original, conflict });
                    }
                }
            }
            Err(e) => self.fail("listing the conflicts in the vault", e),
        }
    }

    fn rebuild_watcher(&mut self) {
        if !self.watch {
            return;
        }
        if let Err(e) = watch(
            &mut self.watcher,
            &self.index,
            &self.root,
            &self.git_dirs,
            &self.tx,
        ) {
            self.fail("watching the vault", e);
        }
    }

    fn apply(&mut self, ev: VaultEvent, b: &mut Batch) {
        match ev {
            // Both handled before the batch is walked: a rescan in `needs_rescan`, a git change
            // in the partition at the top of `process`.
            VaultEvent::Rescan | VaultEvent::Git(_) => {}
            VaultEvent::Changed(p) => {
                if let Some(rel) = self.rel(&p) {
                    self.update(&rel, false, b);
                }
            }
            VaultEvent::Removed(p) => {
                if let Some(rel) = self.rel(&p) {
                    // A temp file renamed over a note is reported as Remove + Create once the
                    // debouncer has that inode cached, which is what an external `sed -i` or a
                    // second accent looks like. Believe a removal only when the path is really
                    // gone, or the tab the user is typing in closes under them.
                    if matches!(walk::stat_one(&self.root, &rel), Ok(Some(_))) {
                        self.update(&rel, false, b);
                    } else {
                        self.remove(&rel, b);
                    }
                }
            }
            VaultEvent::Renamed { from, to } => {
                if let (Some(from), Some(to)) = (self.rel(&from), self.rel(&to)) {
                    if let Err(e) = self.index.remove_file_batched(&from) {
                        self.fail(&format!("removing {from}"), e);
                    }
                    b.removed.insert(from.clone());
                    self.update(&to, false, b);
                    b.dirs.insert(parent_dir(&from).to_string());
                    b.dirs.insert(parent_dir(&to).to_string());
                    self.emit(Event::FileRenamed { from, to });
                }
            }
            VaultEvent::ConflictAppeared(p) => {
                if let Some(conflict) = self.rel(&p) {
                    self.update(&conflict, false, b);
                    if let Some(original) = conflict_original_rel(&conflict)
                        && self.seen_conflicts.insert(conflict.clone())
                    {
                        self.emit(Event::Conflict { original, conflict });
                    }
                }
            }
        }
    }

    /// Take a path, and everything under it, out of the index.
    fn remove(&mut self, rel: &str, b: &mut Batch) {
        if let Err(e) = self.index.remove_file_batched(rel) {
            self.fail(&format!("removing {rel}"), e);
        }
        b.removed.insert(rel.to_string());
        b.dirs.insert(parent_dir(rel).to_string());
        self.seen_conflicts.remove(rel);
        self.emit(Event::FileRemoved(rel.to_string()));
    }

    /// Bring one path in line with the disk. `own` marks a change we made ourselves, which must
    /// never come back to the UI as "someone else edited this note".
    fn update(&mut self, rel: &str, own: bool, b: &mut Batch) {
        if rel.is_empty() {
            return;
        }
        // The links the file holds and the ones its names answer to are resolved as it goes, in a
        // few index lookups: a pass over every link is 0.1–0.35 s a batch at `make vault`.
        match self.index.update_file(&self.root, rel) {
            Ok(Change::Added(kind)) => {
                b.dirs.insert(parent_dir(rel).to_string());
                b.rewatch |= kind == FileKind::Dir;
                // A path this batch removed and is seeing again was rewritten, not created:
                // whoever has it open has to reload it.
                if kind != FileKind::Dir && !own && b.removed.remove(rel) {
                    self.emit(Event::FileChanged(rel.to_string()));
                }
            }
            Ok(Change::Removed) => {
                b.dirs.insert(parent_dir(rel).to_string());
                b.removed.insert(rel.to_string());
            }
            Ok(Change::Updated(kind)) => {
                // Every kind but a directory reports: a PDF rebuilt by a tool or a source file
                // edited in another editor has to refresh in the UI just like a note does.
                if kind != FileKind::Dir && !own {
                    self.emit(Event::FileChanged(rel.to_string()));
                }
            }
            // `Unchanged` is the watcher echoing our own save back at us.
            Ok(Change::Unchanged | Change::Ignored) => {}
            Err(e) => self.fail(&format!("indexing {rel}"), e),
        }
    }

    /// Watcher paths are absolute. Map one back into the vault: the watch set is built from the
    /// vault's own paths, so an event under a linked-in directory arrives spelled through the
    /// link.
    fn rel(&self, abs: &Path) -> Option<String> {
        let mapped = abs.strip_prefix(&self.root).ok()?;
        match mapped.to_str() {
            Some(rel) => Some(rel.to_string()),
            None => {
                tracing::warn!(path = %abs.display(), "dropping a non-UTF-8 path");
                None
            }
        }
    }

    /// Never kill the thread over one bad file: log every failure, tell the UI about each
    /// distinct one once, carry on.
    ///
    /// Everything reported here is index maintenance the user cannot act on, and it is driven by
    /// their typing: an autosave that failed to index raises the same message on the next
    /// keystroke, and the next. DESIGN.md's toast rule is "a thing that happened and is over",
    /// which a failure that repeats per save is not — the same reason a synced vault's conflict
    /// copies became one count instead of a wall of toasts.
    fn fail(&mut self, what: &str, e: impl std::fmt::Display) {
        let message = format!("{what}: {e:#}");
        tracing::warn!("{message}");
        if self.reported.insert(message.clone()) {
            self.emit(Event::Error(message));
        }
    }

    fn emit(&self, event: Event) {
        let _ = self.events.send(event);
    }
}

/// Watch the directories the index holds, plus the git directories: the watcher there is, given
/// the new set ([`Watcher::set_dirs`], which keeps what it has not reported yet), or a new one.
fn watch(
    watcher: &mut Option<Watcher>,
    index: &Index,
    root: &Path,
    git_dirs: &[PathBuf],
    tx: &Sender<Msg>,
) -> Result<()> {
    // The watch set is what the walk kept, one watch per directory: a `.venv` the walk refused
    // must not come back in through a recursive watch on the root.
    let mut dirs = index.dirs(root).unwrap_or_else(|e| {
        tracing::warn!("listing the directories to watch: {e:#}");
        Vec::new()
    });
    // A repository's own directory and the branch tips inside it. Two watches per repo is
    // what tells the git pane a commit happened in a terminal; `notify` refuses a path that
    // does not exist, so a repository removed under us costs a warning, not the watch set.
    for git_dir in git_dirs {
        dirs.push(git_dir.clone());
        dirs.push(git_dir.join("refs/heads"));
    }
    if let Some(w) = watcher
        && w.set_dirs(&dirs)
    {
        return Ok(());
    }
    // Dropped first: two registrations on one tree would double every event.
    *watcher = None;
    let tx = tx.clone();
    *watcher = Some(Watcher::new(root, &dirs, move |e| {
        let _ = tx.send(Msg::Fs(e));
    })?);
    Ok(())
}

/// A directory with something in it, which is what a moved-in tree looks like.
fn has_children(path: &Path) -> bool {
    std::fs::read_dir(path).is_ok_and(|mut d| d.next().is_some())
}

/// News about a `.gitignore`, wherever in the vault it sits.
///
/// One of these decides which *directories* the walk enters (`walk::stat_one`), so editing one
/// changes the shape of the index rather than the contents of a file: adding `mlruns/` has to
/// drop the tree the last walk indexed, and removing it has to walk the tree left lazy. Only a
/// walk can do either, and there is no walk of one subtree — [`Index::reconcile_with`] is the
/// whole vault or nothing — so this asks for the whole thing. A `.gitignore` is edited about as
/// often as a preference, which is what makes that affordable.
fn touches_gitignore(msg: &Msg) -> bool {
    let named = |p: &Path| p.file_name().is_some_and(|n| n == ".gitignore");
    match msg {
        Msg::Fs(VaultEvent::Changed(p) | VaultEvent::Removed(p)) => named(p),
        Msg::Fs(VaultEvent::Renamed { from, to }) => named(from) || named(to),
        // Our own save of one, which the watcher reports as well; whichever arrives first walks.
        Msg::Update { rel, .. } => named(Path::new(rel)),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{Msg, RUN, spawn};
    use crate::tests::*;
    use crate::{Etag, Event, VaultConfig, fs};
    use accent_core::index::Index;
    use accent_core::watch::VaultEvent;
    use std::sync::Arc;
    use std::sync::atomic::AtomicU8;
    use std::sync::mpsc::{RecvTimeoutError, channel};
    use std::time::{Duration, Instant};

    /// Stop is a pause the worker remembers: what it wrote stays, every other reason to walk is
    /// refused until someone asks, and the walk that follows finishes the job. Enough files that
    /// the flag, set the moment `open` returns, reaches a walk that is still going.
    #[test]
    fn a_stopped_vault_keeps_its_index_and_waits_to_be_resumed() {
        let root = tempfile::tempdir().unwrap();
        let notes = 1000;
        for i in 0..notes {
            std::fs::write(root.path().join(format!("n{i}.md")), "body").unwrap();
        }
        let cache = tempfile::tempdir().unwrap();
        let (vault, events) = crate::Vault::open_at(
            root.path(),
            &cache.path().join("index.db"),
            VaultConfig::default(),
        )
        .unwrap();
        vault.stop_indexing().unwrap();

        let first = wait_for(&events, |e| matches!(e, Event::Reconciled(_)), BUDGET);
        let Some(Event::Reconciled(stats)) = first else {
            panic!("no reconcile: {first:?}");
        };
        assert!(stats.stopped, "the first walk was stopped: {stats:?}");
        let partial = vault.file_paths(false).unwrap().len();
        assert!(partial < notes, "a partial index, {partial} of {notes}");

        // A rescan is what a folder moved in, an Android resume and a reconnect all come down to.
        // None of them may restart the walk the user has just stopped.
        vault.rescan().unwrap();
        assert!(
            wait_for(
                &events,
                |e| matches!(e, Event::Reconciled(_)),
                Duration::from_millis(500),
            )
            .is_none(),
            "a paused vault walked again on a rescan"
        );

        vault.resume_indexing().unwrap();
        let done = wait_for(&events, |e| matches!(e, Event::Reconciled(_)), BUDGET);
        let Some(Event::Reconciled(stats)) = done else {
            panic!("no reconcile after resume: {done:?}");
        };
        assert!(!stats.stopped);
        assert_eq!(vault.file_paths(false).unwrap().len(), notes);
    }

    /// Closing a vault mid-walk stops the walk rather than waiting for it, and the next open
    /// carries on by itself: a close is no pause.
    #[test]
    fn closing_a_vault_mid_walk_stops_the_walk_and_the_next_open_finishes_it() {
        let root = tempfile::tempdir().unwrap();
        let notes = 1000;
        for i in 0..notes {
            std::fs::write(root.path().join(format!("n{i}.md")), "body").unwrap();
        }
        let cache = tempfile::tempdir().unwrap();
        let db = cache.path().join("index.db");
        let open = || crate::Vault::open_at(root.path(), &db, VaultConfig::default()).unwrap();

        drop(open());
        let partial = Index::open(&db).unwrap().file_paths(false).unwrap().len();
        assert!(partial < notes, "the close waited for the walk");

        let (vault, events) = open();
        let done = wait_for(&events, |e| matches!(e, Event::Reconciled(_)), BUDGET);
        let Some(Event::Reconciled(stats)) = done else {
            panic!("no reconcile after the reopen: {done:?}");
        };
        assert!(!stats.stopped);
        assert_eq!(vault.file_paths(false).unwrap().len(), notes);
    }

    /// Depends on real inotify events.
    #[test]
    fn external_write_emits_file_changed_and_becomes_searchable() {
        let f = Fixture::open(VaultConfig::default());
        f.write("Note.md", "hello");
        assert!(f.wait(|e| matches!(e, Event::DirsChanged(_))).is_some());

        f.write("Note.md", "kumquat harvest");

        assert!(
            f.wait(|e| matches!(e, Event::FileChanged(p) if p == "Note.md"))
                .is_some(),
            "an external edit must reach the UI"
        );
        assert!(poll_until(
            || !f.vault.search("kumquat", 10, false).unwrap().is_empty(),
            BUDGET
        ));
    }

    /// Non-markdown files are stat-only rows in the index, but an external edit still has to
    /// reach whoever has the file open. Depends on real inotify events.
    #[test]
    fn external_write_to_a_code_file_emits_file_changed() {
        let f = Fixture::open(VaultConfig::default());
        f.write("tool.py", "print(1)\n");
        assert!(f.wait(|e| matches!(e, Event::DirsChanged(_))).is_some());

        f.write("tool.py", "print(2)\n");

        assert!(
            f.wait(|e| matches!(e, Event::FileChanged(p) if p == "tool.py"))
                .is_some(),
            "an external edit to a source file must reach the UI"
        );
    }

    /// The watch set is one watch per directory, built from the index, so a subdirectory that was
    /// already there when the vault opened has to be in it — that is the everyday case of editing
    /// a note in another editor.
    #[test]
    fn an_edit_in_a_pre_existing_subdirectory_reaches_the_ui() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("Projects")).unwrap();
        std::fs::write(root.path().join("Projects/Plan.md"), "one").unwrap();
        let f = Fixture::open_dir(root, VaultConfig::default());

        f.write("Projects/Plan.md", "two");

        assert!(
            f.wait(|e| matches!(e, Event::FileChanged(p) if p == "Projects/Plan.md"))
                .is_some(),
            "an edit inside an indexed subdirectory must reach the UI"
        );
    }

    /// A linked-in folder is watched through its vault path — `inotify_add_watch` resolves the
    /// link — which is what keeps "symlinked folders handled" true now that the watch set is a
    /// list of directories rather than a recursive watch plus the resolved targets.
    #[test]
    fn an_edit_inside_a_symlinked_directory_reaches_the_ui() {
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("Ext.md"), "one").unwrap();
        let root = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("linked")).unwrap();
        let f = Fixture::open_dir(root, VaultConfig::default());

        std::fs::write(outside.path().join("Ext.md"), "two").unwrap();

        assert!(
            f.wait(|e| matches!(e, Event::FileChanged(p) if p == "linked/Ext.md"))
                .is_some(),
            "an edit in a linked-in folder must reach the UI"
        );
    }

    #[test]
    fn own_save_updates_the_index_without_a_file_changed_event() {
        let f = Fixture::open(VaultConfig::default());
        f.write("Note.md", "hello");
        assert!(f.wait(|e| matches!(e, Event::DirsChanged(_))).is_some());

        let (_, etag) = f.vault.read("Note.md").unwrap();
        let saved = f
            .vault
            .save("Note.md", "quokka census", Some(etag))
            .unwrap();
        assert_eq!(saved, Etag::of(&f.vault.root().join("Note.md")).unwrap());

        assert!(poll_until(
            || !f.vault.search("quokka", 10, false).unwrap().is_empty(),
            BUDGET
        ));
        // Well past the watcher's 300 ms debounce, so the echo of our own save has been and gone.
        assert!(
            wait_for(
                &f.events,
                |e| matches!(e, Event::FileChanged(p) if p == "Note.md"),
                Duration::from_secs(2),
            )
            .is_none(),
            "our own save came back as someone else's edit"
        );
    }

    /// Depends on real inotify events.
    #[test]
    fn a_new_file_in_a_subdirectory_reports_its_parent_dir() {
        let f = Fixture::open(VaultConfig::default());
        std::fs::create_dir(f.vault.root().join("sub")).unwrap();
        assert!(f.wait(|e| matches!(e, Event::DirsChanged(_))).is_some());

        f.write("sub/New.md", "fresh");

        match f
            .wait(|e| matches!(e, Event::DirsChanged(d) if d.contains(&"sub".to_string())))
            .expect("no DirsChanged for the parent directory")
        {
            Event::DirsChanged(dirs) => assert_eq!(dirs, ["sub"]),
            other => panic!("expected DirsChanged, got {other:?}"),
        }
    }

    /// Depends on real inotify events.
    #[test]
    fn a_conflict_copy_emits_conflict_and_stays_out_of_search() {
        let f = Fixture::open(VaultConfig::default());
        f.write("Note.md", "mine");
        assert!(f.wait(|e| matches!(e, Event::DirsChanged(_))).is_some());

        f.write(CONFLICT, "wombat census");

        match f
            .wait(|e| matches!(e, Event::Conflict { .. }))
            .expect("no Conflict event")
        {
            Event::Conflict { original, conflict } => {
                assert_eq!(original, "Note.md");
                assert_eq!(conflict, CONFLICT);
            }
            other => panic!("expected Conflict, got {other:?}"),
        }
        assert!(poll_until(
            || f.vault.conflicts().unwrap() == [("Note.md".to_string(), CONFLICT.to_string())],
            BUDGET
        ));
        assert_eq!(f.vault.conflicts_of("Note.md").unwrap(), [CONFLICT]);
        assert!(f.vault.conflicts_of("Other.md").unwrap().is_empty());
        assert!(
            f.vault.search("wombat", 10, false).unwrap().is_empty(),
            "a conflict copy is never a note"
        );
    }

    /// notify reports a temp-plus-rename over a note it has the inode of as `Remove` + `Create`,
    /// which is what an external `sed -i` (or another accent) looks like. A note that is still
    /// on disk must never reach the UI as a removal, or the app closes the tab it is open in.
    #[test]
    fn an_external_atomic_rewrite_is_a_change_not_a_removal() {
        let f = Fixture::open(VaultConfig::default());
        f.write("Note.md", "one\n");
        assert!(f.wait(|e| matches!(e, Event::DirsChanged(_))).is_some());

        let path = f.vault.root().join("Note.md");
        let _ = std::fs::read_to_string(&path).unwrap();
        fs::write_note(&path, "someone else wrote this\n", None).unwrap();

        let (mut removed, mut changed) = (false, false);
        let deadline = Instant::now() + BUDGET;
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match f.events.recv_timeout(left) {
                Ok(Event::FileRemoved(p)) if p == "Note.md" => removed = true,
                Ok(Event::FileChanged(p)) if p == "Note.md" => {
                    changed = true;
                    break;
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
        assert!(
            !removed,
            "a note that is still on disk was reported removed"
        );
        assert!(changed, "the external rewrite never reached the UI");
    }

    /// A Syncthing pull is one batch of files; the links in all of them still have to resolve.
    #[test]
    fn a_burst_of_writes_resolves_the_links_in_all_of_them() {
        let f = Fixture::open(VaultConfig::default());
        for i in 0..10 {
            f.write(&format!("n{i}.md"), "see [[Target]]\n");
        }
        f.write("Target.md", "here\n");

        assert!(poll_until(
            || f.vault.backlinks("Target.md").unwrap().len() == 10,
            BUDGET
        ));
    }

    /// A vault opened on `files`, each `(rel, text)`.
    fn open_with(files: &[(&str, &str)]) -> Fixture {
        let root = tempfile::tempdir().unwrap();
        for (rel, text) in files {
            std::fs::write(root.path().join(rel), text).unwrap();
        }
        Fixture::open_dir(root, VaultConfig::default())
    }

    /// A batch resolves only what its files can have changed, so each kind of change is pinned
    /// here: an edit moves the note's own links.
    #[test]
    fn an_edit_points_its_links_at_their_new_targets() {
        let f = open_with(&[("A.md", "a\n"), ("B.md", "b\n"), ("n.md", "see [[A]]\n")]);
        assert_eq!(f.vault.backlinks("A.md").unwrap().len(), 1);

        f.vault.save("n.md", "see [[B]]\n", None).unwrap();

        assert!(poll_until(
            || f.vault.backlinks("B.md").unwrap().len() == 1,
            BUDGET
        ));
        assert!(f.vault.backlinks("A.md").unwrap().is_empty());
    }

    /// A new note takes the links other notes wrote to it before it existed.
    #[test]
    fn a_new_note_takes_the_links_that_were_waiting_for_it() {
        let f = open_with(&[("n.md", "see [[Later]]\n")]);
        assert_eq!(f.vault.missing_notes().unwrap(), ["Later.md"]);

        f.vault.save("Later.md", "here\n", None).unwrap();

        assert!(poll_until(
            || f.vault.backlinks("Later.md").unwrap().len() == 1,
            BUDGET
        ));
        assert!(f.vault.missing_notes().unwrap().is_empty());
    }

    /// A deleted note leaves the links to it dangling, offered again as a note to write.
    #[test]
    fn a_deleted_note_leaves_the_links_to_it_dangling() {
        let f = open_with(&[("Gone.md", "soon\n"), ("n.md", "see [[Gone]]\n")]);
        assert!(f.vault.missing_notes().unwrap().is_empty());

        f.vault.delete("Gone.md").unwrap();

        assert!(poll_until(
            || f.vault.missing_notes().unwrap() == ["Gone.md"],
            BUDGET
        ));
    }

    /// The usual case: Syncthing left the conflict while accent was closed, so no watcher event
    /// will ever announce it.
    #[test]
    fn conflicts_already_in_the_vault_reach_the_ui_once() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("Note.md"), "mine\n").unwrap();
        std::fs::write(root.path().join(CONFLICT), "theirs\n").unwrap();

        let f = Fixture::open_dir(root, VaultConfig::default());

        match f
            .wait(|e| matches!(e, Event::Conflict { .. }))
            .expect("no Conflict event for a conflict that was already there")
        {
            Event::Conflict { original, conflict } => {
                assert_eq!(
                    (original.as_str(), conflict.as_str()),
                    ("Note.md", CONFLICT)
                );
            }
            other => panic!("expected Conflict, got {other:?}"),
        }

        // A rescan finds the same pair; the UI must not be offered it twice.
        f.vault.rescan().unwrap();
        assert!(f.wait(|e| matches!(e, Event::Reconciled(_))).is_some());
        assert!(
            wait_for(
                &f.events,
                |e| matches!(e, Event::Conflict { .. }),
                Duration::from_secs(1)
            )
            .is_none(),
            "the same conflict was offered twice"
        );
    }

    #[test]
    fn dropping_the_vault_stops_the_worker() {
        let f = Fixture::open(VaultConfig::default());
        let Fixture {
            vault,
            events,
            root: _root,
            _cache,
        } = f;
        drop(vault);

        // The worker holds the only sender: a disconnect proves the thread is gone.
        let deadline = Instant::now() + BUDGET;
        loop {
            match events.recv_timeout(Duration::from_millis(200)) {
                Ok(_) => {}
                Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => {
                    assert!(Instant::now() < deadline, "the worker outlived the vault")
                }
            }
        }
    }

    /// A commit made anywhere but in the app — a shell, another editor, a script — has to reach
    /// the git pane, and `.git` is the one tree the walk deliberately never enters. The watcher
    /// takes the repositories from `repos()` and reports them as their own kind of event, so a
    /// commit never looks like a hundred files appearing in the vault.
    #[test]
    fn a_commit_outside_the_app_reports_as_a_git_change() {
        if std::process::Command::new("git")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }
        let f = Fixture::open(VaultConfig::default());
        let root = f.vault.root().to_path_buf();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(args)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}");
        };
        git(&["init", "-q", "-b", "main"]);
        f.write("a.md", "one\n");
        f.vault.rescan().unwrap();
        assert!(f.wait(|e| matches!(e, Event::Reconciled(_))).is_some());

        // Asking for the repositories is what puts `.git` in the watch set.
        assert_eq!(f.vault.repos().unwrap().len(), 1);
        git(&["add", "a.md"]);
        git(&["commit", "-qm", "one"]);

        assert!(
            f.wait(|e| matches!(e, Event::GitChanged)).is_some(),
            "a commit has to reach the pane"
        );
    }

    /// A `.gitignore` decides which directories the walk enters, so editing one has to walk the
    /// vault again: the tree it starts ignoring leaves the index, and the one it stops ignoring
    /// comes back. Asked of the index rather than of an event, because a reconcile the writes
    /// themselves set off would answer a wait for one.
    #[test]
    fn editing_a_gitignore_walks_the_vault_again() {
        let f = Fixture::open(VaultConfig::default());
        let run = "mlruns/run.md".to_string();
        let indexed = || f.vault.file_paths(false).unwrap().contains(&run);
        f.write("keep.md", "keep\n");
        f.write(&run, "run\n");
        f.vault.rescan().unwrap();
        assert!(poll_until(indexed, BUDGET), "the walk missed the tree");

        f.write(".gitignore", "mlruns/\n");
        assert!(
            poll_until(|| !indexed(), BUDGET),
            "a newly ignored tree stayed in the index"
        );

        f.write(".gitignore", "# nothing\n");
        assert!(
            poll_until(indexed, BUDGET),
            "the tree stayed lazy after it stopped being ignored"
        );
    }

    /// A new watch set must not lose what the old one had seen and not yet reported. The window's
    /// first `repos()` lands about a second after a git vault opens and changes the set, and a
    /// note written just before it was never indexed: the debouncer holding its event for 300 ms
    /// was dropped with the old watcher.
    #[test]
    fn a_write_just_before_the_watch_set_changes_is_still_indexed() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        std::fs::create_dir_all(root_path.join(".git/refs/heads")).unwrap();
        let cache = tempfile::tempdir().unwrap();
        let db = cache.path().join("index.db");
        let (tx, rx) = channel();
        let (events, event_rx) = channel();
        let worker = spawn(
            root_path.clone(),
            Index::open(&db).unwrap(),
            rx,
            tx.clone(),
            events,
            true,
            Arc::new(AtomicU8::new(RUN)),
        )
        .unwrap();
        assert!(wait_for(&event_rx, |e| matches!(e, Event::Reconciled(_)), BUDGET).is_some());

        std::fs::write(root_path.join("late.md"), "late\n").unwrap();
        tx.send(Msg::WatchGit(vec![root_path.join(".git")]))
            .unwrap();

        let indexed = || {
            Index::open(&db)
                .unwrap()
                .file_paths(false)
                .unwrap()
                .contains(&"late.md".to_string())
        };
        assert!(poll_until(indexed, BUDGET), "the write was lost");
        tx.send(Msg::Shutdown).unwrap();
        worker.join().unwrap();
    }

    /// A first index of a large vault takes seconds, and some of what reaches the inbox meanwhile
    /// cannot wait that long: a commit made in a terminal, and a `set_excluded` whose caller is
    /// blocked on the answer — over ssh, against a 10 s deadline. Both are posted before the
    /// worker starts, so they are certainly read while its first walk is in progress.
    #[test]
    fn the_inbox_is_read_between_the_batches_of_a_walk() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        std::fs::write(root_path.join("keep.txt"), "keep").unwrap();
        std::fs::create_dir(root_path.join("build")).unwrap();
        std::fs::write(root_path.join("build/out.txt"), "out").unwrap();
        let cache = tempfile::tempdir().unwrap();
        let db = cache.path().join("index.db");

        let (tx, rx) = channel();
        let (events, event_rx) = channel();
        let (reply, answer) = channel();
        tx.send(Msg::SetExcluded(vec!["build/".to_string()], reply))
            .unwrap();
        tx.send(Msg::Fs(VaultEvent::Git(root_path.join(".git"))))
            .unwrap();
        let worker = spawn(
            root_path.clone(),
            Index::open(&db).unwrap(),
            rx,
            tx.clone(),
            events,
            true,
            Arc::new(AtomicU8::new(RUN)),
        )
        .unwrap();

        let first = wait_for(
            &event_rx,
            |e| matches!(e, Event::GitChanged | Event::Reconciled(_)),
            BUDGET,
        );
        assert!(
            matches!(first, Some(Event::GitChanged)),
            "the git change waited for the walk: {first:?}"
        );
        assert!(
            matches!(answer.try_recv(), Ok(Ok(()))),
            "set_excluded waited for the walk"
        );

        // Written before the walk had added a row, so what left `build/out.txt` out is the set
        // being written again once the walk was over.
        assert!(wait_for(&event_rx, |e| matches!(e, Event::Reconciled(_)), BUDGET).is_some());
        assert_eq!(
            Index::open(&db).unwrap().file_paths(false).unwrap(),
            ["keep.txt"]
        );

        tx.send(Msg::Shutdown).unwrap();
        worker.join().unwrap();
    }
}
