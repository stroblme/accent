//! Markdown analysis: one pulldown-cmark pass -> styling spans, links, tags, headings, HTML.
//! All ranges are byte offsets into the input text. Consumers (GTK, Compose) only apply spans.

use pulldown_cmark::{CodeBlockKind, Event, LinkType, Options, Parser, Tag as Cm, TagEnd};
use serde::{Deserialize, Serialize};
use std::ops::Range;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Analysis {
    pub spans: Vec<Span>,
    pub links: Vec<Link>,
    pub tags: Vec<Tag>,
    pub headings: Vec<Heading>,
    /// First H1, else frontmatter `title:`, else None (caller falls back to file stem).
    pub title: Option<String>,
    /// Raw YAML frontmatter body (without the `---` fences), if present.
    pub frontmatter: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Span {
    pub range: Range<usize>,
    pub style: Style,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum Style {
    Heading(u8),
    Emphasis,
    Strong,
    Strikethrough,
    CodeInline,
    CodeBlock,
    Link,
    WikiLink,
    Image,
    Tag,
    Quote,
    ListMarker,
    TaskMarker {
        checked: bool,
    },
    Math,
    Html,
    Frontmatter,
    /// The syntax characters themselves (`**`, `#`, `[[`, backticks): editors dim these.
    Marker,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Link {
    pub range: Range<usize>,
    pub kind: LinkKind,
    /// Target as written, without `#anchor` and `|alias` (e.g. `Note`, `dir/Note.md`, `paper.pdf`).
    pub target: String,
    /// Part after `#` (heading, `^block`, or PDF `page=3&selection=…`).
    pub anchor: Option<String>,
    pub alias: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum LinkKind {
    /// `[[target]]`
    Wiki,
    /// `![[target]]`
    Embed,
    /// `[text](target)` — only relative/vault targets, not http(s)
    Markdown,
    /// `[text](https://…)`
    External,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Tag {
    pub range: Range<usize>,
    /// Without the leading `#`, nested tags keep the `/`.
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Heading {
    pub range: Range<usize>,
    pub level: u8,
    pub text: String,
}

fn options() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_MATH
        | Options::ENABLE_WIKILINKS
        | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS
        | Options::ENABLE_HEADING_ATTRIBUTES
}

/// A link being built while its inner text events stream past.
struct Pending {
    range: Range<usize>,
    kind: LinkKind,
    target: String,
    anchor: Option<String>,
    /// Display text differs from the target (`[[a|b]]`, `[b](a)`), so it is a real alias.
    aliased: bool,
    text: String,
}

/// Analyse a note. Must be fast enough to run on every (debounced) keystroke.
pub fn analyze(text: &str) -> Analysis {
    let mut a = Analysis::default();
    let mut heading: Option<(Range<usize>, u8, String)> = None;
    let mut open: Vec<Pending> = Vec::new();
    let mut fm_title: Option<String> = None;
    let mut in_meta = false;
    let mut in_code = false;
    let mut in_html = false;

    for (ev, r) in Parser::new_ext(text, options()).into_offset_iter() {
        match ev {
            Event::Start(Cm::Heading { level, .. }) => {
                let hr = trim_eol(text, &r);
                a.spans.push(sp(hr.clone(), Style::Heading(level as u8)));
                if let Some(m) = heading_marker(text, &hr) {
                    a.spans.push(sp(m, Style::Marker));
                }
                heading = Some((hr, level as u8, String::new()));
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some((range, level, text)) = heading.take() {
                    a.headings.push(Heading { range, level, text });
                }
            }
            Event::Start(Cm::Emphasis) => {
                delimited(&mut a.spans, text, r, Style::Emphasis, b"*_", 1)
            }
            Event::Start(Cm::Strong) => delimited(&mut a.spans, text, r, Style::Strong, b"*_", 2),
            Event::Start(Cm::Strikethrough) => {
                let n = if run(text, &r, b"~") >= 2 { 2 } else { 1 };
                delimited(&mut a.spans, text, r, Style::Strikethrough, b"~", n)
            }
            Event::Code(ref t) => {
                let n = run(text, &r, b"`");
                wrapped(&mut a.spans, r, Style::CodeInline, n);
                if let Some(h) = heading.as_mut() {
                    h.2.push_str(t);
                }
                if let Some(l) = open.last_mut() {
                    l.text.push_str(t);
                }
            }
            Event::InlineMath(_) => wrapped(&mut a.spans, r, Style::Math, 1),
            Event::DisplayMath(_) => wrapped(&mut a.spans, r, Style::Math, 2),
            Event::Start(Cm::CodeBlock(kind)) => {
                in_code = true;
                a.spans.push(sp(r.clone(), Style::CodeBlock));
                if !matches!(kind, CodeBlockKind::Indented) {
                    let (o, c) = fence(text, &r);
                    if o > 0 {
                        a.spans.push(sp(r.start..r.start + o, Style::Marker));
                    }
                    if c > 0 && r.end - c >= r.start + o {
                        a.spans.push(sp(r.end - c..r.end, Style::Marker));
                    }
                }
            }
            Event::End(TagEnd::CodeBlock) => in_code = false,
            Event::Start(Cm::BlockQuote(_)) => {
                a.spans.push(sp(trim_eol(text, &r), Style::Quote));
                quote_markers(text, &r, &mut a.spans);
            }
            Event::Start(Cm::Item) => {
                if let Some(m) = list_marker(text, &r) {
                    a.spans.push(sp(m.clone(), Style::ListMarker));
                    a.spans.push(sp(m, Style::Marker));
                }
            }
            Event::TaskListMarker(checked) => a.spans.push(sp(r, Style::TaskMarker { checked })),
            Event::Start(Cm::HtmlBlock) => {
                in_html = true;
                a.spans.push(sp(trim_eol(text, &r), Style::Html));
            }
            Event::End(TagEnd::HtmlBlock) => in_html = false,
            Event::InlineHtml(_) => a.spans.push(sp(r, Style::Html)),
            Event::Start(Cm::MetadataBlock(_)) => {
                in_meta = true;
                a.spans.push(sp(r.clone(), Style::Frontmatter));
                a.spans.push(sp(r.start..r.start + 3, Style::Marker));
                a.spans.push(sp(r.end - 3..r.end, Style::Marker));
                fm_title = frontmatter(text, &r, &mut a);
            }
            Event::End(TagEnd::MetadataBlock(_)) => in_meta = false,
            Event::Start(Cm::Link {
                link_type,
                dest_url,
                ..
            }) => {
                let wiki = matches!(link_type, LinkType::WikiLink { .. });
                a.spans.push(sp(
                    r.clone(),
                    if wiki { Style::WikiLink } else { Style::Link },
                ));
                if wiki {
                    wiki_markers(&mut a.spans, &r, 2);
                }
                open.push(pending(r, &dest_url, link_type, false));
            }
            Event::Start(Cm::Image {
                link_type,
                dest_url,
                ..
            }) => {
                let wiki = matches!(link_type, LinkType::WikiLink { .. });
                a.spans.push(sp(r.clone(), Style::Image));
                if wiki {
                    wiki_markers(&mut a.spans, &r, 3);
                }
                open.push(pending(r, &dest_url, link_type, true));
            }
            Event::End(TagEnd::Link) | Event::End(TagEnd::Image) => {
                if let Some(p) = open.pop() {
                    let alias = if p.aliased && !p.text.is_empty() {
                        Some(p.text)
                    } else {
                        None
                    };
                    a.links.push(Link {
                        range: p.range,
                        kind: p.kind,
                        target: p.target,
                        anchor: p.anchor,
                        alias,
                    });
                }
            }
            Event::Text(ref t) => {
                if let Some(h) = heading.as_mut() {
                    h.2.push_str(t);
                }
                if let Some(l) = open.last_mut() {
                    l.text.push_str(t);
                } else if !in_meta && !in_code && !in_html {
                    scan_tags(text, &r, &mut a.tags, &mut a.spans);
                }
            }
            _ => {}
        }
    }

    a.title = a
        .headings
        .iter()
        .find(|h| h.level == 1)
        .map(|h| h.text.clone())
        .or(fm_title);
    a
}

fn sp(range: Range<usize>, style: Style) -> Span {
    Span { range, style }
}

/// Style the content, dim the delimiters (`**bold**` -> Strong on `bold`, Marker on each `**`).
fn delimited(
    out: &mut Vec<Span>,
    text: &str,
    r: Range<usize>,
    style: Style,
    chars: &[u8],
    n: usize,
) {
    let b = text.as_bytes();
    let fits = r.len() > n * 2
        && b[r.start..r.start + n].iter().all(|c| chars.contains(c))
        && b[r.end - n..r.end].iter().all(|c| chars.contains(c));
    if fits {
        out.push(sp(r.start + n..r.end - n, style));
        out.push(sp(r.start..r.start + n, Style::Marker));
        out.push(sp(r.end - n..r.end, Style::Marker));
    } else {
        out.push(sp(r, style));
    }
}

/// Style the whole range (delimiters included) and dim the delimiters on top.
fn wrapped(out: &mut Vec<Span>, r: Range<usize>, style: Style, n: usize) {
    out.push(sp(r.clone(), style));
    if n > 0 && r.len() >= n * 2 {
        out.push(sp(r.start..r.start + n, Style::Marker));
        out.push(sp(r.end - n..r.end, Style::Marker));
    }
}

fn wiki_markers(out: &mut Vec<Span>, r: &Range<usize>, open: usize) {
    if r.len() > open + 2 {
        out.push(sp(r.start..r.start + open, Style::Marker));
        out.push(sp(r.end - 2..r.end, Style::Marker));
    }
}

fn run(text: &str, r: &Range<usize>, chars: &[u8]) -> usize {
    text.as_bytes()[r.clone()]
        .iter()
        .take_while(|c| chars.contains(c))
        .count()
}

fn trim_eol(text: &str, r: &Range<usize>) -> Range<usize> {
    let b = text.as_bytes();
    let mut end = r.end;
    while end > r.start && matches!(b[end - 1], b'\n' | b'\r') {
        end -= 1;
    }
    r.start..end
}

/// `#`, `##`… plus the spaces after it (ATX only; setext headings have no prefix).
fn heading_marker(text: &str, r: &Range<usize>) -> Option<Range<usize>> {
    let b = text.as_bytes();
    if b.get(r.start) != Some(&b'#') {
        return None;
    }
    let mut i = r.start;
    while i < r.end && b[i] == b'#' {
        i += 1;
    }
    while i < r.end && matches!(b[i], b' ' | b'\t') {
        i += 1;
    }
    Some(r.start..i)
}

/// Opening and closing fence lengths of a fenced code block.
fn fence(text: &str, r: &Range<usize>) -> (usize, usize) {
    let b = text.as_bytes();
    let c = b[r.start];
    if c != b'`' && c != b'~' {
        return (0, 0);
    }
    let mut i = r.start;
    while i < r.end && b[i] == c {
        i += 1;
    }
    let mut j = r.end;
    while j > i && b[j - 1] == c {
        j -= 1;
    }
    (i - r.start, r.end - j)
}

fn list_marker(text: &str, r: &Range<usize>) -> Option<Range<usize>> {
    let b = text.as_bytes();
    let mut i = r.start;
    while i < r.end && matches!(b[i], b' ' | b'\t') {
        i += 1;
    }
    let start = i;
    if i < r.end && matches!(b[i], b'-' | b'*' | b'+') {
        return Some(start..i + 1);
    }
    while i < r.end && b[i].is_ascii_digit() {
        i += 1;
    }
    if i > start && i < r.end && matches!(b[i], b'.' | b')') {
        return Some(start..i + 1);
    }
    None
}

fn quote_markers(text: &str, r: &Range<usize>, out: &mut Vec<Span>) {
    let b = text.as_bytes();
    let mut i = r.start;
    while i < r.end {
        let mut j = i;
        while j < r.end && matches!(b[j], b' ' | b'\t') {
            j += 1;
        }
        if j < r.end && b[j] == b'>' {
            out.push(sp(j..j + 1, Style::Marker));
        }
        match text[i..r.end].find('\n') {
            Some(k) => i += k + 1,
            None => break,
        }
    }
}

fn pending(range: Range<usize>, dest: &str, link_type: LinkType, image: bool) -> Pending {
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

fn split_anchor(dest: &str) -> (&str, Option<&str>) {
    match dest.split_once('#') {
        Some((t, a)) => (t, Some(a)),
        None => (dest, None),
    }
}

/// `scheme:` or `//host` — anything with an authority is not a vault path.
fn is_external(dest: &str) -> bool {
    if dest.starts_with("//") {
        return true;
    }
    match dest.find(':') {
        Some(i) if i > 0 => {
            let s = &dest[..i];
            s.starts_with(|c: char| c.is_ascii_alphabetic())
                && s.bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'+' | b'-' | b'.'))
        }
        _ => false,
    }
}

fn percent_decode(s: &str) -> String {
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

fn percent_encode(s: &str) -> String {
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

/// Characters a tag may follow (Obsidian: start of line, whitespace, or opening punctuation).
const TAG_PREFIX: &str = "([{<,;:'\"*_~|";

fn is_tag_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '-' | '/')
}

/// Scan a text run (never code, never a link target) for `#tag`s.
fn scan_tags(text: &str, r: &Range<usize>, tags: &mut Vec<Tag>, spans: &mut Vec<Span>) {
    let b = text.as_bytes();
    let mut i = r.start;
    while i < r.end {
        if b[i] != b'#' {
            i += 1;
            continue;
        }
        let ok = match text[..i].chars().next_back() {
            None => true,
            Some(c) => c.is_whitespace() || TAG_PREFIX.contains(c),
        };
        if !ok {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < r.end {
            let c = text[j..].chars().next().unwrap();
            if is_tag_char(c) {
                j += c.len_utf8();
            } else {
                break;
            }
        }
        let name = &text[i + 1..j];
        // `#123` is a heading-ish number, not a tag.
        if !name.is_empty() && !name.bytes().all(|c| c.is_ascii_digit()) {
            tags.push(Tag {
                range: i..j,
                name: name.to_string(),
            });
            spans.push(sp(i..j, Style::Tag));
        }
        i = j.max(i + 1);
    }
}

// ponytail: hand-rolled scan for `title:` and `tags:` only; swap in serde_yaml (or
// yaml-rust2) the day we need nested frontmatter, anchors, or multi-line scalars.
fn frontmatter(text: &str, r: &Range<usize>, a: &mut Analysis) -> Option<String> {
    let block = &text[r.clone()];
    let start = block.find('\n').map(|i| r.start + i + 1)?;
    let end = block
        .rfind('\n')
        .map(|i| r.start + i)
        .unwrap_or(r.end)
        .max(start);
    a.frontmatter = Some(text[start..end].to_string());

    let mut title = None;
    let mut in_tags = false;
    let mut off = start;
    for chunk in text[start..end].split_inclusive('\n') {
        let line_start = off;
        off += chunk.len();
        let line = chunk.trim_end_matches(['\n', '\r']);
        if line.starts_with([' ', '\t']) {
            let t = line.trim_start();
            if in_tags && t.starts_with('-') {
                let base = line_start + (line.len() - t.len()) + 1;
                push_fm_tag(&t[1..], base, a);
            }
            continue;
        }
        in_tags = false;
        if let Some(rest) = line.strip_prefix("tags:") {
            let base = line_start + 5;
            let t = rest.trim();
            if t.is_empty() {
                in_tags = true;
            } else if let Some(inner) = t.strip_prefix('[') {
                let inner = inner.strip_suffix(']').unwrap_or(inner);
                let mut p = base + (rest.len() - rest.trim_start().len()) + 1;
                for item in inner.split(',') {
                    push_fm_tag(item, p, a);
                    p += item.len() + 1;
                }
            } else {
                push_fm_tag(rest, base, a);
            }
        } else if let Some(rest) = line.strip_prefix("title:") {
            let t = unquote(rest.trim());
            if !t.is_empty() {
                title = Some(t.to_string());
            }
        }
    }
    title
}

fn unquote(s: &str) -> &str {
    let b = s.as_bytes();
    if b.len() >= 2 && (b[0] == b'"' || b[0] == b'\'') && b[b.len() - 1] == b[0] {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

/// `raw` is one frontmatter tag token; `base` is its byte offset in the document.
fn push_fm_tag(raw: &str, base: usize, a: &mut Analysis) {
    let lead = raw.len() - raw.trim_start().len();
    let tok = raw.trim();
    if tok.is_empty() {
        return;
    }
    let quoted = tok != unquote(tok);
    let tok = unquote(tok);
    let start = base + lead + usize::from(quoted);
    let name = tok.strip_prefix('#').unwrap_or(tok);
    if name.is_empty() {
        return;
    }
    let range = start..start + tok.len();
    a.tags.push(Tag {
        range: range.clone(),
        name: name.to_string(),
    });
    a.spans.push(sp(range, Style::Tag));
}

const IMAGE_EXT: [&str; 8] = ["png", "jpg", "jpeg", "gif", "svg", "webp", "bmp", "avif"];

fn is_image(target: &str) -> bool {
    target
        .rsplit_once('.')
        .is_some_and(|(_, e)| IMAGE_EXT.contains(&e.to_ascii_lowercase().as_str()))
}

fn esc_attr(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('"', "&quot;")
}

fn open_href(target: &str, anchor: Option<&str>) -> String {
    let mut h = format!("accent://open/{}", percent_encode(target));
    if let Some(a) = anchor {
        h.push('#');
        h.push_str(&esc_attr(a));
    }
    h
}

/// Render a note to an HTML fragment for the preview pane (wikilinks become `<a href="accent://…">`).
pub fn to_html(text: &str) -> String {
    let mut evts: Vec<Event> = Vec::new();
    let mut link_wiki: Vec<bool> = Vec::new();
    let mut image_wiki: Vec<bool> = Vec::new();
    let mut skip = 0usize;

    for ev in Parser::new_ext(text, options()) {
        if skip > 0 {
            match ev {
                Event::Start(Cm::Image { .. }) => skip += 1,
                Event::End(TagEnd::Image) => skip -= 1,
                _ => {}
            }
            continue;
        }
        match ev {
            Event::Start(Cm::Link {
                link_type: LinkType::WikiLink { .. },
                ref dest_url,
                ..
            }) => {
                let (t, anchor) = split_anchor(dest_url);
                evts.push(Event::Html(
                    format!("<a href=\"{}\" class=\"wikilink\">", open_href(t, anchor)).into(),
                ));
                link_wiki.push(true);
            }
            Event::Start(Cm::Link { .. }) => {
                link_wiki.push(false);
                evts.push(ev);
            }
            Event::End(TagEnd::Link) => {
                if link_wiki.pop().unwrap_or(false) {
                    evts.push(Event::Html("</a>".into()));
                } else {
                    evts.push(ev);
                }
            }
            Event::Start(Cm::Image {
                link_type: LinkType::WikiLink { .. },
                ref dest_url,
                ..
            }) => {
                let (t, anchor) = split_anchor(dest_url);
                if is_image(t) {
                    evts.push(Event::Html(
                        format!("<img src=\"accent://file/{}\">", percent_encode(t)).into(),
                    ));
                    skip = 1;
                } else {
                    evts.push(Event::Html(
                        format!("<a href=\"{}\" class=\"embed\">", open_href(t, anchor)).into(),
                    ));
                    image_wiki.push(true);
                }
            }
            Event::Start(Cm::Image { .. }) => {
                image_wiki.push(false);
                evts.push(ev);
            }
            Event::End(TagEnd::Image) => {
                if image_wiki.pop().unwrap_or(false) {
                    evts.push(Event::Html("</a>".into()));
                } else {
                    evts.push(ev);
                }
            }
            _ => evts.push(ev),
        }
    }

    let mut out = String::new();
    pulldown_cmark::html::push_html(&mut out, evts.into_iter());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(text: &str, i: usize) -> Link {
        analyze(text).links[i].clone()
    }

    fn names(text: &str) -> Vec<String> {
        analyze(text).tags.into_iter().map(|t| t.name).collect()
    }

    /// The bytes a span actually covers — every offset assertion goes through this.
    fn at<'a>(text: &'a str, s: &Span) -> &'a str {
        &text[s.range.clone()]
    }

    fn spans_of(a: &Analysis, style: Style) -> Vec<Range<usize>> {
        a.spans
            .iter()
            .filter(|s| s.style == style)
            .map(|s| s.range.clone())
            .collect()
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
    fn tags_obsidian_rules() {
        assert_eq!(
            names("#tag #nested/tag #tag_with-dash (#paren)"),
            ["tag", "nested/tag", "tag_with-dash", "paren"]
        );
        assert!(names("#123").is_empty(), "purely numeric is not a tag");
        assert!(
            names("# Heading\n").is_empty(),
            "heading marker is not a tag"
        );
        assert!(names("`#incode`").is_empty(), "code span is not a tag");
        assert!(
            names("```\n#inblock\n```\n").is_empty(),
            "code block is not a tag"
        );
        assert!(
            names("https://x.com/#frag").is_empty(),
            "URL fragment is not a tag"
        );
        assert!(
            names("[#notatag](x.md)").is_empty(),
            "link text is scanned as a link"
        );
    }

    #[test]
    fn tag_ranges_point_at_the_tag() {
        let t = "lead #alpha/beta trail";
        let a = analyze(t);
        assert_eq!(a.tags.len(), 1);
        assert_eq!(&t[a.tags[0].range.clone()], "#alpha/beta");
        assert_eq!(a.tags[0].name, "alpha/beta");
    }

    #[test]
    fn frontmatter_inline_and_block_tags() {
        let t = "---\ntitle: From Meta\ntags: [alpha, beta/gamma]\n---\n\nbody\n";
        let a = analyze(t);
        assert_eq!(names(t), ["alpha", "beta/gamma"]);
        assert_eq!(&t[a.tags[0].range.clone()], "alpha");
        assert_eq!(&t[a.tags[1].range.clone()], "beta/gamma");
        assert_eq!(a.title.as_deref(), Some("From Meta"));
        assert_eq!(
            a.frontmatter.as_deref(),
            Some("title: From Meta\ntags: [alpha, beta/gamma]")
        );

        let t = "---\ntags:\n  - alpha\n  - \"#beta\"\nauthor: me\n---\nbody\n";
        let a = analyze(t);
        assert_eq!(names(t), ["alpha", "beta"]);
        assert_eq!(&t[a.tags[1].range.clone()], "#beta");
        assert!(a.title.is_none());

        assert_eq!(names("---\ntags: single\n---\nx\n"), ["single"]);
    }

    #[test]
    fn headings_and_title_precedence() {
        let t = "# First\n\n## Second **bold**\n\n### Third\n";
        let a = analyze(t);
        assert_eq!(a.headings.len(), 3);
        assert_eq!(a.headings[0].level, 1);
        assert_eq!(a.headings[1].level, 2);
        assert_eq!(a.headings[1].text, "Second bold", "inline markup stripped");
        assert_eq!(&t[a.headings[0].range.clone()], "# First");
        assert_eq!(&t[a.headings[1].range.clone()], "## Second **bold**");
        assert_eq!(a.title.as_deref(), Some("First"));

        // H1 wins over frontmatter title
        let a = analyze("---\ntitle: Meta\n---\n\n# Real\n");
        assert_eq!(a.title.as_deref(), Some("Real"));
        // frontmatter title when there is no H1
        let a = analyze("---\ntitle: Meta\n---\n\n## Sub\n");
        assert_eq!(a.title.as_deref(), Some("Meta"));
        // neither
        assert!(analyze("just text\n").title.is_none());
    }

    #[test]
    fn spans_and_markers_at_exact_offsets() {
        let t = "# Head\n\nSome **bold** and *em* and `code` and ~~out~~.\n";
        let a = analyze(t);

        let heading = a
            .spans
            .iter()
            .find(|s| s.style == Style::Heading(1))
            .unwrap();
        assert_eq!(at(t, heading), "# Head");
        let strong = a.spans.iter().find(|s| s.style == Style::Strong).unwrap();
        assert_eq!(at(t, strong), "bold");
        let em = a.spans.iter().find(|s| s.style == Style::Emphasis).unwrap();
        assert_eq!(at(t, em), "em");
        let code = a
            .spans
            .iter()
            .find(|s| s.style == Style::CodeInline)
            .unwrap();
        assert_eq!(at(t, code), "`code`");
        let strike = a
            .spans
            .iter()
            .find(|s| s.style == Style::Strikethrough)
            .unwrap();
        assert_eq!(at(t, strike), "out");

        let markers: Vec<&str> = a
            .spans
            .iter()
            .filter(|s| s.style == Style::Marker)
            .map(|s| at(t, s))
            .collect();
        assert_eq!(markers, ["# ", "**", "**", "*", "*", "`", "`", "~~", "~~"]);
    }

    #[test]
    fn underscore_emphasis_marker_lengths() {
        let t = "__strong__ and _em_";
        let a = analyze(t);
        let markers: Vec<&str> = a
            .spans
            .iter()
            .filter(|s| s.style == Style::Marker)
            .map(|s| at(t, s))
            .collect();
        assert_eq!(markers, ["__", "__", "_", "_"]);
        assert_eq!(
            at(
                t,
                a.spans.iter().find(|s| s.style == Style::Strong).unwrap()
            ),
            "strong"
        );
    }

    #[test]
    fn block_spans() {
        let t =
            "> quoted\n\n- one\n- [x] done\n\n```rust\nfn x() {}\n```\n\n$$e$$\n\n<div>h</div>\n";
        let a = analyze(t);
        assert_eq!(&t[spans_of(&a, Style::Quote)[0].clone()], "> quoted");
        let list: Vec<&str> = spans_of(&a, Style::ListMarker)
            .iter()
            .map(|r| &t[r.clone()])
            .collect();
        assert_eq!(list, ["-", "-"]);
        let task = spans_of(&a, Style::TaskMarker { checked: true });
        assert_eq!(&t[task[0].clone()], "[x]");
        let code = spans_of(&a, Style::CodeBlock);
        assert_eq!(&t[code[0].clone()], "```rust\nfn x() {}\n```");
        let math = spans_of(&a, Style::Math);
        assert_eq!(&t[math[0].clone()], "$$e$$");
        let html = spans_of(&a, Style::Html);
        assert_eq!(&t[html[0].clone()], "<div>h</div>");
        // the `>` and both fences are dimmed
        let markers: Vec<&str> = spans_of(&a, Style::Marker)
            .iter()
            .map(|r| &t[r.clone()])
            .collect();
        assert!(markers.contains(&">"), "{markers:?}");
        assert!(markers.contains(&"```"), "{markers:?}");
    }

    #[test]
    fn frontmatter_and_wikilink_markers() {
        let t = "---\ntags: [a]\n---\n\n[[Note]] ![[i.png]]\n";
        let a = analyze(t);
        let fm = spans_of(&a, Style::Frontmatter);
        assert_eq!(&t[fm[0].clone()], "---\ntags: [a]\n---");
        let markers: Vec<&str> = spans_of(&a, Style::Marker)
            .iter()
            .map(|r| &t[r.clone()])
            .collect();
        assert!(markers.contains(&"---"), "{markers:?}");
        assert!(markers.contains(&"[["), "{markers:?}");
        assert!(markers.contains(&"]]"), "{markers:?}");
        assert!(markers.contains(&"![["), "{markers:?}");
        assert_eq!(&t[spans_of(&a, Style::WikiLink)[0].clone()], "[[Note]]");
        assert_eq!(&t[spans_of(&a, Style::Image)[0].clone()], "![[i.png]]");
    }

    #[test]
    fn html_rewrites_wikilinks() {
        let h = to_html("[[Note#Head|alias]]");
        assert_eq!(
            h.trim(),
            "<p><a href=\"accent://open/Note#Head\" class=\"wikilink\">alias</a></p>"
        );

        let h = to_html("[[Other Note]]");
        assert!(
            h.contains("<a href=\"accent://open/Other%20Note\" class=\"wikilink\">Other Note</a>"),
            "{h}"
        );

        let h = to_html("![[img.png]]");
        assert!(h.contains("<img src=\"accent://file/img.png\">"), "{h}");
        assert!(!h.contains("alt="), "embed alt text is dropped: {h}");

        // non-image embeds fall back to a link
        let h = to_html("![[paper.pdf#page=3&selection=4,0,4,11]]");
        assert!(
            h.contains(
                "<a href=\"accent://open/paper.pdf#page=3&amp;selection=4,0,4,11\" class=\"embed\">"
            ),
            "{h}"
        );

        // ordinary markdown is untouched
        let h = to_html("# Hi\n\n[x](y.md)\n");
        assert!(h.contains("<h1>Hi</h1>"), "{h}");
        assert!(h.contains("<a href=\"y.md\">x</a>"), "{h}");
    }

    #[test]
    fn empty_and_plain_input() {
        assert_eq!(analyze(""), Analysis::default());
        let a = analyze("plain text\n");
        assert!(a.spans.is_empty() && a.links.is_empty() && a.tags.is_empty());
    }

    #[test]
    fn malformed_input_never_panics_and_ranges_stay_on_char_boundaries() {
        let cases = [
            "---\ntags: a\n",      // unterminated frontmatter
            "---\n---\n",          // empty frontmatter
            "```",                 // lone fence
            "```rust\nunclosed\n", // unterminated block
            "[[",
            "]]",
            "[[]]",
            "![[",
            "[[a|]]",
            "**",
            "*",
            "~~x",
            "$",
            "# ",
            "#",
            "> ",
            "- ",
            "Öl #Straße/größe und ünïcode **fett** `código`\n",
            "🎉 #emoji-tag 🎉 [[Nöte#Übersicht|Älias]]\n",
            "---\ntags: [ä, ö/ü]\ntitle: Ünïcode\n---\n\n# Ü\n",
        ];
        for t in cases {
            let a = analyze(t);
            for s in &a.spans {
                assert!(
                    s.range.end <= t.len()
                        && t.is_char_boundary(s.range.start)
                        && t.is_char_boundary(s.range.end)
                        && s.range.start <= s.range.end,
                    "bad span {:?} in {t:?}",
                    s
                );
            }
            for l in &a.links {
                assert!(t.is_char_boundary(l.range.start) && t.is_char_boundary(l.range.end));
            }
            for g in &a.tags {
                assert_eq!(
                    t[g.range.clone()].trim_start_matches('#'),
                    g.name,
                    "in {t:?}"
                );
            }
            let _ = to_html(t);
        }
    }

    #[test]
    #[ignore = "timing-sensitive; run with --release"]
    fn analyze_1mb_under_200ms() {
        let unit = "# Section\n\nSome **bold** text with [[Note#Sec|alias]], a #tag, `code`,\n\
                    a [link](Other%20Note.md) and ![[img.png]].\n\n- [ ] task item\n- plain item\n\n\
                    > quoted line\n\n```rust\nfn main() {}\n```\n\n";
        let mut doc = String::with_capacity(1 << 20);
        while doc.len() < 1 << 20 {
            doc.push_str(unit);
        }
        let started = std::time::Instant::now();
        let a = analyze(&doc);
        let took = started.elapsed();
        assert!(!a.spans.is_empty() && !a.links.is_empty());
        assert!(
            took.as_millis() < 200,
            "analyze({} bytes) took {took:?}",
            doc.len()
        );
    }
}
