//! Link resolution and what reads its result: backlinks, PDF highlights, dangling targets.

use super::reconcile::link_kind_of;
use super::{Backlink, Index, OutLink, PdfLink};
use crate::markdown;
use crate::path::{self, FileType, file_type, linked_path};
use anyhow::Result;
use rusqlite::{OptionalExtension, params};
use std::collections::HashSet;

/// Obsidian's rule as one subquery: of the files answering to a link's key, the shortest
/// `rel_path` wins, and the older row breaks a tie. Correlated on `links.key`, so it is what an
/// `UPDATE links SET resolved_file = …` assigns; `NULL` when nothing answers.
pub(super) const BEST_FILE: &str =
    "(SELECT k.file_id FROM file_keys k JOIN files f ON f.id = k.file_id
      WHERE k.key = links.key ORDER BY length(f.rel_path), f.id LIMIT 1)";

/// [`BEST_FILE`] for one key handed in, answering with the path.
const BEST_REL_PATH: &str = "SELECT f.rel_path FROM file_keys k JOIN files f ON f.id = k.file_id
     WHERE k.key = ?1 ORDER BY length(f.rel_path), f.id LIMIT 1";

/// [`Index::resolve_links`] narrowed to what one indexed file can have changed: the links it
/// holds, every link whose key it answers to — which is where a dangling link finds its new
/// note, and a resolved one a shorter path — and every link that resolved to it, which a file
/// turned folder no longer answers. All three are index lookups.
pub(super) fn resolve_links_of(tx: &rusqlite::Transaction<'_>, rel: &str) -> Result<()> {
    tx.prepare_cached(&format!(
        "UPDATE links SET resolved_file = {BEST_FILE}
          WHERE src_file = (SELECT id FROM files WHERE rel_path = ?1)
             OR key IN (SELECT k.key FROM file_keys k JOIN files f ON f.id = k.file_id
                         WHERE f.rel_path = ?1)
             OR resolved_file = (SELECT id FROM files WHERE rel_path = ?1)"
    ))?
    .execute([rel])?;
    Ok(())
}

impl Index {
    /// Obsidian link resolution: a target matches a file's path or name, with or without the
    /// extension, case-insensitively; the shortest `rel_path` wins. Unmatched stays NULL.
    ///
    /// The whole `links` table in one statement, for the cold build. A single file's worth is
    /// [`resolve_links_of`], and a removal re-points its own links as it goes. Returns how many
    /// links resolve.
    pub fn resolve_links(&mut self) -> Result<usize> {
        let tx = self.write_tx()?;
        tx.execute(&format!("UPDATE links SET resolved_file = {BEST_FILE}"), [])?;
        let resolved: i64 = tx.query_row(
            "SELECT COUNT(*) FROM links WHERE resolved_file IS NOT NULL",
            [],
            |r| r.get(0),
        )?;
        tx.commit()?;
        Ok(resolved as usize)
    }

    /// The file one link target points at, by the rules of [`resolve_links`](Self::resolve_links).
    /// `None` means the link dangles, which is what the UI offers to create.
    pub fn resolve_target(&self, target: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .prepare_cached(BEST_REL_PATH)?
            .query_row([markdown::link_key(target)], |r| r.get(0))
            .optional()?)
    }

    /// Every link in the vault that points at a *page and selection* of this PDF.
    ///
    /// This is where a highlight lives: the note holds it, the index finds it, and the viewer
    /// paints it. There is no annotations table, because the index is a disposable cache and a
    /// table nothing could rebuild would be wiped by the next schema bump.
    pub fn pdf_links(&self, rel_path: &str) -> Result<Vec<PdfLink>> {
        let mut st = self.conn.prepare_cached(
            "SELECT s.rel_path, l.byte_start, l.anchor, l.alias
             FROM links l
             JOIN files s ON s.id = l.src_file
             JOIN files t ON t.id = l.resolved_file
             WHERE t.rel_path = ?1 AND l.anchor LIKE 'page=%selection=%'
             ORDER BY s.rel_path, l.byte_start",
        )?;
        // The `LIKE` narrows the rows; the anchor is parsed in Rust, where the one parser lives.
        let rows = st.query_map([rel_path], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
            ))
        })?;
        Ok(rows
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .filter_map(|(src_rel_path, byte_start, anchor, alias)| {
                let (page, selection) = crate::markdown::pdf_anchor(&anchor)?;
                Some(PdfLink {
                    src_rel_path,
                    byte_start,
                    page,
                    selection: selection?,
                    alias,
                })
            })
            .collect())
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

    /// `(key, file)` for every link in the note `src` that resolves: what each way the note
    /// names a file meant, keyed the way [`markdown::rewrite_moved`] looks them up. Asked before
    /// a move, while the index still describes the vault as it was.
    pub fn resolved_links(&self, src: &str) -> Result<Vec<(String, String)>> {
        let mut st = self.conn.prepare_cached(
            "SELECT l.key, t.rel_path FROM links l
             JOIN files s ON s.id = l.src_file
             JOIN files t ON t.id = l.resolved_file
             WHERE s.rel_path = ?1",
        )?;
        let rows = st.query_map([src], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The notes at `rel` or under it that hold a markdown link which resolves. Such a link is
    /// written relative to its note, so moving the note alone can leave it pointing at nothing.
    pub fn markdown_link_sources(&self, rel: &str) -> Result<Vec<String>> {
        let (lo, hi) = path::subtree_range(rel);
        let mut st = self.conn.prepare_cached(
            "SELECT DISTINCT s.rel_path FROM files s JOIN links l ON l.src_file = s.id
             WHERE (s.rel_path = ?1 OR (s.rel_path >= ?2 AND s.rel_path < ?3))
               AND l.kind = 2 AND l.resolved_file IS NOT NULL
             ORDER BY s.rel_path",
        )?;
        let rows = st.query_map(params![rel, lo, hi], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// `(source rel_path, target as written, byte offset in the source)` for every link that
    /// resolves to nothing.
    pub fn unresolved_links(&self) -> Result<Vec<(String, String, i64)>> {
        let mut st = self.conn.prepare_cached(
            "SELECT s.rel_path, l.target, l.byte_start FROM links l JOIN files s ON s.id = l.src_file
             WHERE l.resolved_file IS NULL AND l.kind <> 3
             ORDER BY s.rel_path, l.byte_start",
        )?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Every link written in the note `src`, in the order it writes them, and the file each
    /// resolves to: a note's way out, as [`backlinks`](Self::backlinks) is its way in.
    pub fn links_from(&self, src: &str) -> Result<Vec<OutLink>> {
        let mut st = self.conn.prepare_cached(
            "SELECT l.target, t.rel_path, l.kind, l.anchor, l.byte_start FROM links l
             JOIN files s ON s.id = l.src_file
             LEFT JOIN files t ON t.id = l.resolved_file
             WHERE s.rel_path = ?1
             ORDER BY l.byte_start",
        )?;
        let rows = st.query_map([src], |r| {
            Ok(OutLink {
                target: r.get(0)?,
                path: r.get(1)?,
                kind: link_kind_of(r.get(2)?),
                anchor: r.get(3)?,
                byte_start: r.get(4)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Every note a link names that nothing in the vault answers to, once each, by the path New
    /// File would create it at ([`linked_path`]): what Go to File and `[[` completion offer to
    /// write. The stored targets are vault paths already — a markdown link's was resolved from its
    /// note's folder when it was indexed — so `[[Foo]]` and `[t](Foo.md)` from the root are one
    /// row, and so are two spellings that differ only in case.
    ///
    /// A target that is a folder is not a note waiting to be written: an in-note `[t](#anchor)`
    /// is stored as its note's folder. Nor is a missing image or PDF, which New File cannot make.
    pub fn missing_notes(&self) -> Result<Vec<String>> {
        let mut st = self.conn.prepare_cached(
            "SELECT DISTINCT l.target FROM links l
             WHERE l.resolved_file IS NULL AND l.kind <> 3 AND l.target <> ''
               AND NOT EXISTS (SELECT 1 FROM files f WHERE f.rel_path = l.target)
             ORDER BY l.target",
        )?;
        let targets = st
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut seen = HashSet::new();
        Ok(targets
            .iter()
            .map(|target| linked_path(target))
            .filter(|rel| file_type(rel) == FileType::Note && seen.insert(markdown::link_key(rel)))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use crate::index::Change;
    use crate::index::testing::{fixture, open};
    use crate::walk::FileKind;
    use std::fs;

    #[test]
    fn pdf_links_read_page_selection_and_alias() {
        let (vault, db) = fixture();
        fs::write(
            vault.path().join("d.md"),
            "see [[c.pdf#page=1&selection=0,0,0,5|Hello]] and [[c.pdf#page=2]] and [[Beta]]\n",
        )
        .unwrap();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();

        let links = ix.pdf_links("c.pdf").unwrap();
        // Only the one with a selection: a link to a bare page is a jump, not a highlight.
        assert_eq!(links.len(), 1, "{links:?}");
        assert_eq!(links[0].src_rel_path, "d.md");
        assert_eq!(links[0].byte_start, 4);
        assert_eq!(links[0].page, 0, "one-based in the link, zero-based here");
        assert_eq!(links[0].selection, [0, 0, 0, 5]);
        assert_eq!(links[0].alias.as_deref(), Some("Hello"));

        // A note is not a PDF, and nothing points into it that way.
        assert!(ix.pdf_links("sub/Beta.md").unwrap().is_empty());
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
                "INSERT INTO links(src_file, target, key, kind, byte_start, byte_end)
                 VALUES(?1, 'beta', 'beta', 0, 10, 16), (?1, 'sub/Beta.md', 'sub/beta.md', 0, 20, 31),
                       (?1, 'Nope', 'nope', 0, 40, 44)",
                [src],
            )
            .unwrap();
        assert_eq!(ix.resolve_links().unwrap(), 2);
        assert_eq!(ix.backlinks("sub/Beta.md").unwrap().len(), 2);
        assert_eq!(
            ix.unresolved_links().unwrap(),
            vec![("a.md".into(), "Nope".into(), 40)]
        );
        assert_eq!(ix.stats().unwrap().unresolved, 1);
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

    /// The incremental paths: a shallower namesake appearing takes a link over without a full
    /// pass, and its removal hands the link back to the note that had it.
    #[test]
    fn a_new_namesake_takes_a_link_and_hands_it_back_when_removed() {
        let (vault, db) = fixture();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();
        assert_eq!(ix.backlinks("sub/Beta.md").unwrap().len(), 1);

        fs::write(vault.path().join("Beta.md"), "# Beta\n").unwrap();
        assert_eq!(
            ix.update_file(vault.path(), "Beta.md").unwrap(),
            Change::Added(FileKind::Markdown)
        );
        assert_eq!(ix.backlinks("Beta.md").unwrap().len(), 1);
        assert!(ix.backlinks("sub/Beta.md").unwrap().is_empty());

        fs::remove_file(vault.path().join("Beta.md")).unwrap();
        assert_eq!(
            ix.update_file(vault.path(), "Beta.md").unwrap(),
            Change::Removed
        );
        assert_eq!(ix.backlinks("sub/Beta.md").unwrap().len(), 1);
        assert!(ix.unresolved_links().unwrap().is_empty());
    }

    /// A file that becomes a folder of the same name loses its keys, and the links it answered
    /// have to let go of it as they would of a removed file: a folder is no link target.
    #[test]
    fn a_file_turned_folder_lets_its_links_dangle() {
        let (vault, db) = fixture();
        fs::write(vault.path().join("d.md"), "see [[Ideas]]\n").unwrap();
        fs::write(vault.path().join("Ideas"), "plain\n").unwrap();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();
        assert_eq!(ix.backlinks("Ideas").unwrap().len(), 1);

        fs::remove_file(vault.path().join("Ideas")).unwrap();
        fs::create_dir(vault.path().join("Ideas")).unwrap();
        assert_eq!(
            ix.update_file(vault.path(), "Ideas").unwrap(),
            Change::Updated(FileKind::Dir)
        );
        assert!(ix.backlinks("Ideas").unwrap().is_empty());
    }

    /// A note's links in the order it writes them, each with the file it leads to, or none.
    #[test]
    fn links_from_lists_where_each_link_leads() {
        let (vault, db) = fixture();
        fs::write(
            vault.path().join("d.md"),
            "[[Beta#Part]] then [gone](Nowhere.md)\n",
        )
        .unwrap();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();

        let out = ix.links_from("d.md").unwrap();
        let rows: Vec<_> = out
            .iter()
            .map(|l| {
                (
                    l.target.as_str(),
                    l.path.as_deref(),
                    l.anchor.as_deref(),
                    l.byte_start,
                )
            })
            .collect();
        assert_eq!(
            rows,
            [
                ("Beta", Some("sub/Beta.md"), Some("Part"), 0),
                ("Nowhere.md", None, None, 19)
            ]
        );
        assert_eq!(out[1].kind, crate::markdown::LinkKind::Markdown);
    }

    /// A note linked to before it is written is offered once, by the path New File would make,
    /// however the links spell it; writing it takes it off the list.
    #[test]
    fn a_dangling_link_is_a_missing_note_until_the_note_is_written() {
        let (vault, db) = fixture();
        fs::write(
            vault.path().join("d.md"),
            "[[Nowhere/Other Note#Part|there]] [t](Nowhere/other%20note.md) [[pic.png]] [[Beta]] \
             [[Rev 1.2 notes]] [[foo.pdf]]\n",
        )
        .unwrap();
        fs::write(vault.path().join("sub/e.md"), "[t](#anchor) [u](Later)\n").unwrap();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();
        // Not the image or the PDF, the note that is there, or the folder an in-note anchor
        // names; a dot in a name is not an extension.
        assert_eq!(
            ix.missing_notes().unwrap(),
            ["Nowhere/Other Note.md", "Rev 1.2 notes.md", "sub/Later.md"]
        );

        fs::create_dir(vault.path().join("Nowhere")).unwrap();
        fs::write(vault.path().join("Nowhere/Other Note.md"), "# Other\n").unwrap();
        ix.update_file(vault.path(), "Nowhere/Other Note.md")
            .unwrap();
        assert_eq!(
            ix.missing_notes().unwrap(),
            ["Rev 1.2 notes.md", "sub/Later.md"]
        );
    }
}
