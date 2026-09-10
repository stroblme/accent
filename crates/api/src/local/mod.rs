//! The vault on this machine: three SQLite connections, the worker that owns the writing one,
//! and the path arithmetic every method here goes through.
//!
//! [`Local`] is what a local [`Vault`](crate::Vault) is, and it is also the whole of what
//! `accent-cli serve` answers with on a remote host — the rpc dispatch calls exactly these
//! methods. The file operations are in [`files`], the index reads in [`index`].

use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Mutex, MutexGuard};
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
    worker: Option<JoinHandle<()>>,
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
        let handle = worker::spawn(root.clone(), writer, rx, tx.clone(), events)?;

        Ok((
            Local {
                root,
                index: Mutex::new(index),
                search: Mutex::new(search),
                cfg: Mutex::new(cfg),
                lang,
                tx,
                worker: Some(handle),
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
        self.post(Msg::Rescan);
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
}

impl Drop for Local {
    /// Stop the worker before the vault goes away, so no thread outlives the window that opened it.
    ///
    /// ponytail: the join waits for whatever the worker is doing, and a cold reconcile of a large
    /// vault takes seconds. Give `reconcile` a cancellation flag if closing a window ever stalls.
    fn drop(&mut self) {
        // Before the worker, because a provider is still sending diagnostics down its channel.
        self.lang.shutdown();
        let _ = self.tx.send(Msg::Shutdown);
        if let Some(handle) = self.worker.take() {
            let _ = handle.join();
        }
    }
}
