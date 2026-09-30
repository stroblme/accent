//! The tables, and the knobs that shape how they are filled.

/// Bump on any schema change: `open` then drops and recreates the cache.
pub(super) const SCHEMA_VERSION: i64 = 11;

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
-- Every name a file answers a link by (`markdown::path_keys`, lower-cased), one row each, so
-- resolving a link is an index lookup rather than a pass over every path. Directories have none.
CREATE TABLE file_keys(file_id INTEGER NOT NULL, key TEXT NOT NULL);
CREATE TABLE links(
    src_file      INTEGER NOT NULL,
    target        TEXT NOT NULL,
    -- `markdown::link_key(target)`: what is compared against `file_keys.key`. Stored because
    -- the fold is Unicode-aware and SQLite's `lower()` is not.
    key           TEXT NOT NULL,
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
-- A note's frontmatter `aliases:`, the names Go to File and `[[` completion find it by. Not the
-- `aliases` table above, which is the filesystem's: a second path to the same file.
CREATE TABLE note_aliases(file_id INTEGER NOT NULL, name TEXT NOT NULL);
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

-- The other half of that query. A term index answers "starts with", and no number of prefixes
-- makes it answer "contains": `oggle split` is not a term `notes_fts` holds, so the ranked path
-- found nothing where `toggle split` found the note. The trigram tokenizer indexes every
-- three-character window instead, which is what turns `LIKE '%…%'` into an index lookup rather
-- than a pass over every stored body (`Index::infix`).
--
-- `detail='none', columnsize=0` is what makes it affordable, and it costs nothing that is read:
-- FTS5 verifies a `LIKE` against the content row itself, so the token positions and per-column
-- sizes a phrase query would need are dead weight — and the snippet is cut in Rust here anyway,
-- as it is for `notes_fts`. Three candidates, sized on the testvault this comment already
-- measures, now 41 684 entries and 21 360 bodies over 110 MB of text (2026-09-13):
--
--     trigram, detail='none'                   +17.4 MiB   what shipped
--     trigram, detail='full'                  +214.8 MiB   positions nobody reads
--     reversed-token column, prefix='1 2 3'    +75.4 MiB   suffixes only
--
-- The reversed column was the entry's other candidate and the measurement is what dropped it: at
-- four times the size it buys suffix matching alone — `oggle` would find `toggle`, `ggle` would
-- find nothing — so the cheaper index is also the more general one.
--
-- What the vault pays for it, before against after over four interleaved runs on a warm page
-- cache; the ranked path is what the sidebar takes on every keystroke, and it had to stay put:
--
--     index file                                  241.0 → 258.5 MiB
--     first index, whole vault                      7.5 → 12.1 s    (13.6 → 31.3 s, note by note)
--     reconcile with nothing changed                172 → 187 ms
--     ranked query, 1 / 2 / 5 characters        38/30/32 → 38/30/34 ms
--     ranked query, two words                        62 → 61 ms
--     mid-word query, 20 hits                         — → 65 ms
--     mid-word query, a needle in 331 bodies          — → 142 ms
--     a query nothing holds at all                    0 → 2 ms
--
-- The 4.6 s is CPU, not IO: 110 MB of body tokenized a second time, three characters at a step.
-- It was 18 s while every note's body was a statement of its own, FTS5 writing a segment per
-- note at each statement savepoint and merging them back; a batch's bodies are one statement now
-- (`reconcile::write_bodies`, 2026-09-30). Neither the transaction size (500 files a batch
-- measures the same as 250), the page cache (64 MiB of it measures the same as the 2 MiB
-- default), nor FTS5's `hashsize` or `automerge` moves what is left. Growth is bounded by the
-- text and not by the vocabulary, which is why the index grows 7% where a term index of the same
-- bodies grows 30%.
CREATE VIRTUAL TABLE notes_tri USING fts5(
    body, title, content='notes', content_rowid='file_id',
    tokenize='trigram', detail='none', columnsize=0
);
CREATE TRIGGER notes_ai AFTER INSERT ON notes BEGIN
    INSERT INTO notes_fts(rowid, body, title) VALUES (new.file_id, new.body, new.title);
    INSERT INTO notes_tri(rowid, body, title) VALUES (new.file_id, new.body, new.title);
END;
CREATE TRIGGER notes_ad AFTER DELETE ON notes BEGIN
    INSERT INTO notes_fts(notes_fts, rowid, body, title)
    VALUES ('delete', old.file_id, old.body, old.title);
    INSERT INTO notes_tri(notes_tri, rowid, body, title)
    VALUES ('delete', old.file_id, old.body, old.title);
END;
CREATE TRIGGER notes_au AFTER UPDATE ON notes BEGIN
    INSERT INTO notes_fts(notes_fts, rowid, body, title)
    VALUES ('delete', old.file_id, old.body, old.title);
    INSERT INTO notes_tri(notes_tri, rowid, body, title)
    VALUES ('delete', old.file_id, old.body, old.title);
    INSERT INTO notes_fts(rowid, body, title) VALUES (new.file_id, new.body, new.title);
    INSERT INTO notes_tri(rowid, body, title) VALUES (new.file_id, new.body, new.title);
END;

CREATE INDEX idx_links_key      ON links(key);
CREATE INDEX idx_file_keys_key  ON file_keys(key);
CREATE INDEX idx_file_keys_file ON file_keys(file_id);
CREATE INDEX idx_links_resolved ON links(resolved_file);
CREATE INDEX idx_links_src      ON links(src_file);
CREATE INDEX idx_tags_name      ON tags(name);
CREATE INDEX idx_tags_file      ON tags(file_id);
CREATE INDEX idx_headings_file  ON headings(file_id);
CREATE INDEX idx_note_aliases_file ON note_aliases(file_id);
CREATE INDEX idx_files_devino   ON files(dev, ino);
CREATE INDEX idx_files_parent   ON files(parent_dir);
CREATE INDEX idx_files_kind_mt  ON files(kind, mtime_ns DESC);
CREATE INDEX idx_aliases_file   ON aliases(file_id);
"#;

/// The `notes` rows a transaction rewrites, waiting to be written together
/// (`reconcile::write_bodies`): each file's new body, or `NULL` where it has none now. A temp
/// table, so it is the connection's own and never in the file.
pub(super) const BODIES: &str = r#"
CREATE TEMP TABLE bodies(file_id INTEGER PRIMARY KEY, body TEXT, title TEXT);
"#;

pub(super) const DROP_ALL: &str = r#"
DROP TRIGGER IF EXISTS notes_ai;
DROP TRIGGER IF EXISTS notes_ad;
DROP TRIGGER IF EXISTS notes_au;
DROP TABLE IF EXISTS notes_fts;
DROP TABLE IF EXISTS notes_tri;
DROP TABLE IF EXISTS notes;
DROP TABLE IF EXISTS headings;
DROP TABLE IF EXISTS note_aliases;
DROP TABLE IF EXISTS tags;
DROP TABLE IF EXISTS links;
DROP TABLE IF EXISTS file_keys;
DROP TABLE IF EXISTS aliases;
DROP TABLE IF EXISTS files;
"#;
