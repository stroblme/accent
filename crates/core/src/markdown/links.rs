//! Link targets: what a `[[wikilink]]` or `[text](target)` points at, the keys a file answers
//! to, and rewriting the links a move leaves pointing at the wrong place, or a PDF's page edit at
//! the wrong page.

use super::{Analysis, Heading, Link, LinkKind, Pending, Style, analyze, block_ids};
use crate::page_edit::PageEdit;
use crate::path::{self, basename, parent_dir};
use pulldown_cmark::LinkType;
use std::collections::{HashMap, HashSet};
use std::ops::Range;

pub(super) fn pending(
    range: Range<usize>,
    dest: &str,
    link_type: LinkType,
    image: bool,
) -> Pending {
    match link_type {
        LinkType::WikiLink { has_pothole } => {
            let (target, anchor) = split_anchor(dest);
            Pending {
                range,
                kind: if image {
                    LinkKind::Embed
                } else {
                    LinkKind::Wiki
                },
                target: target.to_string(),
                anchor: anchor.map(str::to_string),
                aliased: has_pothole,
                text: String::new(),
            }
        }
        _ if is_external(dest) => Pending {
            range,
            kind: LinkKind::External,
            target: dest.to_string(),
            anchor: None,
            aliased: true,
            text: String::new(),
        },
        _ => {
            let (target, anchor) = split_anchor(dest);
            Pending {
                range,
                kind: LinkKind::Markdown,
                target: percent_decode(target),
                anchor: anchor.map(percent_decode),
                aliased: true,
                text: String::new(),
            }
        }
    }
}

pub(super) fn split_anchor(dest: &str) -> (&str, Option<&str>) {
    match dest.split_once('#') {
        Some((t, a)) => (t, Some(a)),
        None => (dest, None),
    }
}

/// Read a PDF anchor: `page=3` or `page=3&selection=4,0,4,11`, as Obsidian writes them.
///
/// The page comes back **zero-based**, the way [`crate::pdf::Selection`] counts, and the four
/// selection numbers in the order the link spells them. `None` for a heading or a block anchor,
/// which is what tells a link to a note apart from a link into a PDF.
///
/// Here rather than in `pdf.rs` because this is link syntax, not PDF geometry: the index parses
/// it without the `pdf` feature, and so does a build with no libpdfium at all.
pub fn pdf_anchor(anchor: &str) -> Option<(usize, Option<[usize; 4]>)> {
    let (mut page, mut selection) = (None, None);
    for part in anchor.split('&') {
        match part.split_once('=') {
            Some(("page", n)) => page = n.trim().parse::<usize>().ok()?.checked_sub(1),
            // Four numbers or none: a selection we cannot read is a link to the page, which is
            // still where the reader wanted to go.
            Some(("selection", list)) => {
                selection = list
                    .split(',')
                    .map(|n| n.trim().parse::<usize>().ok())
                    .collect::<Option<Vec<_>>>()
                    .and_then(|nums| <[usize; 4]>::try_from(nums).ok());
            }
            _ => {}
        }
    }
    Some((page?, selection))
}

/// The anchor GitHub gives a heading: lowercased, everything but a letter, a digit, a space, `-`
/// and `_` dropped, and every space a `-`. `## Hello, World!` is `#hello-world`.
fn slug(text: &str) -> String {
    text.trim()
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_'))
        .map(|c| if c == ' ' { '-' } else { c })
        .collect()
}

/// Each heading's slug, in the order the note has them. A repeat gets the first free `-1`, `-2`,
/// … the way GitHub tells two `## Notes` apart, so every heading has an anchor of its own.
pub fn slugs<'a>(headings: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut taken = HashSet::new();
    let mut out = Vec::new();
    for text in headings {
        let base = slug(text);
        let (mut s, mut n) = (base.clone(), 0);
        while taken.contains(&s) {
            n += 1;
            s = format!("{base}-{n}");
        }
        taken.insert(s.clone());
        out.push(s);
    }
    out
}

/// The heading an anchor names: by its slug first, as `[text](#my-section)` writes it, then by
/// its text, as `[[Note#My Section]]` and an older `[text](#My%20Section)` do.
pub fn heading_for<'a>(headings: &'a [Heading], anchor: &str) -> Option<&'a Heading> {
    let anchor = anchor.trim();
    slugs(headings.iter().map(|h| h.text.as_str()))
        .iter()
        .position(|s| s.eq_ignore_ascii_case(anchor))
        .map(|i| &headings[i])
        .or_else(|| {
            headings
                .iter()
                .find(|h| h.text.trim().eq_ignore_ascii_case(anchor))
        })
}

/// Where an anchor lands in a note: from the block a `^id` marks through its id, or else the
/// heading [`heading_for`] finds. Go to Definition and a click in the preview both land here.
pub fn anchor_range(text: &str, anchor: &str) -> Option<Range<usize>> {
    match anchor.trim().strip_prefix('^') {
        Some(id) => block_ids(text)
            .into_iter()
            .find(|b| b.id.eq_ignore_ascii_case(id))
            .map(|b| b.start..b.marker.end),
        None => heading_for(&analyze(text).headings, anchor).map(|h| h.range.clone()),
    }
}

/// `scheme:` or `//host` — anything with an authority is not a vault path.
fn is_external(dest: &str) -> bool {
    dest.starts_with("//") || scheme(dest).is_some()
}

/// The scheme `dest` starts with and what follows its `:`, as RFC 3986 spells a scheme: a letter,
/// then letters, digits, `+`, `-` and `.`.
fn scheme(dest: &str) -> Option<(&str, &str)> {
    let (s, rest) = dest.split_once(':')?;
    let named = s.starts_with(|c: char| c.is_ascii_alphabetic())
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'+' | b'-' | b'.'));
    named.then_some((s, rest))
}

/// Whether a link target is handed to the system rather than followed in the vault: a
/// `scheme://` address (the web, but also `ftp://`, `zotero://`, …), or a `mailto:`, `tel:` or
/// `sms:` one. Never `javascript:` or `data:`, which carry what they run, `file:`, which reads the
/// disk, or `accent:`, the preview's own scheme — whatever slashes follow them. The editor's Go
/// to Definition and a click in the preview both ask this, so a link leaves the app the same way
/// from either.
pub fn is_url(target: &str) -> bool {
    let Some((name, rest)) = scheme(target) else {
        return false;
    };
    match name.to_ascii_lowercase().as_str() {
        "javascript" | "data" | "file" | "accent" => false,
        "mailto" | "tel" | "sms" => !rest.is_empty(),
        _ => rest.starts_with("//"),
    }
}

pub fn percent_decode(s: &str) -> String {
    if !s.contains('%') {
        return s.to_string();
    }
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let (Some(h), Some(l)) = (
                (b[i + 1] as char).to_digit(16),
                (b[i + 2] as char).to_digit(16),
            )
        {
            out.push((h * 16 + l) as u8);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `s` with every byte but an ASCII letter or digit, `-._~` and `/` written as `%XX`: what a
/// markdown link's destination can hold with no space or parenthesis to end it early.
pub fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

const IMAGE_EXT: [&str; 8] = ["png", "jpg", "jpeg", "gif", "svg", "webp", "bmp", "avif"];

/// Whether a link target names an image, by extension: `![[x.png]]` embeds, `![[x.pdf]]` links.
pub fn is_image(target: &str) -> bool {
    target
        .rsplit_once('.')
        .is_some_and(|(_, e)| IMAGE_EXT.contains(&e.to_ascii_lowercase().as_str()))
}

/// `"a/b/c.md"` -> `"a/b/c"`; a name without a real extension is returned unchanged.
pub fn strip_ext(s: &str) -> String {
    match s.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && !ext.contains('/') => stem.to_string(),
        _ => s.to_string(),
    }
}

/// Normalise a link target the way Obsidian compares them: trimmed, `./` dropped, `\` as `/`,
/// case-folded. Both sides of link resolution go through this, so it is the one place that
/// decides what "the same target" means.
pub fn link_key(target: &str) -> String {
    target
        .trim()
        .trim_start_matches("./")
        .replace('\\', "/")
        .to_lowercase()
}

/// Every key a file answers to: full path, path without extension, basename, basename without
/// extension. Duplicates are kept out so callers can insert blindly.
pub fn path_keys(rel: &str) -> Vec<String> {
    let base = rel.rsplit('/').next().unwrap_or(rel).to_string();
    let mut keys = Vec::with_capacity(4);
    for k in [
        rel.to_string(),
        strip_ext(rel),
        base.clone(),
        strip_ext(&base),
    ] {
        let k = link_key(&k);
        if !keys.contains(&k) {
            keys.push(k);
        }
    }
    keys
}

/// Rewrite every link in the note `src_old` that a move leaves naming the wrong place, as the
/// note will read once it is at `src_new`, or `None` when no link needs it.
///
/// `targets` is the file each link key resolved to before the move — only the index can say,
/// so the caller asks it first — and `moves` is where each moved file went, old path to new. A
/// link is rewritten when, read from where its note now is, it would no longer name the file it
/// named: a bare `[[Note]]` survives a pure move and `[[Dir/Note]]` does not, and a relative
/// `[t](../a.png)` follows its own note as much as the image. A wikilink or an embed keeps its
/// author's spelling ([`as_written`]); a markdown link becomes the relative path from the
/// note's folder, percent-encoded, its `#anchor` untouched.
///
/// Two path forms the parser does not hand over as links are scanned for as well: reference-style
/// `[ref]: path "title"` definitions and the `src`/`href` of the HTML a note holds
/// ([`scanned_paths`]).
pub fn rewrite_moved(
    text: &str,
    src_old: &str,
    src_new: &str,
    targets: &HashMap<String, String>,
    moves: &HashMap<String, String>,
) -> Option<String> {
    let m = Moved {
        dir_old: parent_dir(src_old),
        dir_new: parent_dir(src_new),
        targets,
        moves,
    };
    let analysis = analyze(text);
    let mut edits: Vec<(Range<usize>, String)> = Vec::new();
    for link in &analysis.links {
        let edit = match link.kind {
            LinkKind::Wiki | LinkKind::Embed => {
                let key = link_key(&link.target);
                let open = if link.kind == LinkKind::Embed { 3 } else { 2 };
                let at = link.range.start + open..link.range.start + open + link.target.len();
                // A parser surprise must never corrupt a note: only touch bytes that are the
                // target.
                moved(&key, targets, moves)
                    // By name from anywhere, so a bare `[[Note]]` still finds a note that moved.
                    .filter(|(_, new)| !path_keys(new).contains(&key))
                    .filter(|_| text.get(at.clone()) == Some(link.target.as_str()))
                    .map(|(old, new)| (at, as_written(&link.target, old, new)))
            }
            LinkKind::Markdown => markdown_edit(text, link, &m),
            LinkKind::External => None,
        };
        edits.extend(edit);
    }
    for at in scanned_paths(text, &analysis) {
        edits.extend(m.edit(text, at));
    }
    // Back to front, so the earlier offsets stay valid. Links nest (an image inside a link), but
    // the bytes rewritten for each never overlap.
    edits.sort_by_key(|(at, _)| std::cmp::Reverse(at.start));
    let mut out = text.to_string();
    for (at, with) in edits {
        out.replace_range(at, &with);
    }
    (out != text).then_some(out)
}

/// `(old path, new path)` of the file a link found by `key` before the move; the same path
/// twice when that file stays where it is.
fn moved<'a>(
    key: &str,
    targets: &'a HashMap<String, String>,
    moves: &'a HashMap<String, String>,
) -> Option<(&'a str, &'a str)> {
    let old = targets.get(key)?;
    Some((old, moves.get(old).unwrap_or(old)))
}

/// What one move is, for the three path forms that have to follow it.
struct Moved<'a> {
    /// The note's folder before the move, and after it.
    dir_old: &'a str,
    dir_new: &'a str,
    targets: &'a HashMap<String, String>,
    moves: &'a HashMap<String, String>,
}

impl Moved<'_> {
    /// What `written` — a path as its author spelled it, relative to the note's folder or rooted
    /// at the vault — has to become to go on naming the file it named, or `None` when it names it
    /// still. The rule an inline link, a reference-style definition and an HTML attribute share.
    ///
    /// Nothing outside the vault is reachable through it: a path is only ever rewritten when the
    /// index resolved it to a file *before* the move, which is what leaves an `https://` URL, a
    /// bare `#anchor` and a `../..` past the root alone.
    fn repoint(&self, written: &str) -> Option<String> {
        let key = path_key(self.dir_old, written)?;
        let (old, new) = moved(&key, self.targets, self.moves)?;
        // A path, so it still finds the file only if it spells that file's path from the note's
        // new folder: the index's by-name fallback is not something a markdown reader shares.
        let rooted = written.starts_with('/');
        let now = link_key(&path::resolve(self.dir_new, written));
        if (rooted || path::stays_inside(self.dir_new, written))
            && [link_key(new), link_key(&strip_ext(new))].contains(&now)
        {
            return None;
        }
        let to = match rooted {
            true => format!("/{new}"),
            false => path::relative(self.dir_new, new),
        };
        Some(ext_as_written(written, old, &to))
    }

    /// [`Moved::repoint`] over the bytes at `at`, which are a destination as written in the note.
    /// Its `#anchor` is not part of the path and stays where the author put it, as a markdown
    /// link's does.
    fn edit(&self, text: &str, at: Range<usize>) -> Option<(Range<usize>, String)> {
        let written = split_anchor(text.get(at.clone())?).0;
        let to = self.repoint(&percent_decode(written))?;
        Some((at.start..at.start + written.len(), percent_encode(&to)))
    }
}

/// The edit that points a markdown link back at its file. The link is written relative to its
/// note's folder, so it is looked up the way the index stores it: resolved from that folder.
fn markdown_edit(text: &str, link: &Link, m: &Moved) -> Option<(Range<usize>, String)> {
    let to = m.repoint(&link.target)?;
    Some((destination(text, link)?, percent_encode(&to)))
}

/// The key a path written in a note in `dir` is looked up by — from that folder, or from the vault
/// root when it starts with `/` — or `None` for an empty one and one that climbs out of the vault.
fn path_key(dir: &str, written: &str) -> Option<String> {
    let rooted = written.starts_with('/');
    (!written.is_empty() && (rooted || path::stays_inside(dir, written)))
        .then(|| link_key(&path::resolve(dir, written)))
}

/// The key of every path the note at `src` spells in `text` — a markdown link's, a reference
/// definition's, an HTML `src` or `href` — as [`rewrite_moved`] and [`repage_links`] look them up
/// in their `targets`. For a caller that resolves them itself, the text not being what the index
/// holds for the note: Save As writes a tab's unsaved edits too.
pub fn path_link_keys(text: &str, src: &str) -> Vec<String> {
    let dir = parent_dir(src);
    let a = analyze(text);
    let links = a
        .links
        .iter()
        .filter(|l| l.kind == LinkKind::Markdown)
        .map(|l| l.target.clone());
    let scanned = scanned_paths(text, &a)
        .into_iter()
        .filter_map(|at| Some(percent_decode(split_anchor(text.get(at)?).0)));
    links
        .chain(scanned)
        .filter_map(|written| path_key(dir, &written))
        .collect()
}

/// Where the two path forms the parser does not hand over as links sit in `text`, each with its
/// `#anchor`: the destination of every reference-style definition, then every `src` and `href` of
/// the HTML the note holds.
fn scanned_paths(text: &str, a: &Analysis) -> Vec<Range<usize>> {
    let mut out = definitions(text, a);
    out.extend(html_values(text, a));
    out
}

/// The destinations of the reference-style definitions: `[ref]: path "title"` on a line of its own.
///
/// pulldown-cmark resolves a definition into the links that use it and never says where the
/// definition itself sits, so this is a scan of its own: a line indented at most three spaces —
/// four would make it code — whose `[label]` is closed by `]:`, outside anything verbatim. A
/// footnote's `[^1]:` is a definition of another kind and its text is no path. The destination is
/// read to the next space; a title after it, and a `<…>` destination's brackets, are left out.
fn definitions(text: &str, a: &Analysis) -> Vec<Range<usize>> {
    let skip = verbatim(a);
    let mut out = Vec::new();
    let mut start = 0;
    for line in text.split_inclusive('\n') {
        let at = definition_destination(line).map(|r| start + r.start..start + r.end);
        start += line.len();
        out.extend(at.filter(|at| !skip.iter().any(|r| r.contains(&at.start))));
    }
    out
}

/// Where a reference-style definition's destination sits in `line`, if the line is one.
fn definition_destination(line: &str) -> Option<Range<usize>> {
    let indent = line.len() - line.trim_start().len();
    let rest = match indent <= 3 {
        true => line[indent..].strip_prefix('[')?,
        false => return None,
    };
    if rest.starts_with('^') {
        return None;
    }
    let close = rest.find("]:")?;
    let after = &rest[close + 2..];
    let lead = after.len() - after.trim_start().len();
    let value = &after[lead..];
    let (open, len) = match value.strip_prefix('<') {
        Some(inner) => (1, inner.find('>')?),
        None => (0, value.find(char::is_whitespace).unwrap_or(value.len())),
    };
    let start = indent + 1 + close + 2 + lead + open;
    (len > 0).then_some(start..start + len)
}

/// The `src` and `href` values in the HTML a note holds — a block of it, or an inline tag.
/// pulldown-cmark hands HTML over as opaque text, so these are scanned too.
fn html_values(text: &str, a: &Analysis) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    for span in a.spans.iter().filter(|s| s.style == Style::Html) {
        let base = span.range.start;
        out.extend(
            attribute_values(&text[span.range.clone()])
                .into_iter()
                .map(|at| base + at.start..base + at.end),
        );
    }
    out
}

/// Every `src=` or `href=` value in `chunk`, as ranges into it.
///
/// A small scanner rather than an HTML parser, which is enough because nothing it finds is
/// rewritten unless [`Moved::repoint`] recognises it as a file in the vault: the name preceded by
/// whitespace, so a `data-src` is not one, an `=`, and a value in double quotes, single quotes or
/// none at all.
fn attribute_values(chunk: &str) -> Vec<Range<usize>> {
    let lower = chunk.to_ascii_lowercase();
    let mut out = Vec::new();
    for (eq, _) in lower.match_indices('=') {
        let name = lower[..eq].trim_end();
        let named = ["src", "href"].into_iter().any(|n| {
            name.strip_suffix(n)
                .and_then(|before| before.chars().next_back())
                .is_some_and(char::is_whitespace)
        });
        if !named {
            continue;
        }
        let rest = &chunk[eq + 1..];
        let lead = rest.len() - rest.trim_start().len();
        let value = &rest[lead..];
        let (open, len) = match value.chars().next() {
            Some(quote @ ('"' | '\'')) => match value[1..].find(quote) {
                Some(len) => (1, len),
                None => continue,
            },
            _ => (
                0,
                value
                    .find(|c: char| c.is_whitespace() || c == '>')
                    .unwrap_or(value.len()),
            ),
        };
        let start = eq + 1 + lead + open;
        if len > 0 {
            out.push(start..start + len);
        }
    }
    out
}

/// The byte ranges the two scans above must keep out of: a code block, a code span, the
/// frontmatter — and, for a definition, the HTML the attribute scan covers instead.
fn verbatim(a: &Analysis) -> Vec<Range<usize>> {
    a.spans
        .iter()
        .filter(|s| {
            matches!(
                s.style,
                Style::CodeBlock | Style::CodeInline | Style::Frontmatter | Style::Html
            )
        })
        .map(|s| s.range.clone())
        .collect()
}

/// Where a markdown link's destination path sits in `text`, without its `#anchor` or title.
///
/// Read from the end of the link, since its text may hold a `](` of its own, and only where the
/// bytes decode to the target the parser saw: a reference-style link has no destination here,
/// and a backslash-escaped one is left as its author wrote it rather than guessed at.
fn destination(text: &str, link: &Link) -> Option<Range<usize>> {
    let inner = text.get(link.range.clone())?.strip_suffix(')')?;
    // Every `](` is a candidate, right to left: a title may hold one too.
    inner.rmatch_indices("](").find_map(|(i, _)| {
        let rest = &inner[i + 2..];
        let lead = rest.len() - rest.trim_start().len();
        let (open, dest) = match rest[lead..].strip_prefix('<') {
            Some(r) => (1, &r[..r.find('>')?]),
            None => (0, rest[lead..].split(char::is_whitespace).next()?),
        };
        let raw = split_anchor(dest).0;
        let start = link.range.start + i + 2 + lead + open;
        (percent_decode(raw) == link.target).then_some(start..start + raw.len())
    })
}

/// What [`repage_links`] made of one note.
#[derive(Debug, Default, PartialEq)]
pub struct Repaged {
    /// The note as it reads now, when a link in it changed.
    pub text: Option<String>,
    /// How many links now name another page.
    pub moved: usize,
    /// The links left naming the page a delete took out, each as its markup reads and which of
    /// the note's links into the PDF that read the same it is: enough to find it again after
    /// edits elsewhere in the note.
    pub left: Vec<(String, usize)>,
}

/// Point the links in the note `src` into the PDF `pdf` at where `edit` took the pages they name:
/// `[[paper.pdf#page=3&selection=…]]`, `![[paper.pdf#page=3]]` and `[t](paper.pdf#page=3)`
/// alike, the page number rewritten and nothing else of the link.
///
/// A link into the page a delete took out names no page any more; it is left as written and
/// handed back in [`Repaged::left`], because guessing at a neighbour would be a wrong link that
/// looks right. `keep` is that list from the delete an Undo takes back: those links name the page
/// coming back, so they stay, where every other link follows the insert and the note reads as it
/// did. `targets` is the file each link key resolves to, as [`rewrite_moved`] takes it.
pub fn repage_links(
    text: &str,
    src: &str,
    pdf: &str,
    targets: &HashMap<String, String>,
    edit: PageEdit,
    keep: &[(String, usize)],
) -> Repaged {
    let dir = parent_dir(src);
    let mut out = Repaged::default();
    let mut edits: Vec<(Range<usize>, String)> = Vec::new();
    // How many links into the PDF have read the same so far: before this rewrite, which is how
    // `keep` counted, and after it, which is how the next one will.
    let (mut before, mut after) = (HashMap::new(), HashMap::new());
    let nth = |seen: &mut HashMap<String, usize>, markup: &str| {
        let n = seen.entry(markup.to_string()).or_insert(0);
        *n += 1;
        *n - 1
    };
    for link in &analyze(text).links {
        let key = match link.kind {
            LinkKind::Wiki | LinkKind::Embed => link_key(&link.target),
            LinkKind::Markdown => link_key(&path::resolve(dir, &link.target)),
            LinkKind::External => continue,
        };
        let Some(written) = text.get(link.range.clone()) else {
            continue;
        };
        if targets.get(&key).map(String::as_str) != Some(pdf) {
            continue;
        }
        let kept = keep.contains(&(written.to_string(), nth(&mut before, written)));
        let number = page_number(text, link).filter(|_| !kept);
        let page = number
            .clone()
            .and_then(|at| text[at].parse::<usize>().ok()?.checked_sub(1));
        let mut now = written.to_string();
        let mut left = false;
        match (number, page.map(|page| (page, edit.map(page)))) {
            (Some(at), Some((page, Some(to)))) if to != page => {
                let with = (to + 1).to_string();
                let start = link.range.start;
                now.replace_range(at.start - start..at.end - start, &with);
                edits.push((at, with));
                out.moved += 1;
            }
            (_, Some((_, None))) => left = true,
            _ => {}
        }
        let n = nth(&mut after, &now);
        if left {
            out.left.push((now, n));
        }
    }
    let mut rewritten = text.to_string();
    for (at, with) in edits.into_iter().rev() {
        rewritten.replace_range(at, &with);
    }
    out.text = (rewritten != text).then_some(rewritten);
    out
}

/// Where the digits of a link's `page=N` sit in `text`, found from the target as written, the
/// `#` after it and the anchor the parser read. `None` for a link with no page, and for one whose
/// bytes do not spell what the parser read (a percent-encoded anchor), which is left alone rather
/// than guessed at.
fn page_number(text: &str, link: &Link) -> Option<Range<usize>> {
    let target = match link.kind {
        LinkKind::Wiki | LinkKind::Embed => {
            let open = if link.kind == LinkKind::Embed { 3 } else { 2 };
            let at = link.range.start + open..link.range.start + open + link.target.len();
            (text.get(at.clone()) == Some(link.target.as_str())).then_some(at)?
        }
        LinkKind::Markdown => destination(text, link)?,
        LinkKind::External => return None,
    };
    let anchor = link.anchor.as_deref()?;
    let start = target.end + 1;
    if text.get(target.end..start) != Some("#")
        || text.get(start..start + anchor.len()) != Some(anchor)
    {
        return None;
    }
    let mut at = start;
    for part in anchor.split('&') {
        if let Some(rest) = part.strip_prefix("page=") {
            let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
            let from = at + "page=".len();
            return (digits > 0).then_some(from..from + digits);
        }
        at += part.len() + 1;
    }
    None
}

/// Spell `new_rel` the way `target` spelled `old_rel`: a bare name stays bare, a path stays a
/// path, and an extension is only written back if the author wrote one.
fn as_written(target: &str, old_rel: &str, new_rel: &str) -> String {
    let full = match target.contains('/') {
        true => new_rel,
        false => basename(new_rel),
    };
    ext_as_written(target, old_rel, full)
}

/// `new` with its extension only where `target` carried `old_rel`'s: `[[Rev 1.2 notes]]` is a
/// name with a dot in it, and inventing an extension the author never wrote is a corrupted link.
/// `![[x.png]]` does carry one, so renaming the image to `x.jpg` writes the new one.
fn ext_as_written(target: &str, old_rel: &str, new: &str) -> String {
    match (ext(target), ext(old_rel)) {
        (Some(written), Some(old)) if written.eq_ignore_ascii_case(old) => new.to_string(),
        _ => strip_ext(new),
    }
}

/// The extension of the file `rel` names, if it has one.
fn ext(rel: &str) -> Option<&str> {
    let base = rel.rsplit('/').next().unwrap_or(rel);
    base.rsplit_once('.')
        .filter(|(stem, _)| !stem.is_empty())
        .map(|(_, ext)| ext)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markdown::testing::link;

    /// What leaves the app for the system, from the editor and the preview alike: an address
    /// with an authority, or one of the three that need none, and never what runs code or reads
    /// the disk, whatever its slashes.
    #[test]
    fn a_url_is_what_the_system_is_handed() {
        for url in [
            "https://e.org/a",
            "HTTP://e.org",
            "ftp://host/f",
            "mailto:a@b.c",
            "tel:+123",
            "sms:+123",
        ] {
            assert!(is_url(url), "{url}");
        }
        for not in [
            "javascript:alert(1)",
            "javascript://%0aalert(1)",
            "JavaScript://x",
            "file:///etc/passwd",
            "data:text/html,<b>x</b>",
            "accent://open/Note",
            "mailto:",
            "Notes/a.md",
            "a:b.md",
            "Notes/a://b",
            "about:blank",
        ] {
            assert!(!is_url(not), "{not}");
        }
    }

    #[test]
    fn wikilink_variants() {
        let t = "[[Note]] [[Note#Heading]] [[Note#^block]] [[Note|alias]] [[Note#Heading|alias]]";
        let a = analyze(t);
        assert_eq!(a.links.len(), 5);
        let expect = [
            ("Note", None, None),
            ("Note", Some("Heading"), None),
            ("Note", Some("^block"), None),
            ("Note", None, Some("alias")),
            ("Note", Some("Heading"), Some("alias")),
        ];
        for (l, (target, anchor, alias)) in a.links.iter().zip(expect) {
            assert_eq!(l.kind, LinkKind::Wiki);
            assert_eq!(l.target, target);
            assert_eq!(l.anchor.as_deref(), anchor);
            assert_eq!(l.alias.as_deref(), alias);
        }
        assert_eq!(&t[a.links[0].range.clone()], "[[Note]]");
        assert_eq!(&t[a.links[4].range.clone()], "[[Note#Heading|alias]]");
    }

    #[test]
    fn pdf_anchor_reads_page_and_selection() {
        assert_eq!(
            pdf_anchor("page=3&selection=4,0,4,11"),
            Some((2, Some([4, 0, 4, 11])))
        );
        assert_eq!(pdf_anchor("page=3"), Some((2, None)));
        // A heading is not a PDF anchor, which is what keeps `[[Note#Heading]]` on the note path.
        assert_eq!(pdf_anchor("Heading"), None);
        // Page numbers are one-based in the link and zero-based here, so page 0 is not a page.
        assert_eq!(pdf_anchor("page=0"), None);
        // A selection that is not four numbers is no selection, but the page still counts.
        assert_eq!(pdf_anchor("page=2&selection=1,2,3"), Some((1, None)));
        assert_eq!(pdf_anchor("page=2&selection=1,2,3,4,5"), Some((1, None)));
    }

    #[test]
    fn embeds_and_pdf_selection() {
        let t = "![[image.png]] ![[paper.pdf#page=3&selection=4,0,4,11]]";
        let a = analyze(t);
        assert_eq!(a.links.len(), 2);
        assert_eq!(a.links[0].kind, LinkKind::Embed);
        assert_eq!(a.links[0].target, "image.png");
        assert_eq!(a.links[0].anchor, None);
        assert_eq!(a.links[1].target, "paper.pdf");
        // the PDF anchor is kept verbatim, commas and all
        assert_eq!(
            a.links[1].anchor.as_deref(),
            Some("page=3&selection=4,0,4,11")
        );
        assert_eq!(
            &t[a.links[1].range.clone()],
            "![[paper.pdf#page=3&selection=4,0,4,11]]"
        );
    }

    #[test]
    fn markdown_link_percent_decoded() {
        let l = link("[text](Other%20Note.md)", 0);
        assert_eq!(l.kind, LinkKind::Markdown);
        assert_eq!(l.target, "Other Note.md");
        assert_eq!(l.alias.as_deref(), Some("text"));
        let l = link("[s](dir/A%20B.md#Some%20Heading)", 0);
        assert_eq!(l.target, "dir/A B.md");
        assert_eq!(l.anchor.as_deref(), Some("Some Heading"));
    }

    #[test]
    fn slugs_follow_github() {
        assert_eq!(slug("Hello, World!"), "hello-world");
        assert_eq!(slug("C++ & Rust"), "c--rust", "each space is a dash");
        assert_eq!(slug("snake_case and-dash 2"), "snake_case-and-dash-2");
        assert_eq!(
            slug("Über Größe"),
            "über-größe",
            "letters are not only ASCII"
        );
        assert_eq!(
            slugs(["Notes", "Intro", "Notes", "notes-1", "Notes"]),
            ["notes", "intro", "notes-1", "notes-1-1", "notes-2"],
            "a repeat takes the first free suffix"
        );
    }

    /// The anchor completion inserts has to land, and so does what already resolved before it.
    #[test]
    fn an_anchor_finds_its_heading_by_slug_or_by_text() {
        let a = analyze("# Intro\n## My Section\n## My Section\n");
        let start = |anchor: &str| heading_for(&a.headings, anchor).map(|h| h.range.start);
        assert_eq!(start("my-section"), Some(8));
        assert_eq!(start("my-section-1"), Some(22), "the second of two");
        assert_eq!(
            start(&link("[x](#My%20Section)", 0).anchor.unwrap()),
            Some(8),
            "a percent-encoded heading text"
        );
        assert_eq!(start("intro"), Some(0));
        assert_eq!(start("nowhere"), None);
    }

    /// `#^id` lands on the block its id marks, in whatever case the link spells it; any other
    /// anchor on a heading.
    #[test]
    fn an_anchor_lands_on_a_block_or_a_heading() {
        let text = "# Intro\nSome prose.\nMore of it. ^Para-1\n";
        let at = |anchor: &str| anchor_range(text, anchor).map(|r| &text[r]);
        assert_eq!(at("^para-1"), Some("Some prose.\nMore of it. ^Para-1"));
        assert_eq!(at("intro"), Some("# Intro"));
        assert_eq!(at("^gone"), None);
        assert_eq!(at("Para-1"), None, "an id is only named with its caret");
    }

    #[test]
    fn external_links_classified() {
        let a = analyze("[x](https://e.com/a#frag) [m](mailto:a@b.c) [r](../rel.md)");
        assert_eq!(a.links[0].kind, LinkKind::External);
        // a URL fragment stays part of the URL
        assert_eq!(a.links[0].target, "https://e.com/a#frag");
        assert_eq!(a.links[0].anchor, None);
        assert_eq!(a.links[1].kind, LinkKind::External);
        assert_eq!(a.links[1].target, "mailto:a@b.c");
        assert_eq!(a.links[2].kind, LinkKind::Markdown);
    }

    #[test]
    fn wikilink_in_code_is_not_a_link() {
        assert!(analyze("`[[NotALink]]`").links.is_empty());
        assert!(analyze("```\n[[NotALink]]\n```\n").links.is_empty());
    }

    #[test]
    fn link_key_normalises_target_spelling() {
        assert_eq!(link_key("  ./Notes\\Deep Work.md "), "notes/deep work.md");
        assert_eq!(link_key("Deep Work"), "deep work");
    }

    #[test]
    fn path_keys_cover_path_and_basename_forms() {
        assert_eq!(
            path_keys("Notes-PHD/Deep Work.md"),
            [
                "notes-phd/deep work.md",
                "notes-phd/deep work",
                "deep work.md",
                "deep work"
            ]
        );
        // A root-level file yields only two distinct keys.
        assert_eq!(path_keys("Index.md"), ["index.md", "index"]);
    }

    /// [`rewrite_moved`] for a note that stays at `Ref.md` while `old` moves to `new`, every key
    /// of `old` resolving to it.
    fn renamed(text: &str, old: &str, new: &str) -> Option<String> {
        let targets = path_keys(old)
            .into_iter()
            .map(|k| (k, old.into()))
            .collect();
        let moves = HashMap::from([(old.to_string(), new.to_string())]);
        rewrite_moved(text, "Ref.md", "Ref.md", &targets, &moves)
    }

    #[test]
    fn a_rename_rewrites_every_wikilink_shape() {
        let src = concat!(
            "[[Old]] and [[Old|alias]] and [[Old#Heading]]\n\n",
            "![[Old]]\n\n",
            "[[Dir/Old]] and [[dir/old.md]]\n\n",
            "```\n[[Old]]\n```\n"
        );
        let want = concat!(
            "[[New]] and [[New|alias]] and [[New#Heading]]\n\n",
            "![[New]]\n\n",
            "[[Notes/New]] and [[Notes/New.md]]\n\n",
            "```\n[[Old]]\n```\n"
        );
        assert_eq!(renamed(src, "Dir/Old.md", "Notes/New.md").unwrap(), want);
        assert_eq!(renamed("[[Other]]", "Old.md", "New.md"), None);
    }

    /// A wikilink resolves by name from anywhere, so only the one that spells a path is stale.
    #[test]
    fn a_pure_move_rewrites_only_the_links_that_spell_a_path() {
        assert_eq!(
            renamed(
                "[[Old]] [[Dir/Old]] [[Old.md]]",
                "Dir/Old.md",
                "Other/Old.md"
            )
            .unwrap(),
            "[[Old]] [[Other/Old]] [[Old.md]]"
        );
    }

    #[test]
    fn a_rename_leaves_links_the_index_did_not_resolve_to_it_alone() {
        // `[[Old]]` belongs to a different note; renaming `Dir/Old.md` must not hijack it.
        let targets = HashMap::from([
            ("dir/old.md".to_string(), "Dir/Old.md".to_string()),
            ("dir/old".to_string(), "Dir/Old.md".to_string()),
        ]);
        let moves = HashMap::from([("Dir/Old.md".to_string(), "Dir/Renamed.md".to_string())]);
        let src = "deep: [[Dir/Old]]\nshallow: [[Old]]\n";
        assert_eq!(
            rewrite_moved(src, "Ref.md", "Ref.md", &targets, &moves).unwrap(),
            "deep: [[Dir/Renamed]]\nshallow: [[Old]]\n"
        );
    }

    /// A dot in a name is not an extension: `[[Rev 1.2 notes]]` must not gain a `.md`, while an
    /// image's extension is the author's and changes with the file.
    #[test]
    fn only_a_written_extension_is_written_back() {
        assert_eq!(
            renamed(
                "[[Rev 1.2 notes]]\n",
                "Rev 1.2 notes.md",
                "Rev 1.3 notes.md"
            )
            .unwrap(),
            "[[Rev 1.3 notes]]\n"
        );
        assert_eq!(
            renamed("![[x.png]] ![](x.png)", "x.png", "x.jpg").unwrap(),
            "![[x.jpg]] ![](x.jpg)"
        );
    }

    #[test]
    fn a_rename_keeps_alias_anchor_and_title() {
        assert_eq!(
            renamed("see [[Old#Deep Work|this one]].", "Old.md", "Dir/New.md").unwrap(),
            "see [[New#Deep Work|this one]]."
        );
        assert_eq!(
            renamed(
                "[t](My%20Note.md#Part%20Two \"a ](title\") [u](<My Note.md>)",
                "My Note.md",
                "Your Note.md"
            )
            .unwrap(),
            "[t](Your%20Note.md#Part%20Two \"a ](title\") [u](<Your%20Note.md>)"
        );
    }

    /// Markdown links are relative to their note: each is rewritten as the path from its folder,
    /// and what is not a vault path is left as written.
    #[test]
    fn markdown_links_follow_their_target_from_the_notes_folder() {
        let src = concat!(
            "[t](../img/x.png) ![](../img/x.png) [p](../a.pdf#page=3&selection=1,2,3,4)\n",
            "[h](#h) [o](../../x.png) [w](https://e.com/img/x.png) [r][ref]\n\n",
            "[ref]: ../img/x.png\n"
        );
        let targets = HashMap::from([
            ("img/x.png".to_string(), "img/x.png".to_string()),
            ("a.pdf".to_string(), "a.pdf".to_string()),
            ("x.png".to_string(), "x.png".to_string()),
        ]);
        let moves = HashMap::from([
            ("img/x.png".to_string(), "pics/y.png".to_string()),
            ("a.pdf".to_string(), "docs/a.pdf".to_string()),
            ("x.png".to_string(), "z.png".to_string()),
        ]);
        assert_eq!(
            rewrite_moved(src, "notes/Ref.md", "notes/Ref.md", &targets, &moves).unwrap(),
            concat!(
                "[t](../pics/y.png) ![](../pics/y.png) [p](../docs/a.pdf#page=3&selection=1,2,3,4)\n",
                "[h](#h) [o](../../x.png) [w](https://e.com/img/x.png) [r][ref]\n\n",
                "[ref]: ../pics/y.png\n"
            )
        );
    }

    /// A reference-style definition is scanned for on its own: the parser resolves it into the
    /// links that use it and never says where it sits.
    #[test]
    fn a_reference_definition_follows_its_target() {
        let src = concat!(
            "[r][one] [s][two] [t][three] [u][four]\n\n",
            "[one]: Dir/Old.md \"A title\"\n",
            "   [two]: <Dir/Old.md>\n",
            "[three]: https://e.com/Dir/Old.md\n",
            "[four]: ./Dir/Old.md#Heading\n",
            "[^1]: Dir/Old.md is a footnote, not a path\n",
            "```\n[five]: Dir/Old.md\n```\n"
        );
        assert_eq!(
            renamed(src, "Dir/Old.md", "Notes/New.md").unwrap(),
            concat!(
                "[r][one] [s][two] [t][three] [u][four]\n\n",
                "[one]: Notes/New.md \"A title\"\n",
                "   [two]: <Notes/New.md>\n",
                "[three]: https://e.com/Dir/Old.md\n",
                "[four]: Notes/New.md#Heading\n",
                "[^1]: Dir/Old.md is a footnote, not a path\n",
                "```\n[five]: Dir/Old.md\n```\n"
            )
        );
    }

    /// HTML is opaque to the parser, so its `src` and `href` are scanned for too — in a block of
    /// it and in an inline tag, quoted either way or not at all, and for vault paths alone.
    #[test]
    fn html_src_and_href_follow_their_target() {
        let src = concat!(
            "<figure>\n<img src=\"Dir/Old.png\" alt=\"x\">\n</figure>\n\n",
            "Inline <img src='Dir/Old.png'> and <a href=Dir/Old.png>bare</a>.\n\n",
            "<a href=\"https://e.com/Dir/Old.png\">out</a> <a href=\"#here\">anchor</a>\n\n",
            "<img data-src=\"Dir/Old.png\">\n\n",
            "```\n<img src=\"Dir/Old.png\">\n```\n"
        );
        assert_eq!(
            renamed(src, "Dir/Old.png", "Pics/New.png").unwrap(),
            concat!(
                "<figure>\n<img src=\"Pics/New.png\" alt=\"x\">\n</figure>\n\n",
                "Inline <img src='Pics/New.png'> and <a href=Pics/New.png>bare</a>.\n\n",
                "<a href=\"https://e.com/Dir/Old.png\">out</a> <a href=\"#here\">anchor</a>\n\n",
                "<img data-src=\"Dir/Old.png\">\n\n",
                "```\n<img src=\"Dir/Old.png\">\n```\n"
            )
        );
    }

    /// A note that moves takes its relative links with it, whether or not what they name moved.
    #[test]
    fn a_moved_note_keeps_its_own_markdown_links() {
        let targets = HashMap::from([
            ("img/a.png".to_string(), "img/a.png".to_string()),
            ("notes/b.md".to_string(), "notes/b.md".to_string()),
            ("c".to_string(), "c.md".to_string()),
        ]);
        let moves = HashMap::from([("notes/n.md".to_string(), "notes/deep/n.md".to_string())]);
        assert_eq!(
            rewrite_moved(
                "[t](../img/a.png) [s](b.md) [[c]]",
                "notes/n.md",
                "notes/deep/n.md",
                &targets,
                &moves
            )
            .unwrap(),
            "[t](../../img/a.png) [s](../b.md) [[c]]"
        );
    }

    /// [`repage_links`] over a note at `notes/n.md`, every key of `Papers/p.pdf` and of
    /// `other.pdf` resolving to its file.
    fn repaged(text: &str, edit: PageEdit, keep: &[(String, usize)]) -> Repaged {
        let targets = path_keys("Papers/p.pdf")
            .into_iter()
            .map(|k| (k, "Papers/p.pdf".to_string()))
            .chain([("other.pdf".to_string(), "other.pdf".to_string())])
            .collect();
        repage_links(text, "notes/n.md", "Papers/p.pdf", &targets, edit, keep)
    }

    /// A highlight, a jump and a markdown link all follow a moved page, and nothing else is read
    /// as a page: a link with no page, another PDF's, one in code.
    #[test]
    fn a_page_edit_moves_every_link_shape_and_nothing_else() {
        let src = concat!(
            "[[Papers/p.pdf#page=1&selection=0,0,0,4|Hello]] ![[p.pdf#page=2]]\n",
            "[three](../Papers/p.pdf#page=3) [[p.pdf]] [[other.pdf#page=1]]\n\n",
            "```\n[[p.pdf#page=1]]\n```\n"
        );
        let got = repaged(src, PageEdit::Move { from: 0, to: 2 }, &[]);
        assert_eq!(
            got.text.as_deref(),
            Some(concat!(
                "[[Papers/p.pdf#page=3&selection=0,0,0,4|Hello]] ![[p.pdf#page=1]]\n",
                "[three](../Papers/p.pdf#page=2) [[p.pdf]] [[other.pdf#page=1]]\n\n",
                "```\n[[p.pdf#page=1]]\n```\n"
            ))
        );
        assert_eq!((got.moved, got.left), (3, vec![]));
        // An edit that leaves every page it names where it was writes nothing.
        let still = repaged(src, PageEdit::Insert(3), &[]);
        assert_eq!((still.text, still.moved), (None, 0));
    }

    /// A link into a deleted page is left and handed back; the Undo that puts the page back is
    /// given it to keep, and every other link follows the insert, so the note reads as it did —
    /// also when a link that moved now reads the same as one that was left, either way round.
    #[test]
    fn undoing_a_delete_puts_back_exactly_what_it_changed() {
        for src in [
            "[[p.pdf#page=2]] [[p.pdf#page=3]] [[p.pdf#page=2|again]] [[p.pdf#page=1]]",
            "[[p.pdf#page=3]] [[p.pdf#page=2]]",
        ] {
            let deleted = repaged(src, PageEdit::Delete(1), &[]);
            let text = deleted.text.expect("a link moved");
            assert!(!deleted.left.is_empty(), "{src}");
            let undone = repaged(&text, PageEdit::Insert(1), &deleted.left);
            assert_eq!(undone.text.as_deref(), Some(src));
            assert!(undone.left.is_empty());
        }
        let deleted = repaged(
            "[[p.pdf#page=2]] [[p.pdf#page=3]] [[p.pdf#page=2|again]]",
            PageEdit::Delete(1),
            &[],
        );
        assert_eq!(deleted.moved, 1);
        assert_eq!(
            deleted.left,
            [
                ("[[p.pdf#page=2]]".to_string(), 0),
                ("[[p.pdf#page=2|again]]".to_string(), 0)
            ]
        );
    }
}
