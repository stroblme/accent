//! Disk to rows: the full walk and the watcher's single-file path share one `upsert`.

use super::links::{BEST_FILE, resolve_links_of};
use super::schema::{BATCH, MAX_INDEXED_BODY, OPENED_CAP};
use super::{Change, Index, Phase, Progress, ReconcileStats};
use crate::note::Syntax;
use crate::query::Doc;
use crate::walk::{self, FileKind, ScanOptions};
use crate::{Error, Result};
use crate::{markdown, path};
use rusqlite::{OptionalExtension, params};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

/// Scan-progress interval, frequent enough to show a long walk advancing while avoiding extra
/// reports for quick scans.
const SCAN_TICK: Duration = Duration::from_millis(250);

/// The file text a walk holds at once in each of its two stages: the files its threads read and
/// parse or that wait to be written, and the bodies of the batch being written. That is four
/// notes at [`MAX_TEXT`](crate::fs::MAX_TEXT), while files of a megabyte still keep every thread
/// busy.
const ALLOWANCE: u64 = 4 * crate::fs::MAX_TEXT;

impl Index {
    /// Walk `root` and bring the index in line with it. Returns what changed.
    pub fn reconcile(
        &mut self,
        root: &Path,
        mut on_progress: impl FnMut(Progress),
    ) -> Result<ReconcileStats> {
        self.reconcile_with(root, "", &ScanOptions::default(), &|| false, |_, p| {
            on_progress(p)
        })
    }

    /// Reconcile one folder with explicit walk options. `on_progress` receives the index between
    /// batches, with no transaction open, so it may write during a long walk.
    ///
    /// `stop` is checked for each scanned entry and write batch. Stopping preserves written rows;
    /// the next reconcile resumes by diffing the vault. [`ReconcileStats::stopped`] identifies a
    /// partial result.
    ///
    /// `dir` is vault-relative; `""` walks the whole vault. A folder walk changes no rows outside
    /// that folder. Paths already indexed outside it remain aliases, even if a full walk would
    /// choose a different path. A missing or excluded folder loses its indexed descendants. A
    /// linked folder uses the traversal rules of the pass that follows the link.
    pub fn reconcile_with(
        &mut self,
        root: &Path,
        dir: &str,
        opts: &ScanOptions,
        stop: &(dyn Fn() -> bool + Sync),
        mut on_progress: impl FnMut(&mut Index, Progress),
    ) -> Result<ReconcileStats> {
        let canonical_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        let top = match dir {
            "" => None,
            dir => match walk::stat_one(root, dir) {
                Ok(Some(top)) if top.kind == FileKind::Dir => Some(top),
                Ok(_) => None,
                Err(Error::NotFound(_)) => None,
                Err(e) => return Err(e),
            },
        };
        match &top {
            Some(top) if !top.canonical.starts_with(&canonical_root) => {
                return self.reconcile_with(root, "", opts, stop, on_progress);
            }
            None if !dir.is_empty() => {
                return Ok(ReconcileStats {
                    dir: dir.to_string(),
                    removed: self.remove_file_batched(dir)?,
                    ..Default::default()
                });
            }
            _ => {}
        }
        let linked = self.linked_outside(&canonical_root, dir)?;
        let t_scan = Instant::now();
        // The scan writes nothing and has no total until it ends, which on a large vault over
        // slow storage (Android's shared storage) is seconds. So it walks on a thread of its own,
        // and this one says how many files it has found every [`SCAN_TICK`] until it answers.
        let found = AtomicUsize::new(0);
        let scan = std::thread::scope(|s| {
            let (tx, rx) = mpsc::channel();
            let found = &found;
            let walk = s.spawn(move || {
                let _ = tx.send(walk::scan_until(root, dir, &linked, opts, stop, found));
            });
            loop {
                match rx.recv_timeout(SCAN_TICK) {
                    Ok(scan) => break scan,
                    Err(RecvTimeoutError::Timeout) => on_progress(
                        self,
                        Progress {
                            phase: Phase::Scan,
                            done: found.load(Ordering::Relaxed),
                            total: 0,
                        },
                    ),
                    // Only a walk that panicked hangs up without answering.
                    Err(RecvTimeoutError::Disconnected) => {
                        std::panic::resume_unwind(walk.join().unwrap_err())
                    }
                }
            }
        });
        let mut scan = scan;
        if let Some(top) = top {
            self.alias_held_outside(dir, &mut scan)?;
            scan.files.insert(0, top);
        }
        self.add_opened(root, dir, &mut scan)?;
        let mut stats = ReconcileStats {
            dir: dir.to_string(),
            scanned: scan.files.len(),
            aliases: scan.aliases.len(),
            conflicts: scan
                .files
                .iter()
                .filter(|f| f.kind == FileKind::Conflict)
                .count(),
            skipped_symlinks: scan
                .skipped
                .iter()
                .filter(|s| {
                    !matches!(
                        s.reason,
                        walk::SkipReason::DependencyTree | walk::SkipReason::GitIgnored
                    )
                })
                .count(),
            scan_ms: t_scan.elapsed().as_millis() as u64,
            ..Default::default()
        };
        // A scan cut short is the one result that cannot be used at all: the files it never
        // reached are missing from `scan.files`, and the diff below reads everything missing as a
        // deletion. So it is dropped whole — nothing written, nothing removed, the index exactly
        // as the last full walk left it — and the batch loop is where a stop keeps its work.
        if stop() {
            stats.stopped = true;
            return Ok(stats);
        }
        stats.ignored_dirs = scan
            .skipped
            .iter()
            .filter(|s| s.reason == walk::SkipReason::GitIgnored)
            .filter_map(|s| {
                let rel = s.path.strip_prefix(root).ok()?;
                Some(rel.to_string_lossy().into_owned())
            })
            .collect();
        on_progress(
            self,
            Progress {
                phase: Phase::Scan,
                done: scan.files.len(),
                total: scan.files.len(),
            },
        );

        // One pass over `files` into memory. 47k rows of five integers is a few MB and turns the
        // per-file diff into a hash lookup instead of a query.
        let mut existing: HashMap<String, (i64, i64, i64, i64)> = HashMap::new();
        let (lo, hi) = path::subtree_range(dir);
        {
            // Two statements, as one `?1 = '' OR …` would scan the table for a folder too.
            let mut st = self.conn.prepare(match dir {
                "" => "SELECT id, rel_path, mtime_ns, size, ino FROM files",
                _ => {
                    "SELECT id, rel_path, mtime_ns, size, ino FROM files
                      WHERE rel_path = ?1 OR (rel_path >= ?2 AND rel_path < ?3)"
                }
            })?;
            let mut rows = match dir {
                "" => st.query([])?,
                _ => st.query(params![dir, lo, hi])?,
            };
            while let Some(r) = rows.next()? {
                existing.insert(r.get(1)?, (r.get(0)?, r.get(2)?, r.get(3)?, r.get(4)?));
            }
        }

        // Nothing to compare against: the cold build, which resolves its links in one pass at
        // the end rather than file by file. Never a folder's walk, the rest of the vault's links
        // being resolved already.
        let cold = dir.is_empty() && existing.is_empty();

        // The scanned entries to write, by index.
        let mut jobs: Vec<usize> = Vec::new();
        for (idx, f) in scan.files.iter().enumerate() {
            match existing.remove(&f.rel_path) {
                // `remove` doubles as the "seen" marker: leftovers are deletions.
                Some((_, mtime_ns, size, ino))
                    if mtime_ns == f.mtime_ns && size == f.size as i64 && ino == f.ino as i64 =>
                {
                    stats.unchanged += 1;
                }
                _ => jobs.push(idx),
            }
        }
        let removed: Vec<i64> = existing.values().map(|(id, ..)| *id).collect();
        stats.removed = removed.len();

        let dirty = !jobs.is_empty() || !removed.is_empty();
        let total = jobs.len();

        if !removed.is_empty() {
            let tx = self.write_tx()?;
            for id in &removed {
                delete_file_rows(&tx, *id)?;
            }
            write_bodies(&tx)?;
            tx.commit()?;
        }

        // The files are read and parsed on a pool, and written here as the walk found them: the
        // one connection writing is the one writer SQLite takes, and the walk's order is what
        // writes a folder's row before its children's.
        let mut done = 0usize;
        if cold {
            self.sync_writes(false)?;
        }
        let written = in_order(
            &jobs,
            |&job| read_size(&scan.files[job]),
            |&job| read_content(&scan.files[job]),
            |contents| -> Result<()> {
                let mut jobs = jobs.iter().peekable();
                while jobs.peek().is_some() {
                    let tx = self.write_tx()?;
                    // The files whose content really changed: a touched one kept its links and
                    // its keys.
                    let mut changed = Vec::new();
                    // A batch's bodies wait in memory for `write_bodies`, so a batch also ends
                    // once they reach the allowance.
                    let mut bodies = 0;
                    for (&job, content) in jobs.by_ref().take(BATCH).zip(&mut *contents) {
                        // Per file rather than per batch: a batch is 500 files, and one of 500
                        // large ones measured 2.5 s on the bench vault — a Stop that waits that
                        // long is not a Stop. The transaction still commits: every file in it is
                        // a whole row either way.
                        if stop() {
                            stats.stopped = true;
                            break;
                        }
                        let touched = stats.touched;
                        let f = &scan.files[job];
                        bodies += content.text.as_ref().map_or(0, String::len) as u64;
                        upsert(&tx, f, content, &mut stats)?;
                        if stats.touched == touched {
                            changed.push(&f.rel_path);
                        }
                        if f.kind != FileKind::Dir {
                            stats.changed.push(f.rel_path.clone());
                        }
                        done += 1;
                        if bodies >= ALLOWANCE {
                            break;
                        }
                    }
                    write_bodies(&tx)?;
                    // What these files can have changed is resolved in the transaction that
                    // wrote them, so no reader between two batches sees a changed note link to
                    // nothing. A note a later batch adds takes the links waiting for it then:
                    // they are the ones its keys name. The cold build has no earlier answer to
                    // keep and resolves once, at the end.
                    if !cold {
                        for rel in changed {
                            resolve_links_of(&tx, rel)?;
                        }
                    }
                    tx.commit()?;
                    on_progress(
                        self,
                        Progress {
                            phase: Phase::Index,
                            done,
                            total,
                        },
                    );
                    if stats.stopped {
                        break;
                    }
                }
                Ok(())
            },
        );
        // Synced again however the walk ended, stopped, failed or done.
        if cold {
            self.sync_writes(true)?;
        }
        written?;

        if dirty {
            // Aliases are a handful of rows; rewriting them beats diffing them.
            let tx = self.write_tx()?;
            tx.execute(
                "DELETE FROM aliases WHERE ?1 = '' OR (rel_path >= ?2 AND rel_path < ?3)",
                params![dir, lo, hi],
            )?;
            for a in &scan.aliases {
                tx.prepare_cached(
                    "INSERT OR REPLACE INTO aliases(rel_path, file_id)
                     SELECT ?1, id FROM files WHERE rel_path = ?2",
                )?
                .execute(params![a.rel_path, a.target_rel_path])?;
            }
            tx.commit()?;
        }
        // A removed file re-pointed its incoming links as it went, and a warm walk's batches
        // resolved their own.
        if cold {
            self.resolve_links()?;
            on_progress(
                self,
                Progress {
                    phase: Phase::Resolve,
                    done: total,
                    total,
                },
            );
        }

        Ok(stats)
    }

    /// Set whether commits wait for disk sync. Initial indexing skips syncs for speed. It holds
    /// [`unsynced_marker`](super::unsynced_marker) locked and persists its name before unsynced
    /// writes. After a crash, [`Index::open_or_rebuild`] checks an unlocked marker and rebuilds
    /// a damaged index. Re-enabling sync persists the index before removing the marker.
    fn sync_writes(&mut self, sync: bool) -> Result<()> {
        let db = PathBuf::from(self.conn.path().unwrap_or_default());
        let marker = super::unsynced_marker(&db);
        let io = |path: &Path, e| Error::io(path.display(), e);
        if sync {
            self.conn.pragma_update(None, "synchronous", "NORMAL")?;
            for path in [db.clone(), super::beside(&db, "-wal")] {
                if let Ok(file) = std::fs::File::open(&path) {
                    file.sync_all().map_err(|e| io(&path, e))?;
                }
            }
            if self.unsynced.take().is_some() {
                std::fs::remove_file(&marker).map_err(|e| io(&marker, e))?;
            }
        } else {
            let file = std::fs::File::create(&marker).map_err(|e| io(&marker, e))?;
            file.lock().map_err(|e| io(&marker, e))?;
            // The marker's name in its folder is on the disk too, or it may not be found.
            if let Some(dir) = marker.parent() {
                std::fs::File::open(dir)
                    .and_then(|d| d.sync_all())
                    .map_err(|e| io(dir, e))?;
            }
            self.unsynced = Some(file);
            self.conn.pragma_update(None, "synchronous", "OFF")?;
        }
        Ok(())
    }

    /// The canonical folders the index reaches through a link outside the folder `dir`, which a
    /// link inside it must not bring in a second time. None for the whole vault, whose walk finds
    /// them itself.
    fn linked_outside(&self, canonical_root: &Path, dir: &str) -> Result<Vec<PathBuf>> {
        if dir.is_empty() {
            return Ok(Vec::new());
        }
        let (lo, hi) = path::subtree_range(dir);
        let mut st = self.conn.prepare(
            "SELECT canonical FROM files
              WHERE kind = ?1 AND rel_path <> ?2 AND (rel_path < ?3 OR rel_path >= ?4)",
        )?;
        let rows = st.query_map(params![FileKind::Dir.as_i64(), dir, lo, hi], |r| {
            r.get::<_, String>(0)
        })?;
        let mut linked = Vec::new();
        for canonical in rows {
            let canonical = PathBuf::from(canonical?);
            if !canonical.starts_with(canonical_root) {
                linked.push(canonical);
            }
        }
        Ok(linked)
    }

    /// Add to `scan` the opened files under `dir` it did not walk to
    /// ([`keep_opened`](Self::keep_opened)), so the diff keeps them; one gone or unreadable stays out,
    /// and the diff takes its row and its place.
    fn add_opened(&self, root: &Path, dir: &str, scan: &mut walk::ScanResult) -> Result<()> {
        let opened: Vec<String> = {
            let mut st = self.conn.prepare("SELECT rel_path FROM opened")?;
            let rows = st.query_map([], |r| r.get(0))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        let walked: HashSet<String> = scan.files.iter().map(|f| f.rel_path.clone()).collect();
        let (under, _) = path::subtree_range(dir);
        for rel in opened {
            if !(dir.is_empty() || rel.starts_with(&under)) || walked.contains(&rel) {
                continue;
            }
            if let Ok(Some(meta)) = walk::stat_opened(root, &rel) {
                scan.files.push(meta);
            }
        }
        Ok(())
    }

    /// Turn what a folder's `scan` found that the index holds by a path outside the folder into an
    /// alias of that path: the vault-wide `(dev, ino)` dedup, which a walk of the folder alone
    /// cannot make.
    fn alias_held_outside(&self, dir: &str, scan: &mut walk::ScanResult) -> Result<()> {
        let (lo, hi) = path::subtree_range(dir);
        let mut st = self.conn.prepare(
            "SELECT rel_path FROM files
              WHERE dev = ?1 AND ino = ?2 AND rel_path <> ?3 AND (rel_path < ?4 OR rel_path >= ?5)",
        )?;
        let mut kept = Vec::with_capacity(scan.files.len());
        for f in std::mem::take(&mut scan.files) {
            let held: Option<String> = st
                .query_row(params![f.dev as i64, f.ino as i64, dir, lo, hi], |r| {
                    r.get(0)
                })
                .optional()?;
            match held {
                Some(target_rel_path) => scan.aliases.push(walk::Alias {
                    rel_path: f.rel_path,
                    target_rel_path,
                    canonical: f.canonical,
                }),
                None => kept.push(f),
            }
        }
        scan.files = kept;
        Ok(())
    }

    /// Update one vault-relative path after a watcher event. Matching `(mtime_ns, size, ino)`
    /// leaves the row untouched and returns [`Change::Unchanged`], avoiding reloads on save
    /// echoes.
    ///
    /// This path does not deduplicate `(dev, ino)`; duplicate symlink entries remain until full
    /// reconciliation.
    pub fn update_file(&mut self, root: &Path, rel: &str) -> Result<Change> {
        let meta = match self.stat(root, rel) {
            Ok(Some(meta)) => meta,
            Ok(None) => return Ok(Change::Ignored),
            Err(Error::NotFound(_)) => {
                self.remove_file_batched(rel)?;
                return Ok(Change::Removed);
            }
            Err(e) => return Err(e),
        };
        self.write_one(&meta)
    }

    /// Keep `rel`, an opened file, in the index although the walk leaves it out or search
    /// excludes it: the last [`OPENED_CAP`] opened are kept, and a walk, an edit and a git refresh
    /// keep them. A file the index holds and searches already takes no place.
    pub fn keep_opened(&mut self, root: &Path, rel: &str) -> Result<()> {
        let walked = match walk::stat_one(root, rel) {
            Err(Error::NotFound(_)) => return Ok(()),
            walked => walked?,
        };
        if walked.is_some() && !self.excluded(rel)? && !self.is_opened(rel)? {
            return Ok(());
        }
        let Some(meta) = walk::stat_opened(root, rel)? else {
            return Ok(());
        };
        self.write_one(&meta)?;
        let tx = self.write_tx()?;
        tx.execute(
            "UPDATE files SET git_ignored = 0 WHERE rel_path = ?1",
            [rel],
        )?;
        tx.execute(
            "INSERT INTO opened(rel_path, seq) VALUES (?1, (SELECT COALESCE(MAX(seq), 0) + 1 FROM opened))
             ON CONFLICT(rel_path) DO UPDATE SET seq = excluded.seq",
            [rel],
        )?;
        let dropped: Vec<String> = {
            let mut st =
                tx.prepare("SELECT rel_path FROM opened ORDER BY seq DESC LIMIT -1 OFFSET ?1")?;
            let rows = st.query_map([OPENED_CAP as i64], |r| r.get(0))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        for gone in &dropped {
            tx.execute("DELETE FROM opened WHERE rel_path = ?1", [gone])?;
        }
        tx.commit()?;
        // A gitignored file the walk holds keeps its row, which the next git refresh excludes.
        for gone in &dropped {
            if matches!(walk::stat_one(root, gone), Ok(None)) {
                self.remove_file_batched(gone)?;
            }
        }
        Ok(())
    }

    pub fn is_opened(&self, rel: &str) -> Result<bool> {
        let mut st = self
            .conn
            .prepare_cached("SELECT 1 FROM opened WHERE rel_path = ?1")?;
        Ok(st.exists([rel])?)
    }

    /// Whether search leaves `rel`'s row out (`set_excluded`).
    fn excluded(&self, rel: &str) -> Result<bool> {
        let mut st = self
            .conn
            .prepare_cached("SELECT 1 FROM files WHERE rel_path = ?1 AND git_ignored <> 0")?;
        Ok(st.exists([rel])?)
    }

    /// [`walk::stat_one`], and for an opened file the stat it refuses
    /// ([`keep_opened`](Self::keep_opened)): whether the index holds `rel`, and what its row reads.
    pub fn stat(&self, root: &Path, rel: &str) -> Result<Option<walk::FileMeta>> {
        match walk::stat_one(root, rel)? {
            None if self.is_opened(rel)? => walk::stat_opened(root, rel),
            meta => Ok(meta),
        }
    }

    /// Write one stat'd file's rows, unless its stat is the row's already.
    fn write_one(&mut self, meta: &walk::FileMeta) -> Result<Change> {
        let rel = meta.rel_path.as_str();
        let existing: Option<(i64, i64, i64, i64)> = self
            .conn
            .prepare_cached("SELECT id, mtime_ns, size, ino FROM files WHERE rel_path = ?1")?
            .query_row([rel], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .optional()?;
        if let Some((_, mtime_ns, size, ino)) = existing
            && mtime_ns == meta.mtime_ns
            && size == meta.size as i64
            && ino == meta.ino as i64
        {
            return Ok(Change::Unchanged);
        }

        let existing_id = existing.map(|(id, ..)| id);
        let content = read_content(meta);
        let tx = self.write_tx()?;
        upsert(&tx, meta, content, &mut ReconcileStats::default())?;
        write_bodies(&tx)?;
        // The file's own links were just written unresolved, and a new note can be what a link
        // written long before it existed was waiting for — or a shorter path for one that
        // resolved deeper. Both are the links its keys name, and nothing else moved. In the same
        // transaction, or a reader in between sees the note link to nothing: a page edit asking
        // for the backlinks of a PDF right after the last one rewrote a note would skip it.
        resolve_links_of(&tx, rel)?;
        tx.commit()?;
        Ok(match existing_id {
            Some(_) => Change::Updated(meta.kind),
            None => Change::Added(meta.kind),
        })
    }

    /// Drop `rel` and everything below it. A directory removal arrives as one event, so the
    /// subtree has to go with it. Returns how many rows went.
    ///
    /// The links that pointed into the subtree are re-resolved as each row goes
    /// ([`delete_file_rows`]), so there is nothing for a batch caller to defer.
    pub fn remove_file_batched(&mut self, rel: &str) -> Result<usize> {
        let (lo, hi) = path::subtree_range(rel);
        let ids: Vec<i64> = {
            let mut st = self.conn.prepare_cached(
                "SELECT id FROM files WHERE rel_path = ?1 OR (rel_path >= ?2 AND rel_path < ?3)",
            )?;
            let rows = st.query_map(params![rel, lo, hi], |r| r.get(0))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        if ids.is_empty() {
            return Ok(0);
        }

        let tx = self.write_tx()?;
        for id in &ids {
            delete_file_rows(&tx, *id)?;
        }
        write_bodies(&tx)?;
        tx.commit()?;
        Ok(ids.len())
    }
}

/// Run `work` over `items` on a pool of threads, one per core, and hand `consume` the results in
/// the order of `items` as they come in. At most two a thread wait to be consumed, and together
/// with the items being worked on they hold no more than [`ALLOWANCE`] of `size`, so a slow
/// consumer holds the pool back rather than piling its results up in memory. Once `consume`
/// returns, the pool stops after the items it is working on.
fn in_order<T: Sync, R: Send, O>(
    items: &[T],
    size: impl Fn(&T) -> u64 + Sync,
    work: impl Fn(&T) -> R + Sync,
    consume: impl FnOnce(&mut dyn Iterator<Item = R>) -> O,
) -> O {
    let threads = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(items.len());
    let allowance = Allowance {
        left: Mutex::new(ALLOWANCE),
        freed: Condvar::new(),
    };
    // A receiver per item, queued in the order of `items` as each is taken, which its result is
    // sent to once it is worked out: the queue is the order, and its bound is the memory bound.
    let (order, results) = mpsc::sync_channel::<mpsc::Receiver<(R, Held<'_>)>>(2 * threads);
    let next = Mutex::new((items.iter(), order));
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                loop {
                    let (item, held, result) = {
                        let mut next = next.lock().unwrap();
                        let (items, order) = &mut *next;
                        let Some(item) = items.next() else { break };
                        // Taken in the order `consume` gives the bytes back in, so whatever this
                        // waits for is ahead of it and is consumed without it.
                        let held = allowance.take(size(item));
                        let (result, receiver) = mpsc::sync_channel(1);
                        // Hung up: `consume` is done.
                        if order.send(receiver).is_err() {
                            break;
                        }
                        (item, held, result)
                    };
                    let _ = result.send((work(item), held));
                }
            });
        }
        // Moved in, so that it hangs up on the pool as `consume` returns, before the scope waits
        // for the threads, and gives back the bytes of what was never consumed. Its sender lives
        // in `next`, so the queue never ends of itself: `take` ends it. A result that never comes
        // is a thread's panic, raised here rather than waited for.
        let results = results;
        let mut receivers = results.iter().take(items.len());
        // An item holds its bytes until the next is asked for, by when `consume` is done with
        // it, and gives them back before that wait, which may be for them.
        let mut last = None;
        consume(&mut std::iter::from_fn(|| {
            drop(last.take());
            let (result, held) = receivers
                .next()?
                .recv()
                .expect("a thread of the pool panicked");
            last = Some(held);
            Some(result)
        }))
    })
}

/// What is left of [`ALLOWANCE`] for the items of one [`in_order`].
struct Allowance {
    left: Mutex<u64>,
    freed: Condvar,
}

impl Allowance {
    /// Wait for `bytes`, at most the whole allowance, and hold them until the answer is dropped.
    fn take(&self, bytes: u64) -> Held<'_> {
        let bytes = bytes.min(ALLOWANCE);
        let left = self.left.lock().unwrap();
        let mut left = self.freed.wait_while(left, |left| *left < bytes).unwrap();
        *left -= bytes;
        Held {
            allowance: self,
            bytes,
        }
    }
}

/// The bytes of the [`Allowance`] one item holds, given back when it is dropped.
struct Held<'a> {
    allowance: &'a Allowance,
    bytes: u64,
}

impl Drop for Held<'_> {
    fn drop(&mut self) {
        *self.allowance.left.lock().unwrap() += self.bytes;
        // One thread at most waits: the one holding the queue of items.
        self.allowance.freed.notify_one();
    }
}

/// What a file's content gives its rows, worked out before the transaction that writes them: the
/// part of indexing a file that needs no database, which a walk spreads over a pool of threads.
struct Content {
    /// Of the normalised text, never of the raw file. Safe, because a hash is only ever compared
    /// against an earlier hash of the same file taken the same way.
    hash: Option<blake3::Hash>,
    text: Option<String>,
    analysis: Option<markdown::Analysis>,
    symbols: Vec<crate::code::Decl>,
    /// What `kind:` calls the file, `files.doc`.
    doc: Option<Doc>,
}

/// The bytes [`read_content`] reads of `f`, by the same rules: what it holds of a walk's
/// [`ALLOWANCE`]. A file it leaves unread holds none, however large.
fn read_size(f: &walk::FileMeta) -> u64 {
    let read = match f.kind {
        FileKind::Markdown => true,
        FileKind::Other => {
            path::is_diagram(&f.rel_path)
                || path::is_notebook(&f.rel_path)
                || (f.size <= MAX_INDEXED_BODY && !path::is_binary(&f.rel_path))
        }
        _ => false,
    };
    match read {
        true => f.size.min(crate::fs::MAX_TEXT),
        false => 0,
    }
}

/// Read, hash and parse one scanned entry for [`upsert`]. Use the editor's text reader so indexed
/// byte offsets match its buffer, including normalised CRLF and replacement characters in notes.
/// Lossy source files have no indexed body. Only notes may exceed [`MAX_INDEXED_BODY`]; the walk's
/// stat avoids reading other oversized files. Binaries keep stat-only rows, and so does a PDF
/// until a pass of its own reads its text ([`Index::claim_pdfs`]). Unreadable files also keep
/// their stat rows, with the missing body logged at debug level.
fn read_content(f: &walk::FileMeta) -> Content {
    let mut sniffed_diagram = false;
    let body = || match crate::fs::read_text(&f.canonical) {
        Ok(read) => Some(read),
        Err(e) => {
            tracing::debug!("{}: no body indexed: {e}", f.canonical.display());
            None
        }
    };
    let read = match f.kind {
        FileKind::Markdown => body(),
        // A diagram's XML would make every style key a hit, so what is indexed is its labels,
        // one line each (`accent_drawio::text`). A label is short however big the file is with
        // its pictures, so the cap does not apply.
        FileKind::Other if crate::path::is_diagram(&f.rel_path) => body().and_then(diagram_labels),
        // An `.ipynb`'s JSON would make every output's base64 and every metadata key a hit, so
        // what is indexed is its cells' sources, however big its outputs make the file.
        FileKind::Other if crate::path::is_notebook(&f.rel_path) => {
            body().and_then(|read| notebook_sources(read, f))
        }
        // An `.xml` draw.io wrote is known by its first element once its text is in.
        FileKind::Other if f.size <= MAX_INDEXED_BODY && !path::is_binary(&f.rel_path) => body()
            .and_then(|read| match read {
                crate::fs::Read::Text(t) if crate::path::holds_diagram(&f.rel_path, &t.text) => {
                    sniffed_diagram = true;
                    diagram_labels(crate::fs::Read::Text(t))
                }
                read => Some(read),
            }),
        _ => None,
    };
    let text = match read {
        Some(crate::fs::Read::Text(t)) if f.kind == FileKind::Markdown || !t.lossy => Some(t.text),
        // Vanished or unreadable mid-walk, binary, or over the cap: keep the stat row, drop the
        // content.
        _ => None,
    };
    // Only a note and an org file have links, tags and headings. A `.py`'s `#` comments are not
    // tags and its `#!` line is not a heading, so nothing else reaches an analyser.
    let analysis = match f.kind == FileKind::Markdown || path::is_org(&f.rel_path) {
        true => text.as_deref().map(|t| Syntax::of(&f.rel_path).analyze(t)),
        false => None,
    };
    // A code file's declarations, read from the text its body is.
    let symbols = match (f.kind, text.as_deref()) {
        (FileKind::Other, Some(text)) => crate::code::lang_of(&f.rel_path)
            .map(|lang| crate::code::symbols(lang, text))
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    // A `.py` is a notebook where its text makes marimo's app, an `.xml` a diagram where its
    // first element is draw.io's; anything else is what its name says.
    let doc = match f.kind {
        FileKind::Dir | FileKind::Conflict => None,
        _ if sniffed_diagram => Some(Doc::Diagram),
        _ if text
            .as_deref()
            .is_some_and(|t| path::holds_notebook(&f.rel_path, t)) =>
        {
            Some(Doc::Notebook)
        }
        _ => Doc::named(&f.rel_path),
    };
    Content {
        hash: text.as_deref().map(|t| blake3::hash(t.as_bytes())),
        text,
        analysis,
        symbols,
        doc,
    }
}

/// Replace one scanned entry's file row and everything derived from it with what its
/// [`Content`] says. Returns the file id.
///
/// The one place a file becomes index rows, shared by the full reconcile and the watcher's
/// [`Index::update_file`] so the two can never drift apart.
fn upsert(
    tx: &rusqlite::Transaction<'_>,
    f: &walk::FileMeta,
    content: Content,
    stats: &mut ReconcileStats,
) -> Result<i64> {
    let Content {
        hash,
        text,
        analysis,
        symbols,
        doc,
    } = content;
    // The row it replaces, as this transaction sees it rather than as the caller's diff did:
    // another process sharing the index (`accent-cli mcp` beside the app) may have written it
    // since, and a row taken for new would keep the links and tags it already had twice over.
    let existing_id: Option<i64> = tx
        .prepare_cached("SELECT id FROM files WHERE rel_path = ?1")?
        .query_row([&f.rel_path], |r| r.get(0))
        .optional()?;
    stats.bytes_read += text.as_ref().map_or(0, |t| t.len() as u64);

    // Syncthing preserves origin mtimes, so mtime alone lies both ways; the hash is
    // the arbiter for "did the content really change".
    let same_content = match (existing_id, hash.as_ref()) {
        (Some(id), Some(h)) => {
            let old: Option<Vec<u8>> = tx
                .prepare_cached("SELECT content_hash FROM files WHERE id = ?1")?
                .query_row([id], |r| r.get(0))
                .optional()?
                .flatten();
            old.as_deref() == Some(h.as_bytes().as_slice())
        }
        _ => false,
    };

    if same_content && let Some(id) = existing_id {
        tx.prepare_cached(
            "UPDATE files SET canonical=?2, dev=?3, ino=?4, mtime_ns=?5, size=?6 WHERE id=?1",
        )?
        .execute(params![
            id,
            f.canonical.to_string_lossy(),
            f.dev as i64,
            f.ino as i64,
            f.mtime_ns,
            f.size as i64,
        ])?;
        stats.touched += 1;
        return Ok(id);
    }

    // The `path::stem` fallback is what gives a file the analyser never saw a title.
    let title = analysis
        .as_ref()
        .and_then(|a| a.title.clone())
        .or_else(|| Some(path::stem(&f.rel_path)));

    // `git_ignored` is the parent directory's, and only on an insert: an existing row keeps
    // whatever [`Index::set_excluded`] last said about it, and a new one under a directory that
    // is already excluded has to be excluded too. Git reports a wholly ignored directory as a
    // single entry, so a file created inside one leaves the exclusion set unchanged and no
    // second `set_excluded` is ever written to mark it — it would otherwise stay in search until
    // something unrelated moved the set. A full walk answers the same way: `walk::scan` orders
    // its files shallowest first, so a directory is always upserted before its children.
    let id: i64 = tx
        .prepare_cached(
            "INSERT INTO files(rel_path, parent_dir, canonical, dev, ino, mtime_ns, size, kind, title, content_hash, doc, git_ignored)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,
                    COALESCE((SELECT git_ignored FROM files WHERE rel_path = ?2), 0))
             ON CONFLICT(rel_path) DO UPDATE SET
                parent_dir=excluded.parent_dir,
                canonical=excluded.canonical, dev=excluded.dev, ino=excluded.ino,
                mtime_ns=excluded.mtime_ns, size=excluded.size, kind=excluded.kind,
                title=excluded.title, content_hash=excluded.content_hash, doc=excluded.doc,
                text_pages=NULL
             RETURNING id",
        )?
        .query_row(
            params![
                f.rel_path,
                path::parent_dir(&f.rel_path),
                f.canonical.to_string_lossy(),
                f.dev as i64,
                f.ino as i64,
                f.mtime_ns,
                f.size as i64,
                f.kind.as_i64(),
                title,
                hash.as_ref().map(|h| h.as_bytes().to_vec()),
                doc.map_or(0, Doc::as_i64),
            ],
            |r| r.get(0),
        )?;

    if existing_id.is_some() {
        clear_derived(tx, id)?;
        stats.updated += 1;
    } else {
        stats.added += 1;
    }

    // The names a link reaches this file by. A directory is not a link target. Rewritten rather
    // than kept because a row can change kind under the same path.
    tx.prepare_cached("DELETE FROM file_keys WHERE file_id = ?1")?
        .execute([id])?;
    if f.kind != FileKind::Dir {
        for key in markdown::path_keys(&f.rel_path) {
            tx.prepare_cached("INSERT INTO file_keys(file_id, key) VALUES(?1, ?2)")?
                .execute(params![id, key])?;
        }
    }

    if let Some(a) = analysis.as_ref() {
        for l in &a.links {
            // A wikilink names a note from the vault root; a markdown link names it from the
            // note's own directory, so it is turned into a vault path here, where that
            // directory is known, and resolves by the same rules as everything else.
            let target = match l.kind {
                markdown::LinkKind::Markdown => {
                    path::resolve(path::parent_dir(&f.rel_path), &l.target)
                }
                _ => l.target.clone(),
            };
            tx.prepare_cached(
                "INSERT INTO links(src_file, target, key, resolved_file, kind, anchor, alias, byte_start, byte_end)
                 VALUES(?1,?2,?3,NULL,?4,?5,?6,?7,?8)",
            )?
            .execute(params![
                id,
                target,
                markdown::link_key(&target),
                link_kind_i64(l.kind),
                l.anchor,
                l.alias,
                l.range.start as i64,
                l.range.end as i64,
            ])?;
        }
        for t in &a.tags {
            tx.prepare_cached("INSERT INTO tags(file_id, name, byte_start) VALUES(?1,?2,?3)")?
                .execute(params![id, t.name, t.range.start as i64])?;
        }
        for h in &a.headings {
            tx.prepare_cached(
                "INSERT INTO headings(file_id, level, text, byte_start) VALUES(?1,?2,?3,?4)",
            )?
            .execute(params![id, h.level as i64, h.text, h.range.start as i64])?;
        }
        for name in &a.aliases {
            tx.prepare_cached("INSERT INTO note_aliases(file_id, name) VALUES(?1,?2)")?
                .execute(params![id, name])?;
        }
    }
    if !symbols.is_empty() {
        let test_file = crate::code::is_test_path(&f.rel_path);
        for d in symbols {
            tx.prepare_cached(
                "INSERT INTO symbols(file_id, kind, name, container, byte_start, byte_end, line,
                                     end_line, signature, test)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            )?
            .execute(params![
                id,
                d.kind.as_i64(),
                d.name,
                d.container,
                d.range.start as i64,
                d.range.end as i64,
                d.line,
                d.end_line,
                d.signature,
                d.test || test_file,
            ])?;
        }
    }
    // Set aside for [`write_bodies`], which the caller runs before it commits.
    if let Some(body) = text.as_ref() {
        tx.prepare_cached(
            "INSERT OR REPLACE INTO temp.bodies(file_id, body, title) VALUES(?1,?2,?3)",
        )?
        .execute(params![id, body, title.as_deref().unwrap_or_default()])?;
    }
    Ok(id)
}

/// Apply pending body deletions and insertions together through the FTS triggers. Per-file
/// writes create extra FTS segments at statement savepoints; batching avoids costly segment
/// merges while producing the same index.
pub(super) fn write_bodies(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    tx.prepare_cached("DELETE FROM notes WHERE file_id IN (SELECT file_id FROM temp.bodies)")?
        .execute([])?;
    tx.prepare_cached(
        "INSERT INTO notes(file_id, body, title)
         SELECT file_id, body, title FROM temp.bodies WHERE body IS NOT NULL",
    )?
    .execute([])?;
    tx.prepare_cached("DELETE FROM temp.bodies")?.execute([])?;
    Ok(())
}

/// A diagram read as text, turned into its labels: `None` for one that does not parse or shows
/// no text, which keeps its stat row and nothing to be found by.
fn diagram_labels(read: crate::fs::Read) -> Option<crate::fs::Read> {
    let crate::fs::Read::Text(mut t) = read else {
        return None;
    };
    let file = accent_drawio::File::from_bytes(t.text.as_bytes())
        .inspect_err(|e| tracing::debug!("no labels indexed: {e}"))
        .ok()?;
    t.text = accent_drawio::text::search_text(&file);
    (!t.text.is_empty()).then_some(crate::fs::Read::Text(t))
}

/// An `.ipynb` read as text, turned into its cells' sources (`notebook::search_text`); one that
/// does not read as a notebook is the text it is, under the cap any other file has.
fn notebook_sources(read: crate::fs::Read, f: &walk::FileMeta) -> Option<crate::fs::Read> {
    let crate::fs::Read::Text(mut t) = read else {
        return None;
    };
    match crate::notebook::search_text(&f.rel_path, &t.text) {
        Some(sources) => t.text = sources,
        None if f.size > MAX_INDEXED_BODY => return None,
        None => {}
    }
    Some(crate::fs::Read::Text(t))
}

/// Everything a file's content produced, so a re-read starts clean — and a file that could not
/// be read keeps no stale body to be found by.
///
/// The `notes` row goes with the caller's [`write_bodies`], and is deleted there and inserted
/// afresh rather than `REPLACE`d: REPLACE would only fire the FTS delete trigger with
/// `recursive_triggers` on.
fn clear_derived(tx: &rusqlite::Transaction<'_>, id: i64) -> Result<()> {
    tx.prepare_cached("DELETE FROM links WHERE src_file = ?1")?
        .execute([id])?;
    tx.prepare_cached("DELETE FROM tags WHERE file_id = ?1")?
        .execute([id])?;
    tx.prepare_cached("DELETE FROM headings WHERE file_id = ?1")?
        .execute([id])?;
    tx.prepare_cached("DELETE FROM symbols WHERE file_id = ?1")?
        .execute([id])?;
    tx.prepare_cached("DELETE FROM note_aliases WHERE file_id = ?1")?
        .execute([id])?;
    tx.prepare_cached("INSERT OR REPLACE INTO temp.bodies(file_id) VALUES(?1)")?
        .execute([id])?;
    Ok(())
}

/// Every row of one file, and the links that pointed at it re-resolved: with its keys gone they
/// find the next candidate — a namesake deeper in the vault — or dangle.
pub(super) fn delete_file_rows(tx: &rusqlite::Transaction<'_>, id: i64) -> Result<()> {
    clear_derived(tx, id)?;
    tx.prepare_cached("DELETE FROM aliases WHERE file_id = ?1")?
        .execute([id])?;
    tx.prepare_cached("DELETE FROM file_keys WHERE file_id = ?1")?
        .execute([id])?;
    tx.prepare_cached(&format!(
        "UPDATE links SET resolved_file = {BEST_FILE} WHERE resolved_file = ?1"
    ))?
    .execute([id])?;
    tx.prepare_cached(
        "DELETE FROM opened WHERE rel_path = (SELECT rel_path FROM files WHERE id = ?1)",
    )?
    .execute([id])?;
    tx.prepare_cached("DELETE FROM files WHERE id = ?1")?
        .execute([id])?;
    Ok(())
}

fn link_kind_i64(k: markdown::LinkKind) -> i64 {
    match k {
        markdown::LinkKind::Wiki => 0,
        markdown::LinkKind::Embed => 1,
        markdown::LinkKind::Markdown => 2,
        markdown::LinkKind::External => 3,
    }
}

/// [`link_kind_i64`] read back.
pub(super) fn link_kind_of(k: i64) -> markdown::LinkKind {
    match k {
        0 => markdown::LinkKind::Wiki,
        1 => markdown::LinkKind::Embed,
        2 => markdown::LinkKind::Markdown,
        _ => markdown::LinkKind::External,
    }
}

#[cfg(test)]
mod tests;
