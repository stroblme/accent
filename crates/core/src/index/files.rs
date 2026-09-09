//! Listing what the index holds: the lazy tree, the switcher's paths, tags, stats, exclusions.

use super::{FileRow, HeadingRow, Index, Stats};
use crate::path;
use crate::walk::FileKind;
use anyhow::Result;
use rusqlite::{OptionalExtension, params};
use std::path::Path;

impl Index {
    /// Record what search leaves out, so a query can leave it out.
    ///
    /// `entries` is git's own answer (`Panel::ignored`, vault-relative) plus whatever the user
    /// named in `[search] exclude`: one set, because they answer the same question and a query
    /// has one column to read. A wholly ignored directory arrives from git as **one** entry with
    /// a trailing slash rather than a row per file inside it, which is why matching the entry and
    /// its subtree is enough and why this is tens of statements rather than thousands. The prefix
    /// test is a range on the `rel_path` unique index, the same shape
    /// [`remove_file_batched`](Self::remove_file_batched) uses, because a `LIKE` would fall back
    /// to a full scan under SQLite's default case-insensitive `LIKE`.
    ///
    /// One transaction, and the whole column is cleared first: a path that stopped being excluded
    /// has no entry to carry the news, so the set is replaced rather than merged. An empty
    /// `entries` therefore un-excludes everything, which is exactly right for a vault whose
    /// repository went away.
    pub fn set_excluded(&mut self, entries: &[String]) -> Result<()> {
        let tx = self.write_tx()?;
        tx.execute(
            "UPDATE files SET git_ignored = 0 WHERE git_ignored <> 0",
            [],
        )?;
        {
            let mut st = tx.prepare_cached(
                "UPDATE files SET git_ignored = 1
                  WHERE rel_path = ?1 OR (rel_path >= ?2 AND rel_path < ?3)",
            )?;
            for entry in entries {
                let base = entry.trim_end_matches('/');
                if base.is_empty() {
                    continue;
                }
                let (lo, hi) = path::subtree_range(base);
                st.execute(params![base, lo, hi])?;
            }
        }
        tx.commit()?;
        Ok(())
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
    ///
    /// Git-ignored files are left out unless `include_ignored`, but **a note is never left out**:
    /// a vault that gitignores its own markdown is the ordinary case, not the exception. The file
    /// tree is the escape hatch either way — it lists an ignored file, dimmed.
    pub fn file_paths(&self, include_ignored: bool) -> Result<Vec<String>> {
        let mut st = self.conn.prepare_cached(
            "SELECT rel_path FROM files
              WHERE kind IN (?1, ?2, ?3) AND (?4 OR git_ignored = 0 OR kind = ?1)
              ORDER BY kind <> ?1, rel_path COLLATE NOCASE",
        )?;
        let rows = st.query_map(
            params![
                FileKind::Markdown.as_i64(),
                FileKind::Pdf.as_i64(),
                FileKind::Other.as_i64(),
                include_ignored,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::testing::{fixture, open};
    use std::fs;

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
            ix.file_paths(false).unwrap(),
            vec!["a.md", "sub/Beta.md", "c.pdf", "tool.py"]
        );
    }

    /// A vault of one note, one source file and one build artefact, all holding the same token.
    fn ignore_fixture() -> (tempfile::TempDir, tempfile::TempDir) {
        let vault = tempfile::tempdir().unwrap();
        fs::create_dir(vault.path().join("notes")).unwrap();
        fs::create_dir(vault.path().join("paper")).unwrap();
        fs::write(vault.path().join("notes/a.md"), "# A\nzorblat\n").unwrap();
        fs::write(vault.path().join("paper/main.tex"), "\\title{zorblat}\n").unwrap();
        fs::write(vault.path().join("paper/main.aux"), "\\relax zorblat\n").unwrap();
        (vault, tempfile::tempdir().unwrap())
    }

    fn hits(ix: &Index, include_ignored: bool) -> Vec<String> {
        let mut out: Vec<String> = ix
            .search("zorblat", 10, include_ignored)
            .unwrap()
            .into_iter()
            .map(|h| h.rel_path)
            .collect();
        out.sort();
        out
    }

    /// One ignored file is left out of both query paths, and put back on request.
    #[test]
    fn git_ignored_files_are_left_out_of_search_and_grep() {
        let (vault, db) = ignore_fixture();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();
        ix.set_excluded(&["paper/main.aux".to_string()]).unwrap();

        assert_eq!(hits(&ix, false), ["notes/a.md", "paper/main.tex"]);
        assert_eq!(
            hits(&ix, true),
            ["notes/a.md", "paper/main.aux", "paper/main.tex"]
        );

        let re = crate::search::pattern("zorblat", crate::search::Options::default()).unwrap();
        // The rows are what the exclusion moves; grep's count is markdown only, so the one note
        // is all of it either way.
        assert_eq!(ix.grep(&re, 10, false).unwrap().0.len(), 2);
        assert_eq!(ix.grep(&re, 10, true).unwrap().0.len(), 3);
        assert_eq!(ix.grep(&re, 10, true).unwrap().1, 1);

        // Go to File follows the default; the tree is the escape hatch, not a toggle here.
        assert_eq!(
            ix.file_paths(false).unwrap(),
            ["notes/a.md", "paper/main.tex"]
        );
        assert_eq!(ix.file_paths(true).unwrap().len(), 3);
    }

    /// Git reports a wholly ignored tree as one trailing-slash entry, and the note under it must
    /// survive: `walk.rs`'s rule that hiding the user's notes is never right, one layer down.
    #[test]
    fn a_wholly_ignored_directory_hides_everything_under_it_but_the_notes() {
        let (vault, db) = ignore_fixture();
        fs::write(vault.path().join("notes/b.tex"), "zorblat\n").unwrap();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();
        ix.set_excluded(&["notes/".to_string()]).unwrap();

        assert_eq!(
            hits(&ix, false),
            ["notes/a.md", "paper/main.aux", "paper/main.tex"],
            "the note under the ignored directory stays, the .tex beside it goes"
        );
        assert_eq!(hits(&ix, true).len(), 4);
    }

    /// A vault with no repository at all: nothing is ignored, so nothing is filtered.
    #[test]
    fn an_empty_ignore_set_filters_nothing() {
        let (vault, db) = ignore_fixture();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();

        assert_eq!(hits(&ix, false).len(), 3);
        ix.set_excluded(&["paper/main.aux".to_string()]).unwrap();
        assert_eq!(hits(&ix, false).len(), 2);
        // The set is replaced, not merged: a file that stopped being ignored comes back.
        ix.set_excluded(&[]).unwrap();
        assert_eq!(hits(&ix, false).len(), 3);
    }
}
