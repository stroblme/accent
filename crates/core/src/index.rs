//! SQLite (FTS5) index of the vault: files, aliases, links, tags, headings.
//!
//! The index is a **disposable cache**: the markdown files are the source of truth. On a schema
//! mismatch we drop everything and rebuild rather than migrate.

use crate::markdown;
use crate::walk::{self, FileKind, ScanOptions};
use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::ops::Range;
use std::path::Path;
use std::time::Instant;

use crate::search::Regex;

/// Bump on any schema change: `open` then drops and recreates the cache.
const SCHEMA_VERSION: i64 = 5;
/// Biggest non-markdown file whose text goes into the index.
///
/// Deliberately far stricter than [`crate::fs::MAX_TEXT`] (16 MiB), which is the cap on what a
/// tab will *open*: opening a 15 MiB generated file is something the user asked for once, while
/// indexing it is something the vault pays for on every reconcile, in database size and in the
/// FTS terms every later query has to merge. Measured on `testvault/`: none of its 17 700
/// non-note text files reach 1 MiB, so the cap costs nothing a real vault would notice. A note
/// is never subject to it — markdown is read whatever its size.
const MAX_INDEXED_BODY: u64 = 1024 * 1024;
/// Files per write transaction. Big enough to amortise the WAL commit, small enough that a
/// killed process loses little work and progress reporting stays lively.
const BATCH: usize = 500;
/// Bytes of a matched line [`Index::grep`] keeps before and after the match. A note can hold a
/// single line megabytes long (an embedded data URI), and a sidebar row must not carry all of it.
const CLIP_BEFORE: usize = 40;
const CLIP_AFTER: usize = 200;

const SCHEMA: &str = r#"
CREATE TABLE files(
    id           INTEGER PRIMARY KEY,
    rel_path     TEXT UNIQUE NOT NULL,
    parent_dir   TEXT NOT NULL,
    canonical    TEXT NOT NULL,
    dev          INTEGER NOT NULL,
    ino          INTEGER NOT NULL,
    mtime_ns     INTEGER NOT NULL,
    size         INTEGER NOT NULL,
    kind         INTEGER NOT NULL,
    title        TEXT,
    content_hash BLOB
);
CREATE TABLE aliases(rel_path TEXT PRIMARY KEY, file_id INTEGER NOT NULL);
CREATE TABLE links(
    src_file      INTEGER NOT NULL,
    target        TEXT NOT NULL,
    resolved_file INTEGER,
    kind          INTEGER NOT NULL,
    anchor        TEXT,
    byte_start    INTEGER NOT NULL,
    byte_end      INTEGER NOT NULL
);
CREATE TABLE tags(file_id INTEGER NOT NULL, name TEXT NOT NULL, byte_start INTEGER NOT NULL);
CREATE TABLE headings(file_id INTEGER NOT NULL, level INTEGER NOT NULL, text TEXT NOT NULL, byte_start INTEGER NOT NULL);
CREATE TABLE notes(file_id INTEGER PRIMARY KEY, body TEXT NOT NULL, title TEXT NOT NULL);

-- `body` stays column 0 so `snippet(notes_fts, 0, ...)` keeps quoting the note text, and so the
-- bm25 weights below read in the same order: body first, title second.
--
-- `prefix` is what makes the sidebar usable: every query ends in a prefix term (see `fts_query`),
-- and without a prefix index FTS5 answers `t*` by expanding it over the term index once per row
-- the snippet is cut for. Measured on the 3.6k-note testvault, a one-character query took 5.1 s
-- without it and 41 ms with it; the index file grew from 106 to 134 MiB, which a disposable cache
-- can afford.
CREATE VIRTUAL TABLE notes_fts USING fts5(
    body, title, content='notes', content_rowid='file_id',
    tokenize="unicode61 remove_diacritics 2", prefix='1 2 3'
);
CREATE TRIGGER notes_ai AFTER INSERT ON notes BEGIN
    INSERT INTO notes_fts(rowid, body, title) VALUES (new.file_id, new.body, new.title);
END;
CREATE TRIGGER notes_ad AFTER DELETE ON notes BEGIN
    INSERT INTO notes_fts(notes_fts, rowid, body, title)
    VALUES ('delete', old.file_id, old.body, old.title);
END;
CREATE TRIGGER notes_au AFTER UPDATE ON notes BEGIN
    INSERT INTO notes_fts(notes_fts, rowid, body, title)
    VALUES ('delete', old.file_id, old.body, old.title);
    INSERT INTO notes_fts(rowid, body, title) VALUES (new.file_id, new.body, new.title);
END;

CREATE INDEX idx_links_target   ON links(target);
CREATE INDEX idx_links_resolved ON links(resolved_file);
CREATE INDEX idx_links_src      ON links(src_file);
CREATE INDEX idx_tags_name      ON tags(name);
CREATE INDEX idx_tags_file      ON tags(file_id);
CREATE INDEX idx_headings_file  ON headings(file_id);
CREATE INDEX idx_files_devino   ON files(dev, ino);
CREATE INDEX idx_files_parent   ON files(parent_dir);
CREATE INDEX idx_files_kind_mt  ON files(kind, mtime_ns DESC);
CREATE INDEX idx_aliases_file   ON aliases(file_id);
"#;

const DROP_ALL: &str = r#"
DROP TRIGGER IF EXISTS notes_ai;
DROP TRIGGER IF EXISTS notes_ad;
DROP TRIGGER IF EXISTS notes_au;
DROP TABLE IF EXISTS notes_fts;
DROP TABLE IF EXISTS notes;
DROP TABLE IF EXISTS headings;
DROP TABLE IF EXISTS tags;
DROP TABLE IF EXISTS links;
DROP TABLE IF EXISTS aliases;
DROP TABLE IF EXISTS files;
"#;

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

// ---------------------------------------------------------------- public data

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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRow {
    pub id: i64,
    pub rel_path: String,
    pub kind: FileKind,
    pub title: Option<String>,
    pub size: i64,
    pub mtime_ns: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchHit {
    pub rel_path: String,
    pub title: Option<String>,
    pub snippet: String,
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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeadingRow {
    pub level: u8,
    pub text: String,
    pub byte_start: i64,
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
}

// ---------------------------------------------------------------------- open

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

    /// For tests and short-lived tooling.
    pub fn open_in_memory() -> Result<Self> {
        Self::from_conn(Connection::open_in_memory()?)
    }

    fn from_conn(conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "temp_store", "MEMORY")?;

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
        Ok(Index { conn })
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }
}

// ----------------------------------------------------------------- reconcile

/// What the diff decided to do with one scanned entry.
struct Job {
    idx: usize,
    existing_id: Option<i64>,
}

impl Index {
    /// Walk `root` and bring the index in line with it. Returns what changed.
    pub fn reconcile(
        &mut self,
        root: &Path,
        on_progress: impl FnMut(Progress),
    ) -> Result<ReconcileStats> {
        self.reconcile_with(root, &ScanOptions::default(), on_progress)
    }

    /// [`reconcile`](Self::reconcile) with explicit walk options.
    pub fn reconcile_with(
        &mut self,
        root: &Path,
        opts: &ScanOptions,
        mut on_progress: impl FnMut(Progress),
    ) -> Result<ReconcileStats> {
        let t_scan = Instant::now();
        let scan = walk::scan(root, opts);
        let mut stats = ReconcileStats {
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
                .filter(|s| s.reason != walk::SkipReason::DependencyTree)
                .count(),
            scan_ms: t_scan.elapsed().as_millis() as u64,
            ..Default::default()
        };
        on_progress(Progress {
            phase: Phase::Scan,
            done: scan.files.len(),
            total: scan.files.len(),
        });

        // One pass over `files` into memory. 47k rows of five integers is a few MB and turns the
        // per-file diff into a hash lookup instead of a query.
        let mut existing: HashMap<String, (i64, i64, i64, i64)> = HashMap::new();
        {
            let mut st = self
                .conn
                .prepare("SELECT id, rel_path, mtime_ns, size, ino FROM files")?;
            let mut rows = st.query([])?;
            while let Some(r) = rows.next()? {
                existing.insert(r.get(1)?, (r.get(0)?, r.get(2)?, r.get(3)?, r.get(4)?));
            }
        }

        let mut jobs: Vec<Job> = Vec::new();
        for (idx, f) in scan.files.iter().enumerate() {
            match existing.remove(&f.rel_path) {
                // `remove` doubles as the "seen" marker: leftovers are deletions.
                Some((id, mtime_ns, size, ino)) => {
                    if mtime_ns == f.mtime_ns && size == f.size as i64 && ino == f.ino as i64 {
                        stats.unchanged += 1;
                    } else {
                        jobs.push(Job {
                            idx,
                            existing_id: Some(id),
                        });
                    }
                }
                None => jobs.push(Job {
                    idx,
                    existing_id: None,
                }),
            }
        }
        let removed: Vec<i64> = existing.values().map(|(id, ..)| *id).collect();
        stats.removed = removed.len();

        let dirty = !jobs.is_empty() || !removed.is_empty();
        let total = jobs.len();

        if !removed.is_empty() {
            let tx = self.conn.transaction()?;
            for id in &removed {
                delete_file_rows(&tx, *id)?;
            }
            tx.commit()?;
        }

        let mut done = 0usize;
        for chunk in jobs.chunks(BATCH) {
            let tx = self.conn.transaction()?;
            for job in chunk {
                upsert(&tx, &scan.files[job.idx], job.existing_id, &mut stats)?;
                done += 1;
            }
            tx.commit()?;
            on_progress(Progress {
                phase: Phase::Index,
                done,
                total,
            });
        }

        if dirty {
            // Aliases are a handful of rows; rewriting them beats diffing them.
            let tx = self.conn.transaction()?;
            tx.execute("DELETE FROM aliases", [])?;
            for a in &scan.aliases {
                tx.prepare_cached(
                    "INSERT OR REPLACE INTO aliases(rel_path, file_id)
                     SELECT ?1, id FROM files WHERE rel_path = ?2",
                )?
                .execute(params![a.rel_path, a.target_rel_path])?;
            }
            tx.commit()?;
            self.resolve_links()?;
            on_progress(Progress {
                phase: Phase::Resolve,
                done: total,
                total,
            });
        }

        Ok(stats)
    }

    /// Obsidian link resolution: a target matches a file's path or name, with or without the
    /// extension, case-insensitively; the shortest `rel_path` wins. Unmatched stays NULL.
    ///
    /// ponytail: this re-resolves the whole `links` table, because adding one note can resolve
    /// dangling links anywhere in the vault. It costs 225 ms at the 56k links of `testvault/`,
    /// so callers that change many files must batch (see [`update_file_batched`](Self::update_file_batched))
    /// and pay it once. Narrow it to the targets whose candidate set actually changed if even
    /// once per batch becomes too much.
    pub fn resolve_links(&mut self) -> Result<usize> {
        let mut by_key: HashMap<String, (usize, i64)> = HashMap::new();
        {
            let mut st = self
                .conn
                .prepare("SELECT id, rel_path FROM files WHERE kind <> 0")?;
            let mut rows = st.query([])?;
            while let Some(r) = rows.next()? {
                let id: i64 = r.get(0)?;
                let rel: String = r.get(1)?;
                let len = rel.len();
                for key in markdown::path_keys(&rel) {
                    match by_key.get(&key) {
                        Some((best, _)) if *best <= len => {}
                        _ => {
                            by_key.insert(key, (len, id));
                        }
                    }
                }
            }
        }

        let targets: Vec<String> = {
            let mut st = self.conn.prepare("SELECT DISTINCT target FROM links")?;
            let rows = st.query_map([], |r| r.get(0))?;
            rows.collect::<rusqlite::Result<_>>()?
        };

        let tx = self.conn.transaction()?;
        tx.execute("UPDATE links SET resolved_file = NULL", [])?;
        let mut resolved = 0usize;
        {
            let mut up = tx.prepare("UPDATE links SET resolved_file = ?1 WHERE target = ?2")?;
            for t in &targets {
                let key = markdown::link_key(t);
                if let Some((_, id)) = by_key.get(&key) {
                    resolved += up.execute(params![id, t])?;
                }
            }
        }
        tx.commit()?;
        Ok(resolved)
    }

    /// Bring one path in line with the disk. This is the watcher's entry point: the caller turns a
    /// filesystem event into a vault-relative path and lets the index decide what it means.
    ///
    /// A row whose `(mtime_ns, size, ino)` still match the disk is left completely untouched, which
    /// is what makes the echo of our own save free, and lets the UI ignore the resulting
    /// [`Change::Unchanged`] instead of reloading the buffer the user is typing in.
    ///
    /// ponytail: no `(dev, ino)` dedup here, so a note linked in twice gets a row per path until
    /// the next full reconcile collapses them. Aliases are rare and never wrong, only duplicated.
    pub fn update_file(&mut self, root: &Path, rel: &str) -> Result<Change> {
        let change = self.update_file_batched(root, rel)?;
        // A new note can resolve links written long before it existed, and a removed one can
        // hand its incoming links to a namesake deeper in the vault.
        if matches!(
            change,
            Change::Added(_) | Change::Updated(_) | Change::Removed
        ) {
            self.resolve_links()?;
        }
        Ok(change)
    }

    /// [`update_file`](Self::update_file) without the link resolution, for a caller that is
    /// working through a batch of files and calls [`resolve_links`](Self::resolve_links) once
    /// when it is done. Links into the file stay unresolved until it does.
    pub fn update_file_batched(&mut self, root: &Path, rel: &str) -> Result<Change> {
        let meta = match walk::stat_one(root, rel) {
            Ok(Some(meta)) => meta,
            Ok(None) => return Ok(Change::Ignored),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                self.remove_file_batched(rel)?;
                return Ok(Change::Removed);
            }
            Err(e) => return Err(e).with_context(|| format!("stat {rel}")),
        };

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
        let tx = self.conn.transaction()?;
        upsert(&tx, &meta, existing_id, &mut ReconcileStats::default())?;
        tx.commit()?;
        Ok(match existing_id {
            Some(_) => Change::Updated(meta.kind),
            None => Change::Added(meta.kind),
        })
    }

    /// Drop `rel` and everything below it. A directory removal arrives as one event, so the
    /// subtree has to go with it. Returns how many rows went.
    pub fn remove_file(&mut self, rel: &str) -> Result<usize> {
        let removed = self.remove_file_batched(rel)?;
        if removed > 0 {
            // Links into the removed subtree are NULL again; some may now match a shallower file.
            self.resolve_links()?;
        }
        Ok(removed)
    }

    /// [`remove_file`](Self::remove_file) without the link resolution; see
    /// [`update_file_batched`](Self::update_file_batched).
    pub fn remove_file_batched(&mut self, rel: &str) -> Result<usize> {
        // `'0'` is the byte after `'/'`, so `[rel/, rel0)` is exactly the descendants of `rel`
        // and the range stays on the `rel_path` index.
        let ids: Vec<i64> = {
            let mut st = self.conn.prepare_cached(
                "SELECT id FROM files WHERE rel_path = ?1 OR (rel_path >= ?2 AND rel_path < ?3)",
            )?;
            let rows = st.query_map(params![rel, format!("{rel}/"), format!("{rel}0")], |r| {
                r.get(0)
            })?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        if ids.is_empty() {
            return Ok(0);
        }

        let tx = self.conn.transaction()?;
        for id in &ids {
            delete_file_rows(&tx, *id)?;
        }
        tx.commit()?;
        Ok(ids.len())
    }

    /// The file one link target points at, by the rules of [`resolve_links`](Self::resolve_links).
    /// `None` means the link dangles, which is what the UI offers to create.
    ///
    /// ponytail: one pass over the file paths, not the key map `resolve_links` builds, because a
    /// map costs four strings per file and this answers a single click. If a caller ever needs
    /// hundreds of targets at once, give it a batch method that builds the map once.
    pub fn resolve_target(&self, target: &str) -> Result<Option<String>> {
        let key = markdown::link_key(target);
        let mut st = self
            .conn
            .prepare_cached("SELECT rel_path FROM files WHERE kind <> 0")?;
        let mut rows = st.query([])?;
        let mut best: Option<String> = None;
        while let Some(r) = rows.next()? {
            let rel: String = r.get(0)?;
            let shorter = best.as_ref().is_none_or(|b| rel.len() < b.len());
            if shorter && markdown::path_keys(&rel).contains(&key) {
                best = Some(rel);
            }
        }
        Ok(best)
    }

    /// Headings of one note in document order: the outline pane and heading-scoped edits.
    pub fn headings(&self, rel: &str) -> Result<Vec<HeadingRow>> {
        let mut st = self.conn.prepare_cached(
            "SELECT h.level, h.text, h.byte_start FROM headings h
             JOIN files f ON f.id = h.file_id
             WHERE f.rel_path = ?1 ORDER BY h.byte_start",
        )?;
        let rows = st.query_map([rel], |r| {
            Ok(HeadingRow {
                level: r.get::<_, i64>(0)? as u8,
                text: r.get(1)?,
                byte_start: r.get(2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Every `*.sync-conflict-*` copy in the vault, for the resolve UI.
    pub fn conflicts(&self) -> Result<Vec<String>> {
        let mut st = self
            .conn
            .prepare_cached("SELECT rel_path FROM files WHERE kind = ?1 ORDER BY rel_path")?;
        let rows = st.query_map([FileKind::Conflict.as_i64()], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Every directory the walk kept, as an absolute vault path: the watcher's watch set.
    ///
    /// A tree the walk skipped — `.git`, a `.venv`, a cargo `target/` — never got a row here, so
    /// watching this list instead of the root recursively is what keeps those trees out of the
    /// kernel's inotify budget as well as out of the index. The root itself is not a row and is
    /// the watcher's own responsibility.
    pub fn dirs(&self, root: &Path) -> Result<Vec<std::path::PathBuf>> {
        let mut st = self
            .conn
            .prepare_cached("SELECT rel_path FROM files WHERE kind = 0 ORDER BY rel_path")?;
        let rows = st.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for rel in rows {
            out.push(root.join(rel?));
        }
        Ok(out)
    }

    /// `(canonical target, rel_path of the link)` for every directory symlink pointing out of the
    /// vault. The watch set above reaches these through the link, so this is what maps an event
    /// path that arrives canonical anyway back into the vault.
    pub fn symlink_dirs(&self, root: &Path) -> Result<Vec<(std::path::PathBuf, String)>> {
        let canonical_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        let mut st = self
            .conn
            .prepare_cached("SELECT canonical, rel_path FROM files WHERE kind = 0")?;
        let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        let mut out = Vec::new();
        for row in rows {
            let (canonical, rel) = row?;
            let canonical = std::path::PathBuf::from(canonical);
            if !canonical.starts_with(&canonical_root) && root.join(&rel).is_symlink() {
                out.push((canonical, rel));
            }
        }
        Ok(out)
    }
}

/// Index one scanned entry: read and hash markdown, then replace its file row and everything
/// derived from it. `existing_id` is the row it replaces, if any. Returns the file id.
///
/// The one place a file becomes index rows, shared by the full reconcile and the watcher's
/// [`Index::update_file`] so the two can never drift apart.
fn upsert(
    tx: &rusqlite::Transaction<'_>,
    f: &walk::FileMeta,
    existing_id: Option<i64>,
    stats: &mut ReconcileStats,
) -> Result<i64> {
    // A note is read whole and hashed raw. Everything else that decodes as text under
    // [`MAX_INDEXED_BODY`] is read through `fs::read_text`, which brings the NUL sniff, the CRLF
    // normalisation and the `lossy` flag with it; a lossy decode is dropped because its byte
    // offsets would no longer point at what is on disk. PDFs and binaries stay cheap
    // `(mtime, size, ino)` rows; the `pdf` feature will add text extraction and can reuse the
    // same hash column when it does.
    //
    // The two branches hash different bytes — a note's raw file, a text file's normalised text.
    // That is safe and deliberate: a hash is only ever compared against an earlier hash of the
    // same file by the same branch, never across kinds, so the two need not agree.
    //
    // The size test uses the stat the walk already took rather than letting `read_text` pull a
    // 15 MiB file into memory only for the cap to throw it away.
    let (hash, text) = match f.kind {
        FileKind::Markdown => match std::fs::read(&f.canonical) {
            Ok(bytes) => {
                stats.bytes_read += bytes.len() as u64;
                let h = blake3::hash(&bytes);
                (Some(h), Some(String::from_utf8_lossy(&bytes).into_owned()))
            }
            // Vanished or unreadable mid-walk: keep the stat row, drop the content.
            Err(_) => (None, None),
        },
        FileKind::Other if f.size <= MAX_INDEXED_BODY => match crate::fs::read_text(&f.canonical) {
            Ok(crate::fs::Read::Text(t)) if !t.lossy => {
                stats.bytes_read += t.text.len() as u64;
                let h = blake3::hash(t.text.as_bytes());
                (Some(h), Some(t.text))
            }
            _ => (None, None),
        },
        _ => (None, None),
    };

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

    // Only a note is markdown. A `.py`'s `#` comments are not tags and its `#!` line is not a
    // heading, so nothing but a note reaches the analyser; the `file_stem` fallback below is what
    // gives every other file a title.
    let analysis = match f.kind {
        FileKind::Markdown => text.as_deref().map(markdown::analyze),
        _ => None,
    };
    let title = analysis
        .as_ref()
        .and_then(|a| a.title.clone())
        .or_else(|| file_stem(&f.rel_path));

    let id: i64 = tx
        .prepare_cached(
            "INSERT INTO files(rel_path, parent_dir, canonical, dev, ino, mtime_ns, size, kind, title, content_hash)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
             ON CONFLICT(rel_path) DO UPDATE SET
                parent_dir=excluded.parent_dir,
                canonical=excluded.canonical, dev=excluded.dev, ino=excluded.ino,
                mtime_ns=excluded.mtime_ns, size=excluded.size, kind=excluded.kind,
                title=excluded.title, content_hash=excluded.content_hash
             RETURNING id",
        )?
        .query_row(
            params![
                f.rel_path,
                parent_dir(&f.rel_path),
                f.canonical.to_string_lossy(),
                f.dev as i64,
                f.ino as i64,
                f.mtime_ns,
                f.size as i64,
                f.kind.as_i64(),
                title,
                hash.as_ref().map(|h| h.as_bytes().to_vec()),
            ],
            |r| r.get(0),
        )?;

    if existing_id.is_some() {
        clear_derived(tx, id)?;
        stats.updated += 1;
    } else {
        stats.added += 1;
    }

    if let Some(a) = analysis.as_ref() {
        for l in &a.links {
            tx.prepare_cached(
                "INSERT INTO links(src_file, target, resolved_file, kind, anchor, byte_start, byte_end)
                 VALUES(?1,?2,NULL,?3,?4,?5,?6)",
            )?
            .execute(params![
                id,
                l.target,
                link_kind_i64(l.kind),
                l.anchor,
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
    }
    if let Some(body) = text.as_ref() {
        // Explicit delete + insert: REPLACE would only fire the FTS delete trigger
        // with recursive_triggers on.
        tx.prepare_cached("DELETE FROM notes WHERE file_id = ?1")?
            .execute([id])?;
        tx.prepare_cached("INSERT INTO notes(file_id, body, title) VALUES(?1,?2,?3)")?
            .execute(params![id, body, title.as_deref().unwrap_or_default()])?;
    }
    Ok(id)
}

fn clear_derived(tx: &rusqlite::Transaction<'_>, id: i64) -> Result<()> {
    tx.prepare_cached("DELETE FROM links WHERE src_file = ?1")?
        .execute([id])?;
    tx.prepare_cached("DELETE FROM tags WHERE file_id = ?1")?
        .execute([id])?;
    tx.prepare_cached("DELETE FROM headings WHERE file_id = ?1")?
        .execute([id])?;
    Ok(())
}

fn delete_file_rows(tx: &rusqlite::Transaction<'_>, id: i64) -> Result<()> {
    clear_derived(tx, id)?;
    tx.prepare_cached("DELETE FROM notes WHERE file_id = ?1")?
        .execute([id])?;
    tx.prepare_cached("DELETE FROM aliases WHERE file_id = ?1")?
        .execute([id])?;
    tx.prepare_cached("UPDATE links SET resolved_file = NULL WHERE resolved_file = ?1")?
        .execute([id])?;
    tx.prepare_cached("DELETE FROM files WHERE id = ?1")?
        .execute([id])?;
    Ok(())
}

/// Directory part of a vault-relative path: `"a/b/c.md"` -> `"a/b"`, `"a.md"` -> `""`.
/// Stored per row so one directory level is an index lookup rather than a scan.
fn parent_dir(rel_path: &str) -> &str {
    match rel_path.rsplit_once('/') {
        Some((dir, _)) => dir,
        None => "",
    }
}

fn file_stem(rel_path: &str) -> Option<String> {
    let base = rel_path.rsplit('/').next()?;
    Some(markdown::strip_ext(base))
}

fn link_kind_i64(k: markdown::LinkKind) -> i64 {
    match k {
        markdown::LinkKind::Wiki => 0,
        markdown::LinkKind::Embed => 1,
        markdown::LinkKind::Markdown => 2,
        markdown::LinkKind::External => 3,
    }
}

// ------------------------------------------------------------------- queries

impl Index {
    /// Direct children of `prefix` ("" = vault root). Lazy tree: one level per call.
    ///
    /// Matches on the indexed `parent_dir` column, so the cost is proportional to the number of
    /// children returned. The previous `substr(rel_path, ...)` prefix test was unsargable and made
    /// every expansion a full scan of the vault.
    pub fn list_files(&self, prefix: &str) -> Result<Vec<FileRow>> {
        let mut st = self.conn.prepare_cached(
            "SELECT id, rel_path, kind, title, size, mtime_ns FROM files
             WHERE parent_dir = ?1
             ORDER BY kind <> 0, rel_path COLLATE NOCASE",
        )?;
        let rows = st.query_map([prefix.trim_matches('/')], file_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Every markdown note's rel_path, for the file switcher's fuzzy match.
    pub fn note_paths(&self) -> Result<Vec<String>> {
        let mut st = self.conn.prepare_cached(
            "SELECT rel_path FROM files WHERE kind = ?1 ORDER BY rel_path COLLATE NOCASE",
        )?;
        let rows = st.query_map([FileKind::Markdown.as_i64()], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Every openable file's rel_path, notes first and then the rest by path.
    ///
    /// The switcher opens more than markdown now, so it needs this rather than
    /// [`note_paths`](Self::note_paths), which stays markdown-only because wikilink completion may
    /// only ever offer notes. Directories are not files to open and conflict copies are reached
    /// through the resolve UI, so neither is listed.
    pub fn file_paths(&self) -> Result<Vec<String>> {
        let mut st = self.conn.prepare_cached(
            "SELECT rel_path FROM files WHERE kind IN (?1, ?2, ?3)
             ORDER BY kind <> ?1, rel_path COLLATE NOCASE",
        )?;
        let rows = st.query_map(
            params![
                FileKind::Markdown.as_i64(),
                FileKind::Pdf.as_i64(),
                FileKind::Other.as_i64(),
            ],
            |r| r.get(0),
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The `limit` most recently modified notes: what the switcher lists before the user types.
    pub fn recent_notes(&self, limit: usize) -> Result<Vec<String>> {
        let mut st = self.conn.prepare_cached(
            "SELECT rel_path FROM files WHERE kind = ?1 ORDER BY mtime_ns DESC LIMIT ?2",
        )?;
        let rows = st.query_map(params![FileKind::Markdown.as_i64(), limit as i64], |r| {
            r.get(0)
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn get_file(&self, rel_path: &str) -> Result<Option<FileRow>> {
        let mut st = self.conn.prepare_cached(
            "SELECT id, rel_path, kind, title, size, mtime_ns FROM files WHERE rel_path = ?1",
        )?;
        Ok(st.query_row([rel_path], file_row).optional()?)
    }

    /// Full-text search over the titles and bodies the index holds: every note, plus every other
    /// file that decoded as text under [`MAX_INDEXED_BODY`]. Directories, PDFs, conflict copies
    /// and binaries have no `notes` row and so can never be a hit.
    ///
    /// Ranking is "the note you named, then the notes that are about it": a title equal to the
    /// query, ignoring case, comes first, and the rest go by `bm25` with the title weighted ten
    /// times the body. bm25 is negative in SQLite, so ascending is best-first, and the weights
    /// follow the `notes_fts` column order (body, title).
    ///
    /// Weight 10 was measured on the 3.6k notes of `testvault/`: searching a note's own title
    /// puts that note first for 47 of 60 sampled notes, against 2 of 60 with the title
    /// unweighted. Raising it to 20 buys one more note and costs a lot: three quarters of an
    /// ordinary body search's top ten then come from a title word rather than the body.
    /// The snippet is cut here rather than by FTS5's `snippet()`, and the ranking runs in a
    /// subquery so only the rows that survive it are quoted at all. Both are about the same
    /// measurement: on the 3.6k-note `testvault/` a one-character query took 2.4 s and a
    /// two-character one 0.5 s, and `snippet()` was every millisecond of it. It re-derives the
    /// match positions from the term index, which for a prefix term means merging the doclist of
    /// every term that starts with those letters, per row — 19 ms a row for `t*`. Finding the
    /// same window with `instr` over the body the index already stores costs a tenth of that.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        let q = fts_query(query);
        if q.is_empty() {
            return Ok(Vec::new());
        }
        let mut st = self.conn.prepare_cached(
            "SELECT f.rel_path, f.title,
                    substr(notes_fts.body, max(1, instr(lower(notes_fts.body), ?4) - 40), 240)
             FROM notes_fts JOIN files f ON f.id = notes_fts.rowid
             WHERE notes_fts MATCH ?1 AND notes_fts.rowid IN (
                 SELECT notes_fts.rowid FROM notes_fts JOIN files g ON g.id = notes_fts.rowid
                  WHERE notes_fts MATCH ?1
                  ORDER BY lower(ifnull(g.title, '')) = lower(?2) DESC, bm25(notes_fts, 1.0, 10.0)
                  LIMIT ?3)
             ORDER BY lower(ifnull(f.title, '')) = lower(?2) DESC, bm25(notes_fts, 1.0, 10.0)",
        )?;
        let terms = terms(query);
        // The most specific term makes the most useful window, and SQLite's `lower` is ASCII, so
        // the needle is folded the same way or `instr` would never find it.
        let window = terms
            .iter()
            .max_by_key(|t| t.len())
            .cloned()
            .unwrap_or_default();
        let rows = st.query_map(
            params![q, query.trim(), limit as i64, window.to_ascii_lowercase()],
            |r| {
                let body: String = r.get(2)?;
                Ok(SearchHit {
                    rel_path: r.get(0)?,
                    title: r.get(1)?,
                    snippet: mark_terms(&body, &terms),
                })
            },
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Every hit of `re` in an indexed body, in `rel_path` order: at most `limit` of them, plus
    /// the total the scan saw, so a truncated list can still say how much a Replace All would
    /// touch.
    ///
    /// This is the exact-match counterpart of [`search`](Self::search): FTS5 answers "which files
    /// are about this", regexes answer "where exactly does this text occur". The bodies are
    /// already in the index, so nothing is read from disk, and the statement streams them one row
    /// at a time rather than materialising the whole vault's text.
    pub fn grep(&self, re: &Regex, limit: usize) -> Result<(Vec<Match>, usize)> {
        let mut st = self.conn.prepare_cached(GREP_SQL)?;
        let mut rows = st.query([])?;
        let (mut out, mut total) = (Vec::new(), 0usize);
        while let Some(row) = rows.next()? {
            let body: String = row.get(2)?;
            // Tested before the other columns are fetched: most notes do not match, and their
            // path and title would be allocated for nothing.
            if !re.is_match(&body) {
                continue;
            }
            let (rel_path, title): (String, Option<String>) = (row.get(0)?, row.get(1)?);
            Self::matches_in(
                &rel_path,
                title.as_deref(),
                &body,
                re,
                limit,
                &mut out,
                &mut total,
            );
        }
        Ok((out, total))
    }

    /// Every hit of `re` in one body, appended to `out` and counted in `total`.
    ///
    /// Lifted out of [`grep`](Self::grep) so the façade can run the same matching over files the
    /// index holds no body for — one too large for [`MAX_INDEXED_BODY`], or one under a tree the
    /// walk never entered — and hand the sidebar rows it cannot tell apart from a note's. It
    /// touches neither the index nor the disk: the caller supplies the text and says where it
    /// came from.
    ///
    /// `limit` caps `out` across all bodies rather than per body, and `total` keeps counting past
    /// it, so a truncated list can still say how much a Replace All would touch.
    pub fn matches_in(
        rel: &str,
        title: Option<&str>,
        body: &str,
        re: &Regex,
        limit: usize,
        out: &mut Vec<Match>,
        total: &mut usize,
    ) {
        // `find_iter` walks forward, so the line number follows it instead of being counted
        // from the start of the note for every hit.
        let (mut cursor, mut line, mut line_start) = (0usize, 1u32, 0usize);
        for m in re.find_iter(body) {
            *total += 1;
            if out.len() >= limit {
                continue;
            }
            while cursor < m.start() {
                if body.as_bytes()[cursor] == b'\n' {
                    line += 1;
                    line_start = cursor + 1;
                }
                cursor += 1;
            }
            let rest = &body[line_start..];
            let line_text = rest
                .split('\n')
                .next()
                .unwrap_or(rest)
                .trim_end_matches('\r');
            let start = m.start() - line_start;
            let end = (m.end() - line_start).min(line_text.len());
            let (line_text, range) = clip(line_text, start..end);
            out.push(Match {
                rel_path: rel.to_string(),
                title: title.map(str::to_string),
                line,
                line_text,
                range,
                offset: m.start(),
            });
        }
    }

    /// The notes whose body matches at all, in `rel_path` order. Uncapped on purpose: a global
    /// replace has to visit every file, not only the ones the sidebar had room to list.
    ///
    /// Markdown only, unlike [`grep`](Self::grep), which now reaches every indexed body: Replace
    /// All is Replace in Notes, and rewriting a source file from a notes app is not what the
    /// button offers.
    pub fn grep_paths(&self, re: &Regex) -> Result<Vec<String>> {
        let mut st = self.conn.prepare_cached(GREP_NOTES_SQL)?;
        let mut rows = st.query([FileKind::Markdown.as_i64()])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            let body: String = row.get(2)?;
            if re.is_match(&body) {
                out.push(row.get(0)?);
            }
        }
        Ok(out)
    }

    pub fn backlinks(&self, rel_path: &str) -> Result<Vec<Backlink>> {
        let mut st = self.conn.prepare_cached(
            "SELECT s.rel_path, l.byte_start, l.byte_end
             FROM links l
             JOIN files s ON s.id = l.src_file
             JOIN files t ON t.id = l.resolved_file
             WHERE t.rel_path = ?1
             ORDER BY s.rel_path, l.byte_start",
        )?;
        let rows = st.query_map([rel_path], |r| {
            Ok(Backlink {
                src_rel_path: r.get(0)?,
                byte_start: r.get(1)?,
                byte_end: r.get(2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn tags(&self) -> Result<Vec<(String, i64)>> {
        let mut st = self.conn.prepare_cached(
            "SELECT name, COUNT(*) c FROM tags GROUP BY name ORDER BY c DESC, name",
        )?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn files_with_tag(&self, name: &str) -> Result<Vec<FileRow>> {
        let mut st = self.conn.prepare_cached(
            "SELECT DISTINCT f.id, f.rel_path, f.kind, f.title, f.size, f.mtime_ns
             FROM tags t JOIN files f ON f.id = t.file_id
             WHERE t.name = ?1 ORDER BY f.rel_path",
        )?;
        let rows = st.query_map([name], file_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// `(source rel_path, target as written)` for every link that resolves to nothing.
    pub fn unresolved_links(&self) -> Result<Vec<(String, String)>> {
        let mut st = self.conn.prepare_cached(
            "SELECT s.rel_path, l.target FROM links l JOIN files s ON s.id = l.src_file
             WHERE l.resolved_file IS NULL AND l.kind <> 3
             ORDER BY s.rel_path, l.byte_start",
        )?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn stats(&self) -> Result<Stats> {
        let one = |sql: &str| -> Result<i64> { Ok(self.conn.query_row(sql, [], |r| r.get(0))?) };
        Ok(Stats {
            files: one("SELECT COUNT(*) FROM files WHERE kind <> 0")?,
            dirs: one("SELECT COUNT(*) FROM files WHERE kind = 0")?,
            notes: one("SELECT COUNT(*) FROM notes")?,
            links: one("SELECT COUNT(*) FROM links")?,
            tags: one("SELECT COUNT(DISTINCT name) FROM tags")?,
            conflicts: one("SELECT COUNT(*) FROM files WHERE kind = 4")?,
            aliases: one("SELECT COUNT(*) FROM aliases")?,
        })
    }
}

fn file_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<FileRow> {
    Ok(FileRow {
        id: r.get(0)?,
        rel_path: r.get(1)?,
        kind: FileKind::from_i64(r.get(2)?),
        title: r.get(3)?,
        size: r.get(4)?,
        mtime_ns: r.get(5)?,
    })
}

/// Every indexed path, title and text, for the sidebar's regex scan. Ordered so a capped list
/// and an uncapped one agree on which matches they drop.
const GREP_SQL: &str = "SELECT f.rel_path, f.title, n.body
     FROM notes n JOIN files f ON f.id = n.file_id
     ORDER BY f.rel_path";

/// [`GREP_SQL`] narrowed to markdown (`?1` is [`FileKind::Markdown`]), for the one caller that
/// rewrites what it finds.
const GREP_NOTES_SQL: &str = "SELECT f.rel_path, f.title, n.body
     FROM notes n JOIN files f ON f.id = n.file_id
     WHERE f.kind = ?1
     ORDER BY f.rel_path";

/// The slice of a matched line worth putting in a sidebar row, and where the match sits in it.
/// An elided end is marked with an ellipsis, so a clipped line does not read as the whole line.
fn clip(line: &str, range: Range<usize>) -> (String, Range<usize>) {
    let mut start = range.start.saturating_sub(CLIP_BEFORE);
    while start > 0 && !line.is_char_boundary(start) {
        start -= 1;
    }
    let mut end = range.end.saturating_add(CLIP_AFTER).min(line.len());
    while end < line.len() && !line.is_char_boundary(end) {
        end += 1;
    }
    let lead = match start {
        0 => "",
        _ => "…",
    };
    let tail = match end < line.len() {
        true => "…",
        false => "",
    };
    let text = format!("{lead}{}{tail}", &line[start..end]);
    // `start` is never past the match, so the shift back onto the clipped text cannot underflow.
    let at = |i: usize| i - start + lead.len();
    (text, at(range.start)..at(range.end))
}

/// The query's words: the units [`fts_query`] turns into FTS terms, and the ones a snippet marks.
fn terms(query: &str) -> Vec<&str> {
    query.split_whitespace().collect()
}

/// Wrap every occurrence of a query term in the guillemets the UI turns into bold, the way FTS5's
/// own `snippet()` did. The input is the 240-character window SQLite already cut, so this is a
/// pass over a row of text rather than over a note.
///
/// ponytail: matching is ASCII-case-insensitive rather than the `unicode61 remove_diacritics 2`
/// tokenizer that ranked the note, and the window carries no leading ellipsis because knowing
/// where it starts would cost a second `lower(body)` per row. A hit found only through diacritic
/// folding is therefore quoted without being marked. The snippet is a preview; ranking is exact.
fn mark_terms(window: &str, terms: &[&str]) -> String {
    let lower = window.to_ascii_lowercase();
    let needles: Vec<String> = terms
        .iter()
        .filter(|t| !t.is_empty())
        .map(|t| t.to_ascii_lowercase())
        .collect();
    let mut out = String::with_capacity(window.len() + 8 * needles.len());
    let mut i = 0;
    while i < window.len() {
        match needles.iter().find(|n| lower[i..].starts_with(n.as_str())) {
            Some(n) => {
                out.push('«');
                out.push_str(&window[i..i + n.len()]);
                out.push('»');
                i += n.len();
            }
            None => {
                let c = window[i..].chars().next().expect("i is a char boundary");
                out.push(c);
                i += c.len_utf8();
            }
        }
    }
    out
}

/// Turn a user query into safe FTS5 syntax: every token quoted, the last one a prefix match.
/// ponytail: no operator support (`AND`, `NEAR`, `-`). Quoting everything means a stray `"` or
/// `*` can never produce a syntax error; expose raw FTS later behind an explicit flag if wanted.
fn fts_query(q: &str) -> String {
    let toks: Vec<&str> = q.split_whitespace().collect();
    let last = toks.len().saturating_sub(1);
    toks.iter()
        .enumerate()
        .map(|(i, t)| {
            let esc = t.replace('"', "\"\"");
            if i == last {
                format!("\"{esc}\"*")
            } else {
                format!("\"{esc}\"")
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn fixture() -> (tempfile::TempDir, tempfile::TempDir) {
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

    fn open(db: &tempfile::TempDir) -> Index {
        Index::open(&db.path().join("i.db")).unwrap()
    }

    #[test]
    fn first_pass_indexes_then_second_pass_is_a_no_op() {
        let (vault, db) = fixture();
        let mut ix = open(&db);
        let s1 = ix.reconcile(vault.path(), |_| {}).unwrap();
        assert_eq!(s1.added, 5, "{s1:?}"); // 4 files + 1 dir
        assert_eq!(s1.unchanged, 0);
        assert_eq!(s1.conflicts, 1);

        let s2 = ix.reconcile(vault.path(), |_| {}).unwrap();
        assert_eq!(s2.added + s2.updated + s2.touched + s2.removed, 0, "{s2:?}");
        assert_eq!(s2.unchanged, 5);
        assert_eq!(s2.bytes_read, 0, "warm pass must not open any file");

        let st = ix.stats().unwrap();
        assert_eq!(st.files, 4);
        assert_eq!(st.notes, 2, "conflicts and pdfs are not searchable notes");
        assert_eq!(st.conflicts, 1);
    }

    #[test]
    fn touched_but_identical_content_only_updates_stats() {
        let (vault, db) = fixture();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();

        // Rewrite byte-identical content: mtime moves, hash does not.
        let p = vault.path().join("a.md");
        let body = fs::read(&p).unwrap();
        fs::write(&p, &body).unwrap();

        let s = ix.reconcile(vault.path(), |_| {}).unwrap();
        assert_eq!(s.touched, 1, "{s:?}");
        assert_eq!(s.updated, 0, "{s:?}");
        assert_eq!(s.unchanged, 4, "{s:?}");
    }

    #[test]
    fn content_change_reindexes_and_deletion_removes() {
        let (vault, db) = fixture();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();

        fs::write(vault.path().join("a.md"), "# Alpha\ncompletely new text\n").unwrap();
        fs::remove_file(vault.path().join("c.pdf")).unwrap();
        let s = ix.reconcile(vault.path(), |_| {}).unwrap();
        assert_eq!(s.updated, 1, "{s:?}");
        assert_eq!(s.removed, 1, "{s:?}");
        assert!(ix.get_file("c.pdf").unwrap().is_none());
        assert!(ix.get_file("a.md").unwrap().is_some());
    }

    #[test]
    fn lazy_tree_lists_one_level() {
        let (vault, db) = fixture();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();

        let top: Vec<_> = ix
            .list_files("")
            .unwrap()
            .into_iter()
            .map(|f| f.rel_path)
            .collect();
        assert_eq!(
            top,
            vec![
                "sub",
                "a.md",
                "a.sync-conflict-20240101-120000-ABCDEFG.md",
                "c.pdf"
            ]
        );
        let sub: Vec<_> = ix
            .list_files("sub")
            .unwrap()
            .into_iter()
            .map(|f| f.rel_path)
            .collect();
        assert_eq!(sub, vec!["sub/Beta.md"]);
    }

    #[test]
    fn fts_search_finds_note_bodies() {
        let (vault, db) = fixture();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();

        let hits = ix.search("ferris", 10).unwrap();
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!(hits[0].rel_path, "sub/Beta.md");
        assert!(hits[0].snippet.contains("ferris"), "{:?}", hits[0].snippet);

        // Conflicts are stored but never searchable.
        assert!(ix.search("conflicted", 10).unwrap().is_empty());
        // Garbage in must not be a SQL/FTS syntax error.
        assert!(ix.search("\"unbalanced AND *", 10).unwrap().is_empty());
        assert!(ix.search("", 10).unwrap().is_empty());
    }

    #[test]
    fn a_snippet_marks_every_query_term_it_can_see() {
        let words = terms("Ferris the crab");
        assert_eq!(
            mark_terms("A FERRIS and a crab, plus THE rest", &words),
            "A «FERRIS» and a «crab», plus «THE» rest"
        );
        // A term the window does not hold is simply not marked, and an empty query marks nothing.
        assert_eq!(mark_terms("nothing here", &words), "nothing here");
        assert_eq!(mark_terms("äöü ferris", &words[..1]), "äöü «ferris»");
        assert_eq!(mark_terms("as is", &terms("")), "as is");
    }

    #[test]
    fn grep_lists_one_row_per_match_with_its_line() {
        let (vault, db) = fixture();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();
        fs::write(
            vault.path().join("a.md"),
            "# Alpha\nferris and ferris\nlater ferris\n",
        )
        .unwrap();
        ix.reconcile(vault.path(), |_| {}).unwrap();

        let re = crate::search::pattern("ferris", crate::search::Options::default()).unwrap();
        let (hits, total) = ix.grep(&re, 10).unwrap();
        assert_eq!(total, 4, "three in a.md, one in sub/Beta.md: {hits:?}");
        assert_eq!(hits.len(), 4);
        assert_eq!(hits[0].rel_path, "a.md");
        assert_eq!((hits[0].line, hits[1].line, hits[2].line), (2, 2, 3));
        assert_eq!(&hits[0].line_text[hits[0].range.clone()], "ferris");
        assert_eq!(&hits[1].line_text[hits[1].range.clone()], "ferris");
        // The offset addresses the note, not the line, so opening it can place the caret.
        let body = fs::read_to_string(vault.path().join("a.md")).unwrap();
        assert_eq!(&body[hits[2].offset..hits[2].offset + 6], "ferris");

        // The cap truncates the list but not the count a Replace All is measured against.
        let (few, total) = ix.grep(&re, 2).unwrap();
        assert_eq!((few.len(), total), (2, 4));
        assert_eq!(ix.grep_paths(&re).unwrap(), ["a.md", "sub/Beta.md"]);
    }

    #[test]
    fn clip_keeps_the_match_visible_in_a_long_line() {
        let line = format!("{}MATCH{}", "ä".repeat(500), "b".repeat(500));
        let at = line.find("MATCH").unwrap();
        let (text, range) = clip(&line, at..at + 5);
        assert_eq!(&text[range], "MATCH");
        assert!(text.starts_with('…') && text.ends_with('…'), "{text}");
        assert!(text.len() < 300, "{}", text.len());

        // A short line is passed through untouched.
        let (text, range) = clip("a MATCH b", 2..7);
        assert_eq!((text.as_str(), &text[range]), ("a MATCH b", "MATCH"));
    }

    /// Two notes for the ranking tests: the query is `target.md`'s title, and also a phrase
    /// `spam.md` repeats. Ranking on the body alone sorts these the wrong way round.
    fn ranking_vault() -> (tempfile::TempDir, tempfile::TempDir) {
        let vault = tempfile::tempdir().unwrap();
        fs::write(
            vault.path().join("target.md"),
            "# Quantum Coherence Ledger\nA short paragraph on where the numbers come from.\n",
        )
        .unwrap();
        // A long note that says the words a handful of times, which is what beat the note the
        // user was looking for before the title was indexed.
        fs::write(
            vault.path().join("spam.md"),
            format!(
                "# Meeting Notes\n{}",
                "quantum coherence ledger, plus a sentence of unrelated meeting prose. ".repeat(5)
            ),
        )
        .unwrap();
        (vault, tempfile::tempdir().unwrap())
    }

    #[test]
    fn exact_title_outranks_a_body_that_repeats_the_words() {
        let (vault, db) = ranking_vault();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();

        let hits = ix.search("Quantum Coherence Ledger", 10).unwrap();
        assert_eq!(hits.len(), 2, "{hits:?}");
        assert_eq!(hits[0].rel_path, "target.md", "{hits:?}");

        // Case and stray whitespace must not lose the exact-title match.
        let hits = ix.search("  quantum COHERENCE ledger ", 10).unwrap();
        assert_eq!(hits[0].rel_path, "target.md", "{hits:?}");
    }

    #[test]
    fn partial_title_match_outranks_a_body_match() {
        let (vault, db) = ranking_vault();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();

        // Not the whole title, so only the bm25 title weight can decide this one.
        let hits = ix.search("coherence ledger", 10).unwrap();
        assert_eq!(hits.len(), 2, "{hits:?}");
        assert_eq!(hits[0].rel_path, "target.md", "{hits:?}");
    }

    /// The delete half of the FTS triggers, which is the half that fails silently: a stale title
    /// row would keep answering searches for a name the note no longer has.
    #[test]
    fn retitling_a_note_leaves_no_stale_title_in_the_index() {
        let (vault, db) = ranking_vault();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();

        fs::write(
            vault.path().join("target.md"),
            "# Photon Budget\nA short paragraph on where the numbers come from.\n",
        )
        .unwrap();
        assert_eq!(
            ix.update_file(vault.path(), "target.md").unwrap(),
            Change::Updated(FileKind::Markdown)
        );

        assert_eq!(
            ix.search("Photon Budget", 10).unwrap()[0].rel_path,
            "target.md"
        );
        let hits = ix.search("Quantum Coherence Ledger", 10).unwrap();
        assert_eq!(
            hits.iter().map(|h| h.rel_path.as_str()).collect::<Vec<_>>(),
            ["spam.md"],
            "the old title is still in the index"
        );
        ix.conn
            .execute(
                "INSERT INTO notes_fts(notes_fts) VALUES('integrity-check')",
                [],
            )
            .unwrap();
    }

    #[test]
    fn snippet_comes_from_the_body_not_the_title() {
        let vault = tempfile::tempdir().unwrap();
        // No H1 and no frontmatter, so the title is the file stem and lives only in the title
        // column: a hit on it can only quote the body.
        fs::write(
            vault.path().join("Kryptonite Ledger.md"),
            "plain prose about a lattice, never naming the file\n",
        )
        .unwrap();
        let db = tempfile::tempdir().unwrap();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();

        let hits = ix.search("kryptonite", 10).unwrap();
        assert_eq!(hits.len(), 1, "the title must be searchable: {hits:?}");
        assert!(
            hits[0].snippet.starts_with("plain prose"),
            "{:?}",
            hits[0].snippet
        );
        assert!(
            !hits[0].snippet.contains("Kryptonite"),
            "the snippet must quote the body, not the title: {:?}",
            hits[0].snippet
        );
    }

    #[test]
    fn edited_note_leaves_the_fts_index_consistent() {
        let (vault, db) = fixture();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();
        fs::write(
            vault.path().join("sub/Beta.md"),
            "# Beta\nnow about crabs\n",
        )
        .unwrap();
        ix.reconcile(vault.path(), |_| {}).unwrap();

        assert!(ix.search("ferris", 10).unwrap().is_empty(), "stale FTS row");
        assert_eq!(ix.search("crabs", 10).unwrap().len(), 1);
    }

    #[test]
    fn links_resolve_by_shortest_path_and_backlinks_read_back() {
        let (vault, db) = fixture();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();

        // End to end: `a.md` really contains `[[Beta]]`, which must reach `sub/Beta.md`.
        let bl = ix.backlinks("sub/Beta.md").unwrap();
        assert_eq!(bl.len(), 1, "{bl:?}");
        assert_eq!(bl[0].src_rel_path, "a.md");

        // Then drive the resolver directly, to pin the matching rules independently of the
        // analyser: bare name, full path, and a target that matches nothing.
        let src: i64 = ix.get_file("a.md").unwrap().unwrap().id;
        ix.conn.execute("DELETE FROM links", []).unwrap();
        ix.conn
            .execute(
                "INSERT INTO links(src_file, target, kind, byte_start, byte_end)
                 VALUES(?1, 'beta', 0, 10, 16), (?1, 'sub/Beta.md', 0, 20, 31), (?1, 'Nope', 0, 40, 44)",
                [src],
            )
            .unwrap();
        assert_eq!(ix.resolve_links().unwrap(), 2);
        assert_eq!(ix.backlinks("sub/Beta.md").unwrap().len(), 2);
        assert_eq!(
            ix.unresolved_links().unwrap(),
            vec![("a.md".into(), "Nope".into())]
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

    #[test]
    fn aliases_are_recorded() {
        let vault = tempfile::tempdir().unwrap();
        fs::write(vault.path().join("a.md"), "x").unwrap();
        std::os::unix::fs::symlink(vault.path().join("a.md"), vault.path().join("b.md")).unwrap();
        let db = tempfile::tempdir().unwrap();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();
        assert_eq!(ix.stats().unwrap().aliases, 1);
    }

    /// Every row two indexes must agree on, keyed by path: `ino`, `mtime` and `canonical` are
    /// per-vault by nature, so comparing them across two temp dirs would say nothing.
    fn dump(ix: &Index) -> Vec<String> {
        let mut out = Vec::new();
        let mut push = |sql: &str, cols: usize| {
            let mut st = ix.conn.prepare(sql).unwrap();
            let rows = st
                .query_map([], |r| {
                    let mut line = String::new();
                    for i in 0..cols {
                        line.push_str(&format!("{:?}|", r.get_ref(i).unwrap()));
                    }
                    Ok(line)
                })
                .unwrap();
            out.extend(rows.map(|r| r.unwrap()));
        };
        push(
            "SELECT rel_path, parent_dir, kind, title FROM files ORDER BY rel_path",
            4,
        );
        push(
            "SELECT s.rel_path, l.target, t.rel_path, l.kind, l.anchor, l.byte_start, l.byte_end
             FROM links l JOIN files s ON s.id = l.src_file
             LEFT JOIN files t ON t.id = l.resolved_file
             ORDER BY s.rel_path, l.byte_start",
            7,
        );
        push(
            "SELECT f.rel_path, g.name, g.byte_start FROM tags g JOIN files f ON f.id = g.file_id
             ORDER BY 1, 3",
            3,
        );
        push(
            "SELECT f.rel_path, h.level, h.text, h.byte_start FROM headings h
             JOIN files f ON f.id = h.file_id ORDER BY 1, 4",
            4,
        );
        out
    }

    #[test]
    fn update_file_adds_note_and_resolves_incoming_links() {
        let (vault, db) = fixture();
        fs::write(vault.path().join("a.md"), "# Alpha\nsee [[Gamma]]\n").unwrap();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();
        assert_eq!(ix.unresolved_links().unwrap().len(), 1);

        fs::write(vault.path().join("Gamma.md"), "# Gamma\n").unwrap();
        assert_eq!(
            ix.update_file(vault.path(), "Gamma.md").unwrap(),
            Change::Added(FileKind::Markdown)
        );
        assert!(
            ix.unresolved_links().unwrap().is_empty(),
            "the new note must resolve the link that was waiting for it"
        );
        assert_eq!(ix.backlinks("Gamma.md").unwrap().len(), 1);
        assert_eq!(
            ix.get_file("Gamma.md").unwrap().unwrap().title.unwrap(),
            "Gamma"
        );
    }

    /// The worker indexes a whole batch and resolves once at the end: resolution is a
    /// whole-vault pass, so paying for it per file turns a Syncthing pull into seconds of work.
    #[test]
    fn batched_update_leaves_link_resolution_to_the_caller() {
        let (vault, db) = fixture();
        fs::write(vault.path().join("a.md"), "# Alpha\nsee [[Gamma]]\n").unwrap();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();
        assert_eq!(ix.unresolved_links().unwrap().len(), 1);

        fs::write(vault.path().join("Gamma.md"), "# Gamma\n").unwrap();
        assert_eq!(
            ix.update_file_batched(vault.path(), "Gamma.md").unwrap(),
            Change::Added(FileKind::Markdown)
        );
        assert_eq!(
            ix.unresolved_links().unwrap().len(),
            1,
            "the batched variant must not resolve on its own"
        );

        ix.resolve_links().unwrap();
        assert!(ix.unresolved_links().unwrap().is_empty());
        assert_eq!(ix.backlinks("Gamma.md").unwrap().len(), 1);
    }

    #[test]
    fn update_file_unchanged_when_stat_matches() {
        let (vault, db) = fixture();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();

        // A hand-edited title only survives if `update_file` wrote nothing at all.
        ix.conn
            .execute(
                "UPDATE files SET title = 'sentinel' WHERE rel_path = 'a.md'",
                [],
            )
            .unwrap();
        assert_eq!(
            ix.update_file(vault.path(), "a.md").unwrap(),
            Change::Unchanged
        );
        assert_eq!(
            ix.get_file("a.md").unwrap().unwrap().title.as_deref(),
            Some("sentinel")
        );
    }

    #[test]
    fn update_file_keeps_fts_consistent() {
        let (vault, db) = fixture();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();

        fs::write(
            vault.path().join("sub/Beta.md"),
            "# Beta\nnow about crabs\n",
        )
        .unwrap();
        assert_eq!(
            ix.update_file(vault.path(), "sub/Beta.md").unwrap(),
            Change::Updated(FileKind::Markdown)
        );
        assert!(ix.search("ferris", 10).unwrap().is_empty(), "stale FTS row");
        assert_eq!(ix.search("crabs", 10).unwrap().len(), 1);
    }

    #[test]
    fn update_file_ignores_temp_and_hard_skipped_names() {
        let (vault, db) = fixture();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();

        fs::create_dir(vault.path().join(".git")).unwrap();
        fs::write(vault.path().join(".git/config"), "c").unwrap();
        fs::write(vault.path().join(".syncthing.n.md.tmp"), "t").unwrap();
        fs::write(vault.path().join(".accent-xyz"), "t").unwrap();

        for rel in [".git/config", ".syncthing.n.md.tmp", ".accent-xyz"] {
            assert_eq!(
                ix.update_file(vault.path(), rel).unwrap(),
                Change::Ignored,
                "{rel}"
            );
            assert!(ix.get_file(rel).unwrap().is_none(), "{rel}");
        }
    }

    #[test]
    fn remove_file_drops_prefix_and_unresolves_links() {
        let (vault, db) = fixture();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();
        assert_eq!(ix.backlinks("sub/Beta.md").unwrap().len(), 1);

        // One event for the directory has to take the note under it with it.
        assert_eq!(ix.remove_file("sub").unwrap(), 2);
        assert!(ix.get_file("sub").unwrap().is_none());
        assert!(ix.get_file("sub/Beta.md").unwrap().is_none());
        assert!(ix.search("ferris", 10).unwrap().is_empty());
        assert_eq!(
            ix.unresolved_links().unwrap(),
            vec![("a.md".to_string(), "Beta".to_string())]
        );
        assert_eq!(
            ix.remove_file("sub").unwrap(),
            0,
            "removing twice is a no-op"
        );
    }

    #[test]
    fn resolve_target_prefers_shortest_path_case_insensitively() {
        let (vault, db) = fixture();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();

        assert_eq!(
            ix.resolve_target("beta").unwrap().as_deref(),
            Some("sub/Beta.md")
        );
        assert_eq!(
            ix.resolve_target("Beta.md").unwrap().as_deref(),
            Some("sub/Beta.md")
        );
        assert_eq!(
            ix.resolve_target("./A.MD").unwrap().as_deref(),
            Some("a.md")
        );
        assert_eq!(
            ix.resolve_target("sub/beta.md").unwrap().as_deref(),
            Some("sub/Beta.md")
        );
        assert_eq!(ix.resolve_target("Nope").unwrap(), None);
        assert_eq!(
            ix.resolve_target("sub").unwrap(),
            None,
            "a directory is not a link target"
        );

        // A deeper namesake must not win over the shallower one.
        fs::create_dir(vault.path().join("sub/deep")).unwrap();
        fs::write(vault.path().join("sub/deep/Beta.md"), "# Beta\n").unwrap();
        ix.reconcile(vault.path(), |_| {}).unwrap();
        assert_eq!(
            ix.resolve_target("beta").unwrap().as_deref(),
            Some("sub/Beta.md")
        );
    }

    /// The incremental path must produce the rows a full reconcile would, or the index slowly
    /// drifts away from the vault between restarts.
    #[test]
    fn reconcile_and_update_file_yield_identical_rows() {
        let (whole, db_whole) = fixture();
        let mut ix_whole = open(&db_whole);
        ix_whole.reconcile(whole.path(), |_| {}).unwrap();

        let (partial, db_partial) = fixture();
        let note = partial.path().join("sub/Beta.md");
        let body = fs::read_to_string(&note).unwrap();
        fs::remove_file(&note).unwrap();
        let mut ix_partial = open(&db_partial);
        ix_partial.reconcile(partial.path(), |_| {}).unwrap();

        fs::write(&note, &body).unwrap();
        assert_eq!(
            ix_partial
                .update_file(partial.path(), "sub/Beta.md")
                .unwrap(),
            Change::Added(FileKind::Markdown)
        );
        assert_eq!(dump(&ix_partial), dump(&ix_whole));
    }

    #[test]
    fn headings_read_back() {
        let (vault, db) = fixture();
        fs::write(vault.path().join("h.md"), "# One\ntext\n\n## Two\n").unwrap();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();

        let hs = ix.headings("h.md").unwrap();
        assert_eq!(
            hs.iter()
                .map(|h| (h.level, h.text.as_str()))
                .collect::<Vec<_>>(),
            vec![(1, "One"), (2, "Two")]
        );
        assert_eq!(hs[0].byte_start, 0);
        assert!(ix.headings("nope.md").unwrap().is_empty());
    }

    #[test]
    fn conflicts_lists_conflict_copies() {
        let (vault, db) = fixture();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();
        assert_eq!(
            ix.conflicts().unwrap(),
            vec!["a.sync-conflict-20240101-120000-ABCDEFG.md".to_string()]
        );
    }

    #[test]
    fn symlink_dirs_lists_external_targets() {
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("ext.md"), "ext").unwrap();
        let vault = tempfile::tempdir().unwrap();
        fs::create_dir(vault.path().join("plain")).unwrap();
        std::os::unix::fs::symlink(outside.path(), vault.path().join("linked")).unwrap();
        let db = tempfile::tempdir().unwrap();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();

        // Only the link is listed here; the plain directory is an ordinary member of the watch set.
        assert_eq!(
            ix.dirs(vault.path()).unwrap(),
            vec![vault.path().join("linked"), vault.path().join("plain")]
        );
        assert_eq!(
            ix.symlink_dirs(vault.path()).unwrap(),
            vec![(outside.path().canonicalize().unwrap(), "linked".to_string())]
        );
        assert_eq!(ix.stats().unwrap().dirs, 2);
    }

    #[test]
    fn parent_dir_of_rel_path() {
        assert_eq!(parent_dir("a.md"), "");
        assert_eq!(parent_dir("sub/Beta.md"), "sub");
        assert_eq!(parent_dir("a/b/c.md"), "a/b");
    }

    /// `list_files` is one directory level: the root must not report grandchildren, and a
    /// subdirectory must report its own children whatever the prefix's slashes look like.
    #[test]
    fn list_files_returns_direct_children_only() {
        let (vault, db) = fixture();
        fs::create_dir(vault.path().join("sub/deep")).unwrap();
        fs::write(vault.path().join("sub/deep/d.md"), "# D").unwrap();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();

        let root: Vec<String> = ix
            .list_files("")
            .unwrap()
            .into_iter()
            .map(|f| f.rel_path)
            .collect();
        assert!(root.contains(&"sub".to_string()), "{root:?}");
        assert!(root.contains(&"a.md".to_string()), "{root:?}");
        assert!(
            !root.iter().any(|r| r.contains('/')),
            "root level leaked a grandchild: {root:?}"
        );
        // Directories sort before files.
        assert_eq!(root[0], "sub", "{root:?}");

        for prefix in ["sub", "/sub/", "sub/"] {
            let kids: Vec<String> = ix
                .list_files(prefix)
                .unwrap()
                .into_iter()
                .map(|f| f.rel_path)
                .collect();
            assert_eq!(kids, vec!["sub/deep", "sub/Beta.md"], "prefix {prefix:?}");
        }

        assert_eq!(ix.list_files("nope").unwrap().len(), 0);
    }

    #[test]
    fn note_paths_and_recent_notes_list_markdown_only() {
        let (vault, db) = fixture();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();

        // c.pdf, the conflict copy and the `sub` directory are not notes.
        assert_eq!(ix.note_paths().unwrap(), vec!["a.md", "sub/Beta.md"]);
        assert_eq!(ix.recent_notes(50).unwrap().len(), 2);
        assert_eq!(ix.recent_notes(1).unwrap().len(), 1, "limit is honoured");
    }

    #[test]
    fn file_paths_lists_notes_first_then_the_other_kinds() {
        let (vault, db) = fixture();
        fs::write(vault.path().join("tool.py"), "print('hi')\n").unwrap();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();

        // Notes first, then everything else by path. The `sub` directory and the conflict copy
        // are in neither list.
        assert_eq!(
            ix.file_paths().unwrap(),
            vec!["a.md", "sub/Beta.md", "c.pdf", "tool.py"]
        );
    }

    /// A text file that is not a note is searchable, a huge or binary one is not, and none of
    /// them is analysed as markdown.
    #[test]
    fn non_markdown_text_is_indexed_within_the_cap() {
        let (vault, db) = fixture();
        fs::write(
            vault.path().join("tool.py"),
            "# zorblat helper\nimport os  # not #atag\n",
        )
        .unwrap();
        fs::write(vault.path().join("big.txt"), "zorblat\n".repeat(300_000)).unwrap();
        fs::write(vault.path().join("bin.dat"), b"\0zorblat\n").unwrap();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();

        assert!(vault.path().join("big.txt").metadata().unwrap().len() > MAX_INDEXED_BODY);

        // Both query paths reach the source file with no change of their own.
        let hits = ix.search("zorblat", 10).unwrap();
        assert_eq!(
            hits.iter().map(|h| h.rel_path.as_str()).collect::<Vec<_>>(),
            ["tool.py"],
            "{hits:?}"
        );
        let re = crate::search::pattern("zorblat", crate::search::Options::default()).unwrap();
        let (matches, total) = ix.grep(&re, 10).unwrap();
        assert_eq!(total, 1);
        assert_eq!(matches[0].rel_path, "tool.py");

        // Over the cap and binary: a stat row each, and nothing to match against.
        let body_count = |rel: &str| -> i64 {
            ix.conn
                .query_row(
                    "SELECT COUNT(*) FROM notes n JOIN files f ON f.id = n.file_id
                     WHERE f.rel_path = ?1",
                    [rel],
                    |r| r.get(0),
                )
                .unwrap()
        };
        assert_eq!(body_count("tool.py"), 1);
        assert_eq!(body_count("big.txt"), 0, "over MAX_INDEXED_BODY");
        assert_eq!(body_count("bin.dat"), 0, "a NUL byte is not text");
        assert!(
            ix.get_file("big.txt").unwrap().is_some(),
            "still a file row"
        );
        assert!(
            ix.get_file("bin.dat").unwrap().is_some(),
            "still a file row"
        );

        // The markdown analyser never sees it: `#` is a comment, not a tag or a heading.
        assert!(ix.headings("tool.py").unwrap().is_empty());
        assert!(
            !ix.tags().unwrap().iter().any(|(t, _)| t == "atag"),
            "{:?}",
            ix.tags().unwrap()
        );
        assert_eq!(
            ix.get_file("tool.py").unwrap().unwrap().title.as_deref(),
            Some("tool"),
            "the file stem is the title fallback"
        );

        // Replace All stays notes-only: `a` is in all three bodies, only the notes come back.
        let re = crate::search::pattern("a", crate::search::Options::default()).unwrap();
        assert_eq!(ix.grep_paths(&re).unwrap(), vec!["a.md", "sub/Beta.md"]);
    }
}
