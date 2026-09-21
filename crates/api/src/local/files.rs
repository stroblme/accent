//! What [`Local`](super::Local) does to the files themselves: read, write, delete, the templates
//! a new note is made from, the rename that carries a note's links with it, and the conflict
//! copies a sync leaves behind.

use std::collections::{BTreeSet, HashMap};
use std::io;

use anyhow::{Context, Result};

use accent_core::path::{basename, stem};
use accent_core::{diff, markdown, search, template};

use super::{Local, Msg};
use crate::paths::{accent_conflict_name, with_md};
use crate::{
    DiffLine, Etag, FileEdits, FileKind, Regex, RenamePlan, RenameReport, ReplaceReport, SaveError,
    fs,
};

impl Local {
    pub fn read(&self, rel: &str) -> io::Result<(String, Etag)> {
        fs::read_note(&self.resolve(rel)?)
    }

    /// Write a note, then tell the worker about it: the index is correct within a millisecond
    /// instead of a watcher debounce later, and the save never comes back as [`Event::FileChanged`].
    ///
    /// ponytail: the write runs on the calling thread, because an fsync of a note is well under a
    /// frame on an SSD. If a slow disk ever shows up, move the write to the worker and answer
    /// with an event.
    pub fn save(&self, rel: &str, text: &str, expected: Option<Etag>) -> Result<Etag, SaveError> {
        let etag = fs::write_note(&self.resolve(rel)?, text, expected)?;
        self.post(Msg::Update {
            rel: rel.to_string(),
            own: true,
        });
        Ok(etag)
    }

    /// Read any file as text, saying so when it is binary or too big to hold. What a tab opens
    /// with; [`read`](Self::read) is the note-shaped version the rename and conflict paths use.
    pub fn read_text(&self, rel: &str) -> io::Result<fs::Read> {
        fs::read_text(&self.resolve(rel)?)
    }

    /// The file's etag, or `None` when there is no file there. One `stat`, which is how a tab
    /// asks "did this change under me" and how the app asks "does this path exist".
    pub fn stat(&self, rel: &str) -> io::Result<Option<Etag>> {
        match Etag::of(&self.resolve(rel)?) {
            Ok(etag) => Ok(Some(etag)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Delete a file or a whole directory, permanently.
    ///
    /// The desktop trashes through `gio` instead and never calls this; it exists for a vault on
    /// another machine, where there is no session bus to ask and no trash to ask it about. The
    /// UI is what makes that difference visible, by confirming the way it already confirms a
    /// delete the trash could not take.
    pub fn delete(&self, rel: &str) -> io::Result<()> {
        let path = self.resolve(rel)?;
        match path.is_dir() {
            true => std::fs::remove_dir_all(&path)?,
            false => std::fs::remove_file(&path)?,
        }
        self.post(Msg::Update {
            rel: rel.to_string(),
            own: true,
        });
        Ok(())
    }

    /// A template's text, whether it is named by a vault-relative path or by a bare file name in
    /// the templates directory. Failing, it names both places it looked.
    fn read_template(&self, name: &str) -> Result<String> {
        // The config lock is taken before the filesystem, never inside the index lock.
        let tried = template::candidates(&self.config().templates_dir, name);
        for rel in &tried {
            let path = self.resolve(rel)?;
            if path.is_file() {
                let (text, _) =
                    fs::read_note(&path).with_context(|| format!("reading template {rel}"))?;
                return Ok(text);
            }
        }
        anyhow::bail!("no template {name}: looked at {}", tried.join(" and "))
    }

    /// A template's text as a note called `title` would get it: the body with every placeholder
    /// expanded, and the byte offsets of its `{{cursor}}` stops. What Insert Template puts at
    /// the caret, and what [`create_note`](Self::create_note) writes.
    pub fn render_template(&self, template: &str, title: &str) -> Result<(String, Vec<usize>)> {
        // Through `parse` first: `accent-target:` is accent's directive, not the note's text.
        Ok(template::render(
            &template::parse(&self.read_template(template)?).body,
            title,
            chrono::Local::now().naive_local(),
        ))
    }

    /// Create a file, optionally from a template. Returns the path it was created at and where
    /// the caret belongs. The name is taken as it is given: `notes` is a file called `notes`, not
    /// a note called `notes.md`. Callers that mean markdown say so (`note_from_template` does).
    pub fn create_note(&self, rel: &str, template: Option<&str>) -> Result<(String, Vec<usize>)> {
        let rel = rel.to_string();
        let (text, cursor) = match template {
            Some(t) => self.render_template(t, &stem(&rel))?,
            None => (String::new(), Vec::new()),
        };
        fs::create_note(&self.resolve(&rel)?, &text).with_context(|| format!("creating {rel}"))?;
        self.post(Msg::Update {
            rel: rel.clone(),
            own: true,
        });
        Ok((rel, cursor))
    }

    pub fn create_dir(&self, rel: &str) -> io::Result<()> {
        std::fs::create_dir_all(self.resolve(rel)?)?;
        // ponytail: only the deepest component is indexed straight away; intermediate levels of a
        // nested path wait for the watcher or the next reconcile. Post one update per component
        // if the tree ever looks wrong right after "New folder".
        self.post(Msg::Update {
            rel: rel.to_string(),
            own: true,
        });
        Ok(())
    }

    /// Copy a file, or a whole directory, from one place in the vault to another.
    ///
    /// Vault-relative on both ends, so on a remote vault this runs where the files are and an
    /// in-vault paste costs no bytes over the link. It never overwrites: the caller picks a name
    /// nothing holds yet, which is what makes a paste beside its source a "(copy)".
    pub fn copy(&self, from: &str, to: &str) -> io::Result<()> {
        let (src, dest) = (self.resolve(from)?, self.resolve(to)?);
        match src.is_dir() {
            true => copy_tree(&src, &dest)?,
            false => {
                std::fs::copy(&src, &dest)?;
            }
        }
        self.post(Msg::Update {
            rel: to.to_string(),
            own: true,
        });
        Ok(())
    }

    /// What moving these files and folders would touch, so the UI can show it before anything
    /// is written: the notes whose links the moves would leave naming the wrong place, and the
    /// source files whose imports the language servers already running say would.
    ///
    /// A dry run of the rewrite rather than a list of backlinks, so a move whose backlinks are
    /// all bare `[[Note]]`s names nothing and asks nothing. The candidates are the notes linking
    /// to anything that moves, and the moved notes whose markdown links are relative to them.
    /// The servers are asked here, before anything moves, because they may read the disk to
    /// answer: rust-analyzer asks it whether the path is a folder.
    pub fn plan_moves(&self, moves: &[(String, String)]) -> Result<RenamePlan> {
        let files = self.moved_files(moves)?;
        let mut candidates = BTreeSet::new();
        {
            let index = self.index();
            for old in files.keys() {
                candidates.extend(index.backlinks(old)?.into_iter().map(|b| b.src_rel_path));
            }
            for (from, _) in moves {
                candidates.extend(index.markdown_link_sources(from)?);
            }
        }
        let targets = self.link_targets(&candidates)?;
        let rewrites = candidates
            .into_iter()
            // One that cannot be read is listed all the same: the rewrite will say why it failed.
            .filter(|rel| match self.read(rel) {
                Ok((text, _)) => {
                    let now = files.get(rel).unwrap_or(rel);
                    markdown::rewrite_moved(&text, rel, now, &targets, &files).is_some()
                }
                Err(_) => true,
            })
            .collect();
        let kinds = moves
            .iter()
            .map(|(from, to)| Ok((from.clone(), to.clone(), self.resolve(from)?.is_dir())))
            .collect::<io::Result<Vec<_>>>()?;
        let (imports, asked) = self.lang.will_rename(&kinds);
        let mut unchecked: Vec<String> = files
            .keys()
            .filter(|old| imports_by_path(old) && !asked.iter().any(|from| is_under(old, from)))
            .cloned()
            .collect();
        unchecked.sort();
        Ok(RenamePlan {
            moves: moves.to_vec(),
            rewrites,
            imports,
            unchecked,
        })
    }

    /// Apply a [`RenamePlan`]: the moves in order, stopping at the first that fails, then, with
    /// `update`, the link rewrites for what did move and the import edits. Only a failure before
    /// the first move fails the call; a move or a file that could not be done is reported
    /// instead, because a half-renamed vault is worse than one whose report says exactly what
    /// happened.
    ///
    /// Not cut off at [`crate::vault::MOVE_BOUND`] the way a rewrite is, for the same reason: by
    /// the time the links are being rewritten the files have already moved, and there is no second
    /// run that could finish them. The bound is what a remote caller waits, not a cap here.
    pub fn rename(&self, plan: &RenamePlan, update: bool) -> Result<RenameReport> {
        // Which file each link names is a question only the index can answer, and only while it
        // still describes the vault as it was: ask before the first move.
        let (mut files, targets) = match update {
            true => (
                self.moved_files(&plan.moves)?,
                self.link_targets(&plan.rewrites)?,
            ),
            false => Default::default(),
        };
        let mut report = RenameReport::default();
        for (from, to) in &plan.moves {
            if let Err(e) = self
                .resolve(from)
                .and_then(|path| fs::rename(&path, &self.resolve(to)?))
            {
                report.not_moved = Some((from.clone(), e.to_string()));
                break;
            }
            // Both ends, so the index and the tree do not wait for inotify.
            for rel in [from, to] {
                self.post(Msg::Update {
                    rel: rel.clone(),
                    own: true,
                });
            }
            report.moved.push((from.clone(), to.clone()));
        }
        if !update {
            return Ok(report);
        }
        // A link to a file that is still where it was is not stale.
        files.retain(|old, _| report.moved.iter().any(|(from, _)| is_under(old, from)));
        for rel in &plan.rewrites {
            let now = files.get(rel).unwrap_or(rel);
            match self.rewrite_one(rel, now, &targets, &files) {
                Ok(true) => report.rewritten.push(now.clone()),
                Ok(false) => {}
                Err(e) => {
                    tracing::warn!("rewriting links in {now}: {e:#}");
                    report.failed.push((now.clone(), format!("{e:#}")));
                }
            }
        }
        // The imports name where every file was going, so a batch that stopped part way gets
        // none of them.
        if report.not_moved.is_some() {
            return Ok(report);
        }
        for found in &plan.imports {
            let now = files.get(&found.rel).unwrap_or(&found.rel);
            match self.edit_one(now, found) {
                Ok(()) => report.rewritten.push(now.clone()),
                Err(e) => {
                    tracing::warn!("updating imports in {now}: {e:#}");
                    report.failed.push((now.clone(), format!("{e:#}")));
                }
            }
        }
        Ok(report)
    }

    /// Make a language server's edits to the file now at `now`, back to front, gated on the etag
    /// they were measured against: a file that changed since the plan is refused, not guessed at,
    /// and so is a set of edits that overlap.
    fn edit_one(&self, now: &str, found: &FileEdits) -> Result<()> {
        let path = self.resolve(now)?;
        let (mut text, _) = fs::read_note(&path)?;
        let mut edits = found.edits.clone();
        edits.sort_by_key(|(start, ..)| std::cmp::Reverse(*start));
        let mut end = text.len();
        for (start, stop, with) in &edits {
            anyhow::ensure!(
                start <= stop
                    && *stop <= end
                    && text.is_char_boundary(*start)
                    && text.is_char_boundary(*stop),
                "the language server's edits overlap"
            );
            text.replace_range(*start..*stop, with);
            end = *start;
        }
        fs::write_note(&path, &text, Some(found.etag))?;
        self.post(Msg::Update {
            rel: now.to_string(),
            own: true,
        });
        Ok(())
    }

    /// Every file the moves take somewhere, old path to new: a folder's one by one, as the
    /// index lists them.
    fn moved_files(&self, moves: &[(String, String)]) -> Result<HashMap<String, String>> {
        let index = self.index();
        let mut out = HashMap::new();
        for (from, to) in moves {
            for old in index.files_under(from)? {
                let new = format!("{to}{}", &old[from.len()..]);
                out.insert(old, new);
            }
        }
        Ok(out)
    }

    /// What every link key in these notes resolves to.
    fn link_targets<'a>(
        &self,
        notes: impl IntoIterator<Item = &'a String>,
    ) -> Result<HashMap<String, String>> {
        let index = self.index();
        let mut out = HashMap::new();
        for rel in notes {
            out.extend(index.resolved_links(rel)?);
        }
        Ok(out)
    }

    /// Rewrite the note that was at `old` and is at `now`, gated on the etag it is read with.
    /// `Ok(false)` when it turned out to link nowhere near what moved.
    fn rewrite_one(
        &self,
        old: &str,
        now: &str,
        targets: &HashMap<String, String>,
        files: &HashMap<String, String>,
    ) -> Result<bool> {
        let path = self.resolve(now)?;
        let (text, etag) = fs::read_note(&path)?;
        let Some(rewritten) = markdown::rewrite_moved(&text, old, now, targets, files) else {
            return Ok(false);
        };
        fs::write_note(&path, &rewritten, Some(etag))?;
        self.post(Msg::Update {
            rel: now.to_string(),
            own: true,
        });
        Ok(true)
    }

    /// Replace every match of `re` in every file whose indexed body has one — the files
    /// [`grep`](Self::grep) lists and counts under the same `include_ignored`, notes or not.
    ///
    /// `literal` takes `$1` in `replacement` as two characters rather than a capture group, which
    /// is what the sidebar's non-regex modes mean. Same shape as [`rename`](Self::rename): a file
    /// that could not be written is reported rather than fatal, because a vault where most of the
    /// replacements landed is a real outcome the user has to be told about.
    ///
    /// It reads, substitutes and fsyncs one file at a time on the calling thread, which costs
    /// far more than a main loop can spend: 1.9 s across 245 notes and 35 s across 3.3k of them,
    /// measured on the 3.6k-note generated vault. Callers with a UI run it on a worker thread —
    /// the desktop app does, and the handle is `Send + Sync` so a binding can too. It is bounded
    /// by [`crate::vault::REPLACE_BOUND`], which is what a remote caller waits for: without it the
    /// caller gave up after ten seconds while the host went on rewriting.
    ///
    /// ponytail: no progress callback. The one caller shows an indeterminate bar, and a fraction
    /// nothing renders would be machinery for its own sake. Nor is it undoable — see NOTEPAD.
    pub fn replace_all(
        &self,
        re: &Regex,
        replacement: &str,
        literal: bool,
        include_ignored: bool,
    ) -> Result<ReplaceReport> {
        self.replace_within(
            re,
            replacement,
            literal,
            include_ignored,
            crate::vault::REPLACE_BOUND,
        )
    }

    /// [`replace_all`](Self::replace_all) under a budget, which is what makes the bound a remote
    /// caller waits for a promise rather than a guess.
    ///
    /// It stops *between* files, never inside one: each is read, substituted and written whole,
    /// so the vault is consistent wherever it stops, and the files it never reached are listed as
    /// not written rather than passed over silently. Running it again finishes them — the pattern
    /// still matches exactly those.
    fn replace_within(
        &self,
        re: &Regex,
        replacement: &str,
        literal: bool,
        include_ignored: bool,
        budget: std::time::Duration,
    ) -> Result<ReplaceReport> {
        let deadline = std::time::Instant::now() + budget;
        let mut report = ReplaceReport::default();
        // Collected before the first write: the guard must not still be held while files are
        // rewritten, and the worker reindexes them as they land.
        for rel in self.searcher().grep_paths(re, include_ignored)? {
            if std::time::Instant::now() >= deadline {
                report
                    .failed
                    .push((rel, format!("the rewrite stopped after {budget:?}")));
                continue;
            }
            match self.replace_one(&rel, re, replacement, literal) {
                Ok(0) => {}
                Ok(n) => {
                    report.rewritten.push(rel);
                    report.matches += n;
                }
                Err(e) => {
                    tracing::warn!("replacing in {rel}: {e:#}");
                    report.failed.push((rel, format!("{e:#}")));
                }
            }
        }
        // The rewrites are on disk; the index is a worker batch behind them. The caller is the
        // Search pane, which asks its question again the moment this returns and asks it of the
        // index — so it returns once the index agrees rather than a moment before.
        self.settle_index();
        Ok(report)
    }

    /// How many matches this file lost. The count comes from the file rather than from the index,
    /// which may be a watcher debounce behind what is on disk.
    fn replace_one(
        &self,
        rel: &str,
        re: &Regex,
        replacement: &str,
        literal: bool,
    ) -> Result<usize> {
        let path = self.resolve(rel)?;
        let (text, etag) = fs::read_note(&path)?;
        let matches = re.find_iter(&text).count();
        if matches == 0 {
            return Ok(0);
        }
        let rewritten = match literal {
            true => re.replace_all(&text, search::NoExpand(replacement)),
            false => re.replace_all(&text, replacement),
        };
        fs::write_note(&path, &rewritten, Some(etag))?;
        self.post(Msg::Update {
            rel: rel.to_string(),
            own: true,
        });
        Ok(matches)
    }

    /// Keep theirs: the conflict copy's bytes replace the original.
    ///
    /// This is a force-write, not a gated one — the caller has already seen the diff and chosen.
    /// So that the choice stays undoable, what the original held is copied to a sibling
    /// `*.sync-conflict-<timestamp>-accent.md` first: a name the vault keeps out of the note
    /// index and the resolve dialog offers back. The copy that won stays on disk too, because
    /// deleting it belongs in the system trash. Keeping mine needs no call here at all, it is
    /// just trashing the copy.
    pub fn adopt_conflict(&self, original: &str, conflict: &str) -> Result<Etag> {
        let (text, _) = fs::read_note(&self.resolve(conflict)?)
            .with_context(|| format!("reading {conflict}"))?;
        let target = self.resolve(original)?;
        let keep = target.with_file_name(accent_conflict_name(
            basename(original),
            chrono::Local::now().naive_local(),
        ));
        // ponytail: a second adopt within the same second overwrites the first copy, because the
        // name only carries whole seconds. Add a counter suffix if that ever costs anyone a note.
        std::fs::copy(&target, &keep)
            .with_context(|| format!("keeping {original} as {}", keep.display()))?;
        let etag = Etag::of(&target).with_context(|| format!("stat {original}"))?;
        let written = fs::write_note(&target, &text, Some(etag))
            .with_context(|| format!("writing {original}"))?;
        self.post(Msg::Update {
            rel: original.to_string(),
            own: true,
        });
        Ok(written)
    }

    /// Side-by-side rows for the conflict UI: mine on the left, theirs on the right.
    pub fn conflict_diff(&self, original: &str, conflict: &str) -> Result<Vec<DiffLine>> {
        let (mine, _) = fs::read_note(&self.resolve(original)?)
            .with_context(|| format!("reading {original}"))?;
        let (theirs, _) = fs::read_note(&self.resolve(conflict)?)
            .with_context(|| format!("reading {conflict}"))?;
        Ok(diff::lines(&mine, &theirs))
    }

    /// Where a template says its notes go today, or `None` when it does not say.
    ///
    /// The `accent-target:` directive goes through the same renderer the body does, so
    /// `Daily/{{date:%Y-%m-%d}}.md` is today's daily note. `{{title}}` in a target is the
    /// template's own name: there is no note yet to take one from.
    pub fn template_target(&self, template: &str) -> Result<Option<String>> {
        let Some(target) = template::parse(&self.read_template(template)?).target else {
            return Ok(None);
        };
        let (rel, _) =
            template::render(&target, &stem(template), chrono::Local::now().naive_local());
        Ok(Some(with_md(&rel)))
    }

    /// Create the note a template names, or open the one it already made.
    ///
    /// Invoked twice on the same day, a dated target hands back the note it made the first time,
    /// byte for byte: that is what makes a daily note daily. `None` is a template that says
    /// nothing about where its notes go, which is the caller's cue that it needs a name.
    pub fn note_from_template(&self, template: &str) -> Result<Option<(String, Vec<usize>)>> {
        let Some(rel) = self.template_target(template)? else {
            return Ok(None);
        };
        if self.resolve(&rel)?.exists() {
            return Ok(Some((rel, Vec::new())));
        }
        self.create_note(&rel, Some(template)).map(Some)
    }

    /// The markdown files directly inside the configured templates directory.
    pub fn templates(&self) -> Result<Vec<String>> {
        // The config lock is taken before the index lock, never inside it.
        let dir = self.config().templates_dir;
        Ok(self
            .index()
            .list_files(&dir)?
            .into_iter()
            .filter(|f| f.kind == FileKind::Markdown)
            .map(|f| f.rel_path)
            .collect())
    }

    /// The templates whose `accent-target:` says where their notes go.
    pub fn template_targets(&self) -> Result<Vec<String>> {
        Ok(self
            .templates()?
            .into_iter()
            .filter(|t| matches!(self.template_target(t), Ok(Some(_))))
            .collect())
    }
}

/// Source files a language server may be asked to follow when they move: what rust-analyzer and
/// typescript-language-server answer `willRename` for. A moved one no running server was asked
/// about is reported as unchecked.
const IMPORTING: &[&str] = &["rs", "ts", "tsx", "js", "jsx", "mjs", "cjs", "mts", "cts"];

/// Whether `rel` is a source file other files import by its path.
fn imports_by_path(rel: &str) -> bool {
    std::path::Path::new(rel)
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| IMPORTING.contains(&ext))
}

/// Whether `rel` is `dir` or inside it.
fn is_under(rel: &str, dir: &str) -> bool {
    rel.strip_prefix(dir)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// `cp -r`: `std::fs` copies one file, and a pasted folder is the one caller that needs the rest.
/// Follows a symlink rather than recreating it, which is what copying its contents means.
fn copy_tree(from: &std::path::Path, to: &std::path::Path) -> io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let (src, dest) = (entry.path(), to.join(entry.file_name()));
        match entry.file_type()?.is_dir() {
            true => copy_tree(&src, &dest)?,
            false => {
                std::fs::copy(&src, &dest)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::tests::*;
    use crate::{Etag, Event, Op, Options, VaultConfig, fs};

    /// `a.md` links to `B.md`; renaming it to `C.md` must move the link with it.
    fn linked_vault() -> Fixture {
        let f = Fixture::open(VaultConfig::default());
        f.write("a.md", "see [[B]] for details\n");
        f.write("B.md", "the target\n");
        f.vault.rescan().unwrap();
        assert!(f.wait(|e| matches!(e, Event::Reconciled(_))).is_some());
        f
    }

    /// One move, the shape a rename and a single dragged row have.
    fn one(from: &str, to: &str) -> Vec<(String, String)> {
        vec![(from.to_string(), to.to_string())]
    }

    /// A vault holding these notes, indexed.
    fn vault_of(files: &[(&str, &str)]) -> Fixture {
        let f = Fixture::open(VaultConfig::default());
        for (rel, text) in files {
            f.write(rel, text);
        }
        f.vault.rescan().unwrap();
        assert!(f.wait(|e| matches!(e, Event::Reconciled(_))).is_some());
        f
    }

    #[test]
    fn rename_rewrites_wikilinks_and_backlinks_follow() {
        let f = linked_vault();

        let plan = f.vault.plan_moves(&one("B.md", "C.md")).unwrap();
        assert_eq!(plan.rewrites, ["a.md"]);

        let report = f.vault.rename(&plan, true).unwrap();
        assert_eq!(report.moved, one("B.md", "C.md"));
        assert_eq!(report.rewritten, ["a.md"]);
        assert!(report.failed.is_empty());
        assert_eq!(f.read("a.md"), "see [[C]] for details\n");

        assert!(poll_until(
            || f.vault
                .backlinks("C.md")
                .unwrap()
                .iter()
                .any(|b| b.src_rel_path == "a.md"),
            BUDGET
        ));
    }

    #[test]
    fn replace_all_rewrites_every_match_and_reindexes() {
        let f = Fixture::open(VaultConfig::default());
        f.write("a.md", "colour and colour\n");
        f.write("sub/b.md", "Colour\n");
        f.write("c.md", "nothing here\n");
        f.vault.rescan().unwrap();
        assert!(f.wait(|e| matches!(e, Event::Reconciled(_))).is_some());

        let plain = Options::default();
        assert_eq!(f.vault.grep("colour", plain, 10, false).unwrap().1, 3);

        let report = f
            .vault
            .replace_all("colour", plain, "color", true, false)
            .unwrap();
        assert_eq!(report.rewritten, ["a.md", "sub/b.md"]);
        assert_eq!(report.matches, 3);
        assert!(report.failed.is_empty());
        assert_eq!(f.read("a.md"), "color and color\n");
        assert_eq!(f.read("sub/b.md"), "color\n", "case-insensitive by default");
        assert_eq!(f.read("c.md"), "nothing here\n");

        // Not polled: the call returns once the worker has taken the rewrites in, because the
        // Search pane asks its question again the moment it does.
        assert_eq!(
            f.vault.grep("colour", plain, 10, false).unwrap().1,
            0,
            "the rewrites must be in the index by the time replace_all returns"
        );
    }

    /// The host's own bound, which is what a remote caller's wait is derived from: the rewrite
    /// stops between notes and says which it never reached, rather than running past the moment
    /// the caller has stopped waiting. Driven with no budget at all, so it stops before the first.
    #[test]
    fn a_rewrite_out_of_time_stops_between_notes_and_reports_the_rest() {
        let f = Fixture::open(VaultConfig::default());
        f.write("a.md", "colour\n");
        f.write("b.md", "colour\n");
        f.vault.rescan().unwrap();
        assert!(f.wait(|e| matches!(e, Event::Reconciled(_))).is_some());

        let crate::vault::Backend::Local(local) = &f.vault.backend else {
            panic!("the fixture opens a local vault");
        };
        let re = accent_core::search::pattern("colour", Options::default()).unwrap();
        let report = local
            .replace_within(&re, "color", true, false, std::time::Duration::ZERO)
            .unwrap();

        assert!(report.rewritten.is_empty());
        assert_eq!(report.matches, 0);
        assert_eq!(
            report.failed.iter().map(|(rel, _)| rel).collect::<Vec<_>>(),
            ["a.md", "b.md"],
            "every note it did not reach is named: {report:?}"
        );
        assert_eq!(f.read("a.md"), "colour\n", "no note is left half written");
        // The pattern still matches them, so asking again finishes the job.
        assert_eq!(
            f.vault
                .grep("colour", Options::default(), 10, false)
                .unwrap()
                .1,
            2
        );
    }

    /// The Search pane's "Replace All (N)": N is what the rewrite touches, and it touches every
    /// file the list shows from the index, a source file as much as a note.
    #[test]
    fn the_replace_count_is_every_indexed_match_and_the_rewrite_touches_them_all() {
        let f = Fixture::open(VaultConfig::default());
        f.write("a.md", "zorblat once\n");
        f.write("tool.py", "zorblat\nzorblat again\n");
        f.vault.rescan().unwrap();
        assert!(f.wait(|e| matches!(e, Event::Reconciled(_))).is_some());

        let plain = Options::default();
        let (hits, total) = f.vault.grep("zorblat", plain, 10, false).unwrap();
        assert_eq!(hits.len(), 3, "the list shows both files: {hits:?}");
        assert_eq!(total, 3, "every listed match is a rewrite");
        // And that is exactly what the rewrite then visits.
        let report = f
            .vault
            .replace_all("zorblat", plain, "zzz", true, false)
            .unwrap();
        assert_eq!(report.rewritten, ["a.md", "tool.py"]);
        assert_eq!(report.matches, 3);
        assert_eq!(f.read("tool.py"), "zzz\nzzz again\n");
    }

    /// Only regex mode expands `$1`; a literal replacement is written as typed.
    #[test]
    fn replace_expands_groups_only_outside_literal_mode() {
        let f = Fixture::open(VaultConfig::default());
        f.write("a.md", "hello world\n");
        f.vault.rescan().unwrap();
        assert!(f.wait(|e| matches!(e, Event::Reconciled(_))).is_some());

        let opts = Options {
            regex: true,
            ..Options::default()
        };
        f.vault
            .replace_all(r"hello (\w+)", opts, "bye $1", false, false)
            .unwrap();
        assert_eq!(f.read("a.md"), "bye world\n");
    }

    #[test]
    fn rename_without_rewrite_leaves_links_alone() {
        let f = linked_vault();

        let plan = f.vault.plan_moves(&one("B.md", "C.md")).unwrap();
        let report = f.vault.rename(&plan, false).unwrap();

        assert!(report.rewritten.is_empty());
        assert_eq!(f.read("a.md"), "see [[B]] for details\n");
        assert!(f.vault.root().join("C.md").exists());
    }

    /// A note that keeps its name keeps its bare links; only the one spelling its path is stale.
    #[test]
    fn a_pure_move_rewrites_only_path_links() {
        let f = vault_of(&[
            ("Dir/B.md", "the target\n"),
            ("a.md", "[[B]]\n"),
            ("p.md", "[[Dir/B]]\n"),
        ]);
        let plan = f.vault.plan_moves(&one("Dir/B.md", "sub/B.md")).unwrap();
        assert_eq!(plan.rewrites, ["p.md"]);

        std::fs::create_dir(f.vault.root().join("sub")).unwrap();
        assert_eq!(f.vault.rename(&plan, true).unwrap().not_moved, None);
        assert_eq!(f.read("p.md"), "[[sub/B]]\n");
        assert_eq!(f.read("a.md"), "[[B]]\n");
    }

    /// A folder takes every file in it along: links into it are rewritten, and so are the
    /// relative links its own notes hold.
    #[test]
    fn a_folder_move_rewrites_links_into_it_and_out_of_it() {
        let f = vault_of(&[
            ("a/b/note.md", "[i](../../img/x.png) ![[x.png]]\n"),
            ("img/x.png", "png"),
            ("Ref.md", "[[a/b/note]] [t](a/b/note.md) [[note]]\n"),
        ]);
        let plan = f.vault.plan_moves(&one("a/b", "c")).unwrap();
        assert_eq!(plan.rewrites, ["Ref.md", "a/b/note.md"]);

        let report = f.vault.rename(&plan, true).unwrap();
        assert_eq!(report.rewritten, ["Ref.md", "c/note.md"]);
        assert_eq!(f.read("Ref.md"), "[[c/note]] [t](c/note.md) [[note]]\n");
        assert_eq!(f.read("c/note.md"), "[i](../img/x.png) ![[x.png]]\n");
    }

    /// Two notes moved together that link each other, and an image renamed with them: every
    /// note is written once, with what all the moves did.
    #[test]
    fn a_batch_rewrites_each_note_once() {
        let f = vault_of(&[
            ("p/x.md", "[y](../q/y.md) ![](../img/a.png)\n"),
            ("q/y.md", "[x](../p/x.md) ![[a.png]]\n"),
            ("img/a.png", "png"),
        ]);
        let moves = vec![
            ("p/x.md".to_string(), "r/x.md".to_string()),
            ("q/y.md".to_string(), "r/y.md".to_string()),
            ("img/a.png".to_string(), "img/b.png".to_string()),
        ];
        std::fs::create_dir(f.vault.root().join("r")).unwrap();
        let plan = f.vault.plan_moves(&moves).unwrap();
        assert_eq!(plan.rewrites, ["p/x.md", "q/y.md"]);

        let report = f.vault.rename(&plan, true).unwrap();
        assert_eq!(report.moved, moves);
        assert_eq!(report.rewritten, ["r/x.md", "r/y.md"]);
        assert!(report.failed.is_empty(), "{:?}", report.failed);
        assert_eq!(f.read("r/x.md"), "[y](y.md) ![](../img/b.png)\n");
        assert_eq!(f.read("r/y.md"), "[x](x.md) ![[b.png]]\n");
    }

    /// A move that fails stops the batch, and links to what did not move stay as they were.
    #[test]
    fn a_failed_move_stops_the_batch() {
        let f = vault_of(&[
            ("p/x.md", "x\n"),
            ("q/y.md", "y\n"),
            ("r/y.md", "in the way\n"),
            ("Ref.md", "[[p/x]] [[q/y]]\n"),
        ]);
        let moves = vec![
            ("p/x.md".to_string(), "r/x.md".to_string()),
            ("q/y.md".to_string(), "r/y.md".to_string()),
        ];
        let plan = f.vault.plan_moves(&moves).unwrap();
        let report = f.vault.rename(&plan, true).unwrap();
        assert_eq!(report.moved, moves[..1]);
        assert_eq!(
            report.not_moved.map(|(from, _)| from).as_deref(),
            Some("q/y.md")
        );
        assert_eq!(f.read("Ref.md"), "[[r/x]] [[q/y]]\n");
    }

    #[test]
    fn create_note_from_a_template_expands_it_and_returns_the_cursor() {
        let f = Fixture::open(VaultConfig::default());
        f.write("Templates/Note.md", "# {{title}}\n\n{{cursor}}body\n");

        let (rel, cursor) = f
            .vault
            .create_note("Inbox/Weekly sync.md", Some("Templates/Note.md"))
            .unwrap();

        assert_eq!(rel, "Inbox/Weekly sync.md");
        let text = f.read(&rel);
        assert_eq!(text, "# Weekly sync\n\nbody\n");
        assert_eq!(&text[cursor[0]..], "body\n");

        // Once indexed, the same file is what the "new note from template" dialog offers.
        assert!(poll_until(
            || f.vault.templates().unwrap() == ["Templates/Note.md"],
            BUDGET
        ));
    }

    /// The daily note, now that it is only a template with a dated target: created the first
    /// time it is asked for and opened untouched every time after, which is the whole behaviour.
    #[test]
    fn a_dated_target_is_created_once_and_reused() {
        let f = Fixture::open(VaultConfig::default());
        f.write(
            "Templates/Daily.md",
            "---\naccent-target: Daily/{{date:%Y-%m-%d}}.md\n---\n\n# {{date}}\n\n{{cursor}}",
        );

        let (rel, cursor) = f.vault.note_from_template("Daily.md").unwrap().unwrap();
        assert!(rel.starts_with("Daily/") && rel.ends_with(".md"), "{rel}");
        assert!(!cursor.is_empty(), "a fresh note places the caret");
        let text = f.read(&rel);
        assert!(
            !text.contains("accent-target"),
            "the directive is accent's: {text}"
        );

        let (again, cursor) = f.vault.note_from_template("Daily.md").unwrap().unwrap();
        assert_eq!(again, rel);
        assert!(
            cursor.is_empty(),
            "an existing note is opened, not rewritten"
        );
        assert_eq!(f.read(&rel), text);
    }

    #[test]
    fn a_template_that_names_no_target_creates_nothing() {
        let f = Fixture::open(VaultConfig::default());
        f.write("Templates/Meeting.md", "# {{title}}\n\nbody\n");

        assert_eq!(f.vault.template_target("Meeting.md").unwrap(), None);
        assert_eq!(f.vault.note_from_template("Meeting.md").unwrap(), None);
    }

    /// What New from Template lists: only the templates that say where their notes go.
    #[test]
    fn template_targets_are_the_templates_that_name_one() {
        let f = Fixture::open(VaultConfig::default());
        f.write("Templates/Meeting.md", "# {{title}}\n");
        f.write(
            "Templates/Weekly.md",
            "---\naccent-target: Logs/{{title}}\n---\n\nx\n",
        );
        assert!(poll_until(
            || f.vault.template_targets().unwrap() == ["Templates/Weekly.md"],
            BUDGET
        ));
    }

    #[test]
    fn a_target_is_rendered_and_made_markdown() {
        let f = Fixture::open(VaultConfig::default());
        f.write(
            "Templates/Weekly.md",
            "---\naccent-target: Logs/{{title}}\n---\n\nx\n",
        );

        // `{{title}}` is the template's own stem, and a target without `.md` is still a note.
        assert_eq!(
            f.vault.template_target("Weekly.md").unwrap().as_deref(),
            Some("Logs/Weekly.md")
        );
    }

    #[test]
    fn adopt_conflict_replaces_the_original_and_leaves_the_copy() {
        let f = Fixture::open(VaultConfig::default());
        f.write("Note.md", "mine\n");
        f.write(CONFLICT, "theirs\n");

        let etag = f.vault.adopt_conflict("Note.md", CONFLICT).unwrap();

        assert_eq!(f.read("Note.md"), "theirs\n");
        assert_eq!(etag, Etag::of(&f.vault.root().join("Note.md")).unwrap());
        assert!(
            f.vault.root().join(CONFLICT).exists(),
            "deleting the copy is the UI's job, through the trash"
        );
    }

    #[test]
    fn conflict_diff_reports_the_changed_line() {
        let f = Fixture::open(VaultConfig::default());
        f.write("Note.md", "alpha\nbravo\n");
        f.write(CONFLICT, "alpha\nbravo two\n");

        let d = f.vault.conflict_diff("Note.md", CONFLICT).unwrap();

        assert_eq!(
            d.iter()
                .filter(|l| l.op != Op::Equal)
                .map(|l| (l.op, l.text.as_str()))
                .collect::<Vec<_>>(),
            [(Op::Delete, "bravo"), (Op::Insert, "bravo two")]
        );
    }

    /// `[[Old]]` here belongs to a different note; renaming `Dir/Old.md` must leave it alone.
    #[test]
    fn rename_leaves_a_link_that_resolves_elsewhere_alone() {
        let f = Fixture::open(VaultConfig::default());
        f.write("Dir/Old.md", "the deep one\n");
        f.write("Old.md", "a different note\n");
        f.write("Ref.md", "deep: [[Dir/Old]]\nshallow: [[Old]]\n");
        f.vault.rescan().unwrap();
        assert!(f.wait(|e| matches!(e, Event::Reconciled(_))).is_some());

        let plan = f
            .vault
            .plan_moves(&one("Dir/Old.md", "Dir/Renamed.md"))
            .unwrap();
        assert_eq!(plan.rewrites, ["Ref.md"]);
        let report = f.vault.rename(&plan, true).unwrap();

        assert_eq!(report.rewritten, ["Ref.md"]);
        assert_eq!(
            f.read("Ref.md"),
            "deep: [[Dir/Renamed]]\nshallow: [[Old]]\n"
        );
    }

    /// The same two links, with nothing else called `Old`: now both do point at the renamed
    /// note, and both have to move with it.
    #[test]
    fn rename_rewrites_every_link_that_resolves_to_it() {
        let f = Fixture::open(VaultConfig::default());
        f.write("Dir/Old.md", "the deep one\n");
        f.write("Ref.md", "deep: [[Dir/Old]]\nshallow: [[Old]]\n");
        f.vault.rescan().unwrap();
        assert!(f.wait(|e| matches!(e, Event::Reconciled(_))).is_some());

        let plan = f
            .vault
            .plan_moves(&one("Dir/Old.md", "Dir/Renamed.md"))
            .unwrap();
        let report = f.vault.rename(&plan, true).unwrap();

        assert_eq!(report.rewritten, ["Ref.md"]);
        assert_eq!(
            f.read("Ref.md"),
            "deep: [[Dir/Renamed]]\nshallow: [[Renamed]]\n"
        );
    }

    /// Moving a folder takes its subtree with it in the index, or every note under it drops out
    /// of search, backlinks and the switcher until the next restart.
    #[test]
    fn renaming_a_directory_keeps_its_subtree_in_the_index() {
        let f = Fixture::open(VaultConfig::default());
        f.write("a/b/note.md", "kumquat harvest\n");
        f.write("a/b/sub/x.md", "deep\n");
        f.vault.rescan().unwrap();
        assert!(f.wait(|e| matches!(e, Event::Reconciled(_))).is_some());
        assert_eq!(
            f.vault.file_paths(false).unwrap(),
            ["a/b/note.md", "a/b/sub/x.md"]
        );

        let plan = f.vault.plan_moves(&one("a/b", "a/c")).unwrap();
        f.vault.rename(&plan, true).unwrap();

        assert!(
            poll_until(
                || f.vault.file_paths(false).unwrap() == ["a/c/note.md", "a/c/sub/x.md"],
                BUDGET
            ),
            "the subtree is gone from the index: {:?}",
            f.vault.file_paths(false).unwrap()
        );
        assert_eq!(
            names(&f.vault.list_dir("a/c").unwrap()),
            ["a/c/sub", "a/c/note.md"]
        );
        assert!(!f.vault.search("kumquat", 10, false).unwrap().is_empty());
    }

    /// "Keep theirs" overwrites the user's own version, so what it replaced has to survive
    /// somewhere: a conflict copy of its own, which the resolve dialog can undo from.
    #[test]
    fn adopt_conflict_keeps_the_replaced_version_as_a_conflict_copy() {
        let f = Fixture::open(VaultConfig::default());
        f.write("Note.md", "mine\n");
        f.write(CONFLICT, "theirs\n");

        f.vault.adopt_conflict("Note.md", CONFLICT).unwrap();

        assert_eq!(f.read("Note.md"), "theirs\n");
        let backups: Vec<String> = std::fs::read_dir(f.vault.root())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| fs::is_sync_conflict(n) && n != CONFLICT)
            .collect();
        assert_eq!(backups.len(), 1, "{backups:?}");
        assert_eq!(
            fs::conflict_original(&backups[0]).as_deref(),
            Some("Note.md")
        );
        assert_eq!(f.read(&backups[0]), "mine\n");
    }
}
