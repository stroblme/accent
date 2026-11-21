//! Import resolution: the file of the vault each import a code file writes names
//! (`code::imports`), kept in a table of its own beside `links` (the schema says why).

use super::Index;
use crate::Result;

/// [`BEST_FILE`](super::links::BEST_FILE)'s sibling: of the files whose `module_key` is the
/// import's or the module above's, the one `import_rank` puts first, the shorter path and then
/// the older row breaking a tie. Correlated on `imports`, so it is what an
/// `UPDATE imports SET resolved_file = …` assigns; `NULL` when nothing answers. A second path to
/// a file (`aliases`) is no candidate: what an importer's tree names is the file's own path.
pub(super) const BEST_IMPORT: &str = "(SELECT f.id FROM files s, files f
      WHERE s.id = imports.src_file AND f.module_key IN (imports.key, imports.up_key)
        AND import_rank(s.rel_path, imports.target, imports.near, imports.up_key IS NOT NULL,
                        f.rel_path) IS NOT NULL
      ORDER BY import_rank(s.rel_path, imports.target, imports.near, imports.up_key IS NOT NULL,
                           f.rel_path),
               length(f.rel_path), f.id
      LIMIT 1)";

/// [`Index::resolve_imports`] narrowed to what one written file can have changed: its own
/// imports, every import its module key may answer, and every import that resolved to it, which
/// a file turned folder no longer answers.
pub(super) fn resolve_imports_of(tx: &rusqlite::Transaction<'_>, rel: &str) -> Result<()> {
    tx.prepare_cached(&format!(
        "UPDATE imports SET resolved_file = {BEST_IMPORT}
          WHERE src_file = (SELECT id FROM files WHERE rel_path = ?1)
             OR key = (SELECT module_key FROM files WHERE rel_path = ?1)
             OR up_key = (SELECT module_key FROM files WHERE rel_path = ?1)
             OR resolved_file = (SELECT id FROM files WHERE rel_path = ?1)"
    ))?
    .execute([rel])?;
    Ok(())
}

impl Index {
    /// Every import resolved in one statement, which the cold build runs once at its end.
    pub(super) fn resolve_imports(&mut self) -> Result<()> {
        let tx = self.write_tx()?;
        tx.execute(
            &format!("UPDATE imports SET resolved_file = {BEST_IMPORT}"),
            [],
        )?;
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::index::Index;
    use crate::index::testing::{fixture, open};
    use std::fs;

    /// `(target, the file it resolves to)` for each import `rel` writes.
    fn resolved(ix: &Index, rel: &str) -> Vec<(String, Option<String>)> {
        let mut st = ix
            .conn
            .prepare(
                "SELECT i.target, f.rel_path FROM imports i JOIN files s ON s.id = i.src_file
                 LEFT JOIN files f ON f.id = i.resolved_file
                 WHERE s.rel_path = ?1 ORDER BY i.target",
            )
            .unwrap();
        let rows = st.query_map([rel], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        rows.collect::<rusqlite::Result<_>>().unwrap()
    }

    fn row(target: &str, file: Option<&str>) -> (String, Option<String>) {
        (target.into(), file.map(Into::into))
    }

    #[test]
    fn a_cold_build_resolves_every_import() {
        let (vault, db) = fixture();
        let root = vault.path();
        fs::create_dir_all(root.join("lib/src/b")).unwrap();
        fs::write(
            root.join("lib/src/lib.rs"),
            "mod a;\nmod b;\nuse crate::b::f;\n",
        )
        .unwrap();
        fs::write(root.join("lib/src/a.rs"), "use super::b::Thing;\n").unwrap();
        fs::write(root.join("lib/src/b/mod.rs"), "pub fn f() {}\n").unwrap();
        let mut ix = open(&db);
        ix.reconcile(root, |_| {}).unwrap();

        assert_eq!(
            resolved(&ix, "lib/src/lib.rs"),
            [
                row("lib/src/a", Some("lib/src/a.rs")),
                row("lib/src/b", Some("lib/src/b/mod.rs")),
                row("lib/src/b/f", Some("lib/src/b/mod.rs")),
            ]
        );
        assert_eq!(
            resolved(&ix, "lib/src/a.rs"),
            [row("lib/src/b", Some("lib/src/b/mod.rs"))]
        );
    }

    /// A module written after the file importing it takes the import, the importer's own one
    /// ahead of a namesake further off; its removal hands the import to the namesake, and an
    /// importer moved keeps finding it.
    #[test]
    fn an_import_follows_its_modules_as_they_come_and_go() {
        let (vault, db) = fixture();
        let root = vault.path();
        fs::create_dir_all(root.join("app")).unwrap();
        fs::create_dir_all(root.join("other/deep")).unwrap();
        fs::write(root.join("app/main.py"), "import utils\n").unwrap();
        let mut ix = open(&db);
        ix.reconcile(root, |_| {}).unwrap();
        assert_eq!(resolved(&ix, "app/main.py"), [row("utils", None)]);

        fs::write(root.join("other/deep/utils.py"), "").unwrap();
        fs::write(root.join("app/utils.py"), "").unwrap();
        ix.reconcile(root, |_| {}).unwrap();
        assert_eq!(
            resolved(&ix, "app/main.py"),
            [row("utils", Some("app/utils.py"))]
        );

        fs::remove_file(root.join("app/utils.py")).unwrap();
        ix.update_file(root, "app/utils.py").unwrap();
        assert_eq!(
            resolved(&ix, "app/main.py"),
            [row("utils", Some("other/deep/utils.py"))]
        );

        fs::rename(root.join("app/main.py"), root.join("other/main.py")).unwrap();
        ix.update_file(root, "app/main.py").unwrap();
        ix.update_file(root, "other/main.py").unwrap();
        assert_eq!(
            resolved(&ix, "other/main.py"),
            [row("utils", Some("other/deep/utils.py"))]
        );

        // A module that turns into a folder of the same name answers no import.
        fs::remove_file(root.join("other/deep/utils.py")).unwrap();
        fs::create_dir(root.join("other/deep/utils.py")).unwrap();
        ix.update_file(root, "other/deep/utils.py").unwrap();
        assert_eq!(resolved(&ix, "other/main.py"), [row("utils", None)]);
    }
}
