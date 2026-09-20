//! Listing what the index holds: the lazy tree, the switcher's paths, tags, stats, exclusions.

use super::{FileRow, Index, Stats};
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

    /// Every file at `rel` or under it, without the folders: what moving `rel` takes along, one
    /// by one. `rel` itself when it is a file.
    pub fn files_under(&self, rel: &str) -> Result<Vec<String>> {
        let (lo, hi) = path::subtree_range(rel);
        let mut st = self.conn.prepare_cached(
            "SELECT rel_path FROM files
              WHERE kind <> 0 AND (rel_path = ?1 OR (rel_path >= ?2 AND rel_path < ?3))",
        )?;
        let rows = st.query_map(params![rel, lo, hi], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Every markdown note's rel_path, for the file switcher's fuzzy match. What `[[` completes
    /// against is [`note_and_pdf_paths`](Self::note_and_pdf_paths), a wikilink naming a PDF as
    /// readily as a note.
    pub fn note_paths(&self) -> Result<Vec<String>> {
        let mut st = self.conn.prepare_cached(
            "SELECT rel_path FROM files WHERE kind = ?1 ORDER BY rel_path COLLATE NOCASE",
        )?;
        let rows = st.query_map([FileKind::Markdown.as_i64()], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Every note's and PDF's rel_path, notes first and then the PDFs by path: what `[[` offers.
    ///
    /// A wikilink points at something to read, which is a note or a PDF; anything else is reached
    /// with `![[embed]]` or a markdown link, and those take [`file_paths`](Self::file_paths).
    /// Git-ignored PDFs are left out the way `file_paths` leaves them out, so a build directory's
    /// output is offered by neither, and a note is listed whatever git says about it.
    pub fn note_and_pdf_paths(&self) -> Result<Vec<String>> {
        let mut st = self.conn.prepare_cached(
            "SELECT rel_path FROM files
              WHERE kind IN (?1, ?2) AND (git_ignored = 0 OR kind = ?1)
              ORDER BY kind <> ?1, rel_path COLLATE NOCASE",
        )?;
        let rows = st.query_map(
            params![FileKind::Markdown.as_i64(), FileKind::Pdf.as_i64()],
            |r| r.get(0),
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Every openable file's rel_path, notes first and then the rest by path.
    ///
    /// The switcher opens more than markdown, and an `![[embed]]` or a markdown link's destination
    /// can name any file at all, so they need this rather than
    /// [`note_and_pdf_paths`](Self::note_and_pdf_paths), which offers only what a wikilink may
    /// name. Directories are not files to open and conflict copies are reached through the
    /// resolve UI, so neither is listed.
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

    /// `(alias, note rel_path)` for every frontmatter alias, by alias: the names Go to File and
    /// `[[` completion also find a note by. A link still resolves by the file's name alone.
    pub fn note_aliases(&self) -> Result<Vec<(String, String)>> {
        let mut st = self.conn.prepare_cached(
            "SELECT a.name, f.rel_path FROM note_aliases a JOIN files f ON f.id = a.file_id
             ORDER BY a.name COLLATE NOCASE, f.rel_path",
        )?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
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
        dependency: false,
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
    fn dirs_lists_a_linked_directory_beside_a_plain_one() {
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("ext.md"), "ext").unwrap();
        let vault = tempfile::tempdir().unwrap();
        fs::create_dir(vault.path().join("plain")).unwrap();
        std::os::unix::fs::symlink(outside.path(), vault.path().join("linked")).unwrap();
        let db = tempfile::tempdir().unwrap();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();

        assert_eq!(
            ix.dirs(vault.path()).unwrap(),
            vec![vault.path().join("linked"), vault.path().join("plain")]
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

    /// `[[` completes against notes and PDFs; a source file is `![[`'s and a markdown link's.
    #[test]
    fn note_and_pdf_paths_lists_notes_first_then_pdfs() {
        let (vault, db) = fixture();
        fs::write(vault.path().join("tool.py"), "print('hi')\n").unwrap();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();

        assert_eq!(
            ix.note_and_pdf_paths().unwrap(),
            vec!["a.md", "sub/Beta.md", "c.pdf"]
        );
    }

    /// Obsidian's three spellings: a list under `aliases:`, a single string, and the singular
    /// `alias:`, and a list flush with its key. An edit that drops an alias takes its row with it.
    #[test]
    fn front_matter_aliases_are_listed_with_their_note() {
        let (vault, db) = fixture();
        let list = "---\naliases:\n  - Ada\n  - \"Bea\"\n---\n# List\n";
        fs::write(vault.path().join("list.md"), list).unwrap();
        fs::write(
            vault.path().join("flow.md"),
            "---\naliases: [Cy, Dee]\n---\n",
        )
        .unwrap();
        fs::write(vault.path().join("one.md"), "---\naliases: Eve\n---\n").unwrap();
        fs::write(vault.path().join("old.md"), "---\nalias: 'Fay'\n---\n").unwrap();
        fs::write(vault.path().join("flush.md"), "---\naliases:\n- Gus\n---\n").unwrap();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();

        let pairs = |ix: &Index| -> Vec<(String, String)> { ix.note_aliases().unwrap() };
        let want = |rows: &[(&str, &str)]| -> Vec<(String, String)> {
            rows.iter()
                .map(|(a, rel)| (a.to_string(), rel.to_string()))
                .collect()
        };
        assert_eq!(
            pairs(&ix),
            want(&[
                ("Ada", "list.md"),
                ("Bea", "list.md"),
                ("Cy", "flow.md"),
                ("Dee", "flow.md"),
                ("Eve", "one.md"),
                ("Fay", "old.md"),
                ("Gus", "flush.md"),
            ])
        );

        fs::write(
            vault.path().join("list.md"),
            "---\naliases:\n  - Bea\n---\n",
        )
        .unwrap();
        ix.update_file(vault.path(), "list.md").unwrap();
        assert_eq!(
            pairs(&ix),
            want(&[
                ("Bea", "list.md"),
                ("Cy", "flow.md"),
                ("Dee", "flow.md"),
                ("Eve", "one.md"),
                ("Fay", "old.md"),
                ("Gus", "flush.md"),
            ])
        );
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

    /// Git reports a wholly ignored directory as one entry, so a file created inside it leaves
    /// the exclusion set unchanged and no second [`Index::set_excluded`] ever comes back to mark
    /// the row. The row has to be excluded as it is indexed, or the artefact stays in search
    /// until something unrelated moves the set.
    #[test]
    fn a_file_added_under_an_ignored_directory_is_excluded_as_it_is_indexed() {
        let (vault, db) = ignore_fixture();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();
        ix.set_excluded(&["paper/".to_string()]).unwrap();
        assert_eq!(hits(&ix, false), ["notes/a.md"]);

        fs::write(vault.path().join("paper/main.log"), "zorblat\n").unwrap();
        ix.update_file_batched(vault.path(), "paper/main.log")
            .unwrap();
        assert_eq!(
            hits(&ix, false),
            ["notes/a.md"],
            "the new artefact is inside `paper/`, which the set already names"
        );

        // And a directory made under it hands the exclusion on to what lands inside.
        fs::create_dir(vault.path().join("paper/out")).unwrap();
        fs::write(vault.path().join("paper/out/deep.log"), "zorblat\n").unwrap();
        ix.update_file_batched(vault.path(), "paper/out").unwrap();
        ix.update_file_batched(vault.path(), "paper/out/deep.log")
            .unwrap();
        assert_eq!(hits(&ix, false), ["notes/a.md"]);

        // A row already in the index keeps whatever the set last said about it.
        assert_eq!(hits(&ix, true).len(), 5);
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
