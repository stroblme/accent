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
use std::path::Path;
use std::time::Instant;

/// Bump on any schema change: `open` then drops and recreates the cache.
const SCHEMA_VERSION: i64 = 1;
/// Files per write transaction. Big enough to amortise the WAL commit, small enough that a
/// killed process loses little work and progress reporting stays lively.
const BATCH: usize = 500;

const SCHEMA: &str = r#"
CREATE TABLE files(
    id           INTEGER PRIMARY KEY,
    rel_path     TEXT UNIQUE NOT NULL,
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
CREATE TABLE notes(file_id INTEGER PRIMARY KEY, body TEXT NOT NULL);

CREATE VIRTUAL TABLE notes_fts USING fts5(
    body, content='notes', content_rowid='file_id', tokenize="unicode61 remove_diacritics 2"
);
CREATE TRIGGER notes_ai AFTER INSERT ON notes BEGIN
    INSERT INTO notes_fts(rowid, body) VALUES (new.file_id, new.body);
END;
CREATE TRIGGER notes_ad AFTER DELETE ON notes BEGIN
    INSERT INTO notes_fts(notes_fts, rowid, body) VALUES ('delete', old.file_id, old.body);
END;
CREATE TRIGGER notes_au AFTER UPDATE ON notes BEGIN
    INSERT INTO notes_fts(notes_fts, rowid, body) VALUES ('delete', old.file_id, old.body);
    INSERT INTO notes_fts(rowid, body) VALUES (new.file_id, new.body);
END;

CREATE INDEX idx_links_target   ON links(target);
CREATE INDEX idx_links_resolved ON links(resolved_file);
CREATE INDEX idx_links_src      ON links(src_file);
CREATE INDEX idx_tags_name      ON tags(name);
CREATE INDEX idx_tags_file      ON tags(file_id);
CREATE INDEX idx_headings_file  ON headings(file_id);
CREATE INDEX idx_files_devino   ON files(dev, ino);
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
    let canonical = vault.canonicalize().unwrap_or_else(|_| vault.to_path_buf());
    let digest = blake3::hash(canonical.as_os_str().as_encoded_bytes()).to_hex();
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".cache")))
        .unwrap_or_else(std::env::temp_dir);
    base.join("accent").join(format!("{}.db", &digest[..16]))
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Backlink {
    pub src_rel_path: String,
    pub byte_start: i64,
    pub byte_end: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stats {
    pub files: i64,
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
            skipped_symlinks: scan.skipped.len(),
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
                let f = &scan.files[job.idx];
                let indexable = f.kind == FileKind::Markdown;

                // ponytail: only markdown is read and hashed. PDFs and binaries are cheap
                // `(mtime, size, ino)` rows here; the `pdf` feature will add text extraction
                // and can reuse the same hash column when it does.
                let (hash, text) = if indexable {
                    match std::fs::read(&f.canonical) {
                        Ok(bytes) => {
                            stats.bytes_read += bytes.len() as u64;
                            let h = blake3::hash(&bytes);
                            (Some(h), Some(String::from_utf8_lossy(&bytes).into_owned()))
                        }
                        // Vanished or unreadable mid-walk: keep the stat row, drop the content.
                        Err(_) => (None, None),
                    }
                } else {
                    (None, None)
                };

                // Syncthing preserves origin mtimes, so mtime alone lies both ways; the hash is
                // the arbiter for "did the content really change".
                let same_content = match (job.existing_id, hash.as_ref()) {
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

                if same_content {
                    tx.prepare_cached(
                        "UPDATE files SET canonical=?2, dev=?3, ino=?4, mtime_ns=?5, size=?6 WHERE id=?1",
                    )?
                    .execute(params![
                        job.existing_id.unwrap(),
                        f.canonical.to_string_lossy(),
                        f.dev as i64,
                        f.ino as i64,
                        f.mtime_ns,
                        f.size as i64,
                    ])?;
                    stats.touched += 1;
                    done += 1;
                    continue;
                }

                let analysis = text.as_deref().map(markdown::analyze);
                let title = analysis
                    .as_ref()
                    .and_then(|a| a.title.clone())
                    .or_else(|| file_stem(&f.rel_path));

                let id: i64 = tx
                    .prepare_cached(
                        "INSERT INTO files(rel_path, canonical, dev, ino, mtime_ns, size, kind, title, content_hash)
                         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)
                         ON CONFLICT(rel_path) DO UPDATE SET
                            canonical=excluded.canonical, dev=excluded.dev, ino=excluded.ino,
                            mtime_ns=excluded.mtime_ns, size=excluded.size, kind=excluded.kind,
                            title=excluded.title, content_hash=excluded.content_hash
                         RETURNING id",
                    )?
                    .query_row(
                        params![
                            f.rel_path,
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

                if job.existing_id.is_some() {
                    clear_derived(&tx, id)?;
                    stats.updated += 1;
                } else {
                    stats.added += 1;
                }

                if let (Some(a), Some(body)) = (analysis.as_ref(), text.as_ref()) {
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
                        tx.prepare_cached(
                            "INSERT INTO tags(file_id, name, byte_start) VALUES(?1,?2,?3)",
                        )?
                        .execute(params![id, t.name, t.range.start as i64])?;
                    }
                    for h in &a.headings {
                        tx.prepare_cached(
                            "INSERT INTO headings(file_id, level, text, byte_start) VALUES(?1,?2,?3,?4)",
                        )?
                        .execute(params![id, h.level as i64, h.text, h.range.start as i64])?;
                    }
                    // Explicit delete + insert: REPLACE would only fire the FTS delete trigger
                    // with recursive_triggers on.
                    tx.prepare_cached("DELETE FROM notes WHERE file_id = ?1")?
                        .execute([id])?;
                    tx.prepare_cached("INSERT INTO notes(file_id, body) VALUES(?1,?2)")?
                        .execute(params![id, body])?;
                }
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
    /// ponytail: this re-resolves the whole `links` table on every dirty reconcile, because
    /// adding one note can resolve dangling links anywhere in the vault. At ~23k links on the
    /// reference vault that is tens of milliseconds and runs off the UI thread. If it ever
    /// shows up, narrow it to the targets whose candidate set actually changed.
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
                let stem = strip_ext(&rel);
                let base = rel.rsplit('/').next().unwrap_or(&rel).to_string();
                let base_stem = strip_ext(&base);
                for key in [rel.clone(), stem, base, base_stem] {
                    let key = key.to_lowercase();
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
            let mut up =
                tx.prepare("UPDATE links SET resolved_file = ?1 WHERE target = ?2")?;
            for t in &targets {
                let key = t
                    .trim()
                    .trim_start_matches("./")
                    .replace('\\', "/")
                    .to_lowercase();
                if let Some((_, id)) = by_key.get(&key) {
                    resolved += up.execute(params![id, t])?;
                }
            }
        }
        tx.commit()?;
        Ok(resolved)
    }
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

fn strip_ext(s: &str) -> String {
    match s.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && !ext.contains('/') => stem.to_string(),
        _ => s.to_string(),
    }
}

fn file_stem(rel_path: &str) -> Option<String> {
    let base = rel_path.rsplit('/').next()?;
    Some(strip_ext(base))
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
    pub fn list_files(&self, prefix: &str) -> Result<Vec<FileRow>> {
        let p = match prefix.trim_matches('/') {
            "" => String::new(),
            d => format!("{d}/"),
        };
        let mut st = self.conn.prepare_cached(
            "SELECT id, rel_path, kind, title, size, mtime_ns FROM files
             WHERE substr(rel_path, 1, length(?1)) = ?1
               AND instr(substr(rel_path, length(?1) + 1), '/') = 0
               AND length(rel_path) > length(?1)
             ORDER BY kind <> 0, rel_path COLLATE NOCASE",
        )?;
        let rows = st.query_map([&p], file_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn get_file(&self, rel_path: &str) -> Result<Option<FileRow>> {
        let mut st = self.conn.prepare_cached(
            "SELECT id, rel_path, kind, title, size, mtime_ns FROM files WHERE rel_path = ?1",
        )?;
        Ok(st.query_row([rel_path], file_row).optional()?)
    }

    /// Full-text search over note bodies. Conflict/PDF/binary files are never in `notes`.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        let q = fts_query(query);
        if q.is_empty() {
            return Ok(Vec::new());
        }
        let mut st = self.conn.prepare_cached(
            "SELECT f.rel_path, f.title, snippet(notes_fts, 0, '«', '»', '…', 12)
             FROM notes_fts JOIN files f ON f.id = notes_fts.rowid
             WHERE notes_fts MATCH ?1
             ORDER BY bm25(notes_fts) LIMIT ?2",
        )?;
        let rows = st.query_map(params![q, limit as i64], |r| {
            Ok(SearchHit {
                rel_path: r.get(0)?,
                title: r.get(1)?,
                snippet: r.get(2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
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
            if i == last { format!("\"{esc}\"*") } else { format!("\"{esc}\"") }
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
        fs::write(vault.path().join("a.md"), "# Alpha\nsee [[Beta]] and #rust\n").unwrap();
        fs::write(vault.path().join("sub/Beta.md"), "# Beta\nbody about ferris\n").unwrap();
        fs::write(vault.path().join("c.pdf"), b"%PDF-1.4 not really").unwrap();
        fs::write(
            vault.path().join("a.sync-conflict-20240101-120000-ABCDEFG.md"),
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
    fn edited_note_leaves_the_fts_index_consistent() {
        let (vault, db) = fixture();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();
        fs::write(vault.path().join("sub/Beta.md"), "# Beta\nnow about crabs\n").unwrap();
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
        assert_eq!(ix.unresolved_links().unwrap(), vec![("a.md".into(), "Nope".into())]);
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
}
