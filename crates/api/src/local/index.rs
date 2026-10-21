//! Index-backed [`Local`] tree, search, link, tag, and repository queries.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::channel;

use crate::Result;

use accent_core::index::Index;
use accent_core::walk::{self, FileKind};
use accent_core::{git, notebook, search};

use super::{Local, Msg};
use crate::language::notes;
use crate::paths::conflict_pairs;
use crate::{
    Backlink, CodeSymbol, FileRow, HeadingHit, Location, Match, Mention, Options, OutLink, PdfLink,
    Repo, SearchHit, Stats, SymbolName, fs, locked,
};

impl Local {
    /// List direct children of a vault-relative directory (`""` for root),
    /// keeping tree cost proportional to visible levels. Merge indexed rows
    /// with [`walk::unindexed_children`] from disk; unindexed rows carry `id == 0`.
    /// Run beside the files so remote trees can also show dependency folders
    /// without indexing, watching or searching them.
    pub fn list_dir(&self, rel: &str) -> Result<Vec<FileRow>> {
        list_dir(&self.index(), &self.root, rel)
    }

    /// Ranked search on a separate connection, superseded by newer pane queries
    /// ([`supersede`](Local::supersede)). `include_ignored` implements All; notes
    /// are included regardless, while other gitignored files require it.
    pub fn search(
        &self,
        query: &str,
        limit: usize,
        include_ignored: bool,
    ) -> Result<Vec<SearchHit>> {
        Ok(self.supersede(|index| index.search(query, limit, include_ignored))?)
    }

    /// The mid-word matches below a ranked search: [`Index::search_mid_word`], on the same
    /// connection as [`search`](Self::search) and superseded as it is.
    pub fn search_mid_word(
        &self,
        query: &str,
        limit: usize,
        include_ignored: bool,
        skip: &[String],
    ) -> Result<Vec<SearchHit>> {
        Ok(self.supersede(|index| index.search_mid_word(query, limit, include_ignored, skip))?)
    }

    /// Exact matches for `query` and `options`, capped at `limit`, plus total
    /// count for [`replace_all`](Self::replace_all). Use the same `include_ignored`
    /// and supersession as [`search`](Self::search). Compile beside the files: a
    /// `Regex` cannot cross RPC, and options such as case folding belong to its builder.
    pub fn grep(
        &self,
        query: &str,
        options: Options,
        limit: usize,
        include_ignored: bool,
    ) -> Result<(Vec<Match>, usize)> {
        let re = search::pattern(query, options)?;
        Ok(self.supersede(|index| index.grep(&re, limit, include_ignored))?)
    }

    /// Search unindexed files for the Search pane's All toggle. Skip indexed
    /// paths to avoid duplicates; return rows only, since Replace All uses the index.
    /// Match inside [`walk::visit`] workers with a shared atomic row budget and
    /// check `stop` before each file. Release the index connection before walking.
    /// Exclude `.git`, `.trash` and symlinked repositories' ignored build output
    /// to bound per-query cost. Sort returned rows by path; which rows survive
    /// a full budget depends on worker order. No cache: each query walks again.
    pub fn grep_unindexed(
        &self,
        query: &str,
        options: Options,
        limit: usize,
        stop: &(dyn Fn() -> bool + Sync),
    ) -> Result<Vec<Match>> {
        let re = &search::pattern(query, options)?;
        // Collected before the walk: the guard must not be held across file I/O.
        let known: HashSet<String> = self.searcher().file_paths(true)?.into_iter().collect();
        let opts = walk::ScanOptions {
            include_skipped: true,
            skip_dependency_trees: false,
            // Off, because a gitignored *directory* is one of the trees this exists to reach:
            // the walk leaves it out of the index and the file tree opens it by hand. The
            // gitignored *files* it also brings back are already indexed, so `known` drops them.
            vault_gitignore: false,
            target_gitignore: true,
            ..walk::ScanOptions::default()
        };
        let out: Mutex<Vec<Match>> = Mutex::new(Vec::new());
        // How many rows are in `out`. Read without the lock, so a file that matches nothing —
        // which is nearly all of them — never touches it at all.
        let found = AtomicUsize::new(0);
        walk::visit(&self.root, &opts, &|f| {
            if found.load(Ordering::Relaxed) >= limit || stop() {
                return false;
            }
            if f.kind == FileKind::Dir || known.contains(&f.rel_path) {
                return true;
            }
            let (mut rows, mut seen) = (Vec::new(), 0usize);
            match fs::read_text(&f.canonical) {
                Ok(fs::Read::Text(t)) if !t.lossy => {
                    if let Some(text) = searched(&f.rel_path, t.text) {
                        Index::matches_in(
                            &f.rel_path,
                            None,
                            &text,
                            re,
                            limit,
                            &mut rows,
                            &mut seen,
                        );
                    }
                }
                Ok(_) => {}
                // A file that vanished or cannot be read is not the query's problem.
                Err(e) => tracing::debug!("grep skipped {}: {e}", f.rel_path),
            }
            if rows.is_empty() {
                return true;
            }
            let mut out = locked(&out);
            out.extend(rows);
            found.store(out.len(), Ordering::Relaxed);
            out.len() < limit
        });
        let mut out = out.into_inner().unwrap_or_else(|e| e.into_inner());
        // Two threads can overshoot the budget between the load and the store; the extra rows
        // are real matches, but the pane asked for `limit` of them.
        out.sort_unstable_by(|a, b| (&a.rel_path, a.line).cmp(&(&b.rel_path, b.line)));
        // `more` rides on a file's last listed row, so a cut that lands inside a file would drop
        // its "+N more in this file" tail and leave the rows above it under-reporting. What the
        // cut takes from that file is folded into the last row that survived — the rows are
        // sorted, so those are the ones at the front of the cut. A file the cut misses entirely
        // is not listed at all, which is the cap the pane already announces.
        if out.len() > limit {
            let cut = out.split_off(limit);
            if let Some(tail) = out.last_mut() {
                let lost: usize = cut
                    .iter()
                    .take_while(|m| m.rel_path == tail.rel_path)
                    .map(|m| 1 + m.more)
                    .sum();
                tail.more += lost;
            }
        }
        Ok(out)
    }

    pub fn tags(&self) -> Result<Vec<(String, i64)>> {
        Ok(self.index().tags()?)
    }

    /// What the index holds, counted.
    pub fn stats(&self) -> Result<Stats> {
        Ok(self.index().stats()?)
    }

    pub fn files_with_tag(&self, tag: &str) -> Result<Vec<FileRow>> {
        Ok(self.index().files_with_tag(tag)?)
    }

    pub fn backlinks(&self, rel: &str) -> Result<Vec<Backlink>> {
        Ok(self.index().backlinks(rel)?)
    }

    /// Locate links resolving to any file at `rel`, for References when no
    /// language provider answers. Notes use their provider's open-buffer text.
    pub fn backlink_locations(&self, rel: &str) -> Result<Vec<Location>> {
        let rows = self.index().backlinks(rel)?;
        Ok(notes::placed(rows, |src| {
            std::fs::read_to_string(Local::join(&self.root, src)?)
                .map_err(|e| accent_core::Error::io(src, e).into())
        }))
    }

    pub fn links_from(&self, rel: &str) -> Result<Vec<OutLink>> {
        Ok(self.index().links_from(rel)?)
    }

    pub fn unresolved_links(&self) -> Result<Vec<(String, String, i64)>> {
        Ok(self.index().unresolved_links()?)
    }

    pub fn headings_matching(&self, words: &[String], limit: usize) -> Result<Vec<HeadingHit>> {
        Ok(self.index().headings_matching(words, limit)?)
    }

    /// Ranked as [`search`](Self::search) is, on the search connection and superseded with it.
    pub fn rank_files(
        &self,
        words: &[String],
        limit: usize,
    ) -> Result<Vec<(String, Option<String>)>> {
        Ok(self.supersede(|index| index.rank_files(words, limit))?)
    }

    pub fn file_symbols(&self, rel: &str) -> Result<Vec<CodeSymbol>> {
        Ok(self.index().file_symbols(rel)?)
    }

    pub fn find_symbols(&self, query: &str, limit: usize) -> Result<Vec<CodeSymbol>> {
        Ok(self.index().find_symbols(query, limit)?)
    }

    pub fn symbol_names(&self) -> Result<Vec<SymbolName>> {
        Ok(self.index().symbol_names()?)
    }

    pub fn symbols_named(
        &self,
        names: &[String],
        near: &str,
        per: usize,
    ) -> Result<Vec<CodeSymbol>> {
        Ok(self.index().symbols_named(names, near, per)?)
    }

    pub fn symbols_by_words(
        &self,
        words: &[String],
        limit: usize,
    ) -> Result<Vec<(CodeSymbol, usize)>> {
        Ok(self.index().symbols_by_words(words, limit)?)
    }

    /// Read on the search connection, as a search is: every code body holding the name's words.
    pub fn mentions(&self, name: &str, limit: usize) -> Result<Vec<Mention>> {
        Ok(self.supersede(|index| index.mentions(name, limit))?)
    }

    pub(crate) fn pdf_links(&self, rel: &str) -> Result<Vec<PdfLink>> {
        Ok(self.index().pdf_links(rel)?)
    }

    /// List openable files, notes first, for Go to File. Use `include_ignored`
    /// false to omit build output; ignored files remain openable from the tree.
    /// Wikilink completion separately uses `Index::note_and_pdf_paths`.
    pub fn file_paths(&self, include_ignored: bool) -> Result<Vec<String>> {
        Ok(self.index().file_paths(include_ignored)?)
    }

    /// Write search exclusions from git refresh and `[search] exclude` through
    /// the sole writing worker, avoiding a write-lock wait while holding the
    /// main thread's reader mutex. Wait for `reply` before returning so the next
    /// query sees exclusions. During reconciliation, apply to current rows
    /// between batches and again after the walk.
    pub fn set_excluded(&self, entries: &[String]) -> Result<()> {
        let gone = || accent_core::Error::Index("the vault worker stopped".to_string());
        let (reply, answer) = channel();
        self.tx
            .send(Msg::SetExcluded(entries.to_vec(), reply))
            .map_err(|_| gone())?;
        Ok(answer.recv().map_err(|_| gone())??)
    }

    pub fn recent_files(&self, limit: usize) -> Result<Vec<String>> {
        Ok(self.index().recent_files(limit)?)
    }

    /// The note a wikilink target points at, or `None` when it dangles and the UI can offer to
    /// create it.
    pub fn resolve_link(&self, target: &str) -> Result<Option<String>> {
        Ok(self.index().resolve_target(target)?)
    }

    /// Missing linked notes by the path New File would create. Exclude targets
    /// that exist in ignored folders.
    pub fn missing_notes(&self) -> Result<Vec<String>> {
        let mut missing = self.index().missing_notes()?;
        let ignored = self.lang.ignored.listing(false);
        missing.retain(|rel| ignored.resolve(rel).is_none());
        Ok(missing)
    }

    /// List ignored-folder notes after indexed files in Go to File. `fresh`
    /// requests a new walk on opening; otherwise reuse the last walk, loading
    /// lazily on the first request.
    pub fn ignored_notes(&self, fresh: bool) -> Result<Vec<String>> {
        Ok(self.lang.ignored.listing(fresh).notes().cloned().collect())
    }

    /// The file a link target names in the folders git ignores, by the rule the index resolves
    /// links by: what following a link the index cannot place opens.
    pub fn resolve_ignored(&self, target: &str) -> Result<Option<String>> {
        let ignored = self.lang.ignored.listing(false);
        Ok(ignored.resolve(target).map(str::to_string))
    }

    /// `(alias, note)` for every frontmatter alias in the vault.
    pub fn note_aliases(&self) -> Result<Vec<(String, String)>> {
        Ok(self.index().note_aliases()?)
    }

    /// `(original, conflict copy)` for every `*.sync-conflict-*` file whose original still exists.
    /// A copy of a note that has since been deleted is nothing the resolve UI can act on.
    pub fn conflicts(&self) -> Result<Vec<(String, String)>> {
        conflict_pairs(&self.index())
    }

    /// The conflict copies of one note, for the banner a tab raises over it.
    pub fn conflicts_of(&self, rel: &str) -> Result<Vec<String>> {
        Ok(conflict_pairs(&self.index())?
            .into_iter()
            .filter(|(original, _)| original == rel)
            .map(|(_, copy)| copy)
            .collect())
    }
}

/// What [`Local::grep_unindexed`] matches of the file `rel` holding `text`, as the index would
/// hold it: a diagram's labels, an `.ipynb`'s cells' sources, any other file's text; `None` for a
/// diagram that does not parse, which the index keeps no body of.
fn searched(rel: &str, text: String) -> Option<String> {
    if accent_core::path::holds_diagram(rel, &text) {
        let file = accent_drawio::File::from_bytes(text.as_bytes()).ok()?;
        return Some(accent_drawio::text::search_text(&file));
    }
    Some(notebook::search_text(rel, &text).unwrap_or(text))
}

/// [`Local::list_dir`] on any reader of the index of the vault at `root`.
pub(crate) fn list_dir(index: &Index, root: &Path, rel: &str) -> Result<Vec<FileRow>> {
    let mut rows = index.list_files(rel)?;
    let held: HashSet<&str> = rows.iter().map(|r| r.rel_path.as_str()).collect();
    // Non-fatal: a directory that vanished mid-listing must not blank the rows the index did
    // answer for.
    let extra = walk::unindexed_children(root, rel, &held).unwrap_or_else(|e| {
        tracing::debug!(dir = rel, "listing the unindexed children: {e}");
        Vec::new()
    });
    if extra.is_empty() {
        return Ok(rows);
    }
    rows.extend(extra.into_iter().map(|(rel_path, kind, why)| FileRow {
        id: 0,
        rel_path,
        kind,
        title: None,
        size: 0,
        mtime_ns: 0,
        dependency: why == walk::Unindexed::Dependency,
    }));
    // `Index::list_files` orders directories first and then by path, case-insensitively; the
    // merged listing has to come out the same way or the disk rows would land in a block of
    // their own at the end. `sort_by_cached_key` folds each path once rather than per compare.
    rows.sort_by_cached_key(|r| (r.kind != FileKind::Dir, r.rel_path.to_ascii_lowercase()));
    Ok(rows)
}

// ----------------------------------------------------------------------- git

impl Local {
    /// Find the repository containing the root and indexed directories with
    /// `.git` entries. Call off the main thread. Since walks skip `.git`,
    /// discover repositories through their containing directories.
    pub fn repos(&self) -> Vec<Repo> {
        // The searcher's connection, not the main thread's: discovery spawns a `git` process per
        // candidate directory and must not hold the lock the UI reads through.
        let dirs = self.searcher().dirs(&self.root).unwrap_or_else(|e| {
            tracing::debug!("listing vault directories for git discovery: {e}");
            Vec::new()
        });
        let repos = git::discover(&self.root, &dirs);
        // The watcher learns the repositories from here rather than finding them itself: this is
        // the only place that knows them, it already runs off the main thread, and the git pane
        // calls it whenever the set could have changed.
        self.post(Msg::WatchGit(
            repos.iter().map(|r| r.git_dir.clone()).collect(),
        ));
        repos
    }
}

#[cfg(test)]
mod tests {
    use crate::tests::*;
    use crate::{Event, Options, VaultConfig};

    /// An opened file in a gitignored folder is searched, and an edit made outside accent while
    /// its folder is watched reaches search as a note's does.
    #[test]
    fn an_opened_ignored_file_follows_its_edits() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join(".gitignore"), "build/\n").unwrap();
        std::fs::create_dir(root.path().join("build")).unwrap();
        std::fs::write(root.path().join("build/out.md"), "zorblat\n").unwrap();
        let f = Fixture::open_dir(root, VaultConfig::default());
        let found = |query: &str| f.vault.search(query, 10, false).unwrap().len();
        assert_eq!(found("zorblat"), 0);

        f.vault.keep_opened("build/out.md").unwrap();
        f.vault.watch_unindexed(&["build".to_string()]).unwrap();
        assert!(poll_until(|| found("zorblat") == 1, BUDGET));
        f.write("build/out.md", "quux\n");
        assert!(
            poll_until(|| found("quux") == 1, BUDGET),
            "the edit missed the index"
        );
    }

    /// Of three queries asked while the first still holds the search connection, the newest runs
    /// once it lets go and the one in between never does: the pane has already moved past it.
    #[test]
    fn a_newer_search_supersedes_the_ones_waiting() {
        let f = Fixture::open(VaultConfig::default());
        let crate::vault::Backend::Local(local) = &f.vault.backend else {
            unreachable!("the fixture is a local vault")
        };
        let until = |done: fn(&(u64, bool)) -> bool| {
            while !done(&crate::locked(&local.pane)) {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        };
        let (go, wait) = std::sync::mpsc::channel::<()>();
        std::thread::scope(|s| {
            let first = s.spawn(move || {
                local.supersede(|_| {
                    let _ = wait.recv();
                    Ok("first")
                })
            });
            until(|pane| pane.1);
            let second = s.spawn(|| local.supersede(|_| Ok("second")));
            until(|pane| pane.0 == 2);
            let third = s.spawn(|| local.supersede(|_| Ok("third")));
            until(|pane| pane.0 == 3);
            go.send(()).unwrap();
            assert_eq!(first.join().unwrap().unwrap(), "first");
            assert!(second.join().unwrap().is_err());
            assert_eq!(third.join().unwrap().unwrap(), "third");
        });
    }

    #[test]
    fn open_reconciles_and_lists_the_root() {
        let f = Fixture::open(VaultConfig::default());
        f.write("Note.md", "hello");
        f.write("sub/Deep.md", "deep");
        f.vault.rescan().unwrap();
        assert!(f.wait(|e| matches!(e, Event::Reconciled(_))).is_some());

        assert_eq!(
            names(&f.vault.list_dir("").unwrap()),
            ["sub", "Note.md"],
            "directories first, then files"
        );
        assert_eq!(names(&f.vault.list_dir("sub").unwrap()), ["sub/Deep.md"]);
    }

    #[test]
    fn list_dir_merges_the_trees_the_index_does_not_hold() {
        let f = Fixture::open(VaultConfig::default());
        f.write("Note.md", "hello");
        f.write("node_modules/pkg/index.js", "js");
        f.write("apples/a.md", "a");
        f.vault.rescan().unwrap();
        assert!(f.wait(|e| matches!(e, Event::Reconciled(_))).is_some());

        let root = f.vault.list_dir("").unwrap();
        assert_eq!(
            names(&root),
            ["apples", "node_modules", "Note.md"],
            "the skipped tree sorts among the indexed rows, not after them"
        );
        // Which of them is in the index is what the file tree reads to decide whether a row may
        // be renamed, moved or dropped onto.
        let id = |rel: &str| root.iter().find(|r| r.rel_path == rel).unwrap().id;
        assert_eq!(id("node_modules"), 0);
        assert!(id("apples") > 0);
        // Its contents come from the disk, one level at a time.
        assert_eq!(
            names(&f.vault.list_dir("node_modules").unwrap()),
            ["node_modules/pkg"]
        );
        assert_eq!(
            names(&f.vault.list_dir("node_modules/pkg").unwrap()),
            ["node_modules/pkg/index.js"]
        );
    }

    /// A gitignored folder costs a cold open nothing: the walk never enters it, the tree lists
    /// it beside its indexed siblings, and All is what reaches what is inside.
    #[test]
    fn a_gitignored_folder_is_opened_by_the_tree_rather_than_indexed() {
        let f = Fixture::open(VaultConfig::default());
        f.write("ml/.gitignore", "mlruns/\n");
        f.write("ml/README.md", "zorblat in a note\n");
        f.write("ml/mlruns/0/run/meta.yaml", "zorblat: 1\n");
        f.vault.rescan().unwrap();
        assert!(f.wait(|e| matches!(e, Event::Reconciled(_))).is_some());

        let paths = f.vault.file_paths(true).unwrap();
        assert!(
            !paths.iter().any(|p| p.contains("mlruns")),
            "the ignored tree reached the index: {paths:?}"
        );

        let rows = f.vault.list_dir("ml").unwrap();
        assert_eq!(names(&rows), ["ml/mlruns", "ml/.gitignore", "ml/README.md"]);
        assert_eq!(
            rows.iter().find(|r| r.rel_path == "ml/mlruns").unwrap().id,
            0,
            "the row comes off the disk, not out of the index"
        );
        assert_eq!(
            names(&f.vault.list_dir("ml/mlruns/0").unwrap()),
            ["ml/mlruns/0/run"]
        );

        // The index cannot see inside it; the All toggle's walk can.
        let plain = Options::default();
        assert_eq!(f.vault.grep("zorblat", plain, 10, true).unwrap().1, 1);
        let hits = f
            .vault
            .grep_unindexed("zorblat", plain, 10, &|| false)
            .unwrap();
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!(hits[0].rel_path, "ml/mlruns/0/run/meta.yaml");
    }

    /// What the file tree's Show Hidden Files has to choose from: every dotfile, and never `.git`.
    #[test]
    fn list_dir_holds_the_dotfiles_and_never_git() {
        let f = Fixture::open(VaultConfig::default());
        f.write(".gitignore", "target/\n");
        f.write(".obsidian/app.json", "{}");
        f.write(".git/config", "c");
        f.vault.rescan().unwrap();
        assert!(f.wait(|e| matches!(e, Event::Reconciled(_))).is_some());

        assert_eq!(
            names(&f.vault.list_dir("").unwrap()),
            [".obsidian", ".gitignore"]
        );
    }

    /// The other half of All: a tree the index never walked is greped from disk, and a file the
    /// index does hold is not greped twice.
    #[test]
    fn grep_unindexed_reaches_the_trees_the_walk_skipped() {
        let f = Fixture::open(VaultConfig::default());
        f.write("a.md", "zorblat in a note\n");
        std::fs::create_dir_all(f.vault.root().join("node_modules")).unwrap();
        f.write("node_modules/dep.js", "// zorblat\n");
        // Matched as the index matches a diagram, by its labels: a style key is no hit.
        f.write(
            "node_modules/flow.drawio",
            r#"<mxfile><diagram><mxGraphModel><root><mxCell id="0" value="Kettle" style="zorblat=1;"/></root></mxGraphModel></diagram></mxfile>"#,
        );
        f.vault.rescan().unwrap();
        assert!(f.wait(|e| matches!(e, Event::Reconciled(_))).is_some());

        let plain = Options::default();
        // The index never walked node_modules, so its own grep cannot see the dependency.
        assert_eq!(f.vault.grep("zorblat", plain, 10, true).unwrap().1, 1);

        let hits = f
            .vault
            .grep_unindexed("zorblat", plain, 10, &|| false)
            .unwrap();
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!(hits[0].rel_path, "node_modules/dep.js");
        assert!(
            !hits.iter().any(|h| h.rel_path == "a.md"),
            "an indexed note must not be greped a second time: {hits:?}"
        );

        // A walk whose question is already out of date opens nothing more.
        let stopped = f.vault.grep_unindexed("zorblat", plain, 10, &|| true);
        assert!(stopped.unwrap().is_empty());
    }

    /// The row budget is shared by the walking threads, so a cap is a cap however many of them
    /// matched at once, and the rows come back in path order rather than in finishing order.
    #[test]
    fn grep_unindexed_honours_the_row_budget_and_answers_in_path_order() {
        let f = Fixture::open(VaultConfig::default());
        for i in 0..50 {
            f.write(&format!("node_modules/pkg{i:02}/dep.js"), "// zorblat\n");
        }
        f.vault.rescan().unwrap();
        assert!(f.wait(|e| matches!(e, Event::Reconciled(_))).is_some());

        let plain = Options::default();
        let hits = f
            .vault
            .grep_unindexed("zorblat", plain, 7, &|| false)
            .unwrap();
        assert_eq!(hits.len(), 7, "{hits:?}");
        let paths: Vec<&str> = hits.iter().map(|h| h.rel_path.as_str()).collect();
        let mut sorted = paths.clone();
        sorted.sort_unstable();
        assert_eq!(paths, sorted, "rows must be ordered for the reader");
    }

    /// Preserve the "+N more in this file" tail when the cap cuts a file's
    /// rows, transferring omitted counts onto the last surviving row.
    #[test]
    fn a_cut_inside_a_file_folds_its_dropped_rows_into_the_tail_row() {
        let f = Fixture::open(VaultConfig::default());
        // Two unindexed files of six matches each. The budget is only read before a file is
        // opened, so both are greped whole and ten rows come back for a budget of seven.
        for name in ["a.js", "b.js"] {
            f.write(&format!("node_modules/{name}"), &"zorblat\n".repeat(6));
        }
        f.vault.rescan().unwrap();
        assert!(f.wait(|e| matches!(e, Event::Reconciled(_))).is_some());

        let hits = f
            .vault
            .grep_unindexed("zorblat", Options::default(), 7, &|| false)
            .unwrap();
        assert_eq!(hits.len(), 7, "{hits:?}");
        // Five of a.js's six matches are listed; the sixth is the per-file cap's own tail.
        assert_eq!(hits[4].rel_path, "node_modules/a.js");
        assert_eq!(hits[4].more, 1, "{hits:?}");
        // b.js got the two rows left of the budget, and the four matches the cut took are on the
        // second of them.
        assert_eq!(hits[6].rel_path, "node_modules/b.js");
        assert_eq!(hits[6].more, 4, "{hits:?}");
    }

    /// The exclusion set is written by the vault worker, and the call still means it is written:
    /// the next query must already leave the excluded file out.
    #[test]
    fn set_excluded_has_landed_when_it_returns() {
        let f = Fixture::open(VaultConfig::default());
        f.write("keep.txt", "keep");
        // Not a note: a note is listed whether or not git ignores it.
        f.write("build/out.txt", "out");
        f.vault.rescan().unwrap();
        assert!(f.wait(|e| matches!(e, Event::Reconciled(_))).is_some());

        f.vault.set_excluded(&["build/".to_string()]).unwrap();
        let paths = f.vault.file_paths(false).unwrap();
        assert!(paths.contains(&"keep.txt".to_string()), "{paths:?}");
        assert!(!paths.contains(&"build/out.txt".to_string()), "{paths:?}");

        f.vault.set_excluded(&[]).unwrap();
        assert!(
            f.vault
                .file_paths(false)
                .unwrap()
                .contains(&"build/out.txt".to_string())
        );
    }

    /// Bodies outside the notes are in the index now, so the one grep reaches them.
    #[test]
    fn grep_reaches_text_outside_notes() {
        let f = Fixture::open(VaultConfig::default());
        f.write("tool.py", "import os\nprint('zorblat')\n");
        f.write("bin.dat", "\0zorblat\n");
        f.vault.rescan().unwrap();
        assert!(f.wait(|e| matches!(e, Event::Reconciled(_))).is_some());

        let (hits, total) = f
            .vault
            .grep("zorblat", Options::default(), 10, false)
            .unwrap();
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!(hits[0].rel_path, "tool.py");
        assert_eq!(hits[0].line, 2);
        // Counted too: the count is what Replace All would rewrite, and it rewrites every
        // indexed body. (The NUL byte is what keeps bin.dat out of the rows.)
        assert_eq!(total, 1);
    }

    /// Any file has backlinks, placed where each link is written: what the References pane lists
    /// for a PDF, an image or a text file, one row per note as for a note.
    #[test]
    fn a_file_that_is_not_a_note_has_its_backlinks_placed() {
        let f = Fixture::open(VaultConfig::default());
        f.write("a.txt", "text");
        f.write("n.md", "# n\n\nsee [[a.txt]]\n");
        f.vault.rescan().unwrap();
        assert!(f.wait(|e| matches!(e, Event::Reconciled(_))).is_some());

        let found = f.vault.backlink_locations("a.txt").unwrap();
        let placed: Vec<_> = found
            .iter()
            .map(|l| (l.path.as_str(), l.range.start.line))
            .collect();
        assert_eq!(placed, [("n.md", 2)]);
    }

    #[test]
    fn resolve_link_matches_a_stem_case_insensitively() {
        let f = Fixture::open(VaultConfig::default());
        f.write("Sub/Meeting Notes.md", "hello");
        f.vault.rescan().unwrap();
        assert!(f.wait(|e| matches!(e, Event::Reconciled(_))).is_some());

        assert_eq!(
            f.vault.resolve_link("meeting notes").unwrap().as_deref(),
            Some("Sub/Meeting Notes.md")
        );
        assert_eq!(f.vault.resolve_link("nothing here").unwrap(), None);
    }

    #[test]
    fn repos_lists_the_vault_repo_and_a_nested_one() {
        // No git binary, nothing to discover; the rest of the vault works either way.
        if std::process::Command::new("git")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }
        let f = Fixture::open(VaultConfig::default());
        let root = f.vault.root().to_path_buf();
        f.write("sub/Note.md", "hi");
        for dir in [root.clone(), root.join("sub")] {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(["init", "-q", "-b", "main"])
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .output()
                .unwrap();
            assert!(out.status.success(), "git init in {}", dir.display());
        }
        f.vault.rescan().unwrap();
        assert!(f.wait(|e| matches!(e, Event::Reconciled(_))).is_some());

        let repos = f.vault.repos().unwrap();
        assert_eq!(
            repos.len(),
            2,
            "the vault's own repository and the nested one"
        );
        assert_eq!(repos[0].root, root);
        assert_eq!(repos[1].root, root.join("sub"));
    }
}
