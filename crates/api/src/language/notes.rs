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
use accent_core::path::{self, FileType, basename, parent_dir, stem};

use super::{
    Completion, Completions, Diagnostic, Fold, Fut, Hover, Kind, Language, Location, Pos, Range,
    Severity, Signature, Support, Symbol, byte_of, pos_of, range_of,
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
    /// `![[`: any file, an image or a PDF as readily as a note.
    Embed,
    /// The `#` of a markdown link's destination: a heading of this note after `[text](#`, of
    /// the note the path names after `[text](Other.md#`. The start is the destination's.
    Anchor,
    /// The rest of a markdown link's destination, `[text](` or `![alt](`: a file's path.
    Path,
    Tag,
}

/// The trigger the caret is inside, where it starts in `head`, and what has been typed since.
///
/// `head` is the current line from its start up to the caret, so nothing here looks at the rest
/// of the note. `[[` is tried first, which is what makes the `#` of `[[Note#Heading]]` an anchor
/// rather than a tag, and a link's destination next, which does the same for `[text](#Heading)`.
///
/// `None` means there is nothing to complete: no trigger on the line, a `#` run that opens the
/// line (an ATX heading marker), or a tag the caret has moved past.
pub(crate) fn context(head: &str) -> Option<(Trigger, usize, &str)> {
    if let Some(start) = head.rfind("[[") {
        let prefix = &head[start + 2..];
        // `]` means the link was closed: the caret is past it, and the rest of the line decides.
        if !prefix.contains(']') {
            let trigger = match head[..start].ends_with('!') {
                true => Trigger::Embed,
                false => Trigger::Wiki,
            };
            return Some((trigger, start, prefix));
        }
    }
    if let Some(open) = head.rfind("](") {
        let dest = &head[open + 2..];
        // A `#` in a destination is never a tag but an anchor: into this note, or into the one
        // the path before it names.
        if !dest.contains(|c: char| c == ')' || c.is_whitespace()) {
            return Some(match dest.split_once('#') {
                Some((_, prefix)) => (Trigger::Anchor, open + 2, prefix),
                None => (Trigger::Path, open + 2, dest),
            });
        }
    }
    let start = head.rfind('#')?;
    let prefix = &head[start + 1..];
    // A `#` run that opens the line, indented or not, is a heading marker, until a letter follows
    // a single `#`: a heading needs `# `, so `#x` can only be a tag.
    let opens_line = head[..start].trim_end_matches('#').trim().is_empty();
    if opens_line && (head[..start].ends_with('#') || !prefix.starts_with(char::is_alphabetic)) {
        return None;
    }
    // Whitespace ends a tag, so the caret is no longer inside one.
    (!prefix.contains(char::is_whitespace)).then_some((Trigger::Tag, start, prefix))
}

/// The headings a wikilink can name, by their text: each once, in the order the note has them.
/// A heading with no text has nothing to be named by.
pub(crate) fn heading_names(headings: &[markdown::Heading]) -> Vec<&str> {
    let mut out: Vec<&str> = Vec::new();
    for h in headings {
        let name = h.text.trim();
        if !name.is_empty() && !out.contains(&name) {
            out.push(name);
        }
    }
    out
}

/// What the index knows a link in the note `rel` by. A markdown link names its file from the
/// note's own folder, so it becomes a vault path first, as it did when the index stored it.
fn target_of(rel: &str, link: &markdown::Link) -> String {
    match link.kind {
        LinkKind::Markdown => path::resolve(parent_dir(rel), &link.target),
        _ => link.target.clone(),
    }
}

/// What a hint asks the index about a link in `rel`, or `None` when it is not the index's to
/// judge: a URL, a pure `#anchor`, and a markdown link that points out of the vault — absolute,
/// or climbing past the root — which [`path::resolve`] would otherwise fold back into it.
fn checked_target(rel: &str, link: &markdown::Link) -> Option<String> {
    let judged = match link.kind {
        LinkKind::Wiki | LinkKind::Embed => true,
        LinkKind::Markdown => path::stays_inside(parent_dir(rel), &link.target),
        LinkKind::External => false,
    };
    (judged && !link.target.is_empty()).then(|| target_of(rel, link))
}

/// The two ways a `[[…]]` link names `rel`: by its bare name — a note's stem, any other file's
/// whole name — and by its path, which drops a note's extension too.
pub(crate) fn link_names(rel: &str) -> (String, String) {
    match path::file_type(rel) {
        FileType::Note => (stem(rel), markdown::strip_ext(rel)),
        _ => (basename(rel).to_string(), rel.to_string()),
    }
}

/// Paths matching what has been typed, and whether there were more than fit.
///
/// The query is looked for *anywhere* in the path, not only at its start: `[[work]]` has to offer
/// `Projects/Rework.md`, the way a link picker does everywhere else. What starts with the query
/// still comes first — that is the file the reader most likely means — and after that the
/// shortest path wins, so a note at the root beats one buried under three directories.
///
/// The `more` half is what makes the popup ask again as the word grows. It used to say the list
/// was complete while handing back twenty of several hundred paths, so the popup narrowed those
/// twenty client-side and everything else stayed unreachable however much was typed.
pub(crate) fn path_candidates(paths: &[String], query: &str) -> (Vec<String>, bool) {
    let query = query.to_lowercase();
    let mut hits: Vec<(bool, usize, &String)> = paths
        .iter()
        .filter_map(|rel| {
            let low = rel.to_lowercase();
            // The whole name, so that `logo.p` still opens `logo.png`.
            let opens = basename(&low).starts_with(&query) || low.starts_with(&query);
            (opens || low.contains(&query)).then_some((!opens, rel.len(), rel))
        })
        .collect();
    // Stable, so paths of equal rank and length keep the index's alphabetical order.
    hits.sort_by_key(|(later, len, _)| (*later, *len));
    let more = hits.len() > COMPLETIONS;
    hits.truncate(COMPLETIONS);
    (
        hits.into_iter().map(|(_, _, rel)| rel.clone()).collect(),
        more,
    )
}

/// Tags matching what has been typed, in the order the index hands them over: most used first.
/// Prefix only, because a tag is one word and typing more of it is how it is narrowed.
pub(crate) fn tag_candidates(tags: &[(String, i64)], prefix: &str) -> (Vec<String>, bool) {
    let prefix = prefix.to_lowercase();
    let hits: Vec<String> = tags
        .iter()
        .filter(|(name, _)| name.to_lowercase().starts_with(&prefix))
        .map(|(name, _)| name.clone())
        .take(COMPLETIONS + 1)
        .collect();
    let more = hits.len() > COMPLETIONS;
    (hits.into_iter().take(COMPLETIONS).collect(), more)
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
        // A section that runs to the end of the note takes in the empty line after its final
        // newline too: that is where a caret sent to the end lands, and it is under this heading.
        let end = match section_end(text, headings, i) {
            last if last + 1 == text.len() => text.len(),
            end => end,
        };
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
        let items = or_empty("diagnose", self.diagnose(rel, text));
        let _ = self.events.send(Event::Diagnostics {
            rel: rel.to_string(),
            items,
        });
    }

    /// One hint per link in `rel` the index cannot resolve: a wikilink by its name, a markdown
    /// link by the vault path it names from the note's folder. Anchors are not checked.
    ///
    /// Then one warning per formula the preview cannot render. A warning and not an error: the
    /// converter rejects some valid LaTeX it does not support.
    fn diagnose(&self, rel: &str, text: &str) -> Result<Vec<Diagnostic>> {
        let a = markdown::analyze(text);
        let (links, targets): (Vec<&markdown::Link>, Vec<String>) = a
            .links
            .iter()
            .filter_map(|l| Some((l, checked_target(rel, l)?)))
            .unzip();
        let mut items = Vec::new();
        if !links.is_empty() {
            let resolved = locked(&self.index).resolve_targets(&targets)?;
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
                    }),
            );
        }
        items.extend(
            markdown::math_errors(text)
                .into_iter()
                .map(|(range, why)| Diagnostic {
                    range: range_of(text, &range),
                    severity: Severity::Warning,
                    message: format!("Cannot render this formula: {why}"),
                    source: Some("accent".to_string()),
                }),
        );
        Ok(items)
    }

    fn completion(&self, rel: &str, pos: Pos) -> Result<Completions> {
        let text = self.text_of(rel)?;
        let Some(caret) = byte_of(&text, pos) else {
            return Ok(Completions::default());
        };
        let line_start = text[..caret].rfind('\n').map_or(0, |i| i + 1);
        let head = &text[line_start..caret];
        let Some((trigger, start, prefix)) = context(head) else {
            return Ok(Completions::default());
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
            Trigger::Wiki | Trigger::Embed => {
                // `typing::pair` closed the `[` as it was typed, so the caret usually sits in
                // front of the `]]` it left. The inserted link brings its own, so they go too.
                let eaten = text[caret..]
                    .chars()
                    .take(2)
                    .take_while(|c| *c == ']')
                    .count();
                replace.end.character += eaten as u32;
                if let Some((note, _)) = prefix.split_once('#') {
                    return self.heading_links(rel, note, replace);
                }

                let index = locked(&self.index);
                let paths = match trigger {
                    Trigger::Wiki => index.note_paths()?,
                    _ => index.file_paths(false)?,
                };
                let (hits, more) = path_candidates(&paths, prefix);
                let mut items = Vec::with_capacity(hits.len());
                for hit in hits {
                    let (name, path) = link_names(&hit);
                    // The bare name only while it reaches this file: where a shorter path
                    // answers to it first, the link has to spell the path out.
                    let bare = index.resolve_target(&name)?.as_ref() == Some(&hit);
                    items.push(Completion {
                        insert: match bare {
                            true => format!("[[{name}]]"),
                            false => format!("[[{path}]]"),
                        },
                        detail: (!bare).then(|| hit.clone()),
                        // The popup narrows by what was typed since the `[[`, which a bare
                        // name never matches; the path lets a folder narrow it too.
                        filter: Some(format!("[[{path}")),
                        label: name,
                        kind: Kind::File,
                        replace,
                        ..empty_item()
                    });
                }
                Ok(Completions {
                    items,
                    incomplete: more,
                })
            }
            Trigger::Anchor => {
                // The path before the `#` names a note from this one's folder, and no path names
                // this note. A file that is not a note has no headings to offer.
                let dest = head[start..].split_once('#').map_or("", |(dest, _)| dest);
                let other = match dest {
                    "" => None,
                    _ => {
                        let target =
                            path::resolve(parent_dir(rel), &markdown::percent_decode(dest));
                        match locked(&self.index).resolve_target(&target)? {
                            Some(note) if note.ends_with(".md") => Some(self.text_of(&note)?),
                            _ => return Ok(Completions::default()),
                        }
                    }
                };
                let headings = markdown::analyze(other.as_deref().unwrap_or(&text)).headings;
                let slugs = markdown::slugs(headings.iter().map(|h| h.text.as_str()));
                let items = headings
                    .iter()
                    .zip(slugs)
                    .filter(|(h, _)| !h.text.trim().is_empty())
                    .map(|(h, slug)| {
                        let anchor = format!("#{slug}");
                        Completion {
                            label: h.text.trim().to_string(),
                            // What goes in is not what the row reads, so its anchor is shown too.
                            detail: Some(anchor.clone()),
                            filter: Some(format!("{dest}{anchor}")),
                            insert: format!("{dest}{anchor}"),
                            replace,
                            ..empty_item()
                        }
                    })
                    .collect();
                Ok(Completions {
                    items,
                    incomplete: false,
                })
            }
            Trigger::Path => {
                let paths = locked(&self.index).file_paths(false)?;
                // Matched against vault paths, so what was typed loses its encoding, and the `./`
                // and `../` a relative link opens with, which say where from rather than what.
                let query = markdown::percent_decode(prefix);
                let (hits, more) = path_candidates(&paths, query.trim_start_matches(['.', '/']));
                let dir = parent_dir(rel);
                let items = hits
                    .into_iter()
                    .map(|hit| {
                        // From this note's folder, which is where the index resolves it from.
                        let link = markdown::percent_encode(&path::relative(dir, &hit));
                        Completion {
                            label: basename(&hit).to_string(),
                            // What goes in is not what the row reads, so it is shown as well.
                            detail: Some(link.clone()),
                            filter: Some(link.clone()),
                            insert: link,
                            kind: Kind::File,
                            replace,
                            ..empty_item()
                        }
                    })
                    .collect();
                Ok(Completions {
                    items,
                    incomplete: more,
                })
            }
            Trigger::Tag => {
                let tags = locked(&self.index).tags()?;
                let (hits, more) = tag_candidates(&tags, prefix);
                Ok(Completions {
                    items: hits
                        .into_iter()
                        .map(|name| Completion {
                            insert: format!("#{name}"),
                            label: format!("#{name}"),
                            kind: Kind::Tag,
                            replace,
                            ..empty_item()
                        })
                        .collect(),
                    incomplete: more,
                })
            }
        }
    }

    /// `[[note#Heading]]` for each heading of the note `note` names, or of this one when it names
    /// none. The link keeps the note as it was typed; one the index cannot find offers nothing,
    /// and neither does a file that is not a note.
    fn heading_links(&self, rel: &str, note: &str, replace: Range) -> Result<Completions> {
        let target = match note {
            "" => rel.to_string(),
            _ => match locked(&self.index).resolve_target(note)? {
                Some(target) if target.ends_with(".md") => target,
                _ => return Ok(Completions::default()),
            },
        };
        let text = self.text_of(&target)?;
        let items = heading_names(&markdown::analyze(&text).headings)
            .into_iter()
            .map(|name| {
                let link = format!("[[{note}#{name}");
                Completion {
                    label: name.to_string(),
                    insert: format!("{link}]]"),
                    filter: Some(link),
                    replace,
                    ..empty_item()
                }
            })
            .collect();
        Ok(Completions {
            items,
            incomplete: false,
        })
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
            let Some(target) = locked(&self.index).resolve_target(&target_of(rel, link))? else {
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
            false => match locked(&self.index).resolve_target(&target_of(rel, link))? {
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
        // A PDF anchor rides along in the path, `paper.pdf#page=3&selection=…`, so following the
        // link reaches the page and the selection rather than the first page.
        //
        // ponytail: in the path rather than in a field of its own, because `Location` is built in
        // eleven places across the api and lsp crates and every one of them would have to name a
        // field that only a PDF ever fills. `Location::is_url` already reads `path` for a `://`,
        // so a path that is not only a path is the shape this type has. A field is the upgrade if
        // anything else ever needs an anchor.
        let path = match link.anchor.as_deref() {
            Some(a) if markdown::pdf_anchor(a).is_some() => format!("{target}#{a}"),
            _ => target,
        };
        Ok(vec![Location { path, range }])
    }

    /// Where the heading an anchor names sits in `rel`, if it is there at all.
    fn heading(&self, rel: &str, anchor: &str) -> Option<Range> {
        let text = self.text_of(rel).ok()?;
        let headings = markdown::analyze(&text).headings;
        markdown::heading_for(&headings, anchor).map(|h| range_of(&text, &h.range))
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
            // One `[` offers nothing, and nor does a `(` outside a link; `context` is what
            // decides, and it wants the second `[` or the `](` of a link's destination.
            completion_triggers: vec!['[', '#', '('],
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
    fn a_link_destination_offers_headings_and_a_line_start_tag_needs_a_letter() {
        assert_eq!(context("[A](#"), Some((Trigger::Anchor, 4, "")));
        assert_eq!(context("see [A](#Se"), Some((Trigger::Anchor, 8, "Se")));
        assert_eq!(
            context("[x](Other.md#se"),
            Some((Trigger::Anchor, 4, "se")),
            "an anchor into another note starts with its path"
        );
        // A closed link leaves the rest of the line to decide.
        assert_eq!(context("[a](#x) #ta"), Some((Trigger::Tag, 8, "ta")));
        assert_eq!(context("[[N]] #ta"), Some((Trigger::Tag, 6, "ta")));
        // A heading needs `# `, so a letter straight after a single `#` makes a tag.
        assert_eq!(context("#x"), Some((Trigger::Tag, 0, "x")));
        assert_eq!(context("##x"), None);
        assert_eq!(context("# "), None);
    }

    #[test]
    fn an_embed_offers_files_and_a_link_destination_offers_paths() {
        assert_eq!(context("![[x"), Some((Trigger::Embed, 1, "x")));
        assert_eq!(context("see ![[sub/"), Some((Trigger::Embed, 5, "sub/")));
        assert_eq!(context("[t](Att"), Some((Trigger::Path, 4, "Att")));
        assert_eq!(context("![a]("), Some((Trigger::Path, 5, "")));
        assert_eq!(context("[t](#h"), Some((Trigger::Anchor, 4, "h")));

        // A note goes by its stem, anything else by its whole name, and both by their path.
        assert_eq!(
            link_names("sub/Beta.md"),
            ("Beta".to_string(), "sub/Beta".to_string())
        );
        assert_eq!(
            link_names("Attachments/logo.png"),
            ("logo.png".to_string(), "Attachments/logo.png".to_string())
        );
    }

    #[test]
    fn a_hint_checks_wikilinks_by_name_and_markdown_links_by_vault_path() {
        let text = "[[Beta]] [b](../Beta%20Two.md#x) [u](https://e.com) [m](mailto:a@b.c) \
                    [h](#x) [[#h]] [o](../../x.md) [r](/x.md)";
        let checked: Vec<Option<String>> = markdown::analyze(text)
            .links
            .iter()
            .map(|l| checked_target("sub/a.md", l))
            .collect();
        let want = [Some("Beta"), Some("Beta Two.md")];
        assert_eq!(checked[..2], want.map(|t| t.map(String::from)));
        // URLs, pure anchors and paths that leave the vault are not the index's to judge.
        assert!(checked[2..].iter().all(Option::is_none), "{checked:?}");
    }

    #[test]
    fn heading_names_keep_their_order_once_each() {
        let a = markdown::analyze("# B\n## A\n#\n### B\n");
        assert_eq!(heading_names(&a.headings), ["B", "A"]);
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
            syms[1].range.end.line, 6,
            "the last section runs to the end, the line after the final newline included"
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
    fn path_candidates_prefer_the_shortest_path_and_tags_keep_their_order() {
        let paths: Vec<String> = ["sub/Alphabet.md", "Alpha.md", "Beta.md"]
            .map(String::from)
            .into();

        assert_eq!(
            path_candidates(&paths, "alp").0,
            ["Alpha.md", "sub/Alphabet.md"]
        );
        assert_eq!(path_candidates(&paths, "sub/").0, ["sub/Alphabet.md"]);
        assert!(path_candidates(&paths, "zzz").0.is_empty());
        // A typed extension still opens the name.
        let files: Vec<String> = ["b/xlogo.png", "a/deep/logo.png"].map(String::from).into();
        assert_eq!(
            path_candidates(&files, "logo.p").0,
            ["a/deep/logo.png", "b/xlogo.png"]
        );

        // The index counts them, so the order it hands them over in is the one to keep.
        let tags = [("alpha".to_string(), 2), ("alphabet".to_string(), 1)];
        assert_eq!(tag_candidates(&tags, "alp").0, ["alpha", "alphabet"]);
        assert!(tag_candidates(&tags, "b").0.is_empty());
    }

    /// The two halves of "[[...]] gets no suggestions": a query in the middle of a name has to
    /// match, and a list that was cut has to say so or the popup never asks again.
    #[test]
    fn a_query_matches_anywhere_and_a_full_list_says_it_is_not_all_of_them() {
        let paths: Vec<String> = ["Rework.md", "Projects/Groundwork.md", "Beta.md"]
            .map(String::from)
            .into();

        let (hits, more) = path_candidates(&paths, "work");
        assert_eq!(
            hits,
            ["Rework.md", "Projects/Groundwork.md"],
            "what starts with the query first, then the shortest path"
        );
        assert!(!more, "everything that matched fits");

        let many: Vec<String> = (0..COMPLETIONS + 5)
            .map(|n| format!("Note{n}.md"))
            .collect();
        let (hits, more) = path_candidates(&many, "note");
        assert_eq!(hits.len(), COMPLETIONS);
        assert!(more, "the popup has to ask again as the word grows");
    }
}
