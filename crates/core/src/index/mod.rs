//! SQLite FTS5 index of vault files, aliases, links, tags, and headings.
//!
//! The index is a disposable cache; vault files are the source of truth. A schema mismatch
//! triggers a rebuild.
//!
//! `Index` methods are grouped by reconciliation, links, file listing, and search.

mod files;
mod filter;
mod links;
mod pdf_text;
mod reconcile;
mod schema;
mod search;
mod similar;
mod symbols;

use crate::walk::FileKind;
use crate::{Error, Result};
pub use pdf_text::UnreadPdf;
pub use rusqlite::InterruptHandle;
use rusqlite::functions::FunctionFlags;
use rusqlite::{Connection, OptionalExtension};
pub(crate) use schema::MAX_INDEXED_BODY;
use schema::{BODIES, SCHEMA, SCHEMA_VERSION};
pub use search::MIN_INFIX;
use search::{first_of, fold, holds_all, snippet_window};
use serde::{Deserialize, Serialize};
use std::ops::Range;
use std::path::{Path, PathBuf};
pub use symbols::{CodeSymbol, Mention, SymbolName};

pub struct Index {
    conn: Connection,
    /// The lock on [`unsynced_marker`] a first index holds while its writes are not synced.
    unsynced: Option<std::fs::File>,
}

/// `db_path` with `suffix` on its file name, as SQLite names an index's `-wal` and `-shm`.
fn beside(db_path: &Path, suffix: &str) -> PathBuf {
    let mut path = db_path.as_os_str().to_owned();
    path.push(suffix);
    path.into()
}

/// The file beside an index that a first index holds locked while it writes without syncing
/// (`Index::sync_writes`), holding the [`boot_id`] it was made in. Found unlocked, it is a first
/// index a crash ended.
fn unsynced_marker(db_path: &Path) -> PathBuf {
    beside(db_path, "-unsynced")
}

/// Lock the file beside an index that one process at a time holds while it decides whether the
/// index stays ([`Index::open_or_rebuild`]) or makes its [`unsynced_marker`]: two openers of one
/// stale index would otherwise each delete the other's new one, and an opener could find a
/// marker before its boot id is in it.
fn lock_beside(db_path: &Path) -> Result<std::fs::File> {
    let path = beside(db_path, "-lock");
    let file = std::fs::File::create(&path).map_err(|e| Error::io(path.display(), e))?;
    file.lock().map_err(|e| Error::io(path.display(), e))?;
    Ok(file)
}

/// The kernel's id for this boot. Writes left unsynced are lost when the machine stops, never
/// when a process does, so a first index a crash ended within this boot is whole. `None` where
/// the kernel does not say, as on macOS.
fn boot_id() -> Option<String> {
    let id = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
    Some(id.trim().to_string())
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
    /// The folder walked, vault-relative: `""` for the whole vault.
    pub dir: String,
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
    /// The folders git ignores that the walk found and did not enter, vault-relative: where the
    /// files it leaves out are ([`crate::walk::ignored_files`]). Not serialized, as `changed`.
    #[serde(skip)]
    pub ignored_dirs: Vec<String>,
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
    pub line: Option<u32>,
    /// On a PDF's row, which is one of its pages rather than an occurrence: that page, 1-based as
    /// a `#page=` link counts, `line` being `None`. `None` on every other file's row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<u32>,
    /// Occurrences in this file the per-file cap left out, on its last listed row and 0 on every
    /// other: [`Match::more`] for the ranked path.
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
    /// On a PDF's row, which is the first match on one of its pages: that page, 1-based, `line`
    /// counting from its top. `None` on every other file's row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<u32>,
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

/// One link written in a note, as [`Index::links_from`] lists it: what the note points at and
/// where that leads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutLink {
    /// The target as written, a markdown link's made the vault path it names.
    pub target: String,
    /// The file it resolves to, `None` when nothing in the vault answers to it.
    pub path: Option<String>,
    pub kind: crate::markdown::LinkKind,
    pub anchor: Option<String>,
    pub byte_start: i64,
}

/// A heading [`Index::headings_matching`] found, in which note, and how many of the words it
/// holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeadingHit {
    pub rel_path: String,
    pub level: u8,
    pub text: String,
    pub byte_start: i64,
    pub words: usize,
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
            std::fs::create_dir_all(parent).map_err(|e| Error::io(parent.display(), e))?;
        }
        let conn = Connection::open(db_path)
            .map_err(|e| Error::Index(format!("{}: {e}", db_path.display())))?;
        Self::from_conn(conn)
    }

    /// [`open`](Self::open), unless the index is not one this build can use: a file SQLite cannot
    /// read (a disk's damage), one an older build wrote, or a first index, which does not sync its
    /// writes, cut short by the machine stopping. Its files are then deleted and a new index
    /// started in their place, the index being only what the vault's files say, and the second
    /// value says what was wrong with it. Deleting costs the same however large the file is,
    /// where dropping the tables of a 15 GB index read all of it under the write lock for minutes
    /// and checking every page of one took as long.
    pub fn open_or_rebuild(db_path: &Path) -> Result<(Self, Option<String>)> {
        if let Some(parent) = db_path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(|e| Error::io(parent.display(), e))?;
        }
        let _deciding = lock_beside(db_path)?;
        // A first index still running elsewhere holds the marker's lock; this one keeps it while
        // it decides.
        let marker = unsynced_marker(db_path);
        let unfinished = std::fs::File::open(&marker)
            .ok()
            .filter(|f| f.try_lock().is_ok());
        let started = unfinished
            .as_ref()
            .map(|f| std::io::read_to_string(f).unwrap_or_default());
        let damage = match started {
            // A marker from this boot is a process's crash, which loses no write, so the index
            // stays and the walk that follows carries on from it. From another boot, or where
            // the kernel names none, any page of it may be damaged.
            Some(started) if boot_id().is_none_or(|id| started.trim() != id) => {
                Some("a first index was cut short by the machine stopping".into())
            }
            _ => unusable(db_path),
        };
        if let Some(why) = &damage {
            tracing::warn!("{}: {why}, building a new index", db_path.display());
            for suffix in ["", "-wal", "-shm"] {
                let path = beside(db_path, suffix);
                match std::fs::remove_file(&path) {
                    Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                        return Err(Error::io(path.display(), e));
                    }
                    _ => {}
                }
            }
        }
        if unfinished.is_some() {
            let _ = std::fs::remove_file(&marker);
        }
        Ok((Self::open(db_path)?, damage))
    }

    /// What stops the statement this connection is running, from any thread: that statement fails
    /// as interrupted, and the next one runs as usual.
    pub fn interrupt_handle(&self) -> InterruptHandle {
        self.conn.get_interrupt_handle()
    }

    fn from_conn(mut conn: Connection) -> Result<Self> {
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
        // Where that window's first match sits in the note, so a hit opens on the match rather
        // than at the top; `NULL` when the body holds none. A second pass over the body,
        // deliberate: it is the same linear fold-compare the window costs, and the alternative is
        // handing whole note bodies back to Rust to search them there.
        conn.create_scalar_function(
            "phrase_start",
            2,
            FunctionFlags::SQLITE_UTF8
                | FunctionFlags::SQLITE_DETERMINISTIC
                | FunctionFlags::SQLITE_INNOCUOUS,
            |ctx| {
                let (body, needles) = (ctx.get_raw(0).as_str()?, ctx.get_raw(1).as_str()?);
                Ok(first_of(body, needles).map(|(at, _)| at as i64))
            },
        )?;
        // Whether a body or its title holds every word of a query, which the trigram index's
        // candidates are checked by (`Index::infix`).
        conn.create_scalar_function(
            "holds_all",
            3,
            FunctionFlags::SQLITE_UTF8
                | FunctionFlags::SQLITE_DETERMINISTIC
                | FunctionFlags::SQLITE_INNOCUOUS,
            |ctx| {
                let body = ctx.get_raw(0).as_str()?;
                let (title, needles) = (ctx.get_raw(1).as_str()?, ctx.get_raw(2).as_str()?);
                Ok(holds_all(body, title, needles))
            },
        )?;
        // A tag folded as search folds a word, so `tag:Paper` finds `#paper` (`index::filter`).
        conn.create_scalar_function(
            "fold",
            1,
            FunctionFlags::SQLITE_UTF8
                | FunctionFlags::SQLITE_DETERMINISTIC
                | FunctionFlags::SQLITE_INNOCUOUS,
            |ctx| Ok(fold(ctx.get_raw(0).as_str()?)),
        )?;

        // A new file gets the schema. An index an older build wrote is never migrated in place:
        // `open_or_rebuild` deletes it before a connection reaches it, a cache being cheaper to
        // build anew than to migrate. Asked once outside a transaction, so an open never queues
        // behind another process's walk, and again inside the one that creates it, which a
        // second opener of the same new file waits for.
        if !current(&conn)? {
            let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            if !current(&tx)? {
                let tables: i64 =
                    tx.query_row("SELECT count(*) FROM sqlite_schema", [], |r| r.get(0))?;
                if tables > 0 {
                    return Err(Error::Index(format!(
                        "the index was built by an older accent (schema {})",
                        schema_version(&tx)?
                    )));
                }
                tx.execute_batch(SCHEMA)?;
                tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            }
            tx.commit()?;
        }
        conn.execute_batch(BODIES)?;
        Ok(Index {
            conn,
            unsynced: None,
        })
    }

    /// SQLite's `data_version`: a number that moves whenever another connection has committed to
    /// the index since this one last asked. What lets a reader keep something it read out of the
    /// index for as long as the index says nothing has changed.
    pub fn data_version(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("PRAGMA data_version", [], |r| r.get(0))?)
    }

    /// Begin an immediate write transaction so lock contention waits under the busy timeout.
    /// Deferred transactions that read before writing can fail immediately with `SQLITE_BUSY`
    /// during lock promotion.
    fn write_tx(&mut self) -> Result<rusqlite::Transaction<'_>> {
        Ok(self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?)
    }
}

/// What makes the index at `db_path` one this build cannot use, if anything: a header or schema
/// SQLite cannot read, which is where garbage shows, or the tables of an older build. `None` for
/// a new file, for one a newer build wrote, which [`Index::open`] refuses rather than this
/// deleting it under that build, and for a file that cannot be opened at all, which
/// [`Index::open`] reports as itself.
fn unusable(db_path: &Path) -> Option<String> {
    let conn = Connection::open(db_path).ok()?;
    match conn.query_row("SELECT count(*) FROM sqlite_schema", [], |r| {
        r.get::<_, i64>(0)
    }) {
        Err(e) => matches!(
            e.sqlite_error_code(),
            Some(rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase)
        )
        .then(|| e.to_string()),
        Ok(0) => None,
        Ok(_) => matches!(current(&conn), Ok(false)).then(|| {
            let version = schema_version(&conn).unwrap_or_default();
            format!("it was built by an older accent (schema {version})")
        }),
    }
}

fn schema_version(conn: &Connection) -> Result<i64> {
    Ok(conn.pragma_query_value(None, "user_version", |r| r.get(0))?)
}

/// Whether the index holds this build's schema, or is to be rebuilt. One a newer build wrote is
/// neither: it is that build's cache, maybe open in it right now, and is refused rather than
/// dropped under it.
fn current(conn: &Connection) -> Result<bool> {
    let version = schema_version(conn)?;
    if version > SCHEMA_VERSION {
        return Err(Error::Index(format!(
            "the index was built by a newer accent (schema {version}, this one reads \
             {SCHEMA_VERSION}): update this one"
        )));
    }
    let has_files: bool = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='files'",
            [],
            |_| Ok(true),
        )
        .optional()?
        .unwrap_or(false);
    Ok(version == SCHEMA_VERSION && has_files)
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

    /// Concurrent writers must wait rather than fail during read-to-write lock promotion;
    /// [`Index::write_tx`] takes the write lock before reading.
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
        let change = ix.update_file(vault.path(), "a.md");
        let waited = t.elapsed();
        other.join().unwrap();

        assert!(change.is_ok(), "{change:?}");
        assert!(
            waited >= held / 2,
            "it did not wait for the other writer: {waited:?}"
        );
    }

    /// An index an older build wrote is deleted and a new one started, which costs the same
    /// however large it is, and a connection that meets one never migrates it in place.
    #[test]
    fn an_index_from_the_previous_schema_is_replaced_by_a_new_one() {
        let (vault, db) = fixture();
        let path = db.path().join("i.db");
        Index::open(&path)
            .unwrap()
            .reconcile(vault.path(), |_| {})
            .unwrap();
        // What a schema-9 database is: everything this one holds, minus the table version 10 added.
        {
            let c = Connection::open(&path).unwrap();
            c.execute_batch("DROP TABLE note_aliases;").unwrap();
            c.pragma_update(None, "user_version", SCHEMA_VERSION - 1)
                .unwrap();
        }
        assert!(Index::open(&path).is_err(), "a plain open took it as it is");

        let (mut ix, why) = Index::open_or_rebuild(&path).unwrap();
        assert!(why.is_some_and(|why| why.contains("older")));
        assert_eq!(ix.stats().unwrap().files, 0, "a new index");
        ix.reconcile(vault.path(), |_| {}).unwrap();
        assert!(
            ix.note_aliases().is_ok(),
            "the new index has the aliases table"
        );
    }

    /// A first index writes without syncing, which only the machine stopping can cost: one a
    /// crash ended within this boot is kept as it is, one from another boot is deleted unread.
    #[test]
    fn a_first_index_a_crash_ended_is_kept_within_its_boot() {
        let (vault, db) = fixture();
        let path = db.path().join("i.db");
        let mut ix = Index::open(&path).unwrap();
        ix.reconcile(vault.path(), |_| {}).unwrap();
        let files = ix.stats().unwrap().files;
        drop(ix);
        // Where the kernel names no boot, as on macOS, every such index is deleted.
        let kept = boot_id().map_or(0, |_| files);

        fs::write(unsynced_marker(&path), boot_id().unwrap_or_default()).unwrap();
        let (ix, _) = Index::open_or_rebuild(&path).unwrap();
        assert_eq!(ix.stats().unwrap().files, kept);
        assert!(!unsynced_marker(&path).exists());

        fs::write(unsynced_marker(&path), "another boot").unwrap();
        let (ix, why) = Index::open_or_rebuild(&path).unwrap();
        assert!(why.is_some());
        assert_eq!(ix.stats().unwrap().files, 0, "a new index");
        assert!(!unsynced_marker(&path).exists());
    }

    /// Two builds can share one index, the app and an `accent-cli mcp` from another build, so an
    /// index a newer one wrote is refused rather than deleted under it.
    #[test]
    fn an_index_a_newer_build_wrote_is_refused() {
        let (vault, db) = fixture();
        let path = db.path().join("i.db");
        Index::open(&path)
            .unwrap()
            .reconcile(vault.path(), |_| {})
            .unwrap();
        let newer = Connection::open(&path).unwrap();
        newer
            .pragma_update(None, "user_version", SCHEMA_VERSION + 1)
            .unwrap();

        let refused = Index::open_or_rebuild(&path)
            .err()
            .expect("a newer index was opened");
        assert!(format!("{refused:#}").contains("newer"), "{refused:#}");
        let files: i64 = newer
            .query_row("SELECT count(*) FROM files", [], |r| r.get(0))
            .unwrap();
        assert!(files > 0, "it was deleted");
    }
}
