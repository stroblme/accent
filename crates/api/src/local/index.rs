//! What [`Local`](super::Local) answers from the index: the tree, the searches, the links and
//! the tags, plus the repository discovery that reads the same directory listing.

use std::collections::HashSet;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::channel;

use anyhow::{Context, Result};

use accent_core::index::Index;
use accent_core::walk::{self, FileKind};
use accent_core::{git, search::Regex};

use super::{Local, Msg};
use crate::paths::conflict_pairs;
use crate::{Backlink, FileRow, Match, PdfLink, Repo, SearchHit, fs, locked};

impl Local {
    /// Direct children of one directory ("" is the vault root): one level per call, so the tree
    /// costs what it shows.
    ///
    /// The index's own rows, plus the trees it deliberately does not hold — `node_modules`, a
    /// `.venv`, a cargo `target/` — read straight off the disk
    /// ([`walk::unindexed_children`]) and merged in, so the file tree can show every folder in
    /// the vault without any of them being indexed, watched or searched. Those rows carry
    /// `id == 0`, which is what tells them apart. It happens here rather than in the tree so
    /// that a vault on another machine gets it too: this runs on the host holding the files.
    pub fn list_dir(&self, rel: &str) -> Result<Vec<FileRow>> {
        let mut rows = self.index().list_files(rel)?;
        let held: HashSet<&str> = rows.iter().map(|r| r.rel_path.as_str()).collect();
        // Non-fatal: a directory that vanished mid-listing must not blank the rows the index did
        // answer for.
        let extra = walk::unindexed_children(&self.root, rel, &held).unwrap_or_else(|e| {
            tracing::debug!(dir = rel, "listing the unindexed children: {e}");
            Vec::new()
        });
        if extra.is_empty() {
            return Ok(rows);
        }
        rows.extend(extra.into_iter().map(|(rel_path, kind)| FileRow {
            id: 0,
            rel_path,
            kind,
            title: None,
            size: 0,
            mtime_ns: 0,
        }));
        // `Index::list_files` orders directories first and then by path, case-insensitively; the
        // merged listing has to come out the same way or the disk rows would land in a block of
        // their own at the end. `sort_by_cached_key` folds each path once rather than per compare.
        rows.sort_by_cached_key(|r| (r.kind != FileKind::Dir, r.rel_path.to_ascii_lowercase()));
        Ok(rows)
    }

    /// Ranked full-text search. On the search connection, so a slow query cannot block the tree.
    ///
    /// `include_ignored` is the sidebar's All toggle: off, what git ignores is left out of the
    /// results; on, it is put back. A note is in either way.
    pub fn search(
        &self,
        query: &str,
        limit: usize,
        include_ignored: bool,
    ) -> Result<Vec<SearchHit>> {
        self.searcher().search(query, limit, include_ignored)
    }

    /// Exact search: one row per match of `re`, capped at `limit`, plus how many of them a
    /// [`replace_all`](Self::replace_all) would rewrite — markdown only, since that is all it
    /// visits. `include_ignored` means what it does in [`search`](Self::search).
    pub fn grep(
        &self,
        re: &Regex,
        limit: usize,
        include_ignored: bool,
    ) -> Result<(Vec<Match>, usize)> {
        self.searcher().grep(re, limit, include_ignored)
    }

    /// The same exact search over the files the index does not hold at all: those under a
    /// dependency tree, a `node_modules` or a `target/` the walk deliberately never entered.
    ///
    /// This is the second half of the Search pane's All toggle, and only the exact-match path
    /// runs it. The first half drops the git-ignored exclusion, which is a column in the index and
    /// which ranked search reads as well; this one reaches what was never indexed, and it can only
    /// be a walk, so a ranked query has no way to fold it in. Everything already in the index is
    /// skipped by path, so no file is greped twice, and the walk stops as soon as `limit` matches
    /// are in hand.
    ///
    /// The matching runs **inside** the walk ([`walk::visit`]), on its threads, rather than over
    /// a [`walk::ScanResult`] it built first: reading 10 000 dependency files one at a time was
    /// most of what a settled query cost. The row budget is therefore shared — an atomic every
    /// thread reads before it opens a file and writes when it has appended — so a query whose
    /// rows fill early stops the walk instead of finishing it. Nothing is read on the index's
    /// connection: the guard is dropped before the walk starts, and the caller is the sidebar's
    /// search worker either way. `.git` and `.trash` stay unreachable, and so does a symlinked
    /// repository's own gitignored build output: that is somebody else's build tree, and leaving
    /// it out is what keeps a per-query walk affordable.
    ///
    /// The rows are sorted by path before they are returned, because the walk answers in whatever
    /// order its threads got there and the reader is looking at a list. *Which* rows survive a
    /// full budget is no longer deterministic — the threads race for it — and cannot be: that is
    /// the price of not reading every file, and the pane already says the list is capped.
    ///
    /// Rows only, no count beside them: nothing here can be rewritten by Replace All, which
    /// visits the indexed notes, so a number of matches past `limit` would have no reader.
    ///
    /// ponytail: the walk runs per query, with no cache, for as long as All is on.
    pub fn grep_unindexed(&self, re: &Regex, limit: usize) -> Result<Vec<Match>> {
        // Collected before the walk: the guard must not be held across file I/O.
        let known: HashSet<String> = self.searcher().file_paths(true)?.into_iter().collect();
        let opts = walk::ScanOptions {
            include_skipped: true,
            skip_dependency_trees: false,
            // Inside the vault, what git ignores is already indexed, so the walk can skip it and
            // the `known` test would have dropped it anyway.
            vault_gitignore: true,
            target_gitignore: true,
            ..walk::ScanOptions::default()
        };
        let out: Mutex<Vec<Match>> = Mutex::new(Vec::new());
        // How many rows are in `out`. Read without the lock, so a file that matches nothing —
        // which is nearly all of them — never touches it at all.
        let found = AtomicUsize::new(0);
        walk::visit(&self.root, &opts, &|f| {
            if found.load(Ordering::Relaxed) >= limit {
                return false;
            }
            if f.kind == FileKind::Dir || known.contains(&f.rel_path) {
                return true;
            }
            let (mut rows, mut seen) = (Vec::new(), 0usize);
            match fs::read_text(&f.canonical) {
                Ok(fs::Read::Text(t)) if !t.lossy => {
                    Index::matches_in(&f.rel_path, None, &t.text, re, limit, &mut rows, &mut seen);
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
        self.index().tags()
    }

    pub fn files_with_tag(&self, tag: &str) -> Result<Vec<FileRow>> {
        self.index().files_with_tag(tag)
    }

    pub fn backlinks(&self, rel: &str) -> Result<Vec<Backlink>> {
        self.index().backlinks(rel)
    }

    pub(crate) fn pdf_links(&self, rel: &str) -> Result<Vec<PdfLink>> {
        self.index().pdf_links(rel)
    }

    pub fn note_paths(&self) -> Result<Vec<String>> {
        self.index().note_paths()
    }

    /// Every file the app can open, notes first: what the palette's switcher lists, now that a
    /// tab is not necessarily a note. [`note_paths`](Self::note_paths) stays markdown-only,
    /// because `[[` completion may only offer notes.
    ///
    /// The palette has no All toggle of its own, so it asks with `include_ignored` false and the
    /// build output stays out of Go to File. The tree still lists an ignored file, dimmed, which
    /// is the way to open one.
    pub fn file_paths(&self, include_ignored: bool) -> Result<Vec<String>> {
        self.index().file_paths(include_ignored)
    }

    /// Hand the index what search leaves out, so every later query can leave it out.
    ///
    /// Called from the git refresh, which is the one place in the app that has already asked git
    /// and where the `[search] exclude` list joins git's answer. The write goes to the vault
    /// worker rather than to the caller's connection, because the worker owns the only writing
    /// one: on the caller's it had to wait for the worker's write lock while holding the mutex
    /// the main thread reads through, and `list_dir` queued behind it. Measured on the 40k-entry
    /// test vault, cold: a read waited 5 006 ms and the write then failed outright with "database
    /// is locked", the busy handler having been starved by a reconcile that takes the write lock
    /// back between every batch.
    ///
    /// It still returns only once the write has landed — the worker answers on `reply` — because
    /// the caller re-runs the query on screen the moment it does. What the caller now waits for is
    /// the worker reaching this message, which during a reconcile is the end of the batch in
    /// progress rather than of the walk: the worker writes the set for the rows already there and
    /// again once the walk is over (`Worker::reconcile`).
    pub fn set_excluded(&self, entries: &[String]) -> Result<()> {
        let (reply, answer) = channel();
        self.tx
            .send(Msg::SetExcluded(entries.to_vec(), reply))
            .map_err(|_| anyhow::anyhow!("the vault worker is gone"))?;
        answer
            .recv()
            .context("the vault worker stopped before it recorded the exclusion set")?
    }

    pub fn recent_notes(&self, limit: usize) -> Result<Vec<String>> {
        self.index().recent_notes(limit)
    }

    /// The note a wikilink target points at, or `None` when it dangles and the UI can offer to
    /// create it.
    pub fn resolve_link(&self, target: &str) -> Result<Option<String>> {
        self.index().resolve_target(target)
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

// ----------------------------------------------------------------------- git

impl Local {
    /// The repositories the vault touches: the one holding the root, plus every indexed directory
    /// carrying a `.git` entry. Runs on the caller's thread, which is never the main one.
    ///
    /// The walk hard-skips `.git`, so a repository is only ever found by the directory holding it.
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
        f.vault.rescan().unwrap();
        assert!(f.wait(|e| matches!(e, Event::Reconciled(_))).is_some());

        let plain = Options::default();
        // The index never walked node_modules, so its own grep cannot see the dependency.
        assert_eq!(f.vault.grep("zorblat", plain, 10, true).unwrap().1, 1);

        let hits = f.vault.grep_unindexed("zorblat", plain, 10).unwrap();
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!(hits[0].rel_path, "node_modules/dep.js");
        assert!(
            !hits.iter().any(|h| h.rel_path == "a.md"),
            "an indexed note must not be greped a second time: {hits:?}"
        );
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
        let hits = f.vault.grep_unindexed("zorblat", plain, 7).unwrap();
        assert_eq!(hits.len(), 7, "{hits:?}");
        let paths: Vec<&str> = hits.iter().map(|h| h.rel_path.as_str()).collect();
        let mut sorted = paths.clone();
        sorted.sort_unstable();
        assert_eq!(paths, sorted, "rows must be ordered for the reader");
    }

    /// The cap can fall inside a file's rows, and the "+N more in this file" tail rides on the
    /// last of them: what the cut takes has to end up on the row before it rather than going with
    /// the row it was on.
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
            .grep_unindexed("zorblat", Options::default(), 7)
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
        // Listed, but not counted: the count is what Replace All would rewrite, and it rewrites
        // notes. (The NUL byte is what keeps bin.dat out of the rows.)
        assert_eq!(total, 0);
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
