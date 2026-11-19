//! Link-preserving moves. A move rewrites the links into and out of what moved and applies the
//! language servers' import edits, a Save As copy keeps its relative links, and the notes linking
//! into a PDF follow its pages through a page edit.

use std::collections::{BTreeSet, HashMap};

use accent_core::markdown;
use accent_core::note::Syntax;
use accent_core::path::is_under;

use super::{Local, Msg, applied};
use crate::{FileEdits, KeptLink, PageEdit, RenamePlan, RenameReport, RepageReport, Result, fs};

impl Local {
    /// Plan link and import rewrites before moving files, for UI confirmation.
    /// Dry-run candidate notes to omit unchanged bare links; include links to
    /// moved files and relative links in moved notes. Ask running language
    /// servers before the move because they may read disk. Settle preceding
    /// writes first so freshly saved links are included.
    pub fn plan_moves(&self, moves: &[(String, String)]) -> Result<RenamePlan> {
        self.settle_index();
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
                    Syntax::of(rel)
                        .rewrite_moved(&text, rel, now, &targets, &files)
                        .is_some()
                }
                Err(_) => true,
            })
            .collect();
        let kinds = moves
            .iter()
            .map(|(from, to)| Ok((from.clone(), to.clone(), self.resolve(from)?.is_dir())))
            .collect::<Result<Vec<_>>>()?;
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

    /// Apply [`RenamePlan`] in order, stopping moves at the first failure. With
    /// `update`, rewrite links for completed moves and apply import edits. Fail
    /// the call only before any move; report partial results thereafter.
    /// [`crate::vault::MOVE_BOUND`] limits caller waiting, not host work: after
    /// files move, interrupted link rewrites cannot be resumed.
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
                .and_then(|path| Ok(fs::rename(&path, &self.resolve(to)?)?))
            {
                report.not_moved = Some((from.clone(), e.to_string()));
                break;
            }
            // Both ends, so the index and the tree do not wait for inotify.
            self.post(Msg::Moved {
                from: from.clone(),
                to: to.clone(),
            });
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

    /// Code action edits to files no tab holds ([`crate::Vault::edit_files`]): each file on its
    /// own, a failure leaving the rest to be written.
    pub fn edit_files(&self, edits: &[FileEdits]) -> Result<Vec<(String, String)>> {
        Ok(edits
            .iter()
            .filter_map(|found| {
                let failed = self.edit_one(&found.rel, found).err()?;
                Some((found.rel.clone(), format!("{failed:#}")))
            })
            .collect())
    }

    /// Apply server edits at `now`, requiring the planned etag.
    fn edit_one(&self, now: &str, found: &FileEdits) -> Result<()> {
        let path = self.resolve_inside(now)?;
        let (text, _) = fs::read_note(&path)?;
        let text = applied(text, &found.edits)?;
        fs::write_note(&path, &text, Some(found.etag))?;
        self.post(Msg::Update {
            rel: now.to_string(),
            own: true,
        });
        Ok(())
    }

    /// Every file the moves take somewhere, old path to new: a folder's one by one, as the
    /// index lists them, and every second path to a file among them, which links name too.
    fn moved_files(&self, moves: &[(String, String)]) -> Result<HashMap<String, String>> {
        let index = self.index();
        let aliases: Vec<String> = index.aliases()?.into_iter().map(|(_, a)| a).collect();
        let mut out = HashMap::new();
        for (from, to) in moves {
            let aliased = aliases.iter().filter(|a| is_under(a, from)).cloned();
            for old in index.files_under(from)?.into_iter().chain(aliased) {
                let new = format!("{to}{}", &old[from.len()..]);
                out.insert(old, new);
            }
        }
        Ok(out)
    }

    /// The path that answers every link key in these notes, which a move rewrites the link to
    /// follow: a link written to a second path's name follows that path, not its file.
    fn link_targets<'a>(
        &self,
        notes: impl IntoIterator<Item = &'a String>,
    ) -> Result<HashMap<String, String>> {
        let index = self.index();
        let mut out = HashMap::new();
        for rel in notes {
            let links = index.resolved_links(rel)?.into_iter();
            out.extend(links.map(|(key, _, path)| (key, path)));
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
        let path = self.resolve_inside(now)?;
        let (text, etag) = fs::read_note(&path)?;
        let Some(rewritten) = Syntax::of(old).rewrite_moved(&text, old, now, targets, files) else {
            return Ok(false);
        };
        fs::write_note(&path, &rewritten, Some(etag))?;
        self.post(Msg::Update {
            rel: now.to_string(),
            own: true,
        });
        Ok(true)
    }

    /// Retarget relative links, reference definitions and HTML paths from
    /// `from` to a Save As copy at `to`. Preserve the original and return `None`
    /// when no paths change. Resolve caller text, including unsaved edits,
    /// rather than relying on stale indexed links.
    pub fn relink_copy(&self, from: &str, to: &str, text: &str) -> Result<Option<String>> {
        let targets = self.path_targets(from, text)?;
        Ok(Syntax::of(from).rewrite_moved(text, from, to, &targets, &HashMap::new()))
    }

    /// What every path `text` spells from the note at `rel` resolves to
    /// ([`Syntax::path_link_keys`]), asked of the index one by one.
    fn path_targets(&self, rel: &str, text: &str) -> Result<HashMap<String, String>> {
        let index = self.index();
        let mut out = HashMap::new();
        for key in Syntax::of(rel).path_link_keys(text, rel) {
            if let Some(file) = index.resolve_target(&key)? {
                out.insert(key, file);
            }
        }
        Ok(out)
    }

    /// Rewrite note links to PDF `rel` after `edit`, using the etag save gate.
    /// Include page jumps and selections; `keep` restores links retained for
    /// delete undo ([`markdown::repage_links`]). Report per-note failures because
    /// the PDF edit already happened. Settle preceding writes before querying
    /// backlinks; the caller must also wait for full indexing, as for renames.
    pub fn repage_links(
        &self,
        rel: &str,
        edit: PageEdit,
        keep: &[KeptLink],
    ) -> Result<RepageReport> {
        self.settle_index();
        let notes: BTreeSet<String> = self
            .index()
            .backlinks(rel)?
            .into_iter()
            .map(|b| b.src_rel_path)
            .collect();
        let mut report = RepageReport::default();
        for note in notes {
            let kept: Vec<(String, usize)> = keep
                .iter()
                .filter(|k| k.note == note)
                .map(|k| (k.anchor.clone(), k.nth))
                .collect();
            match self.repage_one(&note, rel, edit, &kept) {
                Ok(done) => {
                    report.moved += done.moved;
                    report
                        .left
                        .extend(done.left.into_iter().map(|(anchor, nth)| KeptLink {
                            note: note.clone(),
                            anchor,
                            nth,
                        }));
                    if done.text.is_some() {
                        report.rewritten.push(note);
                    }
                }
                Err(e) => {
                    tracing::warn!("following the pages of {rel} in {note}: {e:#}");
                    report.failed.push((note, format!("{e:#}")));
                }
            }
        }
        Ok(report)
    }

    /// [`repage_links`](Self::repage_links) for one note.
    fn repage_one(
        &self,
        note: &str,
        pdf: &str,
        edit: PageEdit,
        keep: &[(String, usize)],
    ) -> Result<markdown::Repaged> {
        let mut targets: HashMap<String, String> = (self.index().resolved_links(note)?)
            .into_iter()
            .map(|(key, file, _)| (key, file))
            .collect();
        let path = self.resolve_inside(note)?;
        let (text, etag) = fs::read_note(&path)?;
        // A reference definition or an HTML `href` is no link the index holds, and may spell a
        // path none of the note's links does.
        targets.extend(self.path_targets(note, &text)?);
        let done = markdown::repage_links(&text, note, pdf, &targets, edit, keep);
        if let Some(rewritten) = &done.text {
            fs::write_note(&path, rewritten, Some(etag))?;
            self.post(Msg::Update {
                rel: note.to_string(),
                own: true,
            });
        }
        Ok(done)
    }
}

/// Source types supported by server `willRename`, such as Rust and TypeScript.
/// Report moved files unchecked if no running server was asked.
const IMPORTING: &[&str] = &["rs", "ts", "tsx", "js", "jsx", "mjs", "cjs", "mts", "cts"];

/// Whether `rel` is a source file other files import by its path.
fn imports_by_path(rel: &str) -> bool {
    std::path::Path::new(rel)
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| IMPORTING.contains(&ext))
}
