//! Link resolution and what reads its result: backlinks, PDF highlights, dangling targets.

use super::{Backlink, Index, PdfLink};
use crate::markdown;
use anyhow::Result;
use rusqlite::params;
use std::collections::HashMap;

impl Index {
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

        let tx = self.write_tx()?;
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

    /// The file one link target points at, by the rules of [`resolve_links`](Self::resolve_links).
    /// `None` means the link dangles, which is what the UI offers to create.
    ///
    /// One pass over the file paths, not the key map `resolve_links` builds, because a map costs
    /// four strings per file and this answers a single click;
    /// [`resolve_targets`](Self::resolve_targets) is the one to ask for a whole note's links.
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

    /// [`resolve_target`](Self::resolve_target) for many targets at once, for the note whose
    /// every wikilink has to be checked on each keystroke: the key map is built once and then
    /// answers every target, instead of one full scan per link.
    pub fn resolve_targets(&self, targets: &[String]) -> Result<Vec<Option<String>>> {
        let mut by_key: HashMap<String, String> = HashMap::new();
        {
            let mut st = self
                .conn
                .prepare_cached("SELECT rel_path FROM files WHERE kind <> 0")?;
            let mut rows = st.query([])?;
            while let Some(r) = rows.next()? {
                let rel: String = r.get(0)?;
                for key in markdown::path_keys(&rel) {
                    // The shortest path answering to a key wins, as in `resolve_links`.
                    match by_key.get(&key) {
                        Some(best) if best.len() <= rel.len() => {}
                        _ => {
                            by_key.insert(key, rel.clone());
                        }
                    }
                }
            }
        }
        Ok(targets
            .iter()
            .map(|t| by_key.get(&markdown::link_key(t)).cloned())
            .collect())
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
}

#[cfg(test)]
mod tests {
    use crate::index::testing::{fixture, open};
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

    #[test]
    fn resolve_targets_answers_many_at_once() {
        let (vault, db) = fixture();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();

        let targets = ["beta".to_string(), "Nope".to_string(), "a.md".to_string()];
        assert_eq!(
            ix.resolve_targets(&targets).unwrap(),
            [Some("sub/Beta.md".to_string()), None, Some("a.md".into())]
        );
        assert!(ix.resolve_targets(&[]).unwrap().is_empty());
    }
}
