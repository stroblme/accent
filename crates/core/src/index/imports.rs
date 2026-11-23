//! Import resolution: the file of the vault each import a code file writes names
//! (`code::imports`), kept in a table of its own beside `links` (the schema says why).

use super::Index;
use crate::{Result, path};

/// [`BEST_FILE`](super::links::BEST_FILE)'s sibling: of the files whose `module_key` is the
/// import's or a module above's, the one `import_rank` puts first, the shorter path and then
/// the older row breaking a tie. Correlated on `imports`, so it is what an
/// `UPDATE imports SET resolved_file = …` assigns; `NULL` when nothing answers. A second path to
/// a file (`aliases`) is no candidate: what an importer's tree names is the file's own path. An
/// import of another crate is read in the folder of a crate of that name, and answered by none
/// while the vault has no such crate.
pub(super) const BEST_IMPORT: &str = "(SELECT f.id FROM files s, files f
      LEFT JOIN crates c ON c.name = imports.crate
      WHERE s.id = imports.src_file
        AND f.module_key IN (imports.key, imports.up_key, imports.up2_key)
        AND (imports.crate IS NULL OR c.dir IS NOT NULL)
        AND import_rank(s.rel_path, ltrim(coalesce(c.dir, '') || '/' || imports.target, '/'),
                        imports.near, (imports.up_key IS NOT NULL) + (imports.up2_key IS NOT NULL),
                        f.rel_path) IS NOT NULL
      ORDER BY import_rank(s.rel_path, ltrim(coalesce(c.dir, '') || '/' || imports.target, '/'),
                           imports.near,
                           (imports.up_key IS NOT NULL) + (imports.up2_key IS NOT NULL),
                           f.rel_path),
               length(f.rel_path), f.id
      LIMIT 1)";

/// [`Index::resolve_imports`] narrowed to what one written file can have changed: its own
/// imports, and where its module key is new to the index (`keyed`, as `reconcile::upsert` says)
/// every import that key may answer and every import that resolved to it, which a file turned
/// folder no longer answers. A file rewritten where it was answers what it answered before,
/// which a crate root, whose key is every `crate::` path's module above, paid 70–160 ms a save
/// for on this repository (AMCOMP, 2026-11-23). A manifest can have moved every other crate's
/// import.
pub(super) fn resolve_imports_of(
    tx: &rusqlite::Transaction<'_>,
    rel: &str,
    keyed: bool,
) -> Result<()> {
    tx.prepare_cached(&format!(
        "UPDATE imports SET resolved_file = {BEST_IMPORT}
          WHERE src_file = (SELECT id FROM files WHERE rel_path = ?1)"
    ))?
    .execute([rel])?;
    if keyed {
        tx.prepare_cached(&format!(
            "UPDATE imports SET resolved_file = {BEST_IMPORT}
              WHERE key = (SELECT module_key FROM files WHERE rel_path = ?1)
                 OR up_key = (SELECT module_key FROM files WHERE rel_path = ?1)
                 OR up2_key = (SELECT module_key FROM files WHERE rel_path = ?1)
                 OR resolved_file = (SELECT id FROM files WHERE rel_path = ?1)"
        ))?
        .execute([rel])?;
    }
    if path::basename(rel) == MANIFEST {
        resolve_crate_imports(tx)?;
    }
    Ok(())
}

/// What makes a folder a crate, whose `[package]` names it ([`crate_name`](crate::code::imports::crate_name)).
pub(super) const MANIFEST: &str = "Cargo.toml";

/// Every import of another crate resolved again, for a manifest written or gone: those of a
/// crate the vault has, and those that found one before. One of a crate from crates.io, which
/// found none and still has none to find, stays as it is.
pub(super) fn resolve_crate_imports(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    tx.prepare_cached(&format!(
        "UPDATE imports SET resolved_file = {BEST_IMPORT}
          WHERE crate IS NOT NULL
            AND (resolved_file IS NOT NULL OR crate IN (SELECT name FROM crates))"
    ))?
    .execute([])?;
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

    /// Another crate's import is read in the folder its `Cargo.toml` names, and goes with the
    /// name; an item of a crate's root and one of a module written inline find the file they are
    /// written in.
    #[test]
    fn a_crate_is_found_by_the_name_its_manifest_gives_it() {
        let (vault, db) = fixture();
        let root = vault.path();
        fs::create_dir_all(root.join("crates/core/src")).unwrap();
        fs::create_dir_all(root.join("app/src")).unwrap();
        let manifest = root.join("crates/core/Cargo.toml");
        fs::write(&manifest, "[package]\nname = \"my-core\"\n").unwrap();
        let lib = "pub mod index;\npub fn locked() {}\nmod testing {\n    pub fn fixture() {}\n}\n";
        fs::write(root.join("crates/core/src/lib.rs"), lib).unwrap();
        let index = "use crate::locked;\nuse crate::testing::fixture;\n";
        fs::write(root.join("crates/core/src/index.rs"), index).unwrap();
        let main = "use my_core::index::Index;\nuse my_core::Vault;\nuse serde::Serialize;\n";
        fs::write(root.join("app/src/main.rs"), main).unwrap();
        let mut ix = open(&db);
        ix.reconcile(root, |_| {}).unwrap();

        let lib = Some("crates/core/src/lib.rs");
        assert_eq!(
            resolved(&ix, "crates/core/src/index.rs"),
            [
                row("crates/core/src/locked", lib),
                row("crates/core/src/testing/fixture", lib),
            ]
        );
        let crates = |ix: &Index| -> Vec<(String, String, Option<String>)> {
            let mut st = ix
                .conn
                .prepare(
                    "SELECT i.crate, i.target, f.rel_path FROM imports i
                     LEFT JOIN files f ON f.id = i.resolved_file
                     WHERE i.crate IS NOT NULL ORDER BY i.crate, i.target",
                )
                .unwrap();
            let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)));
            rows.unwrap().collect::<rusqlite::Result<_>>().unwrap()
        };
        let krate = |name: &str, target: &str, file: Option<&str>| {
            (name.to_string(), target.to_string(), file.map(Into::into))
        };
        assert_eq!(
            crates(&ix),
            [
                krate("my_core", "src", lib),
                krate("my_core", "src/index", Some("crates/core/src/index.rs")),
                krate("serde", "src", None),
            ]
        );

        fs::write(&manifest, "[package]\nname = \"other\"\n").unwrap();
        ix.update_file(root, "crates/core/Cargo.toml").unwrap();
        assert!(crates(&ix).iter().all(|(.., file)| file.is_none()));
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
