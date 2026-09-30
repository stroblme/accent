//! The vault on this machine: three SQLite connections, the worker that owns the writing one,
//! and the path arithmetic every method here goes through.
//!
//! [`Local`] is what a local [`Vault`](crate::Vault) is, and it is also the whole of what
//! `accent-cli serve` answers with on a remote host — the rpc dispatch calls exactly these
//! methods. The file operations are in [`files`], the index reads in [`index`].

use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;

use anyhow::Result;

use accent_core::index::Index;

use crate::paths::outside;
use crate::worker::{self, Msg};
use crate::{Event, VaultConfig, language, locked};

mod files;
mod index;

/// One open vault: the index, the watcher, and the worker thread that owns both writers.
pub(crate) struct Local {
    root: PathBuf,
    /// The caller's connection. The mutex is not about contention (WAL readers never block):
    /// it is what makes `Local` `Send + Sync`, which uniffi will need in Phase 3.
    index: Mutex<Index>,
    /// A reader of its own for the sidebar's search, which runs on a worker thread. A regex scan
    /// of every note holds its connection for as long as it takes, and the main thread's
    /// `list_dir` and `backlinks` must never queue behind one on the same mutex.
    search: Mutex<Index>,
    cfg: Mutex<VaultConfig>,
    /// The language providers answering for the open documents.
    pub(crate) lang: std::sync::Arc<language::Languages>,
    tx: Sender<Msg>,
    /// Raised to stop the walk the worker is in the middle of, saying why: [`worker::PAUSE`] or
    /// [`worker::CLOSE`]. A flag rather than a message because the worker only reads its inbox
    /// between write batches, and the scan half of a walk has no batches at all.
    stop: Arc<AtomicU8>,
    worker: Option<JoinHandle<()>>,
    /// What the last Replace All rewrote, as it was before: [`Local::undo_replace`]'s to write
    /// back. `None` when there is nothing to undo, or when it was too much to keep.
    undo: Mutex<Option<Vec<files::Before>>>,
}

// ---------------------------------------------------------------------- open

impl Local {
    /// Open `root` with its index in the shared cache directory.
    pub(crate) fn open(root: &Path, cfg: VaultConfig) -> Result<(Local, Receiver<Event>)> {
        let db = accent_core::index::default_db_path(root);
        Local::open_at(root, &db, cfg)
    }

    /// [`open`](Self::open) with an explicit index file, for tests and tooling.
    pub(crate) fn open_at(
        root: &Path,
        db: &Path,
        cfg: VaultConfig,
    ) -> Result<(Local, Receiver<Event>)> {
        Local::open_with(root, db, cfg, true)
    }

    /// [`open_at`](Self::open_at) saying whether the vault is watched. `watch` is false on
    /// Android; see [`worker::spawn`].
    pub(crate) fn open_with(
        root: &Path,
        db: &Path,
        cfg: VaultConfig,
        watch: bool,
    ) -> Result<(Local, Receiver<Event>)> {
        // One spelling of the root for everything downstream: index paths, watcher events and
        // symlink targets are all compared against it. A root that is not a folder is refused
        // rather than opened as an empty vault: on a host, that is a mistyped address.
        let root = root
            .canonicalize()
            .ok()
            .filter(|r| r.is_dir())
            .ok_or_else(|| anyhow::anyhow!("{} is not a folder", root.display()))?;
        // The reader opens first because it is the connection that may drop and recreate the
        // schema; the worker's must never see the database half-built.
        let index = Index::open(db)?;
        let search = Index::open(db)?;
        let writer = Index::open(db)?;

        let (tx, rx) = channel::<Msg>();
        let (events, event_rx) = channel::<Event>();
        // The providers send their diagnostics down the same channel the worker's events use.
        let lang = language::Languages::new(root.clone(), db.to_path_buf(), events.clone());
        let stop = Arc::new(AtomicU8::new(worker::RUN));
        let handle = worker::spawn(
            root.clone(),
            writer,
            rx,
            tx.clone(),
            events,
            watch,
            stop.clone(),
        )?;

        Ok((
            Local {
                root,
                index: Mutex::new(index),
                search: Mutex::new(search),
                cfg: Mutex::new(cfg),
                lang,
                tx,
                stop,
                worker: Some(handle),
                undo: Mutex::new(None),
            },
            event_rx,
        ))
    }

    /// The canonical vault root. Every `rel` this API takes or returns is relative to it.
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn config(&self) -> VaultConfig {
        locked(&self.cfg).clone()
    }

    pub fn set_config(&self, cfg: VaultConfig) {
        *locked(&self.cfg) = cfg;
    }

    pub fn set_ghost(&self, on: bool) {
        self.lang.set_ghost(on);
    }

    /// Ask for a full walk: after a resume, or when the UI suspects it missed something.
    pub fn rescan(&self) {
        self.post(Msg::Rescan(String::new()));
    }

    /// Ask for a walk of the folder `dir` alone, which the reader suspects the index has fallen
    /// behind on. A plain vault-relative path: the walk starts where it names.
    pub fn rescan_dir(&self, dir: &str) -> io::Result<()> {
        if !Path::new(dir)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
        {
            return Err(outside(dir));
        }
        self.post(Msg::Rescan(dir.to_string()));
        Ok(())
    }

    /// Stop the walk that is running, keeping everything it has already written.
    ///
    /// This is a pause, not a cancel: the index is a diff of the disk, so the next walk — the one
    /// [`resume_indexing`](Self::resume_indexing) asks for, or simply the next time the vault is
    /// opened — indexes what is left instead of starting over. Until then the vault is usable and
    /// says so: the reconcile it ends reports [`ReconcileStats::stopped`], and nothing else in the
    /// worker will walk again on its own.
    ///
    /// [`ReconcileStats::stopped`]: accent_core::index::ReconcileStats::stopped
    pub fn stop_indexing(&self) {
        self.stop.store(worker::PAUSE, Ordering::Relaxed);
    }

    /// Walk again after [`stop_indexing`](Self::stop_indexing). The only thing that does: a
    /// paused vault ignores every other reason to rescan.
    pub fn resume_indexing(&self) {
        self.stop.store(worker::RUN, Ordering::Relaxed);
        self.post(Msg::Resume);
    }

    /// See [`Vault::watch_unindexed`](crate::Vault::watch_unindexed).
    pub fn watch_unindexed(&self, dirs: &[String]) -> Result<()> {
        self.post(Msg::WatchUnindexed(dirs.to_vec(), true));
        Ok(())
    }

    pub fn unwatch_unindexed(&self, dirs: &[String]) -> Result<()> {
        self.post(Msg::WatchUnindexed(dirs.to_vec(), false));
        Ok(())
    }

    /// Join `rel` to the vault root, refusing anything that would land outside it. Every
    /// path-taking method goes through this: the GTK app sanitises its own input, but an
    /// `accent-target:` of `../Outside/x.md` arrives here straight from a template, and Phase 2's MCP
    /// server and Phase 3's Android bindings call the façade with whatever their caller said.
    ///
    /// The test is deliberately lexical and never `canonicalize`s: a vault links external
    /// directories in on purpose, so resolving would reject the very paths the walk indexed and
    /// a note reached through a directory symlink has to stay openable.
    pub fn resolve(&self, rel: &str) -> io::Result<PathBuf> {
        Local::join(&self.root, rel)
    }

    /// [`resolve`](Self::resolve) for a file a rewrite across the vault is about to write —
    /// Replace All, a move's link updates, a page edit's — refusing one a symlink takes outside
    /// the root. Readers follow such a link, since a vault links folders in on purpose; a rewrite
    /// that nobody aimed at the file must not reach through one into `~/.bashrc`. One
    /// `canonicalize` per file written.
    pub(crate) fn resolve_inside(&self, rel: &str) -> io::Result<PathBuf> {
        let path = self.resolve(rel)?;
        match path.canonicalize()?.starts_with(&self.root) {
            true => Ok(path),
            false => Err(outside(rel)),
        }
    }

    /// [`resolve`](Self::resolve) against any root, so a remote vault can do the same arithmetic
    /// with the root the server reported.
    pub fn join(root: &Path, rel: &str) -> io::Result<PathBuf> {
        let mut out = root.to_path_buf();
        for part in Path::new(rel).components() {
            match part {
                Component::Normal(name) => out.push(name),
                Component::CurDir => {}
                Component::ParentDir => {
                    // `..` may walk back down to the root, never past it.
                    out.pop();
                    if !out.starts_with(root) {
                        return Err(outside(rel));
                    }
                }
                // A root or a prefix component: that is not a vault-relative path at all.
                _ => return Err(outside(rel)),
            }
        }
        Ok(out)
    }

    fn index(&self) -> MutexGuard<'_, Index> {
        locked(&self.index)
    }

    fn searcher(&self) -> MutexGuard<'_, Index> {
        locked(&self.search)
    }

    fn post(&self, msg: Msg) {
        if self.tx.send(msg).is_err() {
            tracing::debug!("vault worker is gone; dropping an index update");
        }
    }

    /// Wait until the worker has taken in every message posted before this one.
    ///
    /// The index has one writer and it is the worker, so a caller that has just written files is
    /// ahead of it: the updates are posted and the write returns. Anyone whose very next move is
    /// to read those files back *out of the index* — the Search pane asking its question again
    /// after a Replace All — waits here rather than reading rows that are one batch old.
    ///
    /// The caller must already be off the main loop: the worker may be in the middle of a walk,
    /// and this then waits for the batch that walk is on. A worker that has gone answers nothing,
    /// which is no reason to fail a write that already reached the disk.
    fn settle_index(&self) {
        let (reply, answer) = channel();
        if self.tx.send(Msg::Settled(reply)).is_err() || answer.recv().is_err() {
            tracing::debug!("vault worker is gone; not waiting for the index");
        }
    }
}

impl Drop for Local {
    /// Stop the worker before the vault goes away, so no thread outlives the window that opened it.
    ///
    /// A walk in progress stops as [`stop_indexing`](Self::stop_indexing) stops it, keeping what
    /// it wrote, so closing never waits for a cold index; the next open carries on from there.
    ///
    /// ponytail: the stopped walk still resolves the links of what it indexed (~0.3 s at 19 000
    /// files of `make vault`). Skipping it would need the index to remember that its links are
    /// stale, since the next walk reads those files as unchanged and resolves only what it adds.
    fn drop(&mut self) {
        // First, so the walk winds down while the language servers stop.
        self.stop.store(worker::CLOSE, Ordering::Relaxed);
        // Before the worker, because a provider is still sending diagnostics down its channel.
        self.lang.shutdown();
        let _ = self.tx.send(Msg::Shutdown);
        if let Some(handle) = self.worker.take() {
            let _ = handle.join();
        }
    }
}
