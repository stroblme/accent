//! Index-backed language provider for notes. Links act as definitions, backlinks as references,
//! headings as symbols, and unresolved links as diagnostics. Pure transformations can be tested
//! without a vault.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use crate::Result;

use accent_core::index::Index;
use accent_core::markdown::{self, LinkKind};
use accent_core::note::{self, Syntax};
use accent_core::org;
use accent_core::path::{self, stem};

use super::{
    CodeAction, Completion, Completions, Diagnostic, Fold, Fut, Hover, Kind, Language, Location,
    Pos, Range, Severity, Signature, Support, Symbol, byte_of, range_of,
};
use crate::local::{Ignored, Listing};
use crate::{Event, Local, locked};

mod completion;
mod pure;
#[cfg(test)]
mod tests;

use pure::{Outline, checked_target, target_of};
pub(crate) use pure::{folds_of, placed, symbols_of};

/// Rows the popup offers before the user has to type more.
const COMPLETIONS: usize = 20;
/// How many of the files changed last an empty `[[` or `#` ranks first, after those the window
/// used: as many as Go to File lists.
const RECENT: usize = 50;
/// How much of a linked note a hover shows.
const HOVER_LINES: usize = 8;

/// Everything the index knows about the notes of one vault, asked the way a language server is.
pub(crate) struct Notes {
    root: PathBuf,
    /// A reader of its own, so a completion never queues behind the caller's connection.
    index: Mutex<Index>,
    /// The text as the editor has it, which is ahead of what is on disk and in the index.
    docs: Mutex<HashMap<String, String>>,
    events: Sender<Event>,
    /// What a link completion ranks, kept between keystrokes. See [`Notes::corpus`].
    corpora: Mutex<Corpora>,
    /// The tags suggested for each note, kept until the index changes. See [`Notes::tags_for`].
    suggested: Mutex<Suggested>,
    /// The files in the folders git ignores, which a link finds when the index does not.
    ignored: Arc<Ignored>,
    /// The files the window used last, the latest first: what a completion ranks first.
    used: Arc<Mutex<Vec<String>>>,
}

/// Each note's suggested tags, as the index held them at its [`Index::data_version`].
type Suggested = (i64, HashMap<String, Arc<Vec<String>>>);

/// Completion paths: the first `named` entries are files, followed by ignored
/// notes (`ignored`), missing linked notes (`missing`), then alias names.
struct Corpus {
    paths: Vec<String>,
    ignored: std::ops::Range<usize>,
    named: usize,
    missing: Vec<String>,
    aliases: Vec<(String, String)>,
}

/// The two corpora a link completion ranks, as the index held them at `version` and the ignored
/// folders as `listing` holds them.
#[derive(Default)]
struct Corpora {
    version: i64,
    listing: Option<Arc<Listing>>,
    /// `[[`: the notes and PDFs, the notes only linked to and the aliases.
    wiki: Option<Arc<Corpus>>,
    /// `![[` and `](`: every file.
    files: Option<Arc<Corpus>>,
}

impl Notes {
    pub(crate) fn open_at(
        root: PathBuf,
        db: &Path,
        events: Sender<Event>,
        ignored: Arc<Ignored>,
        used: Arc<Mutex<Vec<String>>>,
    ) -> Result<Notes> {
        Ok(Notes {
            root,
            index: Mutex::new(Index::open(db)?),
            docs: Mutex::new(HashMap::new()),
            events,
            corpora: Mutex::default(),
            suggested: Mutex::default(),
            ignored,
            used,
        })
    }

    /// The file a link target names: the index's answer, or else one in a folder git ignores,
    /// which the index never walks but a link still reaches.
    fn find(&self, index: &Index, target: &str) -> Result<Option<String>> {
        Ok(match index.resolve_target(target)? {
            Some(rel) => Some(rel),
            None => self
                .ignored
                .listing(false)
                .resolve(target)
                .map(str::to_string),
        })
    }

    /// The note as the editor has it if it is open, else as it is on disk: a link's target is
    /// usually a note nobody has opened.
    fn text_of(&self, rel: &str) -> Result<String> {
        if let Some(text) = locked(&self.docs).get(rel) {
            return Ok(text.clone());
        }
        std::fs::read_to_string(Local::join(&self.root, rel)?)
            .map_err(|e| accent_core::Error::io(rel, e).into())
    }

    /// Say what is wrong with the note as it now stands. An empty list is what clears the last one.
    fn publish(&self, rel: &str, text: &str) {
        let items = or_empty("diagnose", self.diagnose(rel, text));
        let _ = self.events.send(Event::Diagnostics {
            rel: rel.to_string(),
            items,
            stale: false,
        });
    }

    /// Hint for each unresolved link in `rel`, using normalized vault paths
    /// for markdown links; anchors are unchecked. Warn about formulas the
    /// preview cannot render rather than marking valid unsupported LaTeX as errors.
    fn diagnose(&self, rel: &str, text: &str) -> Result<Vec<Diagnostic>> {
        let syntax = Syntax::of(rel);
        let a = syntax.analyze(text);
        let (links, targets): (Vec<&markdown::Link>, Vec<String>) = a
            .links
            .iter()
            .filter_map(|l| Some((l, checked_target(rel, l)?)))
            .unzip();
        let mut items = Vec::new();
        if !links.is_empty() {
            let index = locked(&self.index);
            let resolved = targets
                .iter()
                .map(|target| self.find(&index, target))
                .collect::<Result<Vec<_>>>()?;
            items.extend(
                links
                    .iter()
                    .zip(resolved)
                    .filter(|(_, found)| found.is_none())
                    .map(|(l, _)| Diagnostic {
                        range: range_of(text, &l.range),
                        severity: Severity::Hint,
                        message: format!("No note named {}", l.target),
                        source: Some("accent".to_string()),
                        code: None,
                        published: None,
                    }),
            );
        }
        // Org's formulas are not read yet: only a note's reach the preview.
        let formulas = match syntax {
            Syntax::Markdown => markdown::math_errors(text),
            Syntax::Org => Vec::new(),
        };
        items.extend(formulas.into_iter().map(|(range, why)| Diagnostic {
            range: range_of(text, &range),
            severity: Severity::Warning,
            message: format!("Cannot render this formula: {why}"),
            source: Some("accent".to_string()),
            code: None,
            published: None,
        }));
        Ok(items)
    }

    fn hover(&self, rel: &str, pos: Pos) -> Result<Option<Hover>> {
        let text = self.text_of(rel)?;
        let Some(caret) = byte_of(&text, pos) else {
            return Ok(None);
        };
        let a = Syntax::of(rel).analyze(&text);

        if let Some(link) = a.links.iter().find(|l| l.range.contains(&caret)) {
            let range = Some(range_of(&text, &link.range));
            if link.kind == LinkKind::External || link.target.is_empty() {
                return Ok(None);
            }
            let Some(target) = self.find(&locked(&self.index), &target_of(rel, link))? else {
                return Ok(None);
            };
            // A link to something that is not a note has nothing to preview but its place.
            if !path::is_note(&target) {
                return Ok(Some(Hover {
                    text: target,
                    range,
                }));
            }
            return Ok(Some(Hover {
                text: preview(&self.text_of(&target)?, &target),
                range,
            }));
        }

        if let Some(tag) = a.tags.iter().find(|t| t.range.contains(&caret)) {
            let notes = locked(&self.index).files_with_tag(&tag.name)?.len();
            return Ok(Some(Hover {
                text: format!("#{} — {notes} notes", tag.name),
                range: Some(range_of(&text, &tag.range)),
            }));
        }
        Ok(None)
    }

    fn definition(&self, rel: &str, pos: Pos) -> Result<Vec<Location>> {
        let text = self.text_of(rel)?;
        let Some(caret) = byte_of(&text, pos) else {
            return Ok(Vec::new());
        };
        let a = Syntax::of(rel).analyze(&text);
        let Some(link) = a.links.iter().find(|l| l.range.contains(&caret)) else {
            return Ok(Vec::new());
        };
        if link.kind == LinkKind::External {
            return Ok(vec![Location {
                path: link.target.clone(),
                ..Location::default()
            }]);
        }
        // `[[#Heading]]` has no target: it points into the note the caret is in.
        let target = match link.target.is_empty() {
            true => rel.to_string(),
            false => {
                let asked = target_of(rel, link);
                let resolved = self.find(&locked(&self.index), &asked)?;
                match resolved.or_else(|| self.unwalked(&asked)) {
                    Some(target) => target,
                    // Nothing is there yet: the answer is the file New File would write for it.
                    None => {
                        return Ok(vec![Location {
                            path: path::linked_path(&asked),
                            missing: true,
                            ..Location::default()
                        }]);
                    }
                }
            }
        };
        // An org file link's digits are a line of whatever file it names (`org-link-open-as-file`).
        let lines = Syntax::of(rel) == Syntax::Org && link.kind == LinkKind::Markdown;
        let range = link
            .anchor
            .as_ref()
            .and_then(|anchor| self.anchored(&target, anchor, lines));
        Ok(vec![Location {
            path: target,
            range: range.unwrap_or_default(),
            // What the range cannot say comes back beside it: a PDF's page and selection, so the
            // link reaches them rather than the first page, or a heading or a block the note does
            // not have, so the reader is told why they are at its top.
            anchor: link.anchor.clone().filter(|_| range.is_none()),
            missing: false,
        }])
    }

    /// accent's one fix: a link over `range` that nothing answers to, as its hint says
    /// ([`Self::diagnose`]), offers to create what it names, as following it does
    /// ([`Self::definition`]).
    fn code_actions(&self, rel: &str, range: Range) -> Result<Vec<CodeAction>> {
        let text = self.text_of(rel)?;
        let (Some(from), Some(to)) = (byte_of(&text, range.start), byte_of(&text, range.end))
        else {
            return Ok(Vec::new());
        };
        let a = Syntax::of(rel).analyze(&text);
        let index = locked(&self.index);
        let mut out: Vec<CodeAction> = Vec::new();
        for link in a
            .links
            .iter()
            .filter(|l| l.range.start <= to && from <= l.range.end)
        {
            let Some(asked) = checked_target(rel, link) else {
                continue;
            };
            let path = path::linked_path(&asked);
            let found = self.find(&index, &asked)?.or_else(|| self.unwalked(&asked));
            if found.is_some() || out.iter().any(|a| a.new_file.as_ref() == Some(&path)) {
                continue;
            }
            out.push(CodeAction {
                title: format!("Create note {}", link.target),
                kind: "quickfix".to_string(),
                preferred: true,
                raw: serde_json::Value::Null,
                new_file: Some(path),
            });
        }
        Ok(out)
    }

    /// Check disk for a target in an unindexed tree such as ignored `build/`
    /// or `node_modules`, before offering New File ([`path::linked_path`]).
    /// Run beside the files, including on a remote host.
    fn unwalked(&self, asked: &str) -> Option<String> {
        let path = path::linked_path(asked);
        Local::join(&self.root, &path)
            .ok()?
            .is_file()
            .then_some(path)
    }

    /// Where the heading, the block or the org search an anchor names sits in `rel`, if it is
    /// there at all ([`note::anchor_range`]); with `lines`, digits alone are a line
    /// ([`org::file_line`]).
    fn anchored(&self, rel: &str, anchor: &str, lines: bool) -> Option<Range> {
        let text = self.text_of(rel).ok()?;
        let line = lines.then(|| org::file_line(&text, anchor)).flatten();
        let at = line.or_else(|| note::anchor_range(rel, &text, anchor))?;
        Some(range_of(&text, &at))
    }

    fn symbols(&self, rel: &str) -> Result<Vec<Symbol>> {
        let text = self.text_of(rel)?;
        Ok(symbols_of(&text, &Syntax::of(rel).analyze(&text).headings))
    }

    fn folds(&self, rel: &str) -> Result<Vec<Fold>> {
        let text = self.text_of(rel)?;
        Ok(folds_of(&text, &Syntax::of(rel).analyze(&text)))
    }

    /// Every link that resolves to this note, where it is written.
    fn references(&self, rel: &str) -> Result<Vec<Location>> {
        let rows = locked(&self.index).backlinks(rel)?;
        Ok(placed(rows, |src| self.text_of(src)))
    }
}

impl Language for Notes {
    fn open(&self, rel: &str, _language_id: &str, text: String) -> Result<Support> {
        self.publish(rel, &text);
        locked(&self.docs).insert(rel.to_string(), text);
        // One `[` offers nothing, and nor does a `(` outside a link; `context` is what decides,
        // and it wants the second `[` or the `](` of a link's destination; in org, the second `[`
        // or a `:` in a heading's tags (`org_context`).
        let completion_triggers = match Syntax::of(rel) {
            Syntax::Markdown => vec!['[', '#', '('],
            Syntax::Org => vec!['[', ':'],
        };
        Ok(Support {
            completion_triggers,
            ..Support::default()
        })
    }

    fn change(&self, rel: &str, text: String) -> Result<()> {
        self.publish(rel, &text);
        locked(&self.docs).insert(rel.to_string(), text);
        Ok(())
    }

    /// Refresh hints against the last editor text after reconciliation, since
    /// a dangling link may resolve after its target appears without a source edit.
    fn rediagnose(&self, rel: &str) -> Result<()> {
        let text = locked(&self.docs).get(rel).cloned();
        if let Some(text) = text {
            self.publish(rel, &text);
        }
        Ok(())
    }

    fn close(&self, rel: &str) {
        locked(&self.docs).remove(rel);
        let _ = self.events.send(Event::Diagnostics {
            rel: rel.to_string(),
            items: Vec::new(),
            stale: false,
        });
    }

    fn completion(&self, rel: &str, pos: Pos, _trigger: Option<char>) -> Fut<'_, Completions> {
        let rel = rel.to_string();
        Box::pin(async move { Ok(or_empty("completion", Notes::completion(self, &rel, pos))) })
    }

    fn resolve(&self, _rel: &str, item: Completion) -> Fut<'_, Completion> {
        Box::pin(async move { Ok(item) })
    }

    fn signature_help(&self, _rel: &str, _pos: Pos) -> Fut<'_, Option<Signature>> {
        Box::pin(async { Ok(None) })
    }

    fn hover(&self, rel: &str, pos: Pos) -> Fut<'_, Option<Hover>> {
        let rel = rel.to_string();
        Box::pin(async move { Ok(or_empty("hover", Notes::hover(self, &rel, pos))) })
    }

    fn definition(&self, rel: &str, pos: Pos) -> Fut<'_, Vec<Location>> {
        let rel = rel.to_string();
        Box::pin(async move { Ok(or_empty("definition", Notes::definition(self, &rel, pos))) })
    }

    fn symbols(&self, rel: &str) -> Fut<'_, Vec<Symbol>> {
        let rel = rel.to_string();
        Box::pin(async move { Ok(or_empty("symbols", Notes::symbols(self, &rel))) })
    }

    fn references(&self, rel: &str, _pos: Pos) -> Fut<'_, Vec<Location>> {
        let rel = rel.to_string();
        Box::pin(async move { Ok(or_empty("references", Notes::references(self, &rel))) })
    }

    fn folds(&self, rel: &str) -> Fut<'_, Vec<Fold>> {
        let rel = rel.to_string();
        Box::pin(async move { Ok(or_empty("folds", Notes::folds(self, &rel))) })
    }

    /// No source actions: a note has nothing to organize.
    fn code_actions(
        &self,
        rel: &str,
        range: Range,
        _diagnostics: Vec<Diagnostic>,
        source: bool,
    ) -> Fut<'_, Vec<CodeAction>> {
        let rel = rel.to_string();
        Box::pin(async move {
            if source {
                return Ok(Vec::new());
            }
            Ok(or_empty(
                "code actions",
                Notes::code_actions(self, &rel, range),
            ))
        })
    }
}

/// Return an empty answer on index-query failure, so a recently renamed
/// file cannot break completion, outlines or folds.
fn or_empty<T: Default>(what: &str, r: Result<T>) -> T {
    r.unwrap_or_else(|e| {
        tracing::debug!("notes {what}: {e}");
        T::default()
    })
}

/// Read the PDF [`Outline`] through `accent_pdf`, which every build has, a host's `serve`
/// included: once, on the request thread, only after the prefix resolves to a `.pdf`. A damaged
/// file is read through an xref rebuilt from its objects, as the window's pdfium repairs it.
pub(crate) fn pdf_outline(path: &Path) -> Result<Outline> {
    let outline = (std::fs::File::open(path).map_err(accent_pdf::Error::from))
        .and_then(|file| {
            accent_pdf::read(Arc::new(file), |pdf| {
                accent_pdf::Document::new(pdf, None, &pdf.pages()?).outline()
            })
        })
        .map_err(|e| accent_core::Error::Pdf(e.to_string()))?;
    Ok(outline.into_iter().map(|b| (b.title, b.page)).collect())
}

/// The fields a note's completion never fills, so the interesting ones stay together above.
fn empty_item() -> Completion {
    Completion {
        label: String::new(),
        kind: Kind::Text,
        detail: None,
        doc: None,
        filter: None,
        insert: String::new(),
        is_snippet: false,
        replace: Range::default(),
        extra_edits: Vec::new(),
        resolve: None,
        preselect: false,
        commit: Vec::new(),
    }
}

/// Format a hover with the title in bold and first nonempty content lines.
/// Skip frontmatter and an opening heading that repeats the title.
fn preview(note: &str, rel: &str) -> String {
    let a = markdown::analyze(note);
    let title = a.title.unwrap_or_else(|| stem(rel));
    let mut body = a
        .spans
        .iter()
        .find(|s| s.style == markdown::Style::Frontmatter)
        .map_or(0, |s| s.range.end)
        .min(note.len());
    if let Some(h) = a.headings.first()
        && h.text == title
        && note
            .get(body..h.range.start)
            .is_some_and(|s| s.trim().is_empty())
    {
        body = h.range.end;
    }
    let lines: Vec<&str> = note[body..]
        .lines()
        .filter(|l| !l.trim().is_empty())
        .take(HOVER_LINES)
        .collect();
    format!("**{title}**\n\n{}", lines.join("\n"))
}
