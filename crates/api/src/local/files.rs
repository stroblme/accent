//! What [`Local`](super::Local) does to the files themselves: read, write, delete, the templates
//! a new note is made from, the rename that carries a note's links with it, and the conflict
//! copies a sync leaves behind.

use std::io;

use anyhow::{Context, Result};

use accent_core::path::{basename, stem};
use accent_core::{diff, markdown, search, template};

use super::{Local, Msg};
use crate::paths::{accent_conflict_name, stem_key, with_md};
use crate::{
    DiffLine, Etag, FileKind, Regex, RenamePlan, RenameReport, ReplaceReport, SaveError, fs,
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

    /// What a rename would touch, so the UI can show it before anything is written.
    ///
    /// `rewrites` is empty for a directory, and for a pure move: a wikilink resolves by basename,
    /// so a note that keeps its name keeps its links wherever it lands.
    pub fn plan_rename(&self, from: &str, to: &str) -> Result<RenamePlan> {
        let renamed = stem_key(from) != stem_key(to);
        let mut rewrites = Vec::new();
        if renamed && !self.resolve(from)?.is_dir() {
            rewrites = self
                .index()
                .backlinks(from)?
                .into_iter()
                .map(|b| b.src_rel_path)
                .collect();
            rewrites.sort();
            rewrites.dedup();
        }
        Ok(RenamePlan {
            from: from.to_string(),
            to: to.to_string(),
            rewrites,
        })
    }

    /// Apply a [`RenamePlan`]. The move happens first and is the only step that may fail the
    /// call: a note whose links could not be rewritten is reported instead, because a
    /// half-renamed vault is worse than a fully renamed one with a couple of stale links.
    pub fn rename(&self, plan: &RenamePlan, rewrite_links: bool) -> Result<RenameReport> {
        // Which spellings of the old name really mean this note is a question only the index can
        // answer, and only while it still describes the vault as it was: ask before the move.
        let targets = if rewrite_links {
            self.targets_of(&plan.from)?
        } else {
            Vec::new()
        };
        fs::rename(&self.resolve(&plan.from)?, &self.resolve(&plan.to)?)
            .with_context(|| format!("renaming {} to {}", plan.from, plan.to))?;
        // Both ends, so the index and the tree do not wait for inotify.
        self.post(Msg::Update {
            rel: plan.from.clone(),
            own: true,
        });
        self.post(Msg::Update {
            rel: plan.to.clone(),
            own: true,
        });

        let mut report = RenameReport::default();
        if rewrite_links {
            for rel in &plan.rewrites {
                match self.rewrite_one(rel, &targets, &plan.to) {
                    Ok(true) => report.rewritten.push(rel.clone()),
                    Ok(false) => {}
                    Err(e) => {
                        tracing::warn!("rewriting links in {rel}: {e:#}");
                        report.failed.push((rel.clone(), format!("{e:#}")));
                    }
                }
            }
        }
        Ok(report)
    }

    /// The ways of writing `rel` that really resolve to it. `[[Old]]` in a note that also
    /// links `[[Dir/Old]]` may well be a different note's, and renaming this one must leave that
    /// link exactly as its author wrote it.
    fn targets_of(&self, rel: &str) -> Result<Vec<String>> {
        let index = self.index();
        let mut out = Vec::new();
        for key in markdown::path_keys(rel) {
            if index.resolve_target(&key)?.as_deref() == Some(rel) {
                out.push(key);
            }
        }
        Ok(out)
    }

    /// `Ok(false)` when the note turned out to link nowhere near the renamed file.
    fn rewrite_one(&self, rel: &str, targets: &[String], to: &str) -> Result<bool> {
        let path = self.resolve(rel)?;
        let (text, etag) = fs::read_note(&path)?;
        let Some(rewritten) = markdown::rewrite_targets(&text, targets, to) else {
            return Ok(false);
        };
        fs::write_note(&path, &rewritten, Some(etag))?;
        self.post(Msg::Update {
            rel: rel.to_string(),
            own: true,
        });
        Ok(true)
    }

    /// Replace every match of `re` in every note that has one.
    ///
    /// `literal` takes `$1` in `replacement` as two characters rather than a capture group, which
    /// is what the sidebar's non-regex modes mean. Same shape as [`rename`](Self::rename): a note
    /// that could not be written is reported rather than fatal, because a vault where most of the
    /// replacements landed is a real outcome the user has to be told about.
    ///
    /// It reads, substitutes and fsyncs one note at a time on the calling thread, which costs
    /// far more than a main loop can spend: 1.9 s across 245 notes and 35 s across 3.3k of them,
    /// measured on the 3.6k-note generated vault. Callers with a UI run it on a worker thread —
    /// the desktop app does, and the handle is `Send + Sync` so a binding can too.
    ///
    /// ponytail: no progress callback. The one caller shows an indeterminate bar, and a fraction
    /// nothing renders would be machinery for its own sake. Nor is it undoable — see NOTEPAD.
    pub fn replace_all(
        &self,
        re: &Regex,
        replacement: &str,
        literal: bool,
    ) -> Result<ReplaceReport> {
        let mut report = ReplaceReport::default();
        // Collected before the first write: the guard must not still be held while notes are
        // rewritten, and the worker reindexes them as they land.
        for rel in self.searcher().grep_paths(re)? {
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
        Ok(report)
    }

    /// How many matches this note lost. The count comes from the file rather than from the index,
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

    #[test]
    fn rename_rewrites_wikilinks_and_backlinks_follow() {
        let f = linked_vault();

        let plan = f.vault.plan_rename("B.md", "C.md").unwrap();
        assert_eq!(plan.rewrites, ["a.md"]);

        let report = f.vault.rename(&plan, true).unwrap();
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

        let report = f.vault.replace_all("colour", plain, "color", true).unwrap();
        assert_eq!(report.rewritten, ["a.md", "sub/b.md"]);
        assert_eq!(report.matches, 3);
        assert!(report.failed.is_empty());
        assert_eq!(f.read("a.md"), "color and color\n");
        assert_eq!(f.read("sub/b.md"), "color\n", "case-insensitive by default");
        assert_eq!(f.read("c.md"), "nothing here\n");

        assert!(
            poll_until(
                || f.vault.grep("colour", plain, 10, false).unwrap().1 == 0,
                BUDGET
            ),
            "the rewrites must reach the index without a rescan"
        );
    }

    /// The Search pane's "Replace All (N)": N is what the rewrite touches, not what the list
    /// shows. A source file's matches are rows without being edits.
    #[test]
    fn the_replace_count_is_notes_while_the_rows_are_every_text_file() {
        let f = Fixture::open(VaultConfig::default());
        f.write("a.md", "zorblat once\n");
        f.write("tool.py", "zorblat\nzorblat again\n");
        f.vault.rescan().unwrap();
        assert!(f.wait(|e| matches!(e, Event::Reconciled(_))).is_some());

        let plain = Options::default();
        let (hits, total) = f.vault.grep("zorblat", plain, 10, false).unwrap();
        assert_eq!(hits.len(), 3, "the list shows both files: {hits:?}");
        assert_eq!(total, 1, "only the note's match is a rewrite");
        // And that is exactly what the rewrite then visits.
        let report = f.vault.replace_all("zorblat", plain, "zzz", true).unwrap();
        assert_eq!(report.rewritten, vec!["a.md".to_string()]);
        assert_eq!(report.matches, 1);
        assert_eq!(f.read("tool.py"), "zorblat\nzorblat again\n");
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
            .replace_all(r"hello (\w+)", opts, "bye $1", false)
            .unwrap();
        assert_eq!(f.read("a.md"), "bye world\n");
    }

    #[test]
    fn rename_without_rewrite_leaves_links_alone() {
        let f = linked_vault();

        let plan = f.vault.plan_rename("B.md", "C.md").unwrap();
        let report = f.vault.rename(&plan, false).unwrap();

        assert!(report.rewritten.is_empty());
        assert_eq!(f.read("a.md"), "see [[B]] for details\n");
        assert!(f.vault.root().join("C.md").exists());
    }

    #[test]
    fn a_pure_move_rewrites_nothing() {
        let f = linked_vault();
        assert!(
            f.vault
                .plan_rename("B.md", "sub/B.md")
                .unwrap()
                .rewrites
                .is_empty(),
            "a note that keeps its name keeps its links"
        );
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

        let plan = f.vault.plan_rename("Dir/Old.md", "Dir/Renamed.md").unwrap();
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

        let plan = f.vault.plan_rename("Dir/Old.md", "Dir/Renamed.md").unwrap();
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
            f.vault.note_paths().unwrap(),
            ["a/b/note.md", "a/b/sub/x.md"]
        );

        let plan = f.vault.plan_rename("a/b", "a/c").unwrap();
        f.vault.rename(&plan, true).unwrap();

        assert!(
            poll_until(
                || f.vault.note_paths().unwrap() == ["a/c/note.md", "a/c/sub/x.md"],
                BUDGET
            ),
            "the subtree is gone from the index: {:?}",
            f.vault.note_paths().unwrap()
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
