//! The provider that answers for a note: the index is the language server.
//!
//! A wikilink is a definition, a backlink is a reference, a heading is a symbol, and a link the
//! index cannot resolve is a diagnostic. Everything the editor used to do to a note through its
//! own code paths happens here instead, so a note and a source file are asked the same questions.
//!
//! The pure half sits at the top and is tested without a vault; the provider below only reads
//! the index and the open documents.

use std::cmp::Reverse;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use anyhow::Result;

use accent_core::fuzzy;
use accent_core::index::Index;
use accent_core::markdown::{self, LinkKind};
use accent_core::path::{self, FileType, basename, parent_dir, stem};

use super::{
    Completion, Completions, Diagnostic, Fold, Fut, Hover, Kind, Language, Location, PdfPages, Pos,
    Range, Severity, Signature, Support, Symbol, byte_of, pos_of, range_of,
};
use crate::{Backlink, Event, Local, locked};

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

/// A link to each block id in `text`, `lead` and `close` being what goes around its `^id`:
/// labelled by the id, with the first line of the block it marks beside it.
pub(crate) fn block_links(text: &str, lead: &str, close: &str, replace: Range) -> Vec<Completion> {
    markdown::block_ids(text)
        .into_iter()
        .map(|b| {
            let link = format!("{lead}^{}", b.id);
            let line = text[b.start..b.marker.start].lines().next().unwrap_or("");
            Completion {
                label: format!("^{}", b.id),
                detail: Some(line.trim().to_string()).filter(|l| !l.is_empty()),
                insert: format!("{link}{close}"),
                filter: Some(link),
                replace,
                ..empty_item()
            }
        })
        .collect()
}

/// A PDF's bookmarks as `(title, page index)`, flattened depth-first.
pub(crate) type Outline = Vec<(String, Option<usize>)>;

/// `[[paper.pdf#page=N]]` for each of a PDF's bookmarks, labelled by the bookmark's title.
///
/// `outline` is the document's, flattened depth-first, as `(title, page index)`. A bookmark with
/// no title has nothing to be named by and one that names no page has nowhere to go, so neither
/// is offered — the way a heading with no text is left out. The page in the anchor counts from 1,
/// which is what [`markdown::pdf_anchor`] reads back.
pub(crate) fn page_links(
    outline: &[(String, Option<usize>)],
    note: &str,
    replace: Range,
) -> Vec<Completion> {
    outline
        .iter()
        .filter_map(|(title, page)| {
            let title = title.trim();
            let page = page.filter(|_| !title.is_empty())?;
            let anchor = format!("#page={}", page + 1);
            Some(Completion {
                label: title.to_string(),
                // What goes in is not what the row reads, so the anchor is shown as well.
                detail: Some(anchor.clone()),
                // Narrowed by the title, which is what the popup is showing.
                filter: Some(format!("[[{note}#{title}")),
                insert: format!("[[{note}{anchor}]]"),
                replace,
                ..empty_item()
            })
        })
        .collect()
}

impl PdfPages {
    /// The bookmarks as [`page_links`] rows where `outline` could be read, and the question
    /// itself, unanswered, where it could not: [`pdf_outline`] reads `None` with no PDF reader.
    pub(crate) fn answer(self, outline: Option<Outline>) -> Completions {
        match outline {
            Some(outline) => Completions {
                items: page_links(&outline, &self.note, self.replace),
                ..Completions::default()
            },
            None => Completions {
                pages: Some(self),
                ..Completions::default()
            },
        }
    }
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
/// judge: a URL, a pure `#anchor`, and a markdown link that climbs past the root, which
/// [`path::resolve`] would otherwise fold back into the vault. A leading `/` is the vault root,
/// as the index, Go to Definition and the preview read it.
fn checked_target(rel: &str, link: &markdown::Link) -> Option<String> {
    let judged = match link.kind {
        LinkKind::Wiki | LinkKind::Embed => true,
        LinkKind::Markdown => {
            link.target.starts_with('/') || path::stays_inside(parent_dir(rel), &link.target)
        }
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
/// The query is matched *anywhere* in the path and need not be contiguous, because it is
/// [`fuzzy`]'s match — the matcher the palette and the file switcher use: `[[work]]` offers
/// `Projects/Rework.md` and `[[dpwk]]` offers `deep-work.md`. It is taken as typed, though
/// ([`fuzzy::Query::as_typed`]): its words in order and `é` not folded to `e`, which is how
/// GtkSourceView narrows the same rows afterwards. What starts with the query still
/// comes first — that is the file the reader most likely means — then the better match, then the
/// shortest path, so a note at the root beats one buried under three directories.
///
/// The `more` half is what makes the popup ask again as the word grows. It used to say the list
/// was complete while handing back twenty of several hundred paths, so the popup narrowed those
/// twenty client-side and everything else stayed unreachable however much was typed.
pub(crate) fn path_candidates(paths: &[String], query: &str) -> (Vec<String>, bool) {
    let (hits, more) = ranked_paths(paths, query);
    (hits.into_iter().map(|i| paths[i].clone()).collect(), more)
}

/// [`path_candidates`] by index into `paths`, for a caller whose list holds more than paths.
fn ranked_paths(paths: &[String], query: &str) -> (Vec<usize>, bool) {
    let mut matcher = fuzzy::Query::as_typed(query, fuzzy::Corpus::Paths);
    let query = query.to_lowercase();
    let mut hits: Vec<(bool, Reverse<u32>, usize, usize)> = paths
        .iter()
        .enumerate()
        .filter_map(|(i, rel)| {
            let score = matcher.score(rel)?;
            // Lowercased only for what matched: it is an allocation per path otherwise. The whole
            // name, so that `logo.p` still opens `logo.png`.
            let low = rel.to_lowercase();
            let opens = basename(&low).starts_with(&query) || low.starts_with(&query);
            Some((!opens, Reverse(score), rel.len(), i))
        })
        .collect();
    // Stable, so paths of equal rank and length keep the index's alphabetical order.
    hits.sort_by_key(|(later, score, len, _)| (*later, *score, *len));
    let more = hits.len() > COMPLETIONS;
    hits.truncate(COMPLETIONS);
    (hits.into_iter().map(|(_, _, _, i)| i).collect(), more)
}

/// Tags matching what has been typed, in the order the index hands them over: most used first.
///
/// Matched the same way a path is, so `#bc` offers `a/bc`: a tag is one word but a nested one is
/// several, and the part that is remembered is rarely the first.
pub(crate) fn tag_candidates(tags: &[(String, i64)], prefix: &str) -> (Vec<String>, bool) {
    let mut matcher = fuzzy::Query::as_typed(prefix, fuzzy::Corpus::Words);
    let hits: Vec<String> = tags
        .iter()
        .filter(|(name, _)| matcher.score(name).is_some())
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

/// Where each backlink in `rows` is written, `text_of` reading its source. The rows arrive
/// grouped by source, so each source's text is read once.
pub(crate) fn placed(
    rows: Vec<Backlink>,
    text_of: impl Fn(&str) -> Result<String>,
) -> Vec<Location> {
    let mut out = Vec::with_capacity(rows.len());
    let (mut read, mut source) = (String::new(), String::new());
    for b in rows {
        if read != b.src_rel_path {
            // Unreadable is not an error here: the link is still worth listing, at 0:0.
            source = text_of(&b.src_rel_path).unwrap_or_default();
            read = b.src_rel_path.clone();
        }
        out.push(Location {
            range: range_of(&source, &(b.byte_start as usize..b.byte_end as usize)),
            path: b.src_rel_path,
            ..Location::default()
        });
    }
    out
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
    /// What a link completion ranks, kept between keystrokes. See [`Notes::corpus`].
    corpora: Mutex<Corpora>,
}

/// What one kind of link completion ranks: `paths`, whose first `named` are files and notes only
/// linked to so far (`missing`), and after those one name per entry of `aliases`.
struct Corpus {
    paths: Vec<String>,
    named: usize,
    missing: Vec<String>,
    aliases: Vec<(String, String)>,
}

/// The two corpora a link completion ranks, as the index held them at `version`.
#[derive(Default)]
struct Corpora {
    version: i64,
    /// `[[`: the notes and PDFs, the notes only linked to and the aliases.
    wiki: Option<Arc<Corpus>>,
    /// `![[` and `](`: every file.
    files: Option<Arc<Corpus>>,
}

impl Notes {
    pub(crate) fn open_at(root: PathBuf, db: &Path, events: Sender<Event>) -> Result<Notes> {
        Ok(Notes {
            root,
            index: Mutex::new(Index::open(db)?),
            docs: Mutex::new(HashMap::new()),
            events,
            corpora: Mutex::default(),
        })
    }

    /// The paths a `[[` completion ranks (`wiki`), or the ones a `![[` or a `](` does, read out
    /// of the index once and then kept while [`Index::data_version`] says nothing has been
    /// written: a reconcile, a save or a move reads them again at the next keystroke.
    ///
    /// The popup asks on every keystroke while its answer is cut at the cap, and reading 39 012
    /// paths out of SQLite was most of what each one cost on the generated 40k-file vault: 59 ms
    /// a keystroke for `](` and `![[`, 30 ms for `[[`, where ranking them is a few.
    fn corpus(&self, index: &Index, wiki: bool) -> Result<Arc<Corpus>> {
        let version = index.data_version()?;
        let mut corpora = locked(&self.corpora);
        if corpora.version != version {
            *corpora = Corpora {
                version,
                ..Corpora::default()
            };
        }
        let slot = match wiki {
            true => &mut corpora.wiki,
            false => &mut corpora.files,
        };
        if let Some(corpus) = slot {
            return Ok(corpus.clone());
        }
        let (mut paths, missing, aliases) = match wiki {
            true => (
                index.note_and_pdf_paths()?,
                index.missing_notes()?,
                index.note_aliases()?,
            ),
            false => (index.file_paths(false)?, Vec::new(), Vec::new()),
        };
        // Behind the files, so one that is there leads a note only linked to at the same rank:
        // the ranking is stable.
        paths.extend(missing.iter().cloned());
        // The aliases last, ranked by their own names, and told apart by where they sit.
        let named = paths.len();
        paths.extend(aliases.iter().map(|(alias, _)| alias.clone()));
        let corpus = Arc::new(Corpus {
            paths,
            named,
            missing,
            aliases,
        });
        *slot = Some(corpus.clone());
        Ok(corpus)
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
                if let Some((note, anchor)) = prefix.split_once('#') {
                    return self.heading_links(rel, note, anchor.starts_with('^'), replace);
                }

                let index = locked(&self.index);
                let corpus = self.corpus(&index, trigger == Trigger::Wiki)?;
                let Corpus {
                    paths,
                    named,
                    missing,
                    aliases,
                } = &*corpus;
                let (ranked, more) = ranked_paths(paths, prefix);
                let mut items = Vec::with_capacity(ranked.len());
                for i in ranked {
                    // The link names the file, as any other row writes it, and reads as the
                    // alias: an alias is a name to find a note by, never a link target.
                    if let Some((alias, rel)) = i.checked_sub(*named).map(|j| &aliases[j]) {
                        let (name, path) = link_names(rel);
                        let target = match index.resolve_target(&name)?.as_ref() == Some(rel) {
                            true => name,
                            false => path,
                        };
                        items.push(Completion {
                            insert: format!("[[{target}|{alias}]]"),
                            detail: Some(rel.clone()),
                            filter: Some(format!("[[{alias}")),
                            label: alias.clone(),
                            kind: Kind::File,
                            replace,
                            ..empty_item()
                        });
                        continue;
                    }
                    let hit = paths[i].clone();
                    // A second link to a note not written yet, spelled from the root the way
                    // New File will place it, so both reach it once it is.
                    if missing.contains(&hit) {
                        let path = markdown::strip_ext(&hit);
                        items.push(Completion {
                            insert: format!("[[{path}]]"),
                            detail: Some(format!("{hit}, not created")),
                            filter: Some(format!("[[{hit}")),
                            label: stem(&hit),
                            kind: Kind::File,
                            replace,
                            ..empty_item()
                        });
                        continue;
                    }
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
                        // name never matches; the path lets a folder narrow it too. The path
                        // as the index holds it, extension and all, because that is what the
                        // row was ranked against: `[[Note.md` has to keep `Note.md` on screen.
                        filter: Some(format!("[[{hit}")),
                        label: name,
                        kind: Kind::File,
                        replace,
                        ..empty_item()
                    });
                }
                Ok(Completions {
                    items,
                    incomplete: more,
                    pages: None,
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
                let text = other.as_deref().unwrap_or(&text);
                if prefix.starts_with('^') {
                    return Ok(Completions {
                        items: block_links(text, &format!("{dest}#"), "", replace),
                        ..Completions::default()
                    });
                }
                let headings = markdown::analyze(text).headings;
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
                    pages: None,
                })
            }
            Trigger::Path => {
                let files = self.corpus(&locked(&self.index), false)?;
                // Matched against vault paths, so what was typed loses its encoding, and the `./`
                // and `../` a relative link opens with, which say where from rather than what.
                let query = markdown::percent_decode(prefix);
                let (hits, more) =
                    path_candidates(&files.paths, query.trim_start_matches(['.', '/']));
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
                    pages: None,
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
                    pages: None,
                })
            }
        }
    }

    /// `[[note#Heading]]` for each heading of the note `note` names, or of this one when it names
    /// none — `[[note#^id]]` for each block id instead once a `^` is typed — and
    /// `[[paper.pdf#page=N]]` for each bookmark when what it names is a PDF. The link keeps the
    /// target as it was typed; one the index cannot find offers nothing, and so does a file that is
    /// neither.
    fn heading_links(
        &self,
        rel: &str,
        note: &str,
        blocks: bool,
        replace: Range,
    ) -> Result<Completions> {
        let target = match note {
            "" => rel.to_string(),
            _ => match locked(&self.index).resolve_target(note)? {
                Some(target) => target,
                None => return Ok(Completions::default()),
            },
        };
        if target.ends_with(".pdf") {
            let outline = pdf_outline(&Local::join(&self.root, &target)?)?;
            let pages = PdfPages {
                rel: target,
                note: note.to_string(),
                replace,
            };
            return Ok(pages.answer(outline));
        }
        if !target.ends_with(".md") {
            return Ok(Completions::default());
        }
        let text = self.text_of(&target)?;
        if blocks {
            return Ok(Completions {
                items: block_links(&text, &format!("[[{note}#"), "]]", replace),
                ..Completions::default()
            });
        }
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
            pages: None,
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
        let a = markdown::analyze(&text);
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
                let resolved = locked(&self.index).resolve_target(&asked)?;
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
        let range = link
            .anchor
            .as_ref()
            .and_then(|anchor| self.anchored(&target, anchor));
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

    /// The file a link the index cannot place names in a tree the walk never enters — a
    /// gitignored `build/`, a `node_modules` — which is the one New File would otherwise write
    /// ([`path::linked_path`]). Asked of the disk here, on the host that has the files.
    fn unwalked(&self, asked: &str) -> Option<String> {
        let path = path::linked_path(asked);
        Local::join(&self.root, &path)
            .ok()?
            .is_file()
            .then_some(path)
    }

    /// Where the heading or the block an anchor names sits in `rel`, if it is there at all.
    fn anchored(&self, rel: &str, anchor: &str) -> Option<Range> {
        let text = self.text_of(rel).ok()?;
        markdown::anchor_range(&text, anchor).map(|r| range_of(&text, &r))
    }

    fn symbols(&self, rel: &str) -> Result<Vec<Symbol>> {
        let text = self.text_of(rel)?;
        Ok(symbols_of(&text, &markdown::analyze(&text).headings))
    }

    fn folds(&self, rel: &str) -> Result<Vec<Fold>> {
        let text = self.text_of(rel)?;
        Ok(folds_of(&text, &markdown::analyze(&text)))
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

    /// A hint here is about the index, not about the text: a `[[link]]` is dangling until the
    /// note it names exists. So a reconcile is a reason to say it all again, with the text the
    /// editor last sent rather than what is on disk.
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

/// The [`Outline`] of the PDF at `path`, or `None` where there is no PDF reader to ask.
///
/// One open of one file, on the thread the keystroke came in on and behind pdfium's global lock,
/// so it happens only once the prefix already resolves to a `.pdf`.
#[cfg(feature = "pdf")]
pub(crate) fn pdf_outline(path: &Path) -> Result<Option<Outline>> {
    Ok(Some(
        accent_core::pdf::PdfDoc::open(path)?
            .outline()?
            .into_iter()
            .map(|entry| (entry.title, entry.page))
            .collect(),
    ))
}

/// Without the `pdf` feature there is no pdfium binding to ask, which is `accent-cli serve` on a
/// host: the question goes back to the window ([`PdfPages`]).
#[cfg(not(feature = "pdf"))]
pub(crate) fn pdf_outline(_path: &Path) -> Result<Option<Outline>> {
    Ok(None)
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

/// The note `rel` holds as a hover shows it: its title in bold, then its first lines that say
/// something, with the frontmatter skipped, and the opening heading too when it is the title.
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
        assert!(checked[2..7].iter().all(Option::is_none), "{checked:?}");
        // A leading `/` is the vault root, as the index and the preview read it.
        assert_eq!(checked[7].as_deref(), Some("x.md"));
    }

    /// A note opening on its title says it once, in bold above the excerpt; a title that is not
    /// the note's opening line leaves the excerpt whole.
    #[test]
    fn a_hover_says_the_title_once() {
        assert_eq!(
            preview("---\ntags: [x]\n---\n\n# Title\n\nBody.\n", "a.md"),
            "**Title**\n\nBody."
        );
        assert_eq!(
            preview("Intro.\n# Title\n", "a.md"),
            "**Title**\n\nIntro.\n# Title"
        );
        assert_eq!(preview("Body.\n", "sub/a.md"), "**a**\n\nBody.");
    }

    #[test]
    fn heading_names_keep_their_order_once_each() {
        let a = markdown::analyze("# B\n## A\n#\n### B\n");
        assert_eq!(heading_names(&a.headings), ["B", "A"]);
    }

    /// `[[paper.pdf#` completes to the document's bookmarks, and what it inserts has to be an
    /// anchor `open_target` can land on.
    #[test]
    fn page_links_name_a_bookmarks_page_from_one() {
        let outline = [
            ("Intro".to_string(), Some(0)),
            ("  Method  ".to_string(), Some(4)),
            ("".to_string(), Some(7)),
            ("Nowhere".to_string(), None),
        ];
        let items = page_links(&outline, "paper.pdf", Range::default());

        assert_eq!(
            items.iter().map(|i| i.label.as_str()).collect::<Vec<_>>(),
            ["Intro", "Method"],
            "a bookmark with no title or no page names nothing to go to"
        );
        assert_eq!(items[1].insert, "[[paper.pdf#page=5]]");
        assert_eq!(items[1].filter.as_deref(), Some("[[paper.pdf#Method"));
        // The anchor is the one the reader lands by: page 5 as written is index 4.
        assert_eq!(markdown::pdf_anchor("page=5"), Some((4, None)));
    }

    /// A side with no PDF reader — a host's `serve` — cannot list the bookmarks, so it answers with
    /// the PDF and the link for whoever holds a copy of the file to list them from.
    #[test]
    fn a_pdf_nobody_here_can_read_is_left_to_the_side_with_a_copy() {
        let pages = PdfPages {
            rel: "Papers/paper.pdf".to_string(),
            note: "paper.pdf".to_string(),
            replace: Range::default(),
        };
        let unread = pages.clone().answer(None);
        assert!(unread.items.is_empty());
        assert_eq!(unread.pages.as_ref(), Some(&pages));

        let read = pages.answer(Some(vec![("Intro".to_string(), Some(0))]));
        assert_eq!(
            read.pages, None,
            "listed, so nothing is left for anyone else"
        );
        assert_eq!(read.items[0].insert, "[[paper.pdf#page=1]]");
    }

    /// A second `[[` to a note that is only linked to so far offers it, spelled the way the first
    /// link will reach it once it is written, behind the note that is there.
    #[test]
    fn a_wikilink_completes_to_a_note_not_written_yet() {
        let vault = tempfile::tempdir().unwrap();
        std::fs::write(vault.path().join("Other.md"), "# Other\n").unwrap();
        std::fs::write(vault.path().join("a.md"), "[[Nowhere/Other]]\n[[Oth\n").unwrap();
        let cache = tempfile::tempdir().unwrap();
        let db = cache.path().join("i.db");
        Index::open(&db)
            .unwrap()
            .reconcile(vault.path(), |_| {})
            .unwrap();
        let notes = Notes::open_at(
            vault.path().to_path_buf(),
            &db,
            std::sync::mpsc::channel().0,
        )
        .unwrap();
        let caret = Pos {
            line: 1,
            character: 5,
        };
        let items = notes.completion("a.md", caret).unwrap().items;

        let rows: Vec<(&str, Option<&str>)> = items
            .iter()
            .map(|i| (i.insert.as_str(), i.detail.as_deref()))
            .collect();
        assert_eq!(
            rows,
            [
                ("[[Other]]", None),
                ("[[Nowhere/Other]]", Some("Nowhere/Other.md, not created"))
            ]
        );
    }

    /// `#^id` goes to the block the id marks, from a wikilink or a markdown link, and one the
    /// note does not have comes back as the anchor it missed; `#^` offers the note's ids.
    #[test]
    fn a_block_anchor_is_followed_and_completed() {
        let vault = tempfile::tempdir().unwrap();
        std::fs::write(
            vault.path().join("Other.md"),
            "# Other\nFirst.\nSecond. ^blk\n",
        )
        .unwrap();
        let a = "[[Other#^blk]] [x](Other.md#^BLK) [[Other#^gone]]\n[[Other#^\n[y](Other.md#^\n";
        std::fs::write(vault.path().join("a.md"), a).unwrap();
        let cache = tempfile::tempdir().unwrap();
        let db = cache.path().join("i.db");
        Index::open(&db)
            .unwrap()
            .reconcile(vault.path(), |_| {})
            .unwrap();
        let notes = Notes::open_at(
            vault.path().to_path_buf(),
            &db,
            std::sync::mpsc::channel().0,
        )
        .unwrap();
        let at = |line, character| Pos { line, character };
        let landed = |pos| {
            let loc = notes.definition("a.md", pos).unwrap().remove(0);
            (loc.path, loc.range.start, loc.range.end, loc.anchor)
        };
        let block = ("Other.md".into(), at(1, 0), at(2, 12), None);
        assert_eq!(landed(at(0, 3)), block);
        assert_eq!(landed(at(0, 17)), block, "a markdown link, in any case");
        assert_eq!(
            landed(at(0, 38)),
            ("Other.md".into(), at(0, 0), at(0, 0), Some("^gone".into()))
        );

        let rows = |pos| -> Vec<(String, Option<String>, String)> {
            let items = notes.completion("a.md", pos).unwrap().items;
            items
                .into_iter()
                .map(|i| (i.label, i.detail, i.insert))
                .collect()
        };
        let row = |insert: &str| ("^blk".into(), Some("First.".into()), insert.into());
        assert_eq!(rows(at(1, 9)), [row("[[Other#^blk]]")]);
        assert_eq!(rows(at(2, 14)), [row("Other.md#^blk")]);
    }

    /// The paths a completion ranks are kept between keystrokes, and read again once the index
    /// has been written: a note that arrives while the popup is up is offered at the next one.
    #[test]
    fn a_completion_reads_the_paths_again_once_the_index_changes() {
        let vault = tempfile::tempdir().unwrap();
        std::fs::write(vault.path().join("Other.md"), "# Other\n").unwrap();
        std::fs::write(vault.path().join("a.md"), "[[Oth\n").unwrap();
        let cache = tempfile::tempdir().unwrap();
        let db = cache.path().join("i.db");
        let mut writer = Index::open(&db).unwrap();
        writer.reconcile(vault.path(), |_| {}).unwrap();
        let notes = Notes::open_at(
            vault.path().to_path_buf(),
            &db,
            std::sync::mpsc::channel().0,
        )
        .unwrap();
        let caret = Pos {
            line: 0,
            character: 5,
        };
        let labels = || -> Vec<String> {
            let items = notes.completion("a.md", caret).unwrap().items;
            items.into_iter().map(|i| i.label).collect()
        };
        assert_eq!(labels(), ["Other"]);
        assert_eq!(labels(), ["Other"], "kept, and the same");

        std::fs::write(vault.path().join("Otherwise.md"), "# Otherwise\n").unwrap();
        writer.reconcile(vault.path(), |_| {}).unwrap();
        assert_eq!(labels(), ["Other", "Otherwise"]);
    }

    /// GtkSourceView narrows the popup a second time, keeping a row only while what was typed
    /// since the `[[` is a case-insensitive subsequence of its `filter`. The provider ranks a note
    /// by its path with the extension on, so the filter has to carry the extension as well, or
    /// `[[Other.md` ranked the note and the popup then dropped it.
    #[test]
    fn a_typed_extension_keeps_the_note_row() {
        let vault = tempfile::tempdir().unwrap();
        std::fs::write(vault.path().join("Other.md"), "# Other\n").unwrap();
        std::fs::write(vault.path().join("a.md"), "[[Nowhere/Other]]\n[[Other.md\n").unwrap();
        let cache = tempfile::tempdir().unwrap();
        let db = cache.path().join("i.db");
        Index::open(&db)
            .unwrap()
            .reconcile(vault.path(), |_| {})
            .unwrap();
        let notes = Notes::open_at(
            vault.path().to_path_buf(),
            &db,
            std::sync::mpsc::channel().0,
        )
        .unwrap();
        let caret = Pos {
            line: 1,
            character: 10,
        };
        let items = notes.completion("a.md", caret).unwrap().items;

        let inserts: Vec<&str> = items.iter().map(|i| i.insert.as_str()).collect();
        assert_eq!(inserts, ["[[Other]]", "[[Nowhere/Other]]"]);
        // GtkSourceView's `fuzzy_match`, which the popup runs over every row it is handed.
        let kept = |filter: &str| {
            let mut rest = filter.chars().flat_map(char::to_lowercase);
            "[[other.md".chars().all(|c| rest.any(|f| f == c))
        };
        for item in &items {
            let filter = item.filter.as_deref().unwrap_or(&item.label);
            assert!(kept(filter), "the popup would drop {filter:?}");
        }
    }

    /// A front matter alias is offered by its own name and writes a link to the file, spelled as
    /// any `[[` completion spells it, that reads as the alias: an alias is never a link target.
    #[test]
    fn a_wikilink_completes_an_alias_to_its_note() {
        let vault = tempfile::tempdir().unwrap();
        std::fs::create_dir(vault.path().join("sub")).unwrap();
        std::fs::write(
            vault.path().join("sub/Real Name.md"),
            "---\naliases: [Nickname]\n---\n",
        )
        .unwrap();
        std::fs::write(vault.path().join("a.md"), "[[Nick\n").unwrap();
        let cache = tempfile::tempdir().unwrap();
        let db = cache.path().join("i.db");
        Index::open(&db)
            .unwrap()
            .reconcile(vault.path(), |_| {})
            .unwrap();
        let notes = Notes::open_at(
            vault.path().to_path_buf(),
            &db,
            std::sync::mpsc::channel().0,
        )
        .unwrap();
        let caret = Pos {
            line: 0,
            character: 6,
        };
        let items = notes.completion("a.md", caret).unwrap().items;

        let rows: Vec<(&str, &str, Option<&str>)> = items
            .iter()
            .map(|i| (i.label.as_str(), i.insert.as_str(), i.detail.as_deref()))
            .collect();
        assert_eq!(
            rows,
            [(
                "Nickname",
                "[[Real Name|Nickname]]",
                Some("sub/Real Name.md")
            )]
        );
    }

    /// The whole `[[paper.pdf#` path: the index resolves the name, pdfium reads the outline, and
    /// what goes in is an anchor the reader can land by. Skipped where there is no libpdfium.
    #[cfg(feature = "pdf")]
    #[test]
    fn a_wiki_anchor_into_a_pdf_offers_its_bookmarks() {
        if !accent_core::pdf::available() {
            eprintln!("skipping: no libpdfium");
            return;
        }
        let vault = tempfile::tempdir().unwrap();
        std::fs::write(vault.path().join("paper.pdf"), bookmarked_pdf()).unwrap();
        std::fs::write(vault.path().join("a.md"), "[[paper.pdf#\n").unwrap();
        let cache = tempfile::tempdir().unwrap();
        let db = cache.path().join("i.db");
        Index::open(&db)
            .unwrap()
            .reconcile(vault.path(), |_| {})
            .unwrap();

        let notes = Notes::open_at(
            vault.path().to_path_buf(),
            &db,
            std::sync::mpsc::channel().0,
        )
        .unwrap();
        let items = notes
            .completion(
                "a.md",
                Pos {
                    line: 0,
                    character: 12,
                },
            )
            .unwrap()
            .items;

        assert_eq!(items.len(), 1, "{items:?}");
        assert_eq!(items[0].label, "Second");
        assert_eq!(items[0].insert, "[[paper.pdf#page=2]]");
    }

    /// Two blank pages and one bookmark on the second, written by hand so the test needs no
    /// fixture file. `crates/core/src/pdf/tests.rs` builds a fuller document the same way.
    #[cfg(feature = "pdf")]
    fn bookmarked_pdf() -> Vec<u8> {
        let objs = [
            "<</Type/Catalog/Pages 2 0 R/Outlines 4 0 R>>",
            "<</Type/Pages/Kids[3 0 R 5 0 R]/Count 2>>",
            "<</Type/Page/Parent 2 0 R/MediaBox[0 0 200 100]>>",
            "<</Type/Outlines/First 6 0 R/Last 6 0 R/Count 1>>",
            "<</Type/Page/Parent 2 0 R/MediaBox[0 0 200 100]>>",
            "<</Title(Second)/Parent 4 0 R/Dest[5 0 R /XYZ 0 80 0]>>",
        ];
        let mut out = String::from("%PDF-1.4\n");
        let mut offsets = Vec::new();
        for (i, o) in objs.iter().enumerate() {
            offsets.push(out.len());
            out.push_str(&format!("{} 0 obj\n{o}\nendobj\n", i + 1));
        }
        let xref = out.len();
        out.push_str(&format!(
            "xref\n0 {}\n0000000000 65535 f \n",
            objs.len() + 1
        ));
        for off in &offsets {
            out.push_str(&format!("{off:010} 00000 n \n"));
        }
        out.push_str(&format!(
            "trailer\n<</Size {}/Root 1 0 R>>\nstartxref\n{xref}\n%%EOF\n",
            objs.len() + 1
        ));
        out.into_bytes()
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
        // Not only from the start: the part of a nested tag one remembers is rarely the first.
        let nested = [("a/bc".to_string(), 1), ("other".to_string(), 1)];
        assert_eq!(tag_candidates(&nested, "bc").0, ["a/bc"]);
        assert!(tag_candidates(&tags, "z").0.is_empty());
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
        // Nor does it have to be contiguous, since the matcher is the palette's.
        let scattered: Vec<String> = ["a-bc.md", "Deep-Work.md"].map(String::from).into();
        assert_eq!(path_candidates(&scattered, "bc").0, ["a-bc.md"]);
        assert_eq!(path_candidates(&scattered, "dpwk").0, ["Deep-Work.md"]);

        let many: Vec<String> = (0..COMPLETIONS + 5)
            .map(|n| format!("Note{n}.md"))
            .collect();
        let (hits, more) = path_candidates(&many, "note");
        assert_eq!(hits.len(), COMPLETIONS);
        assert!(more, "the popup has to ask again as the word grows");
    }
}
