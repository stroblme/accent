//! The tables, and the knobs that shape how they are filled.

/// Bump on any schema change: `open` then drops and recreates the cache.
pub(super) const SCHEMA_VERSION: i64 = 7;

/// Biggest non-markdown file whose text goes into the index.
///
/// Deliberately far stricter than [`crate::fs::MAX_TEXT`] (16 MiB), which is the cap on what a
/// tab will *open*: opening a 15 MiB generated file is something the user asked for once, while
/// indexing it is something the vault pays for on every reconcile, in database size and in the
/// FTS terms every later query has to merge. Measured on `testvault/`: none of its 17 700
/// non-note text files reach 1 MiB, so the cap costs nothing a real vault would notice. A note
/// is never subject to it — markdown is read whatever its size.
pub(super) const MAX_INDEXED_BODY: u64 = 1024 * 1024;

/// Files per write transaction. Big enough to amortise the WAL commit, small enough that a
/// killed process loses little work and progress reporting stays lively.
pub(super) const BATCH: usize = 500;

pub(super) const SCHEMA: &str = r#"
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
    content_hash BLOB,
    -- Written by the git refresh, not by the walk: the vault tree is walked whatever the
    -- ignore files say (hiding the user's notes is never right), and this is what lets a
    -- query leave the build output out again without the walk having to guess.
    git_ignored  INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE aliases(rel_path TEXT PRIMARY KEY, file_id INTEGER NOT NULL);
CREATE TABLE links(
    src_file      INTEGER NOT NULL,
    target        TEXT NOT NULL,
    resolved_file INTEGER,
    kind          INTEGER NOT NULL,
    anchor        TEXT,
    -- The link's `|alias`, kept for one reader: a PDF highlight re-anchors by the text it quotes
    -- when the selection numbers no longer fit the document's lines.
    alias         TEXT,
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

pub(super) const DROP_ALL: &str = r#"
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
