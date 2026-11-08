//! Typst through tinymist (DESIGN.md, Language servers): the settings it is started with, which
//! export the main document's PDF beside it on every save; the main document a file belongs to,
//! pinned before the file is opened or saved, so that a chapter's save builds the whole document;
//! its outline, each heading run over its section; and its word on each build.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use accent_lsp::types::{DocumentSymbol, Position};
use serde_json::{Value, json};

use crate::{Local, locked};

/// tinymist's symbol kind for a heading, LSP's namespace.
const HEADING: u32 = 3;

/// What tinymist is started with: the PDF exported beside the main document on each save, and a
/// word on every compile (`tinymist/compileStatus`). It reads them from the
/// `workspace/didChangeConfiguration` sent after the handshake; it never asks for them.
pub(super) fn settings() -> Value {
    json!({"tinymist": {
        "exportPdf": "onSave",
        "outputPath": "$dir/$name",
        "compileStatus": "enable",
    }})
}

/// The files one file includes or imports.
type Includes = Arc<Vec<PathBuf>>;

/// The main documents of one tinymist session.
pub(super) struct Mains {
    /// The session's root, where a path written `/…` starts and outside of which Typst reads
    /// nothing.
    root: PathBuf,
    /// Each file's includes and imports as last read, by its mtime.
    read: Mutex<HashMap<PathBuf, (SystemTime, Includes)>>,
    /// The main tinymist was last told of: it builds one document a session.
    pinned: Mutex<Option<PathBuf>>,
}

impl Mains {
    pub(super) fn new(root: PathBuf) -> Mains {
        Mains {
            root,
            read: Mutex::default(),
            pinned: Mutex::default(),
        }
    }

    /// The `tinymist.pinMain` command for the main document of `file`, an absolute path, unless
    /// that is the one pinned already.
    pub(super) fn pin(&self, file: &Path) -> Option<Value> {
        let main = self.main_of(file);
        let mut pinned = locked(&self.pinned);
        if pinned.as_ref() == Some(&main) {
            return None;
        }
        let command = json!({"command": "tinymist.pinMain", "arguments": [main]});
        *pinned = Some(main);
        Some(command)
    }

    /// The document `file` belongs to: the one pinned where it brings `file` in, so a library two
    /// documents import builds the one being worked on, and a file only a folder beside its own
    /// includes is found; else the file including it, looked for in its folder and then in each
    /// folder above it, nearest first, and so on up to a file nothing includes. `file` itself
    /// where nothing does, and where the includes go round.
    fn main_of(&self, file: &Path) -> PathBuf {
        let pinned = locked(&self.pinned).clone();
        if let Some(pinned) =
            pinned.filter(|pinned| self.document(pinned).iter().any(|f| f == file))
        {
            return pinned;
        }
        let mut main = file.to_path_buf();
        let mut seen = HashSet::from([main.clone()]);
        while let Some(up) = self.includer(&main) {
            if !seen.insert(up.clone()) {
                break;
            }
            main = up;
        }
        main
    }

    /// The first file including or importing `file`, in its folder or a folder above it inside
    /// the root, each folder's files by name.
    fn includer(&self, file: &Path) -> Option<PathBuf> {
        let dir = file.parent()?;
        dir.ancestors()
            .take_while(|folder| folder.starts_with(&self.root))
            .find_map(|folder| {
                let mut files: Vec<PathBuf> = std::fs::read_dir(folder)
                    .into_iter()
                    .flatten()
                    .flatten()
                    .map(|entry| entry.path())
                    .filter(|path| path.extension().is_some_and(|ext| ext == "typ"))
                    .collect();
                files.sort();
                files
                    .into_iter()
                    .find(|other| other != file && self.includes(other).iter().any(|i| i == file))
            })
    }

    /// The files `main` is built from: itself and every file it brings in, at any depth, once each.
    fn document(&self, main: &Path) -> Vec<PathBuf> {
        let (mut todo, mut seen) = (vec![main.to_path_buf()], Vec::new());
        while let Some(at) = todo.pop() {
            if !seen.contains(&at) {
                todo.extend(self.includes(&at).iter().cloned());
                seen.push(at);
            }
        }
        seen
    }

    /// What a `tinymist/compileStatus` says of a build: the main document built, as a vault path
    /// (tinymist names it from the root, `/main.typ`), whether the build failed, and then the vault
    /// paths of the files it is built from, whose tabs say so; `None` while one is compiling, and
    /// for a document outside the vault.
    pub(super) fn compiled(
        &self,
        params: &Value,
        vault_root: &Path,
    ) -> Option<(String, bool, Vec<String>)> {
        let failed = match params.get("status")?.as_str()? {
            "compileSuccess" => false,
            "compileError" => true,
            _ => return None,
        };
        let path = params.get("path")?.as_str()?;
        let main = Local::join(&self.root, path.trim_start_matches('/')).ok()?;
        let rel = |abs: &Path| Some(abs.strip_prefix(vault_root).ok()?.to_str()?.to_string());
        let files = match failed {
            true => self.document(&main).iter().filter_map(|f| rel(f)).collect(),
            false => Vec::new(),
        };
        Some((rel(&main)?, failed, files))
    }

    /// The files `file` includes or imports: from its folder, or from the root where the path
    /// starts with `/`, a package's (`@preview/…`) and one outside the root left out.
    fn includes(&self, file: &Path) -> Includes {
        let Ok(modified) = std::fs::metadata(file).and_then(|m| m.modified()) else {
            return Arc::default();
        };
        if let Some((at, found)) = locked(&self.read).get(file)
            && *at == modified
        {
            return found.clone();
        }
        let text = std::fs::read_to_string(file).unwrap_or_default();
        let dir = file
            .parent()
            .and_then(|dir| dir.strip_prefix(&self.root).ok());
        let found: Vec<PathBuf> = accent_core::code::typst::includes(&text)
            .into_iter()
            .filter(|path| !path.starts_with('@'))
            .filter_map(|path| {
                let rel = match path.strip_prefix('/') {
                    Some(rel) => PathBuf::from(rel),
                    None => dir?.join(&path),
                };
                Local::join(&self.root, rel.to_str()?).ok()
            })
            .collect();
        let found = Arc::new(found);
        locked(&self.read).insert(file.to_path_buf(), (modified, found.clone()));
        found
    }
}

/// tinymist's outline as the Outline lists it: a function with no name (a show rule's `it => …`)
/// left out, and each heading run to the end of its section, the line before the next heading
/// beside it or `last`, as the sticky title reads a symbol's span: tinymist spans the heading's
/// title alone.
pub(super) fn tidy(rows: &mut Vec<DocumentSymbol>, last: u32) {
    rows.retain(|row| row.name != "<anonymous>");
    let heads: Vec<Option<u32>> = rows
        .iter()
        .map(|row| (row.kind == HEADING).then_some(row.range.start.line))
        .collect();
    for (i, row) in rows.iter_mut().enumerate() {
        if row.kind != HEADING {
            continue;
        }
        let next = heads[i + 1..].iter().flatten().next();
        let end = next.map_or(last, |line| line.saturating_sub(1));
        row.range.end = Position {
            line: end.max(row.range.end.line),
            character: u32::MAX,
        };
        if let Some(children) = &mut row.children {
            tidy(children, row.range.end.line);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::language::Pos;

    /// A vault of `files`, each a path and its text.
    fn vault(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (rel, text) in files {
            let path = dir.path().join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        dir
    }

    #[test]
    fn a_file_belongs_to_the_document_including_it() {
        let dir = vault(&[
            ("thesis/main.typ", "#include \"/chapters/one.typ\"\n"),
            (
                "thesis/chapters/one.typ",
                "#include \"sections/deep.typ\"\n#include \"../figures/plot.typ\"\n",
            ),
            ("thesis/chapters/sections/deep.typ", "= Deep\n"),
            ("thesis/figures/plot.typ", "#let plot = 1\n"),
            ("thesis/alone.typ", "// #include \"chapters/one.typ\"\n"),
            ("thesis/a.typ", "#import \"lib.typ\": f\n"),
            ("thesis/b.typ", "#import \"lib.typ\": f\n"),
            ("thesis/lib.typ", "#let f = 1\n"),
            ("loop/x.typ", "#include \"y.typ\"\n"),
            ("loop/y.typ", "#include \"x.typ\"\n"),
        ]);
        let root = dir.path().join("thesis");
        let mains = Mains::new(root.clone());
        let main = root.join("main.typ");
        // A chapter the main takes from the root by `/`, a section below it, and a file nothing
        // includes but a comment.
        assert_eq!(mains.main_of(&root.join("chapters/one.typ")), main);
        assert_eq!(
            mains.main_of(&root.join("chapters/sections/deep.typ")),
            main
        );
        assert_eq!(
            mains.main_of(&root.join("alone.typ")),
            root.join("alone.typ")
        );
        // A file a folder beside its own includes is found from the document pinned.
        let plot = root.join("figures/plot.typ");
        assert_eq!(mains.main_of(&plot), plot);
        assert!(mains.pin(&main).is_some());
        assert_eq!(mains.main_of(&plot), main);
        // A library two documents import is the first one's by name, until the other is pinned.
        let lib = root.join("lib.typ");
        assert_eq!(mains.main_of(&lib), root.join("a.typ"));
        assert!(mains.pin(&root.join("b.typ")).is_some());
        assert_eq!(mains.main_of(&lib), root.join("b.typ"));
        assert!(mains.pin(&lib).is_none(), "b.typ is pinned already");
        // Includes going round end where they started.
        let other = Mains::new(dir.path().join("loop"));
        let x = dir.path().join("loop/x.typ");
        assert!([x.clone(), dir.path().join("loop/y.typ")].contains(&other.main_of(&x)));
    }

    #[test]
    fn a_heading_runs_to_the_next_beside_it_and_an_unnamed_function_goes() {
        let at = |line| accent_lsp::types::Range {
            start: Position { line, character: 0 },
            end: Position { line, character: 4 },
        };
        let row = |name: &str, kind, line, children| DocumentSymbol {
            name: name.to_string(),
            detail: None,
            kind,
            range: at(line),
            selection_range: at(line),
            children: Some(children),
        };
        // `= A <a>` on 0, `== A.1` on 2, a show rule's `it => …` on 3, `#let f` on 4, `= B` on 6,
        // in a file of 9 lines.
        let mut rows = vec![
            row(
                "A",
                HEADING,
                0,
                vec![row("a", 14, 0, vec![]), row("A.1", HEADING, 2, vec![])],
            ),
            row("<anonymous>", 12, 3, vec![]),
            row("f", 12, 4, vec![]),
            row("B", HEADING, 6, vec![]),
        ];
        tidy(&mut rows, 8);
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["A", "f", "B"]);
        let ends = |rows: &[DocumentSymbol]| -> Vec<u32> {
            rows.iter().map(|r| r.range.end.line).collect()
        };
        assert_eq!(ends(&rows), [5, 4, 8]);
        assert_eq!(ends(rows[0].children.as_deref().unwrap()), [0, 5]);
    }

    #[test]
    fn a_build_is_named_by_its_main_document_and_a_failed_one_by_its_files() {
        let dir = vault(&[
            ("thesis/main.typ", "#include \"ch.typ\"\n"),
            ("thesis/ch.typ", "= Chapter\n"),
            ("thesis/other.typ", "= Other\n"),
        ]);
        let mains = Mains::new(dir.path().join("thesis"));
        let status = |status: &str| json!({"path": "/main.typ", "status": status, "pageCount": 1});
        let main = || "thesis/main.typ".to_string();
        assert_eq!(
            mains.compiled(&status("compileError"), dir.path()),
            Some((main(), true, vec![main(), "thesis/ch.typ".to_string()]))
        );
        assert_eq!(
            mains.compiled(&status("compileSuccess"), dir.path()),
            Some((main(), false, Vec::new()))
        );
        assert_eq!(mains.compiled(&status("compiling"), dir.path()), None);
        let elsewhere = Path::new("/elsewhere");
        assert_eq!(mains.compiled(&status("compileError"), elsewhere), None);
    }

    /// tinymist end to end, skipped where it is not installed: a chapter opened alone is answered
    /// from the document including it, its save exports that document's PDF and nothing of its
    /// own, and a main that does not compile says so until it does.
    #[test]
    fn tinymist_builds_the_document_a_chapter_belongs_to() {
        if !super::super::in_path("tinymist") {
            eprintln!("tinymist is not installed: skipping the end-to-end test");
            return;
        }
        let main = "#set heading(numbering: \"1.\")\n= Intro <intro>\n#include \"ch.typ\"\n#bibliography(\"refs.bib\")\n";
        let chapter = "== Chapter <chap>\nSee @intro and @knuth84.\n@\n";
        let dir = vault(&[
            ("main.typ", main),
            ("ch.typ", chapter),
            (
                "refs.bib",
                "@article{knuth84, author = {Knuth, Donald}, title = {Literate Programming}, journal = {The Computer Journal}, year = 1984}\n",
            ),
        ]);
        let cache = tempfile::tempdir().unwrap();
        let (vault, events) = crate::Vault::open_at(
            dir.path(),
            &cache.path().join("index.db"),
            crate::VaultConfig::default(),
        )
        .unwrap();
        let reconciled = |e: &crate::Event| matches!(e, crate::Event::Reconciled(_));
        assert!(crate::tests::wait_for(&events, reconciled, crate::tests::BUDGET).is_some());
        let at = |line, character| Pos { line, character };
        accent_lsp::runtime().block_on(async {
            let support = vault
                .open_document("ch.typ", "typst", chapter.to_string())
                .await
                .unwrap();
            assert_eq!(support.missing, None, "tinymist is on PATH");
            // Asked again until the main document has been compiled, which names them.
            let deadline = std::time::Instant::now() + crate::tests::BUDGET;
            let labels = loop {
                let answer = vault.completion("ch.typ", at(2, 1), Some('@')).await;
                let labels: Vec<String> =
                    answer.unwrap().items.into_iter().map(|c| c.label).collect();
                let both = ["intro", "knuth84"]
                    .iter()
                    .all(|l| labels.iter().any(|x| x == l));
                if both || std::time::Instant::now() > deadline {
                    break labels;
                }
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            };
            for label in ["intro", "knuth84"] {
                assert!(labels.iter().any(|l| l == label), "{label} in {labels:?}");
            }
            let symbols = vault.symbols("ch.typ").await.unwrap();
            assert_eq!(symbols[0].name, "Chapter");
            assert_eq!(symbols[0].range.end.line, 2, "the heading's section");
            let cited = vault.definition("ch.typ", at(1, 18)).await.unwrap();
            assert_eq!(cited[0].path, "refs.bib", "a citation leads to its entry");
            vault.save_document("ch.typ").await.unwrap();
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        while !dir.path().join("main.pdf").exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        assert!(
            dir.path().join("main.pdf").exists(),
            "the main document is exported"
        );
        assert!(!dir.path().join("ch.pdf").exists(), "the chapter is not");
        // Its labels resolve against the main document: alone, `@intro` names nothing.
        let said = events.try_iter().filter_map(|e| match e {
            crate::Event::Diagnostics { rel, items, .. } if rel == "ch.typ" => Some(items),
            _ => None,
        });
        assert!(said.last().is_none_or(|items| items.is_empty()));
        // A main that does not compile, named with the chapter it includes, and again once it does.
        let built = |failed: bool| {
            move |e: &crate::Event| match e {
                crate::Event::Compiled {
                    rel,
                    failed: f,
                    files,
                } => rel == "main.typ" && *f == failed && files.len() == if failed { 2 } else { 0 },
                _ => false,
            }
        };
        accent_lsp::runtime().block_on(async {
            vault
                .open_document("main.typ", "typst", main.to_string())
                .await
                .unwrap();
            let broken = format!("{main}#undefined-function()\n");
            vault.change_document("main.typ", broken).await.unwrap();
        });
        assert!(crate::tests::wait_for(&events, built(true), crate::tests::BUDGET).is_some());
        accent_lsp::runtime().block_on(async {
            vault
                .change_document("main.typ", main.to_string())
                .await
                .unwrap();
        });
        assert!(crate::tests::wait_for(&events, built(false), crate::tests::BUDGET).is_some());
        drop(vault);
    }
}
