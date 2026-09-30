//! SQLite (FTS5) index of the vault: files, aliases, links, tags, headings.
//!
//! The index is a **disposable cache**: the markdown files are the source of truth. On a schema
//! mismatch we drop everything and rebuild rather than migrate.
//!
//! One `Index` type, its methods spread by concern: `reconcile` turns the disk into rows,
//! `links` resolves and reads them, `files` lists them, `search` queries the text.

mod files;
mod links;
mod reconcile;
mod schema;
mod search;

use crate::walk::FileKind;
use anyhow::{Context, Result};
use rusqlite::functions::FunctionFlags;
use rusqlite::{Connection, OptionalExtension};
use schema::{BODIES, DROP_ALL, SCHEMA, SCHEMA_VERSION};
pub use search::MIN_INFIX;
use search::{folded_find, snippet_window};
use serde::{Deserialize, Serialize};
use std::ops::Range;
use std::path::Path;

pub struct Index {
    conn: Connection,
}

/// `$XDG_CACHE_HOME/accent/<blake3 of the canonical vault path>.db`.
/// The index is disposable, so it belongs in the cache dir, never next to the notes.
pub fn default_db_path(vault: &Path) -> std::path::PathBuf {
    crate::config::xdg("XDG_CACHE_HOME", ".cache")
        .join("accent")
        .join(format!("{}.db", crate::config::vault_hash(vault)))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Phase {
    Scan,
    Index,
    Resolve,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Progress {
    pub phase: Phase,
    pub done: usize,
    /// 0 while the scan is still running: the walk has no total until it ends.
    pub total: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReconcileStats {
    /// Entries the walk produced (files + dirs, aliases excluded).
    pub scanned: usize,
    /// `(mtime_ns, size, ino)` matched the index — not even opened.
    pub unchanged: usize,
    /// New rows.
    pub added: usize,
    /// Content actually differed: re-analysed.
    pub updated: usize,
    /// Stat changed but the blake3 hash matched (Syncthing rewrite): stat fields only.
    pub touched: usize,
    pub removed: usize,
    pub aliases: usize,
    pub conflicts: usize,
    pub skipped_symlinks: usize,
    pub bytes_read: u64,
    pub scan_ms: u64,
    /// The walk was asked to stop and did, so every number above is of a **partial** index: what
    /// is in it is right, what is missing is still on disk. The next reconcile is a diff, so it
    /// finishes the remainder rather than starting over — which is why stopping is a pause.
    pub stopped: bool,
    /// The files, directories aside, whose rows the walk wrote: what a tab open on one may have
    /// read before the walk did. The watcher's news of such a change comes after the walk took
    /// it in, and reads as no change, so this is the only word of it. Not serialized: the vault's
    /// worker reports each as a `FileChanged` event of its own.
    #[serde(skip)]
    pub changed: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRow {
    /// The row's id in the index, or `0` for a row the index does not hold at all: the file tree
    /// lists the trees the walk refuses (`crate::walk::unindexed_children`) beside the ones it
    /// holds, and this is what tells the two apart.
    pub id: i64,
    pub rel_path: String,
    pub kind: FileKind,
    pub title: Option<String>,
    pub size: i64,
    pub mtime_ns: i64,
    /// True for a row inside one of the dependency or build trees the walk refuses by name or by
    /// marker file (`crate::walk::Unindexed::Dependency`): somebody else's tree, listed so the
    /// reader can look at it and never edited from here. A row the index does not hold because
    /// git ignores its folder is **not** one of these — that folder is the reader's own.
    ///
    /// Defaulted so a vault served by an accent that predates the field still lists.
    #[serde(default)]
    pub dependency: bool,
}

/// One hit of [`Index::search`]: an occurrence of the query in a file, not a file that holds it.
/// A file that says the query five times is five of these, the way a [`Match`] is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchHit {
    pub rel_path: String,
    pub title: Option<String>,
    pub snippet: String,
    /// Byte range of the phrase in the note, so activating the row can place the caret the way a
    /// [`Match`] does. `None` when the body does not hold it — a hit on the title alone, or one
    /// the tokenizer found across a stretch this cannot fold back together — and the note then
    /// opens at the top.
    pub at: Option<Range<usize>>,
    /// 1-based line the occurrence sits on, the way an editor counts lines; `None` on the one
    /// row a hit with no occurrence in the body makes, which quotes the head of the note instead.
    ///
    /// Defaulted, like [`Match::more`] below it, so a vault served by an accent that predates the
    /// per-match rows still lists — as one row per file, which is what that server sends.
    #[serde(default)]
    pub line: Option<u32>,
    /// Occurrences in this file the per-file cap left out, on its last listed row and 0 on every
    /// other: [`Match::more`] for the ranked path.
    #[serde(default)]
    pub more: usize,
}

/// One hit of [`Index::grep`], which lists a row per match rather than a row per note.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Match {
    pub rel_path: String,
    pub title: Option<String>,
    /// 1-based, the way an editor counts lines.
    pub line: u32,
    /// The line the match sits on, clipped to what a sidebar row can show.
    pub line_text: String,
    /// Byte range of the match inside `line_text`.
    pub range: Range<usize>,
    /// Byte offset of the match in the note, so activating the row can place the caret on it.
    pub offset: usize,
    /// Matches in this file the cap left out, on its last listed row and 0 on every other, so the
    /// list can say "+N more in this file" instead of quietly dropping them.
    pub more: usize,
}

/// What [`Index::update_file`] did, so the caller knows whether to tell the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// A name the index never stores: a temp file or a hard-skipped directory.
    Ignored,
    /// Stat identical to the indexed row: typically the watcher echo of our own save.
    Unchanged,
    Added(FileKind),
    Updated(FileKind),
    Removed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Backlink {
    pub src_rel_path: String,
    pub byte_start: i64,
    pub byte_end: i64,
}

/// A note link that points into a page of a PDF: what paints as a highlight over that page.
///
/// `page` is zero-based like [`crate::pdf::Selection`], `selection` the four numbers the link
/// spells, and `alias` the text it quotes, which re-anchors the highlight when the numbers no
/// longer fit the document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PdfLink {
    pub src_rel_path: String,
    pub byte_start: i64,
    pub page: usize,
    pub selection: [usize; 4],
    pub alias: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stats {
    pub files: i64,
    /// Directories, which the watcher weighs against the inotify budget.
    pub dirs: i64,
    pub notes: i64,
    pub links: i64,
    pub tags: i64,
    pub conflicts: i64,
    pub aliases: i64,
    /// Links nothing in the vault answers to: what [`Index::unresolved_links`] lists.
    pub unresolved: i64,
}

impl Index {
    pub fn open(db_path: &Path) -> Result<Self> {
        if let Some(parent) = db_path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating cache dir {}", parent.display()))?;
        }
        let conn = Connection::open(db_path)
            .with_context(|| format!("opening index {}", db_path.display()))?;
        Self::from_conn(conn)
    }

    fn from_conn(conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "temp_store", "MEMORY")?;
        // The app writes through the vault worker alone, but nothing here enforces that: the CLI
        // opens a connection of its own and so does every test, and a second writer must wait for
        // the first rather than fail. This alone is not enough: see [`Index::write_tx`] for the
        // transaction shape without which the handler this installs is never called. And it is
        // not a substitute for one writer either — a waiter can be starved by a writer that takes
        // the lock back between batches, as a reconcile does, and then it fails after the whole
        // timeout. That is why [`Index::set_excluded`] is now handed to the worker rather than
        // written against it.
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        // [`search`] cuts its snippet inside the query, so the folding it shares with the marking
        // has to be reachable from SQL. Deterministic and innocuous: it is a pure function of its
        // arguments and touches nothing outside them.
        conn.create_scalar_function(
            "snippet_window",
            2,
            FunctionFlags::SQLITE_UTF8
                | FunctionFlags::SQLITE_DETERMINISTIC
                | FunctionFlags::SQLITE_INNOCUOUS,
            |ctx| {
                Ok(snippet_window(
                    ctx.get_raw(0).as_str()?,
                    ctx.get_raw(1).as_str()?,
                ))
            },
        )?;
        // Where that window's phrase sits in the note, so a hit opens on the match rather than at
        // the top; `NULL` when the body does not hold it. A second pass over the body, deliberate:
        // it is the same linear fold-compare the window costs, and the alternative is handing
        // whole note bodies back to Rust to search them there.
        conn.create_scalar_function(
            "phrase_start",
            2,
            FunctionFlags::SQLITE_UTF8
                | FunctionFlags::SQLITE_DETERMINISTIC
                | FunctionFlags::SQLITE_INNOCUOUS,
            |ctx| {
                let (body, needle) = (ctx.get_raw(0).as_str()?, ctx.get_raw(1).as_str()?);
                Ok(folded_find(body, needle).map(|(at, _)| at as i64))
            },
        )?;

        let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        let has_files: bool = conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name='files'",
                [],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false);
        if version != SCHEMA_VERSION || !has_files {
            // It's a cache: rebuilding is cheaper than writing migrations.
            conn.execute_batch(DROP_ALL)?;
            conn.execute_batch(SCHEMA)?;
            conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }
        conn.execute_batch(BODIES)?;
        Ok(Index { conn })
    }

    /// SQLite's `data_version`: a number that moves whenever another connection has committed to
    /// the index since this one last asked. What lets a reader keep something it read out of the
    /// index for as long as the index says nothing has changed.
    pub fn data_version(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("PRAGMA data_version", [], |r| r.get(0))?)
    }

    /// Every write transaction in this file, and `BEGIN IMMEDIATE` rather than rusqlite's default
    /// `BEGIN DEFERRED` because the busy timeout above only works this way.
    ///
    /// A deferred transaction takes its write lock on the first statement that needs one. When
    /// that statement follows a read — [`upsert`] looks up `content_hash` before it writes — the
    /// transaction is already holding a read snapshot, and promoting it while another connection
    /// holds the write lock is a deadlock SQLite refuses to wait on: it returns `SQLITE_BUSY`
    /// straight away and never calls the busy handler. That is the "database is locked" the
    /// indexer reported on every save while the git refresh wrote the ignore set on another
    /// thread. Taking the lock up front makes the same collision a wait of a few microseconds.
    fn write_tx(&mut self) -> Result<rusqlite::Transaction<'_>> {
        Ok(self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?)
    }
}

/// The vault every submodule's tests start from, and the index over it.
#[cfg(test)]
pub(super) mod testing {
    use super::Index;
    use std::fs;

    pub fn fixture() -> (tempfile::TempDir, tempfile::TempDir) {
        let vault = tempfile::tempdir().unwrap();
        fs::create_dir(vault.path().join("sub")).unwrap();
        fs::write(
            vault.path().join("a.md"),
            "# Alpha\nsee [[Beta]] and #rust\n",
        )
        .unwrap();
        fs::write(
            vault.path().join("sub/Beta.md"),
            "# Beta\nbody about ferris\n",
        )
        .unwrap();
        fs::write(vault.path().join("c.pdf"), b"%PDF-1.4 not really").unwrap();
        fs::write(
            vault
                .path()
                .join("a.sync-conflict-20240101-120000-ABCDEFG.md"),
            "conflicted",
        )
        .unwrap();
        let db = tempfile::tempdir().unwrap();
        (vault, db)
    }

    pub fn open(db: &tempfile::TempDir) -> Index {
        Index::open(&db.path().join("i.db")).unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::testing::fixture;
    use super::*;
    use std::fs;
    use std::time::Instant;

    /// The `!BUG` this file's `write_tx` exists for: an index write that collides with another
    /// connection's must **wait** for it, not fail. Before `BEGIN IMMEDIATE` this returned
    /// `SQLITE_BUSY` in under 2 ms, because `upsert` reads `content_hash` before it writes and
    /// SQLite will not let a busy handler block a read-to-write promotion.
    #[test]
    fn a_write_waits_for_another_writer_instead_of_reporting_a_locked_database() {
        let (vault, db) = fixture();
        let path = db.path().join("i.db");
        let mut ix = Index::open(&path).unwrap();
        ix.reconcile(vault.path(), |_| {}).unwrap();

        // Another connection holds the write lock for a while, as the git refresh does when it
        // records the ignore set on its own thread.
        let held = std::time::Duration::from_millis(300);
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let other = std::thread::spawn(move || {
            let mut ix = Index::open(&path).unwrap();
            let tx = ix.write_tx().unwrap();
            tx.execute("UPDATE files SET git_ignored = 0", []).unwrap();
            ready_tx.send(()).unwrap();
            std::thread::sleep(held);
            tx.commit().unwrap();
        });
        ready_rx.recv().unwrap();

        fs::write(vault.path().join("a.md"), "# Alpha\nrewritten\n").unwrap();
        let t = Instant::now();
        let change = ix.update_file_batched(vault.path(), "a.md");
        let waited = t.elapsed();
        other.join().unwrap();

        assert!(change.is_ok(), "{change:?}");
        assert!(
            waited >= held / 2,
            "it did not wait for the other writer: {waited:?}"
        );
    }

    /// The bump to schema 10 as a user meets it: an index the previous version wrote is dropped
    /// whole and built again, and the query the new schema exists for answers on what came back.
    /// A vault of 41.7k files pays this once, so the failure that matters is a half-migration —
    /// the new table missing while `user_version` says it is there.
    #[test]
    fn an_index_from_the_previous_schema_is_rebuilt_whole() {
        let (vault, db) = fixture();
        let path = db.path().join("i.db");
        {
            let mut ix = Index::open(&path).unwrap();
            ix.reconcile(vault.path(), |_| {}).unwrap();
        }
        // What a schema-9 database is: everything this one holds, minus the table version 10 added.
        {
            let c = Connection::open(&path).unwrap();
            c.execute_batch("DROP TABLE note_aliases;").unwrap();
            c.pragma_update(None, "user_version", SCHEMA_VERSION - 1)
                .unwrap();
        }

        let mut ix = Index::open(&path).unwrap();
        assert_eq!(
            ix.stats().unwrap().files,
            0,
            "the old cache must be dropped"
        );
        ix.reconcile(vault.path(), |_| {}).unwrap();
        assert!(
            ix.note_aliases().is_ok(),
            "the rebuilt index has the aliases table"
        );
    }

    #[test]
    fn schema_mismatch_drops_and_rebuilds() {
        let (vault, db) = fixture();
        let path = db.path().join("i.db");
        {
            let mut ix = Index::open(&path).unwrap();
            ix.reconcile(vault.path(), |_| {}).unwrap();
            assert!(ix.stats().unwrap().files > 0);
        }
        {
            let c = Connection::open(&path).unwrap();
            c.pragma_update(None, "user_version", 999i64).unwrap();
        }
        let ix = Index::open(&path).unwrap();
        assert_eq!(ix.stats().unwrap().files, 0, "stale cache must be dropped");
    }
}
