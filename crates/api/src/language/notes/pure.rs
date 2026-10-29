//! The pure half: what a note's text and the index's rows say, worked out without a vault.

use std::cmp::Reverse;

use crate::Result;

use accent_core::fuzzy;
use accent_core::markdown::{self, LinkKind};
use accent_core::org;
use accent_core::path::{self, FileType, basename, parent_dir, stem};

use super::{COMPLETIONS, empty_item};
use crate::Backlink;
use crate::language::{
    Completion, Completions, Fold, Location, PdfPages, Range, Symbol, pos_of, range_of,
};

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

/// Find the completion trigger, its offset in `head`, and the typed suffix.
/// `head` ends at the caret. Prefer `[[`, then markdown destinations, so
/// anchor `#` characters are not tags. Return `None` for no trigger, ATX
/// heading markers or tags the caret has passed.
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

/// Where the fragment `context` finds before `caret` ends on `line`, past the caret over what is
/// still of it: a link's target up to the `]]` closing it, a destination up to its `)`, a tag's
/// own characters (`tag`). Read whole it has to start where it did, or it ends at the caret.
pub(super) fn fragment_end(
    line: &str,
    caret: usize,
    context: fn(&str) -> Option<(Trigger, usize, &str)>,
    tag: fn(char) -> bool,
) -> usize {
    let Some((trigger, start, _)) = context(&line[..caret]) else {
        return caret;
    };
    let after = &line[caret..];
    let rest = match trigger {
        // A link no `]]` closes ends at the caret: what follows it is prose.
        Trigger::Wiki | Trigger::Embed => after
            .find(']')
            .filter(|&i| after[i..].starts_with("]]"))
            .unwrap_or(0),
        Trigger::Anchor | Trigger::Path => after
            .find(|c: char| c == ')' || c.is_whitespace())
            .unwrap_or(after.len()),
        Trigger::Tag => after.find(|c: char| !tag(c)).unwrap_or(after.len()),
    };
    let end = caret + rest;
    match context(&line[..end]) {
        Some((_, at, _)) if at == start => end,
        _ => caret,
    }
}

/// Where the tag being typed at `caret` starts, when that is an item of the front matter's `tags:`
/// list, read as `markdown::frontmatter` reads it, and whether the list is the flow kind
/// (`tags: [a, b`), whose items a `,` ends. An item of the block kind (`- a` under a bare `tags:`)
/// is one tag to its end, commas and all, as YAML reads it.
pub(super) fn front_tag(text: &str, caret: usize) -> Option<(usize, bool)> {
    let body = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))?;
    let lines: Vec<&str> = body[..caret.checked_sub(text.len() - body.len())?]
        .split('\n')
        .map(|l| l.trim_end_matches('\r'))
        .collect();
    let (head, above) = lines.split_last()?;
    if above.iter().any(|l| matches!(l.trim_end(), "---" | "...")) {
        return None;
    }
    let item = |at: usize| {
        let lead = head[at..].len() - head[at..].trim_start().len();
        let start = caret - head.len() + at + lead;
        (!text[start..caret].starts_with(['"', '\''])).then_some(start)
    };
    if let Some(rest) = head.strip_prefix("tags:")
        && let Some(open) = rest.trim_start().strip_prefix('[')
    {
        if open.contains(']') {
            return None;
        }
        let at = head.len() - open.len() + open.rfind(',').map_or(0, |i| i + 1);
        return item(at).map(|start| (start, true));
    }
    let dash = head.len() - head.trim_start().len();
    let after = head[dash..].strip_prefix('-')?;
    if !after.starts_with([' ', '\t']) {
        return None;
    }
    // The key the list hangs off: the first line above that is no item of it.
    let key = above
        .iter()
        .rev()
        .find(|l| !l.starts_with([' ', '\t', '-']))?;
    (key.trim_end() == "tags:")
        .then(|| item(dash + 1))?
        .map(|start| (start, false))
}

/// `name` as an item of a YAML list, quoted only where YAML would read it otherwise: a `,`, a
/// bracket or a brace in a flow list, and anywhere a `: `, a ` #`, an indicator in front or a
/// blank at either end.
pub(super) fn yaml_item(name: &str, flow: bool) -> String {
    let plain = !name
        .starts_with(|c: char| "-?:,[]{}#&*!|>'\"%@`".contains(c) || c.is_whitespace())
        && !name.ends_with(char::is_whitespace)
        && !name.contains(": ")
        && !name.contains(" #")
        && !(flow && name.contains([',', '[', ']', '{', '}']));
    match plain && !name.is_empty() {
        true => name.to_string(),
        false => format!("\"{}\"", name.replace('\\', "\\\\").replace('"', "\\\"")),
    }
}

/// [`context`] in an org file: `[[` opens a link to a file, and a `:` in the tags closing a
/// heading or on the `#+FILETAGS:` line a tag, the start being past that colon. A run of tags
/// opens with a colon after a blank, so `10:30` in a title is no tag.
pub(crate) fn org_context(head: &str) -> Option<(Trigger, usize, &str)> {
    if let Some(start) = head.rfind("[[") {
        let prefix = &head[start + 2..];
        if !prefix.contains(']') {
            return Some((Trigger::Wiki, start, prefix));
        }
    }
    let heading = head.starts_with('*') && head.trim_start_matches('*').starts_with([' ', '\t']);
    let filetags = head
        .get(..11)
        .is_some_and(|k| k.eq_ignore_ascii_case("#+filetags:"));
    let colon = head.rfind(':').filter(|_| heading || filetags)?;
    let prefix = &head[colon + 1..];
    let run = head[..colon].trim_end_matches(|c| c == ':' || org::is_tag_char(c));
    (prefix.chars().all(org::is_tag_char)
        && run.ends_with([' ', '\t'])
        && head[run.len()..].starts_with(':'))
    .then_some((Trigger::Tag, colon + 1, prefix))
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

/// Offer `[[paper.pdf#page=N]]` bookmark rows from depth-first `outline`
/// (title, zero-based page index). Skip untitled or pageless bookmarks.
/// Anchors use one-based pages, as [`markdown::pdf_anchor`] expects.
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
    /// Return [`page_links`] when [`pdf_outline`] can read bookmarks; otherwise
    /// defer the question to a side with a PDF reader.
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

/// Normalize a link in `rel` for the index: markdown paths are relative
/// to the source note's folder and must become vault paths.
pub(super) fn target_of(rel: &str, link: &markdown::Link) -> String {
    match link.kind {
        LinkKind::Markdown => path::resolve(parent_dir(rel), &link.target),
        _ => link.target.clone(),
    }
}

/// Return an index-checkable target in `rel`. Exclude URLs, pure anchors
/// and markdown paths escaping the vault, which [`path::resolve`] would
/// otherwise fold back inside. A leading `/` means the vault root.
pub(super) fn checked_target(rel: &str, link: &markdown::Link) -> Option<String> {
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

/// Whether `rel` has headings a link's `#` can name: a note, or an org file.
pub(super) fn has_headings(rel: &str) -> bool {
    path::is_note(rel) || path::is_org(rel)
}

/// Rank path candidates using [`fuzzy::Query::as_typed`], preserving word
/// order while allowing noncontiguous matches anywhere in the path. Prefer
/// prefixes, then match quality, then shorter paths. Return whether the list
/// was capped so the popup requeries as typing narrows the candidates.
pub(crate) fn path_candidates(paths: &[String], query: &str) -> (Vec<String>, bool) {
    let (hits, more) = ranked_paths(paths, query);
    (hits.into_iter().map(|i| paths[i].clone()).collect(), more)
}

/// [`path_candidates`] by index into `paths`, for a caller whose list holds more than paths.
pub(super) fn ranked_paths(paths: &[String], query: &str) -> (Vec<usize>, bool) {
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

/// Match tags like paths, allowing suffix matches such as `#bc` for `a/bc`.
/// Preserve index order, with most-used tags first.
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
    // The byte before the next heading is the newline that ends the section's last line.
    markdown::section_end(headings, i, text.len())
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
