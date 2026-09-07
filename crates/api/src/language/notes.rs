//! The provider that answers for a note: the index is the language server.
//!
//! A wikilink is a definition, a backlink is a reference, a heading is a symbol, and a link the
//! index cannot resolve is a diagnostic. Everything the editor used to do to a note through its
//! own code paths happens here instead, so a note and a source file are asked the same questions.
//!
//! The pure half sits at the top and is tested without a vault; the provider below only reads
//! the index and the open documents.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::mpsc::Sender;

use anyhow::Result;

use accent_core::index::Index;
use accent_core::markdown::{self, LinkKind};

use super::{
    Completion, Diagnostic, Fold, Fut, Hover, Kind, Language, Location, Pos, Range, Severity,
    Signature, Support, Symbol, byte_of, pos_of, range_of,
};
use crate::{Event, Local, locked};

/// Rows the popup offers before the user has to type more.
const COMPLETIONS: usize = 20;
/// How much of a linked note a hover shows.
const HOVER_LINES: usize = 8;

// ------------------------------------------------------------------ the pure half

/// What the caret is inside of, and so what a completion offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Trigger {
    Wiki,
    Tag,
}

/// The trigger the caret is inside, where it starts in `head`, and what has been typed since.
///
/// `head` is the current line from its start up to the caret, so nothing here looks at the rest
/// of the note. `[[` is tried first, which is what makes the `#` of `[[Note#Heading]]` an anchor
/// rather than a tag.
///
/// `None` means there is nothing to complete: no trigger on the line, a wikilink already closed,
/// a `#` run that opens the line (an ATX heading marker), or a tag the caret has moved past.
pub(crate) fn context(head: &str) -> Option<(Trigger, usize, &str)> {
    if let Some(start) = head.rfind("[[") {
        let prefix = &head[start + 2..];
        // `]` means the link was closed; the caret is past it, not inside it.
        return (!prefix.contains(']')).then_some((Trigger::Wiki, start, prefix));
    }
    let start = head.rfind('#')?;
    // A `#` run that opens the line, indented or not, is a heading marker.
    if head[..start].trim_end_matches('#').trim().is_empty() {
        return None;
    }
    let prefix = &head[start + 1..];
    // Whitespace ends a tag, so the caret is no longer inside one.
    (!prefix.contains(char::is_whitespace)).then_some((Trigger::Tag, start, prefix))
}

/// Notes matching what has been typed: prefix on the name or on the whole path, shortest path
/// first because that is the one the user most likely means.
pub(crate) fn note_candidates(paths: &[String], prefix: &str) -> Vec<String> {
    let prefix = prefix.to_lowercase();
    let mut hits: Vec<String> = paths
        .iter()
        .filter(|rel| {
            markdown::strip_ext(basename(rel))
                .to_lowercase()
                .starts_with(&prefix)
                || rel.to_lowercase().starts_with(&prefix)
        })
        .cloned()
        .collect();
    // Stable, so paths of equal length keep the index's alphabetical order.
    hits.sort_by_key(String::len);
    hits.truncate(COMPLETIONS);
    hits
}

/// Tags matching what has been typed, in the order the index hands them over: most used first.
pub(crate) fn tag_candidates(tags: &[(String, i64)], prefix: &str) -> Vec<String> {
    let prefix = prefix.to_lowercase();
    tags.iter()
        .filter(|(name, _)| name.to_lowercase().starts_with(&prefix))
        .map(|(name, _)| name.clone())
        .take(COMPLETIONS)
        .collect()
}

/// The headings as a tree: a heading is a child of the nearest one above it with a smaller
/// level, and spans everything down to the next heading that is not below it.
pub(crate) fn symbols_of(text: &str, headings: &[markdown::Heading]) -> Vec<Symbol> {
    let mut out: Vec<Symbol> = Vec::new();
    // The path down the tree: one open ancestor per level, deepest last.
    let mut open: Vec<(u8, Symbol)> = Vec::new();
    let close =
        |open: &mut Vec<(u8, Symbol)>, out: &mut Vec<Symbol>, done: Symbol| match open.last_mut() {
            Some((_, parent)) => parent.children.push(done),
            None => out.push(done),
        };

    for (i, h) in headings.iter().enumerate() {
        while open.last().is_some_and(|(level, _)| *level >= h.level) {
            let (_, done) = open.pop().expect("just looked at it");
            close(&mut open, &mut out, done);
        }
        let end = section_end(text, headings, i);
        open.push((
            h.level,
            Symbol {
                name: h.text.clone(),
                range: range_of(text, &(h.range.start..end)),
                selection: range_of(text, &h.range),
                children: Vec::new(),
            },
        ));
    }
    while let Some((_, done)) = open.pop() {
        close(&mut open, &mut out, done);
    }
    out
}

/// What can be hidden behind its first line: a heading's section, a fenced block, the
/// frontmatter. A fold of a single line is no fold at all and is left out.
pub(crate) fn folds_of(text: &str, a: &markdown::Analysis) -> Vec<Fold> {
    let line = |byte: usize| pos_of(text, byte).line;
    let mut out = Vec::new();
    for (i, h) in a.headings.iter().enumerate() {
        out.push(Fold {
            start_line: line(h.range.start),
            end_line: line(section_end(text, &a.headings, i)),
        });
    }
    for span in &a.spans {
        if matches!(
            span.style,
            markdown::Style::CodeBlock | markdown::Style::Frontmatter
        ) {
            out.push(Fold {
                start_line: line(span.range.start),
                end_line: line(span.range.end.saturating_sub(1)),
            });
        }
    }
    out.retain(|f| f.end_line > f.start_line);
    out
}

/// The last byte of the section opened by `headings[i]`: everything down to the next heading
/// that is not below it, or to the end of the note.
fn section_end(text: &str, headings: &[markdown::Heading], i: usize) -> usize {
    headings[i + 1..]
        .iter()
        .find(|next| next.level <= headings[i].level)
        // The byte before the next heading is the newline that ends the section's last line.
        .map_or(text.len(), |next| next.range.start)
        .saturating_sub(1)
        .max(headings[i].range.end)
}

fn basename(rel: &str) -> &str {
    rel.rsplit('/').next().unwrap_or(rel)
}

/// The stem a `[[wikilink]]` names a note by.
fn stem(rel: &str) -> String {
    markdown::strip_ext(basename(rel))
}

// --------------------------------------------------------------------- the provider

/// Everything the index knows about the notes of one vault, asked the way a language server is.
pub(crate) struct Notes {
    root: PathBuf,
    /// A reader of its own, so a completion never queues behind the caller's connection.
    index: Mutex<Index>,
    /// The text as the editor has it, which is ahead of what is on disk and in the index.
    docs: Mutex<HashMap<String, String>>,
    events: Sender<Event>,
}

impl Notes {
    pub(crate) fn open_at(root: PathBuf, db: &Path, events: Sender<Event>) -> Result<Notes> {
        Ok(Notes {
            root,
            index: Mutex::new(Index::open(db)?),
            docs: Mutex::new(HashMap::new()),
            events,
        })
    }

    /// The note as the editor has it if it is open, else as it is on disk: a link's target is
    /// usually a note nobody has opened.
    fn text_of(&self, rel: &str) -> Result<String> {
        if let Some(text) = locked(&self.docs).get(rel) {
            return Ok(text.clone());
        }
        Ok(std::fs::read_to_string(Local::join(&self.root, rel)?)?)
    }

    /// Say what is wrong with the note as it now stands. An empty list is what clears the last one.
    fn publish(&self, rel: &str, text: &str) {
        let items = or_empty("diagnose", self.diagnose(text));
        let _ = self.events.send(Event::Diagnostics {
            rel: rel.to_string(),
            items,
        });
    }

    /// One hint per wikilink the index cannot resolve. `Markdown` links are left alone: they are
    /// relative to the note, and link resolution does no path arithmetic.
    fn diagnose(&self, text: &str) -> Result<Vec<Diagnostic>> {
        let a = markdown::analyze(text);
        let links: Vec<&markdown::Link> = a
            .links
            .iter()
            .filter(|l| matches!(l.kind, LinkKind::Wiki | LinkKind::Embed) && !l.target.is_empty())
            .collect();
        if links.is_empty() {
            return Ok(Vec::new());
        }
        let targets: Vec<String> = links.iter().map(|l| l.target.clone()).collect();
        let resolved = locked(&self.index).resolve_targets(&targets)?;
        Ok(links
            .iter()
            .zip(resolved)
            .filter(|(_, found)| found.is_none())
            .map(|(l, _)| Diagnostic {
                range: range_of(text, &l.range),
                severity: Severity::Hint,
                message: format!("No note named {}", l.target),
                source: Some("accent".to_string()),
            })
            .collect())
    }

    fn completion(&self, rel: &str, pos: Pos) -> Result<Vec<Completion>> {
        let text = self.text_of(rel)?;
        let Some(caret) = byte_of(&text, pos) else {
            return Ok(Vec::new());
        };
        let line_start = text[..caret].rfind('\n').map_or(0, |i| i + 1);
        let head = &text[line_start..caret];
        let Some((trigger, start, prefix)) = context(head) else {
            return Ok(Vec::new());
        };
        let at = |byte: usize| Pos {
            line: pos.line,
            character: head[..byte].chars().count() as u32,
        };
        let mut replace = Range {
            start: at(start),
            end: pos,
        };

        match trigger {
            Trigger::Wiki => {
                // `typing::pair` closed the `[` as it was typed, so the caret usually sits in
                // front of the `]]` it left. The inserted link brings its own, so they go too.
                let eaten = text[caret..]
                    .chars()
                    .take(2)
                    .take_while(|c| *c == ']')
                    .count();
                replace.end.character += eaten as u32;

                let paths = locked(&self.index).note_paths()?;
                // Which stems two notes share, over the whole vault and not only over the hits:
                // a link written as the bare stem would then be ambiguous whatever is offered.
                let mut stems: HashMap<String, usize> = HashMap::new();
                for rel in &paths {
                    *stems.entry(stem(rel)).or_default() += 1;
                }
                Ok(note_candidates(&paths, prefix)
                    .into_iter()
                    .map(|hit| {
                        let label = stem(&hit);
                        let ambiguous = stems.get(&label).is_some_and(|n| *n > 1);
                        Completion {
                            insert: match ambiguous {
                                true => format!("[[{}]]", markdown::strip_ext(&hit)),
                                false => format!("[[{label}]]"),
                            },
                            detail: ambiguous.then(|| hit.clone()),
                            label,
                            kind: Kind::File,
                            replace,
                            ..empty_item()
                        }
                    })
                    .collect())
            }
            Trigger::Tag => {
                let tags = locked(&self.index).tags()?;
                Ok(tag_candidates(&tags, prefix)
                    .into_iter()
                    .map(|name| Completion {
                        insert: format!("#{name}"),
                        label: format!("#{name}"),
                        kind: Kind::Tag,
                        replace,
                        ..empty_item()
                    })
                    .collect())
            }
        }
    }

    fn hover(&self, rel: &str, pos: Pos) -> Result<Option<Hover>> {
        let text = self.text_of(rel)?;
        let Some(caret) = byte_of(&text, pos) else {
            return Ok(None);
        };
        let a = markdown::analyze(&text);

        if let Some(link) = a.links.iter().find(|l| l.range.contains(&caret)) {
            let range = Some(range_of(&text, &link.range));
            if link.kind == LinkKind::External || link.target.is_empty() {
                return Ok(None);
            }
            let Some(target) = locked(&self.index).resolve_target(&link.target)? else {
                return Ok(None);
            };
            // A link to something that is not a note has nothing to preview but its place.
            if !target.ends_with(".md") {
                return Ok(Some(Hover {
                    text: target,
                    range,
                }));
            }
            let note = self.text_of(&target)?;
            let title = markdown::analyze(&note)
                .title
                .unwrap_or_else(|| stem(&target));
            return Ok(Some(Hover {
                text: format!("**{title}**\n\n{}", preview(&note)),
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
        let a = markdown::analyze(&text);
        let Some(link) = a.links.iter().find(|l| l.range.contains(&caret)) else {
            return Ok(Vec::new());
        };
        if link.kind == LinkKind::External {
            return Ok(vec![Location {
                path: link.target.clone(),
                range: Range::default(),
            }]);
        }
        // `[[#Heading]]` has no target: it points into the note the caret is in.
        let target = match link.target.is_empty() {
            true => rel.to_string(),
            false => match locked(&self.index).resolve_target(&link.target)? {
                Some(target) => target,
                // A dangling link goes nowhere; offering to create the note is the app's business.
                None => return Ok(Vec::new()),
            },
        };
        let range = link
            .anchor
            .as_ref()
            .and_then(|anchor| self.heading(&target, anchor))
            .unwrap_or_default();
        Ok(vec![Location {
            path: target,
            range,
        }])
    }

    /// Where the heading an anchor names sits in `rel`, if it is there at all.
    fn heading(&self, rel: &str, anchor: &str) -> Option<Range> {
        let text = self.text_of(rel).ok()?;
        let anchor = anchor.trim();
        markdown::analyze(&text)
            .headings
            .iter()
            .find(|h| h.text.trim().eq_ignore_ascii_case(anchor))
            .map(|h| range_of(&text, &h.range))
    }

    fn symbols(&self, rel: &str) -> Result<Vec<Symbol>> {
        let text = self.text_of(rel)?;
        Ok(symbols_of(&text, &markdown::analyze(&text).headings))
    }

    fn folds(&self, rel: &str) -> Result<Vec<Fold>> {
        let text = self.text_of(rel)?;
        Ok(folds_of(&text, &markdown::analyze(&text)))
    }

    /// Every link that resolves to this note, where it is written. The rows arrive grouped by
    /// source, so each source's text is read once.
    fn references(&self, rel: &str) -> Result<Vec<Location>> {
        let rows = locked(&self.index).backlinks(rel)?;
        let mut out = Vec::with_capacity(rows.len());
        let (mut read, mut source) = (String::new(), String::new());
        for b in rows {
            if read != b.src_rel_path {
                // Unreadable is not an error here: the link is still worth listing, at 0:0.
                source = self.text_of(&b.src_rel_path).unwrap_or_default();
                read = b.src_rel_path.clone();
            }
            out.push(Location {
                range: range_of(&source, &(b.byte_start as usize..b.byte_end as usize)),
                path: b.src_rel_path,
            });
        }
        Ok(out)
    }
}

impl Language for Notes {
    fn open(&self, rel: &str, _language_id: &str, text: String) -> Result<Support> {
        self.publish(rel, &text);
        locked(&self.docs).insert(rel.to_string(), text);
        Ok(Support {
            // One `[` offers nothing; `context` is what decides, and it wants the second one.
            completion_triggers: vec!['[', '#'],
            ..Support::default()
        })
    }

    fn change(&self, rel: &str, text: String) -> Result<()> {
        self.publish(rel, &text);
        locked(&self.docs).insert(rel.to_string(), text);
        Ok(())
    }

    fn close(&self, rel: &str) {
        locked(&self.docs).remove(rel);
        let _ = self.events.send(Event::Diagnostics {
            rel: rel.to_string(),
            items: Vec::new(),
        });
    }

    fn completion(&self, rel: &str, pos: Pos, _trigger: Option<char>) -> Fut<'_, Vec<Completion>> {
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
}

/// A question the index could not answer is answered with nothing: a note whose file has just
/// been renamed away must not take the popup, the outline and the folds down with it.
fn or_empty<T: Default>(what: &str, r: Result<T>) -> T {
    r.unwrap_or_else(|e| {
        tracing::debug!("notes {what}: {e}");
        T::default()
    })
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
    }
}

/// The first lines of a note that say something, with the frontmatter skipped.
fn preview(note: &str) -> String {
    let body = markdown::analyze(note)
        .spans
        .iter()
        .find(|s| s.style == markdown::Style::Frontmatter)
        .map_or(0, |s| s.range.end);
    note[body.min(note.len())..]
        .lines()
        .filter(|l| !l.trim().is_empty())
        .take(HOVER_LINES)
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_finds_a_wikilink_or_a_tag() {
        assert_eq!(context("see [[Dee"), Some((Trigger::Wiki, 4, "Dee")));
        assert_eq!(
            context("[[Deep Work"),
            Some((Trigger::Wiki, 0, "Deep Work"))
        );
        assert_eq!(context("a [Dee"), None, "one bracket is not a wikilink");
        assert_eq!(context("[[Deep Work]] and"), None, "already closed");
        // The `#` of an anchor belongs to the link, not to a tag.
        assert_eq!(
            context("[[Note#Head"),
            Some((Trigger::Wiki, 0, "Note#Head"))
        );

        assert_eq!(
            context("note about #area/"),
            Some((Trigger::Tag, 11, "area/"))
        );
        assert_eq!(context("a #"), Some((Trigger::Tag, 2, "")), "just opened");
        assert_eq!(context("a #area and more"), None, "whitespace ends a tag");
        for head in ["# Heading", "##", "  #", "\t### "] {
            assert_eq!(context(head), None, "{head:?} is a heading marker");
        }
    }

    #[test]
    fn symbols_nest_by_level_and_span_their_section() {
        let text = "# One\ntext\n## Two\nmore\n### Three\n# Four\n";
        let a = markdown::analyze(text);
        let syms = symbols_of(text, &a.headings);

        assert_eq!(syms.len(), 2);
        assert_eq!(syms[0].name, "One");
        assert_eq!(syms[0].selection.start.line, 0);
        assert_eq!(
            syms[0].range.end.line, 4,
            "down to the last line above Four"
        );
        let two = &syms[0].children[0];
        assert_eq!(two.name, "Two");
        assert_eq!(two.children[0].name, "Three", "a deeper heading nests");
        assert_eq!(syms[1].name, "Four");
        assert_eq!(
            syms[1].range.end.line, 5,
            "the last section runs to the end"
        );
    }

    #[test]
    fn folds_cover_sections_fences_and_frontmatter() {
        let text = "---\ntitle: T\n---\n# One\ntext\n```rust\nfn a() {}\n```\n## Leaf\n";
        let folds = folds_of(text, &markdown::analyze(text));

        assert!(folds.contains(&Fold {
            start_line: 0,
            end_line: 2
        }));
        assert!(folds.contains(&Fold {
            start_line: 3,
            end_line: 8
        }));
        assert!(folds.contains(&Fold {
            start_line: 5,
            end_line: 7
        }));
        assert!(
            !folds.iter().any(|f| f.start_line == 8),
            "a heading with nothing under it folds nothing"
        );
    }

    #[test]
    fn note_candidates_prefer_the_shortest_path_and_tags_keep_their_order() {
        let paths: Vec<String> = ["sub/Alphabet.md", "Alpha.md", "Beta.md"]
            .map(String::from)
            .into();

        assert_eq!(
            note_candidates(&paths, "alp"),
            ["Alpha.md", "sub/Alphabet.md"]
        );
        assert_eq!(note_candidates(&paths, "sub/"), ["sub/Alphabet.md"]);
        assert!(note_candidates(&paths, "zzz").is_empty());

        // The index counts them, so the order it hands them over in is the one to keep.
        let tags = [("alpha".to_string(), 2), ("alphabet".to_string(), 1)];
        assert_eq!(tag_candidates(&tags, "alp"), ["alpha", "alphabet"]);
        assert!(tag_candidates(&tags, "b").is_empty());
    }
}
