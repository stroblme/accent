//! Local vault storage: three SQLite connections (four in a watched vault, whose PDFs' text a pass
//! of its own reads), one writing worker and shared path validation. [`Local`] also serves remote RPC requests through
//! `accent-cli serve`; [`files`] handles files and [`index`] handles queries.

use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;

use crate::Result;

use accent_core::index::{Index, InterruptHandle};

use crate::paths::outside;
use crate::worker::{self, Msg};
use crate::{Event, VaultConfig, debug, kernel, language, locked};

mod files;
mod ignored;
mod index;
mod pdf_text;

pub(crate) use ignored::{Ignored, Listing};
pub(crate) use index::list_dir;

/// One open vault: the index, the watcher, and the worker thread that owns both writers.
pub(crate) struct Local {
    root: PathBuf,
    /// Caller read connection. The mutex makes `Local` `Send + Sync`,
    /// including for UniFFI callers.
    index: Mutex<Index>,
    /// Dedicated search reader so long regex scans on a worker cannot hold
    /// the mutex needed by main-thread `list_dir` and `backlinks`.
    search: Mutex<Index>,
    /// The Search pane's turn on `search`: how many of its queries have been asked, and whether
    /// one of them is running there now ([`Local::supersede`]).
    pane: Mutex<(u64, bool)>,
    /// Stops the statement running on `search`.
    interrupt: InterruptHandle,
    cfg: Mutex<VaultConfig>,
    /// The language providers answering for the open documents.
    pub(crate) lang: std::sync::Arc<language::Languages>,
    /// The programs being debugged, each under its adapter.
    pub(crate) debug: Arc<debug::Debugs>,
    /// The notebooks' kernels.
    pub(crate) kernels: Arc<kernel::Kernels>,
    tx: Sender<Msg>,
    /// Scan-interrupt flag: [`worker::PAUSE`] or [`worker::CLOSE`]. The inbox
    /// is unavailable during scanning and read only between write batches,
    /// so interruptions cannot use messages.
    stop: Arc<AtomicU8>,
    worker: Option<JoinHandle<()>>,
    /// The pass reading the PDFs' text behind the walk, in a vault that stays open (a watched one).
    pdf_text: Option<pdf_text::PdfText>,
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
            .ok_or_else(|| {
                accent_core::Error::Invalid(format!("{} is not a folder", root.display()))
            })?;
        // The reader opens first because it is the connection that may drop and recreate the
        // schema, or build anew an index it cannot read; the worker's must never see the
        // database half-built.
        let (index, damage) = Index::open_or_rebuild(db)?;
        let search = Index::open(db)?;
        let writer = Index::open(db)?;

        let (tx, rx) = channel::<Msg>();
        let (events, event_rx) = channel::<Event>();
        // Said once, as the worker says its own failures: the walk that follows is a first index.
        if let Some(damage) = damage {
            let _ = events.send(Event::Error(format!(
                "the index could not be read and is built anew: {damage}"
            )));
        }
        // Only where the vault stays open: a one-shot `accent-cli` query is over before it would
        // have read a page, and a phone has nothing to read the text with.
        let pdf_text = match watch {
            true => Some(pdf_text::PdfText::spawn(
                Index::open(db)?,
                events.clone(),
                accent_core::pdf_text::extractor(),
            )?),
            false => None,
        };
        // The providers send their diagnostics down the same channel the worker's events use.
        let lang = language::Languages::new(root.clone(), db.to_path_buf(), events.clone());
        let debug = Arc::new(debug::Debugs::new(root.clone(), events.clone()));
        let kernels = Arc::new(kernel::Kernels::new(root.clone(), events.clone()));
        let stop = Arc::new(AtomicU8::new(worker::RUN));
        let handle = worker::spawn(
            root.clone(),
            writer,
            rx,
            tx.clone(),
            events,
            watch,
            stop.clone(),
            lang.ignored.clone(),
            pdf_text.as_ref().map(pdf_text::PdfText::waker),
        )?;

        Ok((
            Local {
                root,
                index: Mutex::new(index),
                interrupt: search.interrupt_handle(),
                search: Mutex::new(search),
                pane: Mutex::new((0, false)),
                cfg: Mutex::new(cfg),
                lang,
                debug,
                kernels,
                tx,
                stop,
                worker: Some(handle),
                pdf_text,
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

    pub fn set_words(&self, on: bool) {
        self.lang.set_words(on);
    }

    /// Ask for a full walk: after a resume, or when the UI suspects it missed something.
    pub fn rescan(&self) {
        self.post(Msg::Rescan(String::new()));
    }

    /// Ask for a walk of the folder `dir` alone, which the reader suspects the index has fallen
    /// behind on. A plain vault-relative path: the walk starts where it names.
    pub fn rescan_dir(&self, dir: &str) -> Result<()> {
        if !Path::new(dir)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
        {
            return Err(outside(dir));
        }
        self.post(Msg::Rescan(dir.to_string()));
        Ok(())
    }

    /// Pause the walk while keeping indexed rows. The vault remains usable
    /// and reports [`ReconcileStats::stopped`]. Resume explicitly with
    /// [`resume_indexing`](Self::resume_indexing) or reopen to index the remainder;
    /// other worker triggers cannot restart it.
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

    /// See [`Vault::stop_pdf_text`](crate::Vault::stop_pdf_text).
    pub fn stop_pdf_text(&self) -> Result<()> {
        if let Some(pass) = &self.pdf_text {
            pass.stop();
        }
        Ok(())
    }

    /// See [`Vault::watch_unindexed`](crate::Vault::watch_unindexed).
    pub fn watch_unindexed(&self, dirs: &[String]) -> Result<()> {
        self.post(Msg::WatchUnindexed(dirs.to_vec(), true));
        Ok(())
    }

    /// See [`Vault::keep_opened`](crate::Vault::keep_opened).
    pub fn keep_opened(&self, rel: &str) -> Result<()> {
        self.post(Msg::KeepOpened(rel.to_string()));
        Ok(())
    }

    pub fn unwatch_unindexed(&self, dirs: &[String]) -> Result<()> {
        self.post(Msg::WatchUnindexed(dirs.to_vec(), false));
        Ok(())
    }

    /// Resolve `rel` inside the root, validating every path-taking operation,
    /// including template, MCP and Android input. Check lexically rather than
    /// canonicalizing so deliberately linked external directories remain readable.
    pub fn resolve(&self, rel: &str) -> Result<PathBuf> {
        Local::join(&self.root, rel)
    }

    /// Resolve a bulk-rewrite target and reject symlink escapes with one
    /// `canonicalize`. Reads may follow linked external folders, but Replace All,
    /// move rewrites and page edits must not write outside the vault indirectly.
    pub(crate) fn resolve_inside(&self, rel: &str) -> Result<PathBuf> {
        let path = self.resolve(rel)?;
        let real = path
            .canonicalize()
            .map_err(|e| accent_core::Error::io(rel, e))?;
        match real.starts_with(&self.root) {
            true => Ok(path),
            false => Err(outside(rel)),
        }
    }

    /// [`resolve`](Self::resolve) against any root, so a remote vault can do the same arithmetic
    /// with the root the server reported.
    pub fn join(root: &Path, rel: &str) -> Result<PathBuf> {
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

    /// Run a search query, interrupting the previous query and skipping stale
    /// waiters so rows for the latest typing appear promptly. Git directory
    /// and Replace All file-list queries share the connection but are not cancelled.
    fn supersede<T>(
        &self,
        query: impl FnOnce(&Index) -> accent_core::Result<T>,
    ) -> accent_core::Result<T> {
        let mine = {
            let mut pane = locked(&self.pane);
            // Under the lock, so the query it reaches is still the one holding the connection.
            if pane.1 {
                self.interrupt.interrupt();
            }
            pane.0 += 1;
            pane.0
        };
        let index = self.searcher();
        {
            let mut pane = locked(&self.pane);
            if pane.0 != mine {
                return Err(accent_core::Error::Cancelled);
            }
            pane.1 = true;
        }
        let answer = query(&index);
        locked(&self.pane).1 = false;
        answer
    }

    fn post(&self, msg: Msg) {
        if self.tx.send(msg).is_err() {
            tracing::debug!("vault worker is gone; dropping an index update");
        }
    }

    /// Wait until the worker indexes every preceding message, so queries after
    /// file writes or Replace All see current rows. Call off the main loop: a
    /// scan can delay the reply until its current batch ends. A dead worker
    /// does not fail a write that already reached disk.
    pub(crate) fn settle_index(&self) {
        let (reply, answer) = channel();
        if self.tx.send(Msg::Settled(reply)).is_err() || answer.recv().is_err() {
            tracing::debug!("vault worker is gone; not waiting for the index");
        }
    }

    /// End services while the closing window still holds the vault. Drop calls this again if
    /// another reference outlives the window; each shutdown is safe to repeat.
    pub(crate) fn shutdown_services(&self) {
        self.stop.store(worker::CLOSE, Ordering::Relaxed);
        self.stop_pdf_text().ok();
        self.debug.shutdown();
        self.kernels.shutdown();
        self.lang.shutdown();
    }
}

impl Drop for Local {
    /// Stop and join the worker before dropping the vault. Interrupt active
    /// scans while retaining rows for the next open. Stopped scans still resolve
    /// links for indexed files; skipping this would leave stale links that the
    /// next diff would not revisit.
    fn drop(&mut self) {
        // Before the worker, because a provider is still sending diagnostics down its channel.
        self.shutdown_services();
        let _ = self.tx.send(Msg::Shutdown);
        if let Some(handle) = self.worker.take() {
            let _ = handle.join();
        }
    }
}
