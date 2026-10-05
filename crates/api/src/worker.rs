//! The one thread that writes the index and owns the watcher.
//!
//! Everything here runs on it, so the index has exactly one writer and the UI never waits for
//! one. The inbox is drained in bursts rather than one message at a time: a Syncthing pull of
//! 500 files is one batch, and therefore one [`Event`] for the window.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

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
        unindexed: BTreeSet::new(),
        held: Vec::new(),
        stop,
        paused: false,
        next_walk: Instant::now(),
        due: None,
    };
    std::thread::Builder::new()
        .name("accent-vault".to_string())
        .spawn(move || worker.run())
        .context("spawning the vault worker")
}

/// How long after a walk ends the next one the watcher's news asks for waits: a `.gitignore`
/// saved, which an editor autosaves a second after each pause in the typing, or events the
/// watcher lost. The first ask past it walks at once; every ask inside it is one walk at its end.
const WALK_FLOOR: Duration = Duration::from_secs(2);

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
    /// Folders the index does not walk to start (`true`) or stop watching. See
    /// [`Local::watch_unindexed`](crate::local::Local::watch_unindexed).
    WatchUnindexed(Vec<String>, bool),
    /// What search leaves out, and where to say it has been written. See
    /// [`Local::set_excluded`]: the worker holds the only writing connection.
    SetExcluded(Vec<String>, Sender<Result<()>>),
    /// Answered once everything posted before it has reached the index. The one thing the
    /// worker's batching costs a caller is that a write it has just made is not yet readable;
    /// this is how [`Local::settle_index`] waits for it instead of guessing at a delay.
    Settled(Sender<()>),
    /// Walk this folder, `""` being the whole vault.
    Rescan(String),
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
    /// The folders the index does not walk that the window lists, vault-relative: each watched
    /// one level deep, and what happens in one re-lists it without touching the index.
    unindexed: BTreeSet<String>,
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
    /// When a walk the watcher's news asks for may start: [`WALK_FLOOR`] after the last one.
    next_walk: Instant,
    /// The folder of one asked for before then, which comes as a [`Msg::Rescan`] at
    /// [`Worker::next_walk`]: the folder holding every folder asked for meanwhile.
    due: Option<String>,
}

/// What one batch has accumulated: the directories whose children changed, and the paths it took
/// out of the index.
#[derive(Default)]
struct Batch {
    dirs: BTreeSet<String>,
    removed: BTreeSet<String>,
    /// A directory was added, so the watch set — one watch per directory — is short one entry.
    rewatch: bool,
    /// The directories added, whose watches start at the end of the batch: what is written into
    /// one before then reaches no watch, so whatever one holds by then is walked in.
    made: Vec<String>,
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
        self.reconcile("");

        loop {
            // One `recv` plus the rest of the burst: a Syncthing pull of 500 files is one batch,
            // and therefore one event for the UI. What the last walk held back came first.
            let mut batch = std::mem::take(&mut self.held);
            if batch.is_empty() {
                let wait = self.next_walk.saturating_duration_since(Instant::now());
                let first = match self.due.is_some() {
                    true => match self.rx.recv_timeout(wait) {
                        Ok(msg) => msg,
                        // Cleared here as well as by the walk: a paused vault drops the rescan.
                        Err(RecvTimeoutError::Timeout) => {
                            Msg::Rescan(self.due.take().unwrap_or_default())
                        }
                        Err(RecvTimeoutError::Disconnected) => break,
                    },
                    false => match self.rx.recv() {
                        Ok(msg) => msg,
                        Err(_) => break,
                    },
                };
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
        // children, so `walk_scope` would read a commit as a whole tree moved in and walk the
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
        let mut rewatch = false;
        for msg in &batch {
            if let Msg::WatchUnindexed(dirs, on) = msg {
                for dir in dirs {
                    rewatch |= match on {
                        // The root and every folder the walk entered are watched already, and
                        // their news is the index's: a tab asks for its file's folder without
                        // knowing which kind it is.
                        true if dir.is_empty()
                            || matches!(self.index.get_file(dir), Ok(Some(_))) =>
                        {
                            false
                        }
                        true => self.unindexed.insert(dir.clone()),
                        false => self.unindexed.remove(dir),
                    };
                }
            }
        }
        // A removed directory took its watch with it, so the set must stop naming it: then the
        // rebuild that brings it back — a new directory, a walk — watches it again.
        for msg in &batch {
            if let Msg::Fs(VaultEvent::Removed(p) | VaultEvent::Renamed { from: p, .. }) = msg
                && let Some(watcher) = &mut self.watcher
            {
                watcher.forget(p);
            }
        }
        // News from inside an unindexed folder is that folder's listing and nothing else: kept
        // from the index, and from the rescan test below — a directory moved into a build output
        // is no reason to walk the vault.
        let mut listed = BTreeSet::new();
        let batch: Vec<Msg> = batch
            .into_iter()
            .filter(|m| !self.unindexed_news(m, &mut listed, &mut rewatch))
            .collect();
        if rewatch {
            self.rebuild_watcher();
        }
        if !listed.is_empty() {
            self.emit(Event::UnindexedChanged(listed.into_iter().collect()));
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
        // What only the watcher's news asks for waits out the floor, the batch meanwhile applied a
        // file at a time like any other — the `.gitignore` itself among them.
        let walk = batch
            .iter()
            .filter_map(|m| self.walk_scope(m))
            .reduce(|a, b| common_dir(&a, &b).to_string());
        let mut walk_after = None;
        if let Some(dir) = walk {
            if Instant::now() < self.next_walk
                && batch.iter().all(|m| {
                    self.walk_scope(m).is_none() || self.gitignore_dir(m).is_some() || lost_news(m)
                })
            {
                self.due = Some(match self.due.take() {
                    Some(due) => common_dir(&due, &dir).to_string(),
                    None => dir,
                });
            } else if dir.is_empty() {
                // The walk replaces the index wholesale, but the moves in this batch are still
                // news: a tab open on a path that was renamed under it has to follow.
                for msg in &batch {
                    if let Msg::Fs(VaultEvent::Renamed { from, to }) = msg
                        && let (Some(from), Some(to)) = (self.rel(from), self.rel(to))
                    {
                        self.emit(Event::FileRenamed { from, to });
                    }
                }
                self.reconcile("");
                return;
            } else {
                // A folder's walk leaves the rest of the vault to the batch, applied first.
                walk_after = Some(dir);
            }
        }
        let mut batched = Batch::default();
        for msg in batch {
            match msg {
                // `Settled` is answered by the caller of this one and never reaches here.
                Msg::Rescan(_)
                | Msg::Resume
                | Msg::Shutdown
                | Msg::WatchGit(_)
                | Msg::WatchUnindexed(..)
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
        if let Some(dir) = batched
            .made
            .iter()
            .filter(|rel| has_children(&self.root.join(rel)))
            .map(String::as_str)
            .reduce(common_dir)
        {
            let _ = self.tx.send(Msg::Rescan(dir.to_string()));
        }
        if !batched.dirs.is_empty() {
            self.emit(Event::DirsChanged(batched.dirs.into_iter().collect()));
        }
        if let Some(dir) = walk_after {
            self.reconcile(&dir);
        }
    }

    /// The folder `msg` asks to walk, `""` being the whole vault, or `None` for no walk.
    fn walk_scope(&self, msg: &Msg) -> Option<String> {
        // A paused vault walks again when the user says so and at no other prompting: a folder
        // moved in, a resumed Android app, a reconnect — every one of them would otherwise
        // restart the walk that was just stopped. What the watcher says about single files is
        // still applied, so the partial index keeps up with what is edited in it.
        if self.paused {
            return None;
        }
        if let Some(dir) = self.gitignore_dir(msg) {
            return Some(dir);
        }
        // A directory that shows up with children was moved in whole, and inotify reports nothing
        // about what is inside it: only a walk can find those files. A directory that was renamed
        // — by us or in a terminal — is the same story, and worse: the removal of the old name
        // drops the subtree, and indexing the new one adds back the directory row alone, so every
        // note under it would vanish until the next restart. The walk is of that folder: a
        // `chmod` or a `touch` of one reads as the same change, and walked the whole vault. An
        // *empty* new directory needs no walk, only a watch: see `Batch::rewatch`.
        let folder = |p: &Path| {
            (*p != self.root && has_children(p)).then(|| self.rel(p).unwrap_or_default())
        };
        match msg {
            Msg::Rescan(dir) => Some(dir.clone()),
            Msg::Resume | Msg::Fs(VaultEvent::Rescan) => Some(String::new()),
            Msg::Fs(VaultEvent::Changed(p)) => folder(p),
            Msg::Fs(VaultEvent::Renamed { to, .. }) => folder(to),
            Msg::Update { rel, .. } => folder(&self.root.join(rel)),
            _ => None,
        }
    }

    /// The folder of the `.gitignore` `msg` is news about, wherever in the vault it sits.
    ///
    /// One of these decides which *directories* the walk enters (`walk::stat_one`), so editing one
    /// changes the shape of the index rather than the contents of a file: adding `mlruns/` has to
    /// drop the tree the last walk indexed, and removing it has to walk the tree left lazy. Only a
    /// walk can do either, and a `.gitignore` rules nothing outside its own folder, so that folder
    /// is what is walked — the root's, the common one, being the whole vault. One open in a tab is
    /// saved far more often than it is edited, and [`WALK_FLOOR`] is what keeps that to a walk now
    /// and then.
    fn gitignore_dir(&self, msg: &Msg) -> Option<String> {
        let dir = |rel: &str| {
            let named = Path::new(rel)
                .file_name()
                .is_some_and(|n| n == ".gitignore");
            named.then(|| parent_dir(rel).to_string())
        };
        let watched = |path: &Path| self.rel(path).and_then(|rel| dir(&rel));
        match msg {
            Msg::Fs(VaultEvent::Changed(p) | VaultEvent::Removed(p)) => watched(p),
            Msg::Fs(VaultEvent::Renamed { from, to }) => match (watched(from), watched(to)) {
                (Some(from), Some(to)) => Some(common_dir(&from, &to).to_string()),
                (from, to) => from.or(to),
            },
            // Our own save of one, which the watcher reports as well; whichever arrives first walks.
            Msg::Update { rel, .. } => dir(rel),
            _ => None,
        }
    }

    /// A walk of the folder `dir` (`""` for the whole vault), then a fresh watcher because
    /// directories and the symlinked ones may have come or gone. [`Event::Reconciled`] is emitted
    /// once both are done, so receiving it means the vault is indexed *and* watched.
    ///
    /// A first walk of a large vault takes seconds, so the inbox is read between its batches.
    /// What does not touch the walk is answered there — git news, the git directories to watch,
    /// the exclusion set — and everything else is held for [`Worker::run`], in the order it came.
    fn reconcile(&mut self, dir: &str) {
        // Whatever asked for a walk of this folder or of one inside it is answered by this one.
        if self
            .due
            .as_deref()
            .is_some_and(|due| common_dir(due, dir) == dir)
        {
            self.due = None;
        }
        let mut excluded = None;
        let Worker {
            root,
            index,
            rx,
            tx,
            events,
            watcher,
            git_dirs,
            unindexed,
            held,
            stop,
            ..
        } = self;
        let stats = index.reconcile_with(
            root,
            dir,
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
                                if let Err(e) = watch(watcher, index, root, git_dirs, unindexed, tx)
                                {
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
        self.next_walk = Instant::now() + WALK_FLOOR;
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
            Ok(mut stats) => {
                // What the walk took in before the watcher's news of it could arrive, which then
                // reads as no change (`Worker::update`): an open tab hears of it here or never.
                for rel in std::mem::take(&mut stats.changed) {
                    self.emit(Event::FileChanged(rel));
                }
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
            &self.unindexed,
            &self.tx,
        ) {
            self.fail("watching the vault", e);
        }
    }

    /// Whether `msg` is news from directly inside the unindexed folders alone, adding each folder
    /// whose listing it changed to `listed`.
    ///
    /// A folder the news names itself was removed or made anew, so its own listing changed too,
    /// and it is watched afresh (`rewatch`): one made anew is a new directory. A folder the walk
    /// has entered since — its `.gitignore` line went — is the index's again.
    fn unindexed_news(
        &mut self,
        msg: &Msg,
        listed: &mut BTreeSet<String>,
        rewatch: &mut bool,
    ) -> bool {
        let paths = match msg {
            Msg::Fs(
                VaultEvent::Changed(p) | VaultEvent::Removed(p) | VaultEvent::ConflictAppeared(p),
            ) => vec![p],
            Msg::Fs(VaultEvent::Renamed { from, to }) => vec![from, to],
            _ => return false,
        };
        let mut inside = true;
        for path in paths {
            let Some(rel) = self.rel(path) else {
                return false;
            };
            if self.unindexed.contains(&rel) {
                listed.insert(rel.clone());
                if let Some(watcher) = &mut self.watcher {
                    watcher.forget(path);
                }
                *rewatch = true;
            }
            let parent = parent_dir(&rel);
            if self.unindexed.contains(parent) && matches!(self.index.get_file(parent), Ok(None)) {
                listed.insert(parent.to_string());
            } else {
                inside = false;
            }
        }
        inside
    }

    fn apply(&mut self, ev: VaultEvent, b: &mut Batch) {
        match ev {
            // Both handled before the batch is walked: a rescan in `walk_scope`, a git change
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
                    if let Ok(Some(meta)) = walk::stat_one(&self.root, &rel) {
                        // A directory there again is a new one, made inside this batch, and the
                        // debouncer says nothing of what the old one held: that goes as for a
                        // removal across two batches, and the new one comes back as a directory
                        // made (`Batch::made`), watched afresh — its watch went with the old one
                        // (`forget`).
                        if meta.kind == FileKind::Dir {
                            self.remove(&rel, b);
                        }
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
                if kind == FileKind::Dir {
                    b.rewatch = true;
                    b.made.push(rel.to_string());
                }
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
            // `Unchanged` is the watcher echoing our own save back at us, or a change a walk took
            // in first and reported itself (`ReconcileStats::changed`).
            Ok(Change::Unchanged) => {}
            // Out of the index, but not out of the tree, which lists a gitignored folder or a
            // `node_modules` beside the notes: its folder's listing changed all the same.
            Ok(Change::Ignored) => {
                b.dirs.insert(parent_dir(rel).to_string());
            }
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

/// Watch the directories the index holds, plus the git directories and the unindexed folders the
/// window lists: the watcher there is, given the new set ([`Watcher::set_dirs`], which keeps what
/// it has not reported yet), or a new one.
fn watch(
    watcher: &mut Option<Watcher>,
    index: &Index,
    root: &Path,
    git_dirs: &[PathBuf],
    unindexed: &BTreeSet<String>,
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
    // And each remote's tracking refs, which are all a push moves: one made in a terminal, or
    // one a host finished after the link to it dropped.
    for git_dir in git_dirs {
        dirs.push(git_dir.clone());
        dirs.push(git_dir.join("refs/heads"));
        let remotes = std::fs::read_dir(git_dir.join("refs/remotes"))
            .into_iter()
            .flatten();
        dirs.extend(remotes.flatten().map(|e| e.path()).filter(|p| p.is_dir()));
    }
    // One level each, as every other directory here: never the tree under one.
    dirs.extend(unindexed.iter().map(|rel| root.join(rel)));
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

/// The watcher saying it lost events (a queue overflow), which only a walk makes up for.
fn lost_news(msg: &Msg) -> bool {
    matches!(msg, Msg::Fs(VaultEvent::Rescan))
}

/// The deepest folder holding both `a` and `b`, vault-relative folders both, `""` being the root.
fn common_dir<'a>(a: &'a str, b: &str) -> &'a str {
    let mut len = 0;
    for (x, y) in a.split('/').zip(b.split('/')) {
        if x != y {
            break;
        }
        len += x.len() + 1;
    }
    &a[..len.saturating_sub(1)]
}

#[cfg(test)]
mod tests;
