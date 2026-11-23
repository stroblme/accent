//! Off-thread index writer and filesystem watcher. Batched messages combine bulk sync changes
//! into one [`Event`].

use std::collections::{BTreeSet, HashMap};
use std::os::unix::fs::MetadataExt;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::Result;

use accent_core::index::{Change, Index};
use accent_core::path::{is_note, is_under, parent_dir};
use accent_core::walk::{self, FileKind, ScanOptions};
use accent_core::watch::{DEBOUNCE, VaultEvent, Watcher};

use crate::local::Ignored;
use crate::paths::{conflict_original_rel, conflict_pairs};
use crate::{Etag, Event};

/// Start a vault worker; the returned handle joins it on drop. One that panics ends with
/// [`Event::WorkerGone`].
///
/// Disable `watch` on Android storage where inotify drops events; the app sends
/// [`Msg::Rescan`] on foregrounding instead. `stop` holds [`RUN`], [`PAUSE`] or
/// [`CLOSE`] and interrupts scans while the inbox waits between batches.
/// See [`Worker::reconcile`]. `pdf_text` wakes the pass reading PDFs' text, if one runs.
#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn(
    root: PathBuf,
    index: Index,
    rx: Receiver<Msg>,
    tx: Sender<Msg>,
    events: Sender<Event>,
    watch: bool,
    stop: Arc<AtomicU8>,
    ignored: Arc<Ignored>,
    pdf_text: Option<Sender<()>>,
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
        own: HashMap::new(),
        links: HashMap::new(),
        dangling: BTreeSet::new(),
        stop,
        walked: false,
        burst: Vec::new(),
        burst_ends: None,
        paused: false,
        next_walk: Instant::now(),
        due: None,
        ignored,
        pdf_text,
    };
    std::thread::Builder::new()
        .name("accent-vault".to_string())
        .spawn(move || {
            let events = worker.events.clone();
            // The language providers hold the channel open, so a panic is said here or unheard.
            if let Err(panic) = std::panic::catch_unwind(AssertUnwindSafe(|| worker.run())) {
                let why = panic
                    .downcast_ref::<&str>()
                    .copied()
                    .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
                    .unwrap_or("a panic");
                let _ = events.send(Event::Error(format!(
                    "indexing and watching the vault stopped: {why}"
                )));
                let _ = events.send(Event::WorkerGone);
            }
        })
        .map_err(|e| accent_core::Error::io("the vault worker", e).into())
}

/// Minimum interval between watcher-triggered scans, limiting rescans during
/// `.gitignore` autosaves or lost events. Requests within the interval coalesce
/// into one scan at its end; later requests run immediately.
const WALK_FLOOR: Duration = Duration::from_secs(2);

/// The most files one burst may add and still count as used ([`Event::FilesUsed`]): the batches
/// that add files less than the watcher's [`DEBOUNCE`] apart, with the walks they ask for, as the
/// debouncer may hand a checkout over in parts ([`Worker::burst`]). A download (its file and the
/// `.part` beside it), a few pictures pasted or an agent's turn stay under it, while a checkout, a
/// sync or a folder copied in goes over; and a burst of ten takes at most a fifth of the fifty
/// files the window keeps as used.
const BULK_ADDED: usize = 10;

/// What `stop` says: walk on.
pub(crate) const RUN: u8 = 0;
/// The user stopped the walk: see [`Worker::paused`].
pub(crate) const PAUSE: u8 = 1;
/// The vault is closing, for good: a walk stops as for a pause, and nothing is set up after it.
pub(crate) const CLOSE: u8 = 2;

/// The worker's inbox. The watcher pushes `Fs`, the public API pushes the rest.
pub(crate) enum Msg {
    Fs(VaultEvent),
    /// What the watcher handed over at once ([`Watcher::new`]), taken apart into [`Msg::Fs`] as it
    /// is read ([`Msg::apart`]), so one batch holds all of it: a checkout's files are counted
    /// together ([`BULK_ADDED`]).
    Watched(Vec<VaultEvent>),
    Update {
        rel: String,
        own: bool,
    },
    /// A file or a folder this vault moved ([`Local::rename`](crate::local::Local::rename)),
    /// taken in as an own [`Update`](Self::Update) of each end: what arrives at `to` is no new
    /// file ([`Event::FilesAdded`]).
    Moved {
        from: String,
        to: String,
    },
    /// The etag left by a vault save, used to distinguish its watcher echo
    /// from external edits regardless of event order. See [`Worker::mine`].
    Saved {
        rel: String,
        etag: Etag,
        /// The write left the file's text as it was, ink drawn on a PDF: the row takes the new
        /// stat and keeps the text read of it. See [`Worker::restat`].
        same_text: bool,
    },
    /// Git directories found by `repos()`. The index walk skips `.git`,
    /// so the watcher needs these paths explicitly.
    WatchGit(Vec<PathBuf>),
    /// Folders the index does not walk to start (`true`) or stop watching. See
    /// [`Local::watch_unindexed`](crate::local::Local::watch_unindexed).
    WatchUnindexed(Vec<String>, bool),
    /// A file a tab opened, for the index to keep. See
    /// [`Local::keep_opened`](crate::local::Local::keep_opened).
    KeepOpened(String),
    /// What search leaves out, and where to say it has been written. See
    /// [`crate::local::Local::set_excluded`]: the worker holds the only writing connection.
    SetExcluded(Vec<String>, Sender<accent_core::Result<()>>),
    /// Reply once all preceding messages reach the index, letting
    /// [`crate::local::Local::settle_index`] wait for batched writes without an arbitrary delay.
    Settled(Sender<()>),
    /// Walk this folder, `""` being the whole vault.
    Rescan(String),
    /// Resume after [`Local::stop_indexing`](crate::local::Local::stop_indexing).
    /// Ignore other rescan triggers while paused, so watcher events cannot restart
    /// a scan the user stopped.
    Resume,
    Shutdown,
    /// Panic on the worker's thread, as a bug would.
    #[cfg(test)]
    Panic,
}

impl Msg {
    /// A [`Watched`](Msg::Watched) handover as the [`Fs`](Msg::Fs) messages it holds, any other
    /// message as it is.
    fn apart(self) -> Vec<Msg> {
        match self {
            Msg::Watched(events) => events.into_iter().map(Msg::Fs).collect(),
            msg => vec![msg],
        }
    }
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
    /// Absolute git directories watched for the git pane, never indexed.
    git_dirs: Vec<PathBuf>,
    /// Vault-relative unindexed folders listed by the window. Watch direct children
    /// and refresh their listings without updating the index.
    unindexed: BTreeSet<String>,
    /// What the inbox held that a walk read between its batches and could not answer there. See
    /// [`Worker::reconcile`].
    held: Vec<Msg>,
    /// The etag each file this vault wrote was left at, so the watcher's echo of the write can
    /// be told from another process's change. See [`Worker::mine`].
    own: HashMap<String, Etag>,
    /// The vault's own symlinks to files, by canonical target, routed the target's news. See
    /// [`Worker::apply_linked`].
    links: HashMap<PathBuf, Vec<String>>,
    /// The vault's symlinks to nothing, which the index never holds: what the walk found
    /// (`ReconcileStats::dangling`) and what news has left pointing at nothing since. Each is
    /// looked at again after every batch, the watcher hearing of a target in its own folder alone,
    /// and taken in once its target is there ([`Worker::found_targets`]).
    dangling: BTreeSet<String>,
    /// Set by [`Local::stop_indexing`](crate::local::Local::stop_indexing) and by the vault's
    /// `Drop` from another thread, and read inside the walk: the inbox cannot be reached from there.
    stop: Arc<AtomicU8>,
    /// Whether a walk of the whole vault has finished since the worker started. Before one, a row
    /// added is a file the index had not reached yet rather than a new one ([`Event::FilesAdded`]).
    walked: bool,
    /// The files added by batches that follow each other less than a [`DEBOUNCE`] apart, as the
    /// parts the debouncer hands a checkout over in do: told as added at once
    /// ([`Event::FilesAdded`]) and counted as used together once no more have come for a debounce
    /// ([`Worker::emit_used`]), under the names the window has been told since.
    burst: Vec<String>,
    /// When the burst ends unless more files are added: a [`DEBOUNCE`] after the last were.
    burst_ends: Option<Instant>,
    /// Whether indexing is paused; only [`Msg::Resume`] restarts it in this session.
    /// Not persisted: reopening the vault resumes by diffing the partial index.
    paused: bool,
    /// When a walk the watcher's news asks for may start: [`WALK_FLOOR`] after the last one.
    next_walk: Instant,
    /// Common ancestor of pending scan requests, queued as [`Msg::Rescan`]
    /// at [`Worker::next_walk`].
    due: Option<String>,
    /// Told the ignored folders each walk finds, which are all Go to File's ignored notes walk.
    ignored: Arc<Ignored>,
    /// Wakes the pass that reads PDFs' text: after a walk that finished, and after a batch that
    /// wrote a PDF's row.
    pdf_text: Option<Sender<()>>,
}

/// What one batch has accumulated: the directories whose children changed, and the paths it took
/// out of the index.
#[derive(Default)]
struct Batch {
    dirs: BTreeSet<String>,
    removed: BTreeSet<String>,
    /// A directory was added, so the watch set — one watch per directory — is short one entry.
    rewatch: bool,
    /// New directories to watch at batch end. Scan their contents then,
    /// since writes before the watch starts cannot emit events.
    made: Vec<String>,
    /// A PDF's row was written, so its text is unread.
    pdfs: bool,
    /// Files with a row of their own for the first time, neither rewritten nor moved here: see
    /// [`Event::FilesAdded`].
    added: Vec<String>,
    /// Where a move put a file or a folder the index held: nothing arriving there is new.
    moved: BTreeSet<String>,
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
        self.reconcile("", &BTreeSet::new(), Vec::new());

        loop {
            // One `recv` plus the rest of the burst: a Syncthing pull of 500 files is one batch,
            // and therefore one event for the UI. What the last walk held back came first.
            let mut batch = std::mem::take(&mut self.held);
            if batch.is_empty() {
                // Woken for whichever comes first: the walk that waits out the floor, or the end
                // of a burst, which an inbox still holding news puts off.
                let walk = self.due.is_some().then_some(self.next_walk);
                let first = match walk.into_iter().chain(self.burst_ends).min() {
                    Some(at) => match self
                        .rx
                        .recv_timeout(at.saturating_duration_since(Instant::now()))
                    {
                        Ok(msg) => msg,
                        Err(RecvTimeoutError::Timeout) if self.burst_ends == Some(at) => {
                            self.emit_used();
                            continue;
                        }
                        // Cleared here as well as by the walk: a paused vault drops the rescan.
                        Err(RecvTimeoutError::Timeout) => {
                            Msg::Rescan(self.due.take().unwrap_or_default())
                        }
                        Err(RecvTimeoutError::Disconnected) => break,
                    },
                    None => match self.rx.recv() {
                        Ok(msg) => msg,
                        Err(_) => break,
                    },
                };
                batch.push(first);
            }
            batch.extend(self.rx.try_iter());
            let batch: Vec<Msg> = batch.into_iter().flat_map(Msg::apart).collect();
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
        // Before anything in the batch is taken in: an echo of an earlier change of the file, come
        // late, may be read ahead of the save, with the save already on disk.
        for msg in &batch {
            if let Msg::Saved { rel, etag, .. } = msg {
                self.own.insert(rel.clone(), *etag);
            }
        }
        // Git first, and before anything else looks at these paths. A `.git` directory is full of
        // children, so `walk_scope` would read a commit as a whole tree moved in and walk the
        // vault; and `rel` cannot place a submodule's git directory, which lives outside the
        // vault entirely. Taking them out here leaves the rest of the worker exactly as it was.
        let (git, batch): (Vec<Msg>, Vec<Msg>) = batch
            .into_iter()
            .partition(|m| matches!(m, Msg::Fs(VaultEvent::Git(_))));
        let written: BTreeSet<Option<PathBuf>> = (git.iter())
            .filter_map(|m| match m {
                Msg::Fs(VaultEvent::Git(path)) => Some(git_dir_of(path, &self.git_dirs)),
                _ => None,
            })
            .collect();
        for dir in written {
            self.emit(Event::GitChanged(dir));
        }
        let mut rewatch = self.ref_dirs_moved(&git);
        for msg in &batch {
            if let Msg::WatchGit(dirs) = msg
                && *dirs != self.git_dirs
            {
                self.git_dirs = dirs.clone();
                self.rebuild_watcher();
            }
        }
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
        // Before the watcher's news below, an opened file's going to the index (`unindexed_news`).
        for msg in &batch {
            if let Msg::KeepOpened(rel) = msg
                && let Err(e) = self.index.keep_opened(&self.root, rel)
            {
                self.fail(&format!("keeping {rel} in the index"), e);
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
                // news: a tab open on a path that was renamed under it has to follow, and what
                // they brought is no new file.
                let mut moved = BTreeSet::new();
                for msg in &batch {
                    if let Msg::Fs(VaultEvent::Renamed { from, to }) = msg
                        && let (Some(from), Some(to)) = (self.rel(from), self.rel(to))
                    {
                        if self.moved(&from) {
                            moved.insert(to.clone());
                        }
                        self.renamed(from, to);
                    }
                    if let Msg::Moved { from, to } = msg {
                        self.follow(from, to);
                        moved.insert(to.clone());
                    }
                }
                self.reconcile("", &moved, Vec::new());
                return;
            } else {
                // A folder's walk leaves the rest of the vault to the batch, applied first.
                walk_after = Some(dir);
            }
        }
        let mut batched = Batch::default();
        for msg in batch {
            match msg {
                // `Settled` is answered by the caller of this one and never reaches here, nor does
                // `Watched`, taken apart as it is read.
                Msg::Rescan(_)
                | Msg::Watched(_)
                | Msg::Resume
                | Msg::Shutdown
                | Msg::WatchGit(_)
                | Msg::WatchUnindexed(..)
                | Msg::KeepOpened(_)
                | Msg::SetExcluded(..)
                | Msg::Settled(_) => {}
                Msg::Update { rel, own } => self.posted(rel, own, &mut batched),
                Msg::Moved { from, to } => {
                    self.follow(&from, &to);
                    batched.moved.insert(to.clone());
                    // The new path first, and in a folder the new path of each file a second
                    // path leads to, so a hard link the old rows leave behind is taken in as the
                    // alias of the row its file has there (`retake`), not as a row of its own
                    // ahead of the folder's walk.
                    let aliased: Vec<String> = (self.aliases().into_iter())
                        .filter(|(row, _)| *row != from && is_under(row, &from))
                        .map(|(row, _)| format!("{to}{}", &row[from.len()..]))
                        .collect();
                    self.posted(to, true, &mut batched);
                    for rel in aliased {
                        self.update(&rel, true, &mut batched);
                    }
                    self.posted(from, true, &mut batched);
                }
                Msg::Saved {
                    rel,
                    same_text: true,
                    ..
                } => self.restat(&rel, &mut batched),
                Msg::Saved { rel, .. } => self.update(&rel, true, &mut batched),
                Msg::Fs(ev) => self.apply_linked(ev, &mut batched),
                #[cfg(test)]
                Msg::Panic => panic!("the test's own panic"),
            }
        }
        self.found_targets(&mut batched);
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
        if batched.pdfs {
            self.wake_pdf_text();
        }
        // The files the walk adds are counted with the batch's own: a checkout's new folder is
        // part of the checkout ([`BULK_ADDED`]).
        match walk_after {
            Some(dir) => self.reconcile(&dir, &batched.moved, batched.added),
            None => self.emit_added(batched.added),
        }
    }

    /// Take in a path the public API wrote or removed ([`Msg::Update`]).
    fn posted(&mut self, rel: String, own: bool, b: &mut Batch) {
        self.update(&rel, own, b);
        // Any other write of this vault's own is known by what it left on disk.
        if own {
            match Etag::of(&self.root.join(&rel)) {
                Ok(etag) => self.own.insert(rel, etag),
                Err(_) => self.own.remove(&rel),
            };
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
        // *empty* new directory needs no walk, only a watch: see `Batch::rewatch`. Nor does one
        // outside the vault, in a folder watched for a symlinked file's target.
        let folder = |p: &Path| {
            (*p != self.root && p.starts_with(&self.root) && has_children(p))
                .then(|| self.rel(p).unwrap_or_default())
        };
        match msg {
            Msg::Rescan(dir) => Some(dir.clone()),
            Msg::Resume | Msg::Fs(VaultEvent::Rescan) => Some(String::new()),
            Msg::Fs(VaultEvent::Changed(p)) => folder(p),
            Msg::Fs(VaultEvent::Renamed { to, .. }) => folder(to),
            Msg::Update { rel, .. } | Msg::Saved { rel, .. } | Msg::Moved { to: rel, .. } => {
                folder(&self.root.join(rel))
            }
            _ => None,
        }
    }

    /// Find the folder affected by a `.gitignore` event. Ignore rules change which
    /// directories [`accent_core::walk::stat_one`] enters, so rescan that folder to add or remove
    /// indexed subtrees. [`WALK_FLOOR`] limits rescans during autosaves.
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
            Msg::Update { rel, .. } | Msg::Saved { rel, .. } => dir(rel),
            Msg::Moved { from, to } => match (dir(from), dir(to)) {
                (Some(from), Some(to)) => Some(common_dir(&from, &to).to_string()),
                (from, to) => from.or(to),
            },
            _ => None,
        }
    }

    /// Rescan `dir` (`""` for the vault), then refresh watches for changed directories
    /// and symlinks. [`Event::Reconciled`] means both indexing and watching are ready.
    /// During long scans, process git events, watch changes and exclusions between
    /// batches; defer other messages to [`Worker::run`] in arrival order. The files it adds are
    /// new ([`Event::FilesAdded`]) unless they lie under `moved`, where a move put them, and are
    /// counted with `added`, what the batch asking for the walk added.
    fn reconcile(&mut self, dir: &str, moved: &BTreeSet<String>, mut added: Vec<String>) {
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
            links,
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
                let mut written = BTreeSet::new();
                for msg in rx.try_iter().flat_map(Msg::apart) {
                    match msg {
                        Msg::Fs(VaultEvent::Git(path)) => {
                            written.insert(git_dir_of(&path, git_dirs));
                        }
                        Msg::WatchGit(dirs) => {
                            if dirs != *git_dirs {
                                *git_dirs = dirs;
                                // A failure is left to the rebuild after the walk, which reports it.
                                if let Err(e) =
                                    watch(watcher, index, root, git_dirs, unindexed, links, tx)
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
                for dir in written {
                    let _ = events.send(Event::GitChanged(dir));
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
                if !stats.stopped {
                    let found = std::mem::take(&mut stats.ignored_dirs);
                    self.ignored.found(&stats.dir, found);
                    self.wake_pdf_text();
                    let dir = stats.dir.as_str();
                    self.dangling
                        .retain(|rel| !dir.is_empty() && !is_under(rel, dir));
                    self.dangling.extend(std::mem::take(&mut stats.dangling));
                }
                // What the walk took in before the watcher's news of it could arrive, which then
                // reads as no change (`Worker::unchanged`): in an unwatched vault, an open tab
                // hears of it here or never.
                let aliases = match stats.changed.is_empty() {
                    true => Vec::new(),
                    false => self.aliases(),
                };
                for rel in std::mem::take(&mut stats.changed) {
                    self.emit_changed(rel, &aliases);
                }
                added.extend(
                    std::mem::take(&mut stats.added_files)
                        .into_iter()
                        .filter(|rel| !moved.iter().any(|to| is_under(rel, to))),
                );
                self.emit_added(added);
                self.walked |= stats.dir.is_empty() && !stats.stopped;
                // Emitted for a stopped walk too, carrying `stopped`: it is what tells the window
                // it is looking at a partial index rather than a finished one.
                self.emit(Event::Reconciled(stats));
                self.emit_conflicts();
            }
            Err(e) => {
                self.fail("reconciling the vault", e);
                // Every time, unlike the error: it is what ends the walk on screen.
                self.emit(Event::WalkFailed);
            }
        }
    }

    fn wake_pdf_text(&self) {
        if let Some(wake) = &self.pdf_text {
            let _ = wake.send(());
        }
    }

    /// Report conflicts found by a scan, including those synced while accent was
    /// closed and therefore unseen by its watcher.
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
            &mut self.links,
            &self.tx,
        ) {
            self.fail("watching the vault", e);
        }
        // Said once, as a failure is: a polled vault hears of a change up to 2 s late, and the
        // budget is the reader's to raise.
        if let Some(notice) = self.watcher.as_ref().and_then(Watcher::polling)
            && self.reported.insert(notice.clone())
        {
            tracing::warn!("{notice}");
            self.emit(Event::Error(notice));
        }
    }

    /// Refresh watches for added or removed branch-ref folders, such as a first
    /// fetch or `feature/…` branch. Forget removed folders so later recreation
    /// gets a new watch. See [`watch`].
    fn ref_dirs_moved(&mut self, git: &[Msg]) -> bool {
        let mut made = false;
        for msg in git {
            let Msg::Fs(VaultEvent::Git(path)) = msg else {
                continue;
            };
            if !self
                .git_dirs
                .iter()
                .any(|g| path.starts_with(g.join("refs")))
            {
                continue;
            }
            if path.is_dir() {
                made = true;
            } else if let Some(watcher) = &mut self.watcher {
                watcher.forget(path);
            }
        }
        made
    }

    /// Handle events confined to unindexed folders and add changed listings to `listed`.
    /// Rewatch recreated folders; folders entered after an ignore-rule change
    /// return to normal index handling.
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
                // An opened file's news is the index's too (`Index::keep_opened`).
                inside &= !matches!(self.index.is_opened(&rel), Ok(true));
            } else {
                inside = false;
            }
        }
        inside
    }

    /// [`apply`](Self::apply) `ev`, and again for each of the vault's symlinks to the file it
    /// names: a write to a target is news in the target's folder, never in the link's.
    fn apply_linked(&mut self, ev: VaultEvent, b: &mut Batch) {
        let through = |p: &Path| -> Vec<PathBuf> {
            let target = (p.parent().and_then(|dir| dir.canonicalize().ok()))
                .zip(p.file_name())
                .map(|(dir, name)| dir.join(name));
            (target
                .and_then(|target| self.links.get(&target))
                .into_iter()
                .flatten())
            .map(|rel| self.root.join(rel))
            .collect()
        };
        let linked: Vec<VaultEvent> = match &ev {
            _ if self.links.is_empty() => Vec::new(),
            VaultEvent::Changed(p) | VaultEvent::ConflictAppeared(p) => {
                through(p).into_iter().map(VaultEvent::Changed).collect()
            }
            VaultEvent::Removed(p) => through(p).into_iter().map(VaultEvent::Removed).collect(),
            VaultEvent::Renamed { from, to } => (through(from).into_iter())
                .map(VaultEvent::Removed)
                .chain(through(to).into_iter().map(VaultEvent::Changed))
                .collect(),
            VaultEvent::Git(_) | VaultEvent::Rescan => Vec::new(),
        };
        self.apply(ev, b);
        for ev in linked {
            self.apply(ev, b);
        }
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
                    if let Ok(Some(meta)) = self.index.stat(&self.root, &rel) {
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
                    if self.moved(&from) {
                        b.moved.insert(to.clone());
                    }
                    let aliased = self.aliases_under(&from);
                    if let Err(e) = self.index.remove_file_batched(&from) {
                        self.fail(&format!("removing {from}"), e);
                    }
                    b.removed.insert(from.clone());
                    self.update(&to, false, b);
                    b.dirs.insert(parent_dir(&from).to_string());
                    b.dirs.insert(parent_dir(&to).to_string());
                    self.retake(aliased, b);
                    self.renamed(from, to);
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
        let aliased = self.aliases_under(rel);
        if let Err(e) = self.index.remove_file_batched(rel) {
            self.fail(&format!("removing {rel}"), e);
        }
        b.removed.insert(rel.to_string());
        b.dirs.insert(parent_dir(rel).to_string());
        self.seen_conflicts.remove(rel);
        self.dangles(rel);
        self.emit(Event::FileRemoved(rel.to_string()));
        self.retake(aliased, b);
    }

    /// The second paths to the files at `rel` or under it ([`Index::aliases_under`]).
    fn aliases_under(&mut self, rel: &str) -> Vec<String> {
        self.index.aliases_under(rel).unwrap_or_else(|e| {
            self.fail("listing the aliases", e);
            Vec::new()
        })
    }

    /// Take in afresh each second path to a file whose row went: a hard link keeps the file under
    /// a row of its own, which is no new file, and a link into the vault leads nowhere now.
    fn retake(&mut self, aliases: Vec<String>, b: &mut Batch) {
        for rel in aliases {
            match self.root.join(&rel).exists() {
                true => {
                    b.moved.insert(rel.clone());
                    self.update(&rel, false, b);
                }
                false => self.remove(&rel, b),
            }
        }
    }

    /// The path that keeps the file at `rel` when the vault reaches it by another too, as the walk
    /// keeps one (`walk::scan`), asked only of a path a link leads to or a file of more than one
    /// link, so an ordinary file costs a stat and a `canonicalize`. A link into the vault names
    /// that path, unless the walk does not take it in; a hard link finds the row its file has,
    /// which news never takes from it. The root is canonical (`Local::open`).
    fn keeper(&self, rel: &str) -> Option<String> {
        let path = self.root.join(rel);
        let meta = std::fs::metadata(&path).ok().filter(|m| m.is_file())?;
        let real = path.canonicalize().ok()?;
        if real != path {
            let inside = real.strip_prefix(&self.root).ok().and_then(Path::to_str);
            if let Some(inside) = inside {
                return matches!(walk::stat_one(&self.root, inside), Ok(Some(_)))
                    .then(|| inside.to_string());
            }
        } else if meta.nlink() < 2 {
            return None;
        }
        let row = self.index.holder(meta.dev(), meta.ino(), rel).ok()??;
        let same = std::fs::metadata(self.root.join(&row))
            .is_ok_and(|m| (m.dev(), m.ino()) == (meta.dev(), meta.ino()));
        same.then_some(row)
    }

    /// Keep `rel` among the links to nothing if it is one now: made so, or left so by its target
    /// going ([`Worker::dangling`]).
    fn dangles(&mut self, rel: &str) {
        let path = self.root.join(rel);
        if path.is_symlink() && !path.exists() {
            self.dangling.insert(rel.to_string());
        }
    }

    /// Take in each link to nothing whose target has come since, wherever it came from, as news
    /// of the link: its row is written and its folder listed again. Watched from then on as the
    /// vault's other links to files are ([`watch`]). A link no longer there leaves the set.
    fn found_targets(&mut self, b: &mut Batch) {
        let root = &self.root;
        self.dangling.retain(|rel| root.join(rel).is_symlink());
        let found: Vec<String> = (self.dangling.iter())
            .filter(|rel| root.join(rel).exists())
            .cloned()
            .collect();
        for rel in found {
            self.dangling.remove(&rel);
            // News of its target routed through `links` may have taken it in already.
            if !matches!(self.index.get_file(&rel), Ok(Some(_))) {
                self.update(&rel, false, b);
                b.rewatch = true;
            }
        }
    }

    /// Bring one path in line with the disk. `own` marks a change we made ourselves, which must
    /// never come back to the UI as "someone else edited this note".
    fn update(&mut self, rel: &str, own: bool, b: &mut Batch) {
        if rel.is_empty() {
            return;
        }
        // News naming an alias is its row's: `update_file` would add the alias as a second row.
        if let Some(row) = self.row_of(rel) {
            if let Some(etag) = self.own.get(rel).copied() {
                self.own.insert(row.clone(), etag);
            }
            return self.update(&row, own, b);
        }
        // A second path to a file is the alias of its row, never a row of its own, the file's own
        // path taken in first so the order news comes in does not decide which is which.
        if let Some(keeper) = self.keeper(rel) {
            if !matches!(self.index.get_file(&keeper), Ok(Some(_))) {
                self.update(&keeper, false, b);
            }
            let row = self.row_of(&keeper).unwrap_or(keeper);
            match self.index.alias(rel, &row) {
                Ok(true) => {
                    b.dirs.insert(parent_dir(rel).to_string());
                    return;
                }
                Ok(false) => {}
                Err(e) => return self.fail(&format!("indexing {rel}"), e),
            }
        }
        // What a row that goes held by its second paths, taken in afresh once it has (`retake`).
        let aliased = match self.root.join(rel).exists() {
            true => Vec::new(),
            false => self.aliases_under(rel),
        };
        // The links the file holds and the ones its names answer to are resolved as it goes, in a
        // few index lookups: a pass over every link is 0.1–0.35 s a batch at `make vault`.
        let change = self.index.update_file(&self.root, rel);
        if let Ok(Change::Added(FileKind::Pdf) | Change::Updated(FileKind::Pdf)) = change {
            b.pdfs = true;
        }
        match change {
            Ok(Change::Added(kind)) => {
                b.dirs.insert(parent_dir(rel).to_string());
                if kind == FileKind::Dir {
                    b.rewatch = true;
                    b.made.push(rel.to_string());
                }
                let new = kind != FileKind::Dir
                    && !b.removed.contains(rel)
                    && !b.moved.iter().any(|to| is_under(rel, to));
                if new {
                    b.added.push(rel.to_string());
                }
                // A path this batch removed and is seeing again was rewritten, not created:
                // whoever has it open has to reload it.
                if kind != FileKind::Dir && !own && b.removed.remove(rel) && !self.mine(rel) {
                    self.changed(rel);
                }
            }
            Ok(Change::Removed) => {
                b.dirs.insert(parent_dir(rel).to_string());
                b.removed.insert(rel.to_string());
                self.dangles(rel);
                self.retake(aliased, b);
            }
            Ok(Change::Updated(kind)) => {
                // Every kind but a directory reports: a PDF rebuilt by a tool or a source file
                // edited in another editor has to refresh in the UI just like a note does.
                if kind != FileKind::Dir && !own && !self.mine(rel) {
                    self.changed(rel);
                }
            }
            Ok(Change::Unchanged) if !own => self.unchanged(rel, b),
            Ok(Change::Unchanged) => {}
            // Out of the index, but not out of the tree, which lists a gitignored folder or a
            // `node_modules` beside the notes: its folder's listing changed all the same.
            Ok(Change::Ignored) => {
                b.dirs.insert(parent_dir(rel).to_string());
                if self.root.join(rel).is_dir()
                    && walk::unindexed(&self.root, rel) == Some(walk::Unindexed::Ignored)
                {
                    self.ignored.add(rel);
                }
            }
            Err(e) => self.fail(&format!("indexing {rel}"), e),
        }
    }

    /// An own write of `rel` that left its text as it was: its row takes the new stat and keeps
    /// the text, so the PDF text pass reads it again only where a read of it was under way. A
    /// path the index holds no PDF at is indexed as any own write is.
    fn restat(&mut self, rel: &str, b: &mut Batch) {
        match self.index.restat_pdf(&self.root, rel) {
            Ok(Some(unread)) => b.pdfs |= unread,
            _ => self.update(rel, true, b),
        }
    }

    /// Handle watcher events whose disk state already matches the index. They may
    /// echo our own write, a scan, or another process sharing the index. Only our
    /// saved etag distinguishes an own-write echo; scans may be reported twice.
    /// Always refresh the folder listing because the event cannot identify a new file.
    fn unchanged(&mut self, rel: &str, b: &mut Batch) {
        let path = self.root.join(rel);
        if !path.exists() || self.mine(rel) {
            return;
        }
        b.dirs.insert(parent_dir(rel).to_string());
        match path.is_dir() {
            // Watched from now on, as a directory this vault added would be.
            true => b.rewatch = true,
            false => self.changed(rel),
        }
    }

    /// Tell the window `rel` changed, and each alias of it: the watcher's news names the row
    /// alone. An alias is told rather than updated, which would add it as a second row.
    fn changed(&mut self, rel: &str) {
        let aliases = self.aliases();
        self.emit_changed(rel.to_string(), &aliases);
    }

    /// [`changed`](Self::changed) with the vault's aliases in hand, for a walk's many rows.
    fn emit_changed(&self, rel: String, aliases: &[(String, String)]) {
        for (_, alias) in aliases.iter().filter(|(row, _)| *row == rel) {
            self.emit(Event::FileChanged(alias.clone()));
        }
        self.emit(Event::FileChanged(rel));
    }

    /// Tell the window of `added`, the files one batch and its walk added ([`Event::FilesAdded`]),
    /// and add them to the burst ([`Worker::burst`]). Nothing before the vault's opening walk has
    /// ended, which finds what the index had not reached yet, new or not.
    fn emit_added(&mut self, added: Vec<String>) {
        if !self.walked || added.is_empty() {
            return;
        }
        self.burst.extend(added.iter().cloned());
        // Past `BULK_ADDED` none of the burst counts, whatever else comes, so a sync of thousands
        // keeps no more of it, nor makes each rename `follow` scans them all.
        self.burst.truncate(BULK_ADDED + 1);
        self.burst_ends = Some(Instant::now() + DEBOUNCE);
        self.emit(Event::FilesAdded(added));
    }

    /// End the burst: tell the window which of its files count as used ([`Event::FilesUsed`]),
    /// none of more than [`BULK_ADDED`], nor one gone since.
    fn emit_used(&mut self) {
        self.burst_ends = None;
        let burst = std::mem::take(&mut self.burst);
        if burst.len() > BULK_ADDED {
            return;
        }
        let used: Vec<String> = (burst.into_iter())
            .filter(|rel| self.counts(rel) && matches!(self.index.get_file(rel), Ok(Some(_))))
            .collect();
        if !used.is_empty() {
            self.emit(Event::FilesUsed(used));
        }
    }

    /// Tell the window `from` was renamed `to`, and the burst too ([`Worker::follow`]).
    fn renamed(&mut self, from: String, to: String) {
        self.follow(&from, &to);
        self.emit(Event::FileRenamed { from, to });
    }

    /// Give the files of the burst at `from` or under it their names under `to`, as the window,
    /// told of the move, gives the files it counts as used.
    fn follow(&mut self, from: &str, to: &str) {
        for rel in &mut self.burst {
            if is_under(rel, from) {
                *rel = format!("{to}{}", &rel[from.len()..]);
            }
        }
    }

    /// Whether a file added at `rel` counts as used: not a hidden one (a dot name, or one inside
    /// a dot folder, as an editor's swap file), nor SyncTeX's `main.synctex(busy)` a TeX build
    /// writes and renames, a conflict copy, or a file git ignores that is no note, which
    /// `Index::recent_files` leaves out too.
    fn counts(&self, rel: &str) -> bool {
        !rel.split('/').any(|part| part.starts_with('.'))
            && !rel.ends_with("(busy)")
            && conflict_original_rel(rel).is_none()
            && (is_note(rel) || !walk::ignored_file(&self.root, rel))
    }

    /// Whether a rename from `from` brings no new file: a move of one the index held that would
    /// count, or SyncTeX's `(busy)` file taking its name, which is the build's whatever it is
    /// called. A temporary nobody indexed, or a hidden one (rsync's `.name.XXXXXX`), renamed into
    /// place, is a file written.
    fn moved(&self, from: &str) -> bool {
        from.ends_with("(busy)")
            || (matches!(self.index.get_file(from), Ok(Some(_))) && self.counts(from))
    }

    /// The vault's aliases as `(row, alias)`.
    fn aliases(&mut self) -> Vec<(String, String)> {
        self.index.aliases().unwrap_or_else(|e| {
            self.fail("listing the aliases", e);
            Vec::new()
        })
    }

    /// The row `rel` is a second path to, while it still reaches that row's file: a hard link
    /// replaced since is a file of its own.
    fn row_of(&mut self, rel: &str) -> Option<String> {
        let (row, _) = self.aliases().into_iter().find(|(_, alias)| alias == rel)?;
        let id = |rel: &str| {
            std::fs::metadata(self.root.join(rel))
                .ok()
                .map(|m| (m.dev(), m.ino()))
        };
        (id(rel).is_some() && id(rel) == id(&row)).then_some(row)
    }

    /// Check whether `rel` still has this vault's last saved etag. Suppress its
    /// watcher echo regardless of the order in which save and event arrived.
    fn mine(&self, rel: &str) -> bool {
        self.own
            .get(rel)
            .is_some_and(|own| Etag::of(&self.root.join(rel)).is_ok_and(|now| now == *own))
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

    /// Keep indexing after a bad file: log every failure and notify the UI once
    /// per distinct error. Repeated autosave failures must not generate repeated toasts.
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

/// The git dir of `known` a write at `path`, somewhere in a `.git`, was in: the deepest, as a
/// submodule's lies in its superproject's. `None` for a `.git` none of them is, or one gone
/// since, which is a repository made or removed.
fn git_dir_of(path: &Path, known: &[PathBuf]) -> Option<PathBuf> {
    known
        .iter()
        .filter(|dir| path.starts_with(dir))
        .max_by_key(|dir| dir.as_os_str().len())
        .filter(|dir| dir.is_dir())
        .cloned()
}

/// Update watches for indexed directories, git directories, listed unindexed folders and the
/// folders of symlinked files' targets, refilling `links`. Reuse [`Watcher::set_dirs`] to
/// preserve pending events, or create a watcher.
fn watch(
    watcher: &mut Option<Watcher>,
    index: &Index,
    root: &Path,
    git_dirs: &[PathBuf],
    unindexed: &BTreeSet<String>,
    links: &mut HashMap<PathBuf, Vec<String>>,
    tx: &Sender<Msg>,
) -> Result<()> {
    // The watch set is what the walk kept, one watch per directory: a `.venv` the walk refused
    // must not come back in through a recursive watch on the root.
    let mut dirs = index.dirs(root).unwrap_or_else(|e| {
        tracing::warn!("listing the directories to watch: {e:#}");
        Vec::new()
    });
    // A repository's own directory (`packed-refs` among what is in it) and the branch tips
    // inside it, which is what tells the git pane a commit happened in a terminal; `notify`
    // refuses a path that does not exist, so a repository removed under us costs a warning, not
    // the watch set. And each remote's tracking refs, which are all a push moves: one made in a
    // terminal, or one a host finished after the link to it dropped. Every folder of them, a
    // `feature/x` being a file a level down, and `refs` itself, where a first remote's
    // `refs/remotes` is made ([`Worker::ref_dirs_moved`]).
    for git_dir in git_dirs {
        dirs.push(git_dir.clone());
        dirs.push(git_dir.join("refs"));
        ref_dirs(&git_dir.join("refs/heads"), &mut dirs);
        ref_dirs(&git_dir.join("refs/remotes"), &mut dirs);
    }
    // One level each, as every other directory here: never the tree under one. Not one reached
    // through a link into the vault: a second name for a watched folder would rename its news,
    // and unwatching that name would unwatch the folder.
    let real_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    dirs.extend(
        (unindexed.iter())
            .filter(|rel| {
                let real = root.join(rel).canonicalize();
                real.is_ok_and(|real| real == real_root.join(rel) || !real.starts_with(&real_root))
            })
            .map(|rel| root.join(rel)),
    );
    // A file that is a symlink of its own hears of its target in the target's folder, watched
    // for it unless that folder is watched already under its vault name: a second name for one
    // folder would rename the first's news. The worker routes the news back ([`Worker::apply_linked`]).
    links.clear();
    let linked = index.symlinked_files(root).unwrap_or_else(|e| {
        tracing::warn!("listing the symlinked files to watch: {e:#}");
        Vec::new()
    });
    if !linked.is_empty() {
        let mut seen = index.dir_canonicals().unwrap_or_default();
        seen.insert(root.to_path_buf());
        seen.extend(
            unindexed
                .iter()
                .filter_map(|rel| root.join(rel).canonicalize().ok()),
        );
        for (rel, target) in linked {
            if let Some(dir) = target.parent()
                && seen.insert(dir.to_path_buf())
            {
                dirs.push(dir.to_path_buf());
            }
            links.entry(target).or_default().push(rel);
        }
    }
    if let Some(w) = watcher
        && w.set_dirs(&dirs)
    {
        return Ok(());
    }
    // Dropped first: two registrations on one tree would double every event.
    *watcher = None;
    let tx = tx.clone();
    *watcher = Some(Watcher::new(root, &dirs, move |events| {
        let _ = tx.send(Msg::Watched(events));
    })?);
    Ok(())
}

/// `dir` and every folder under it, where it exists: the folders of a repository's branch refs,
/// a handful in all but a repository fetched from a remote with many branch namespaces.
fn ref_dirs(dir: &Path, dirs: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    dirs.push(dir.to_path_buf());
    for entry in entries.flatten() {
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            ref_dirs(&entry.path(), dirs);
        }
    }
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
