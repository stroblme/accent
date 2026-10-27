//! Org mode as the index and the notes layer read it, line by line: headings (a level, and a
//! title without its TODO keyword, priority, `COMMENT` and tags), heading tags, `#+TITLE`,
//! `#+FILETAGS` and links, into the [`Analysis`] a note has, so whatever reads a note's links,
//! tags and outline reads an org file's too. A block of code or examples, a drawer, a comment
//! line and a fixed-width line hold none of them, nor does `=verbatim=` or `~code~` hold a link.
//!
//! A subset of <https://orgmode.org/worg/org-syntax.html>, not a parser of all of it. Ranges are
//! byte offsets of the text as written. The spans are a note's ([`Style`]), so an org file is
//! styled as a note is: the content in its style, the syntax around it dimmed as [`Style::Marker`].

mod links;
mod search;
mod table;
mod task;

pub use links::{link, path_link_keys, rewrite_moved};
pub use search::{Search, file_line};
pub use table::table_key;
pub use task::toggle_task;

use crate::markdown::{Analysis, CodeBlock, Heading, Span, Style, Tag, heading_for};
use links::OrgLink;
use std::ops::Range;

/// What one pass over a file finds: the analysis, the links as org writes them, which a move's
/// rewrite reads, and the `<<targets>>` an internal link may name.
struct Scan {
    analysis: Analysis,
    links: Vec<OrgLink>,
    targets: Vec<(Range<usize>, String)>,
}

/// Analyse an org file. Two passes, one for the TODO keywords a heading may open with, which
/// any line of the file may set, and one for the rest: fast enough for every keystroke.
pub fn analyze(text: &str) -> Analysis {
    scan(text).analysis
}

/// Where an anchor lands, read as `org-link-search` reads a link's search ([`Search`]): a match,
/// the heading a custom ID names, or by name on the `<<target>>` it names, else on the heading, by
/// its title as `[[*Title]]` and `[[file:x.org::*Title]]` write it or by its slug, as a note's
/// `[t](x.org#title)` does. A file link's line is [`file_line`]'s.
pub fn anchor_range(text: &str, anchor: &str) -> Option<Range<usize>> {
    let name = match Search::of(anchor) {
        Search::Name(name) => name,
        Search::CustomId(id) => return custom_id(text, id),
        search => return search.in_text(text),
    };
    let scan = scan(text);
    scan.targets
        .iter()
        .find(|(_, target)| target.eq_ignore_ascii_case(name))
        .map(|(at, _)| at.clone())
        .or_else(|| heading_for(&scan.analysis.headings, name).map(|h| h.range.clone()))
}

/// The heading whose property drawer sets `CUSTOM_ID` to `id`, ignoring case as
/// `org-find-property` does, or the file's top for a drawer before every heading.
fn custom_id(text: &str, id: &str) -> Option<Range<usize>> {
    let lines = lines(text);
    let mut heading = 0..0;
    let mut i = 0;
    while i < lines.len() {
        let line = &text[lines[i].clone()];
        if is_heading(line) {
            heading = lines[i].clone();
        }
        let Some((last, style)) = closed(text, &lines, i) else {
            i += 1;
            continue;
        };
        let properties =
            style == Style::Frontmatter && line.trim().eq_ignore_ascii_case(":PROPERTIES:");
        let set = |l: &Range<usize>| {
            let s = text[l.clone()].trim();
            s.get(..11)
                .is_some_and(|key| key.eq_ignore_ascii_case(":CUSTOM_ID:"))
                && s[11..].trim().eq_ignore_ascii_case(id)
        };
        if properties && lines[i + 1..last].iter().any(set) {
            return Some(heading);
        }
        i = last + 1;
    }
    None
}

/// Every `#+begin_src` block that names its language, `python` of `#+begin_src python :results
/// output`, with its code: the lines between its first and its last.
pub fn code_blocks(text: &str) -> Vec<CodeBlock> {
    let lines = lines(text);
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let Some((last, style)) = closed(text, &lines, i) else {
            i += 1;
            continue;
        };
        // `#+begin_` is where a block's line starts ([`closed`]); a drawer is no block.
        let mut words = text[lines[i].clone()].trim()[8..].split_whitespace();
        let src = style == Style::CodeBlock
            && words.next().is_some_and(|w| w.eq_ignore_ascii_case("src"));
        if let Some(lang) = words.next().filter(|_| src && last > i + 1) {
            let code = lines[i + 1].start..lines[last].start;
            out.push(CodeBlock {
                lang: lang.to_string(),
                code: vec![code],
            });
        }
        i = last + 1;
    }
    out
}

/// Where text put at the top of an org file goes: below the `#+KEYWORD:` lines and drawers
/// opening it, as a note's goes below its frontmatter.
pub fn body_start(text: &str) -> usize {
    let lines = lines(text);
    let mut i = 0;
    while i < lines.len() {
        match closed(text, &lines, i) {
            Some((last, Style::Frontmatter)) => i = last + 1,
            _ if keyword(text, lines[i].clone()).is_some() => i += 1,
            _ => break,
        }
    }
    lines.get(i).map_or(text.len(), |l| l.start)
}

fn scan(text: &str) -> Scan {
    let lines = lines(text);
    let keywords = keywords(text, &lines);
    let mut s = Scan {
        analysis: Analysis::default(),
        links: Vec::new(),
        targets: Vec::new(),
    };
    let mut title: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].clone();
        if let Some((last, style)) = closed(text, &lines, i) {
            s.span(line.start..lines[last].end, style);
            // A block's `#+begin_…` and `#+end_…` lines are its fences.
            if style == Style::CodeBlock {
                s.span(trimmed(text, line), Style::Marker);
                s.span(trimmed(text, lines[last].clone()), Style::Marker);
            }
            i = last + 1;
            continue;
        }
        let lead = line.start + text[line.clone()].len() - text[line.clone()].trim_start().len();
        let marker = |m: &str| {
            text[lead..line.end]
                .strip_prefix(m)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with([' ', '\t']))
        };
        if let Some(head) = heading(text, line.clone(), &keywords) {
            s.heading(text, line, head);
        } else if text[lead..line.end].starts_with("#+") {
            // A keyword line, or a block's line that is no verbatim block.
            s.span(trimmed(text, line.clone()), Style::Frontmatter);
            match keyword(text, line.clone()) {
                Some((key, value)) if key.eq_ignore_ascii_case("title") && !value.is_empty() => {
                    title.push(&text[value])
                }
                Some((key, value)) if key.eq_ignore_ascii_case("filetags") => {
                    s.tags(tags_in(text, value, |c| c == ':' || c.is_whitespace()))
                }
                Some(_) => {}
                None => inline(text, line, &mut s),
            }
        } else if marker("#") {
            s.span(trimmed(text, line), Style::Marker);
        } else if marker(":") {
            // A fixed-width line: verbatim, as code.
            s.span(lead..lead + 1, Style::Marker);
            s.span(lead + 1..line.end, Style::CodeInline);
        } else {
            let body = s.list_item(text, line.clone());
            inline(text, body..line.end, &mut s);
        }
        i += 1;
    }
    s.analysis.links = s.links.iter().filter_map(|l| l.to_link(text)).collect();
    // Org joins the lines of a title written over several.
    s.analysis.title = (!title.is_empty()).then(|| title.join(" "));
    s
}

/// Every line's range, without its line break.
fn lines(text: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut start = 0;
    for line in text.split_inclusive('\n') {
        out.push(start..start + line.trim_end_matches(['\n', '\r']).len());
        start += line.len();
    }
    out
}

impl Scan {
    fn span(&mut self, range: Range<usize>, style: Style) {
        if !range.is_empty() {
            self.analysis.spans.push(Span { range, style });
        }
    }

    /// Tags, in the accent as a note's are, the colons around them dimmed.
    fn tags(&mut self, tags: Vec<Tag>) {
        for tag in &tags {
            self.span(tag.range.clone(), Style::Tag);
        }
        self.analysis.tags.extend(tags);
    }

    /// A link in the accent, as a note's are, what is not its description dimmed: `[[`, `]]`, a
    /// described link's target, and an address's angle brackets.
    fn link(&mut self, text: &str, link: &OrgLink) {
        let r = link.range.clone();
        self.span(r.clone(), Style::Link);
        match (&link.desc, text.as_bytes()[r.start]) {
            (Some(desc), _) => {
                self.span(r.start..desc.start, Style::Marker);
                self.span(desc.end..r.end, Style::Marker);
            }
            (None, b'[') => {
                self.span(r.start..link.link.start, Style::Marker);
                self.span(link.link.end..r.end, Style::Marker);
            }
            (None, b'<') => {
                self.span(r.start..r.start + 1, Style::Marker);
                self.span(r.end - 1..r.end, Style::Marker);
            }
            _ => {}
        }
    }

    /// A heading's row and spans: its stars dimmed as a note's `#` are, an open keyword a task
    /// marker, a done one, the priority and `COMMENT` dimmed, and its title read for links.
    fn heading(&mut self, text: &str, line: Range<usize>, head: Head) {
        self.span(line.clone(), Style::Heading(head.level));
        self.span(head.stars, Style::Marker);
        if let Some((at, done)) = head.keyword {
            let style = match done {
                true => Style::Marker,
                false => Style::TaskMarker { checked: false },
            };
            self.span(at, style);
        }
        for at in head.marks {
            self.span(at, Style::Marker);
        }
        if let Some(block) = head.tag_block {
            for (i, _) in text[block.clone()].match_indices(':') {
                self.span(block.start + i..block.start + i + 1, Style::Marker);
            }
        }
        self.tags(head.tags);
        self.analysis.headings.push(Heading {
            range: line,
            level: head.level,
            text: text[head.title.clone()].to_string(),
        });
        inline(text, head.title, self);
    }

    /// The bullet or counter opening a list item on `line`, and its checkbox, styled as a note's;
    /// where the item's text starts, or the line's start for a line that is no item. A `*` at the
    /// start of a line opens a heading, so a `*` bullet is indented.
    fn list_item(&mut self, text: &str, line: Range<usize>) -> usize {
        let s = &text[line.clone()];
        let lead = s.len() - s.trim_start_matches([' ', '\t']).len();
        let rest = &s[lead..];
        let bullet = match rest.bytes().next() {
            Some(b'-' | b'+') => 1,
            Some(b'*') if lead > 0 => 1,
            _ => {
                let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
                match rest.as_bytes().get(digits) {
                    Some(b'.' | b')') if digits > 0 => digits + 1,
                    _ => return line.start,
                }
            }
        };
        let after = &rest[bullet..];
        if !(after.is_empty() || after.starts_with([' ', '\t'])) {
            return line.start;
        }
        let at = line.start + lead;
        self.span(at..at + bullet, Style::ListMarker);
        self.span(at..at + bullet, Style::Marker);
        let body = at + bullet + after.len() - after.trim_start().len();
        let tick = text[body..line.end]
            .strip_prefix('[')
            .and_then(|r| r.chars().next())
            .filter(|c| matches!(c, ' ' | '-' | 'X' | 'x'));
        let closes = text[body..line.end].get(2..3) == Some("]")
            && text[body + 3..line.end]
                .chars()
                .next()
                .is_none_or(char::is_whitespace);
        match tick {
            Some(tick) if closes => {
                let checked = tick.eq_ignore_ascii_case(&'x');
                self.span(body..body + 3, Style::TaskMarker { checked });
                body + 3
            }
            _ => body,
        }
    }
}

/// The line `line` without the blanks around it.
fn trimmed(text: &str, line: Range<usize>) -> Range<usize> {
    let s = &text[line.clone()];
    let start = line.start + s.len() - s.trim_start().len();
    start..line.start + s.trim_end().len()
}

/// The TODO keywords a heading may open with, each with whether it is a done one, a sequence
/// per line of the file's `#+TODO`, `#+SEQ_TODO` and `#+TYP_TODO`: every word of the line, a
/// fast-access key such as `(t)` dropped, the ones after a `|` done, or the last one of a line
/// without `|`; org's own TODO and DONE when the file sets none.
fn keywords<'a>(text: &'a str, lines: &[Range<usize>]) -> Vec<Vec<(&'a str, bool)>> {
    let mut out = Vec::new();
    for line in lines {
        let Some((key, value)) = keyword(text, line.clone()) else {
            continue;
        };
        if ["todo", "seq_todo", "typ_todo"]
            .iter()
            .any(|k| key.eq_ignore_ascii_case(k))
        {
            let words: Vec<&str> = text[value]
                .split_whitespace()
                .filter_map(|w| w.split('(').next())
                .filter(|w| !w.is_empty())
                .collect();
            let bar = words.iter().position(|w| *w == "|");
            let done_from = bar.unwrap_or(words.len().saturating_sub(1));
            let sequence: Vec<_> = words
                .iter()
                .enumerate()
                .filter(|(_, w)| **w != "|")
                .map(|(i, w)| (*w, i >= done_from))
                .collect();
            if !sequence.is_empty() {
                out.push(sequence);
            }
        }
    }
    if out.is_empty() {
        out = vec![vec![("TODO", false), ("DONE", true)]];
    }
    out
}

/// `#+KEY: value` as its key and where its value is, trimmed.
fn keyword(text: &str, line: Range<usize>) -> Option<(&str, Range<usize>)> {
    let s = &text[line.clone()];
    let lead = s.len() - s.trim_start().len();
    let rest = s[lead..].strip_prefix("#+")?;
    let (key, value) = rest.split_once(':')?;
    if key.is_empty() || key.contains(char::is_whitespace) {
        return None;
    }
    let start = line.start + lead + 2 + key.len() + 1;
    let start = start + value.len() - value.trim_start().len();
    Some((key, start..start + value.trim().len()))
}

/// The last line of the block or drawer opening on `lines[i]`, and the span it gets: a block of
/// code, an example, an export or a comment (`#+begin_src` … `#+end_src`), whose lines are
/// verbatim, or a drawer (`:PROPERTIES:` … `:END:`). `None` where none opens, and for one that
/// does not close before the next heading, which org reads as plain lines.
fn closed(text: &str, lines: &[Range<usize>], i: usize) -> Option<(usize, Style)> {
    let at = |j: usize| text[lines[j].clone()].trim();
    let first = at(i);
    let (end, style) = match first.get(..8) {
        Some(begin) if begin.eq_ignore_ascii_case("#+begin_") => {
            let name = first[8..].split_whitespace().next()?;
            ["src", "example", "export", "comment"]
                .iter()
                .any(|v| name.eq_ignore_ascii_case(v))
                .then(|| (format!("#+end_{name}"), Style::CodeBlock))?
        }
        _ => {
            let name = first.strip_prefix(':')?.strip_suffix(':')?;
            let named = !name.is_empty()
                && name
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'));
            (named && !name.eq_ignore_ascii_case("end"))
                .then(|| (":END:".to_string(), Style::Frontmatter))?
        }
    };
    (i + 1..lines.len())
        .take_while(|&j| !is_heading(&text[lines[j].clone()]))
        .find(|&j| at(j).eq_ignore_ascii_case(&end))
        .map(|j| (j, style))
}

/// Whether a line is a heading: stars at its start, then a space.
fn is_heading(line: &str) -> bool {
    let stars = line.bytes().take_while(|&c| c == b'*').count();
    stars > 0 && matches!(line.as_bytes().get(stars), Some(b' ' | b'\t'))
}

/// A heading line read into its parts.
struct Head {
    level: u8,
    /// The stars and the blanks after them.
    stars: Range<usize>,
    /// The TODO keyword, and whether it is a done one.
    keyword: Option<(Range<usize>, bool)>,
    /// The priority and `COMMENT`.
    marks: Vec<Range<usize>>,
    title: Range<usize>,
    tags: Vec<Tag>,
    /// The `:a:b:` the tags are written in.
    tag_block: Option<Range<usize>>,
}

/// `* TODO [#A] COMMENT Title :a:b:` read into its parts.
fn heading(text: &str, line: Range<usize>, keywords: &[Vec<(&str, bool)>]) -> Option<Head> {
    let s = &text[line.clone()];
    if !is_heading(s) {
        return None;
    }
    let stars = s.bytes().take_while(|&c| c == b'*').count();
    let mut start = line.start + stars;
    let mut end = line.start + s.trim_end().len();
    // The tags close the line: `:a:b:` after a space.
    let head = &text[start..end];
    let run = head.len()
        - head
            .trim_end_matches(|c: char| c == ':' || is_tag_char(c))
            .len();
    let block = &head[head.len() - run..];
    let (mut tags, mut tag_block) = (Vec::new(), None);
    if run > 1 && block.starts_with(':') && block.ends_with(':') {
        tags = tags_in(text, end - run..end, |c| c == ':');
        tag_block = Some(end - run..end);
        end -= run;
    }
    // Then what opens the title, each a word of its own: a keyword, a priority, `COMMENT`.
    let word = |start: usize| {
        let rest = &text[start..end];
        let from = start + rest.len() - rest.trim_start().len();
        let len = text[from..end]
            .find(char::is_whitespace)
            .unwrap_or(end - from);
        from..from + len
    };
    start = word(start).start;
    let stars_at = line.start..start;
    let mut keyword = None;
    if let Some(&(_, done)) = keywords
        .iter()
        .flatten()
        .find(|(k, _)| *k == &text[word(start)])
    {
        keyword = Some((word(start), done));
        start = word(word(start).end).start;
    }
    let mut marks = Vec::new();
    let w = &text[word(start)];
    if w.len() > 3
        && w.starts_with("[#")
        && w.ends_with(']')
        && w[2..w.len() - 1].bytes().all(|c| c.is_ascii_alphanumeric())
    {
        marks.push(word(start));
        start = word(word(start).end).start;
    }
    if &text[word(start)] == "COMMENT" {
        marks.push(word(start));
        start = word(word(start).end).start;
    }
    let title = start..start + text[start..end].trim_end().len();
    Some(Head {
        level: stars.min(u8::MAX as usize) as u8,
        stars: stars_at,
        keyword,
        marks,
        title,
        tags,
        tag_block,
    })
}

/// A character a tag is written with: a letter, a digit, `_`, `@`, `#` or `%`.
pub fn is_tag_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '@' | '#' | '%')
}

/// The tags written at `at`, each the run between two separators. `sep` takes `:`, which also
/// ends the last one.
fn tags_in(text: &str, at: Range<usize>, sep: impl Fn(char) -> bool) -> Vec<Tag> {
    let mut out = Vec::new();
    let mut from = None;
    for (i, c) in text[at.clone()].char_indices().chain([(at.len(), ':')]) {
        match (sep(c), from) {
            (true, Some(f)) => {
                let range = at.start + f..at.start + i;
                out.push(Tag {
                    name: text[range.clone()].to_string(),
                    range,
                });
                from = None;
            }
            (false, None) => from = Some(i),
            _ => {}
        }
    }
    out
}

/// The links, `<<targets>>` and emphasis of the prose at `at`, read left to right as org reads
/// its objects: a `=verbatim=` or `~code~` run hides the links in it, as a link hides its
/// markers, where `*bold*`, `/italic/`, `_underlined_` and `+struck+` text is read on inside.
fn inline(text: &str, at: Range<usize>, s: &mut Scan) {
    let b = text.as_bytes();
    let push = |link: OrgLink, s: &mut Scan| {
        let end = link.range.end;
        s.link(text, &link);
        s.links.push(link);
        end
    };
    let mut i = at.start;
    while i < at.end {
        let after = match b[i] {
            b'[' => links::bracket(text, i, at.end).map(|l| push(l, s)),
            b'<' if b.get(i + 1) == Some(&b'<') => target(text, i, at.end).map(|(range, name)| {
                s.span(range.start..range.start + 2, Style::Marker);
                s.span(range.end - 2..range.end, Style::Marker);
                s.targets.push((range.clone(), name));
                range.end
            }),
            b'<' => links::angle(text, i, at.end).map(|l| push(l, s)),
            c @ (b'=' | b'~' | b'*' | b'/' | b'_' | b'+') => emphasis(text, at.start, i, at.end)
                .map(|end| {
                    let style = match c {
                        b'*' => Style::Strong,
                        b'/' => Style::Emphasis,
                        b'_' => Style::Underline,
                        b'+' => Style::Strikethrough,
                        _ => Style::CodeInline,
                    };
                    s.span(i..i + 1, Style::Marker);
                    s.span(i + 1..end - 1, style);
                    s.span(end - 1..end, Style::Marker);
                    match style {
                        Style::CodeInline => end,
                        _ => i + 1,
                    }
                }),
            c if c.is_ascii_alphabetic()
                && (i == at.start || !b[i - 1].is_ascii_alphanumeric()) =>
            {
                links::plain(text, i, at.end).map(|l| push(l, s))
            }
            _ => None,
        };
        i = after.unwrap_or(i + 1);
    }
}

/// The `<<target>>` opening at `i`, and its name; never a radio target's `<<<…>>>`.
fn target(text: &str, i: usize, end: usize) -> Option<(Range<usize>, String)> {
    let rest = &text[i + 2..end];
    let name = &rest[..rest.find(">>")?];
    (!name.is_empty() && name.trim() == name && !name.contains(['<', '>']))
        .then(|| (i..i + name.len() + 4, name.to_string()))
}

/// Where the `=verbatim=`, `~code~`, `*bold*`, `/italic/`, `_underlined_` or `+struck+` run
/// opening at `i` ends, by org's rules for its markers:
/// the opening one at the start of the line, after a space or after one of `-({'"`, and not
/// before a space; the closing one the same character, not after a space, and before a space,
/// the end of the line or one of `-.,;:!?')}["\`.
fn emphasis(text: &str, line: usize, i: usize, end: usize) -> Option<usize> {
    let b = text.as_bytes();
    let pre = i == line || b[i - 1].is_ascii_whitespace() || b"-({'\"".contains(&b[i - 1]);
    if !pre || i + 2 >= end || b[i + 1].is_ascii_whitespace() {
        return None;
    }
    (i + 2..end)
        .find(|&j| {
            b[j] == b[i]
                && !b[j - 1].is_ascii_whitespace()
                && (j + 1 == end
                    || b[j + 1].is_ascii_whitespace()
                    || b"-.,;:!?')}[\"\\".contains(&b[j + 1]))
        })
        .map(|j| j + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markdown::LinkKind;

    #[test]
    fn a_source_block_names_its_language() {
        let text = "#+begin_src python :results output\nx = 1\n#+end_src\n#+BEGIN_SRC\nnone\n#+END_SRC\n#+begin_example\nz\n#+end_example\n";
        let blocks: Vec<(String, Vec<&str>)> = code_blocks(text)
            .into_iter()
            .map(|b| (b.lang, b.code.into_iter().map(|r| &text[r]).collect()))
            .collect();
        assert_eq!(blocks, [("python".into(), vec!["x = 1\n"])]);
    }

    fn headings(text: &str) -> Vec<(u8, String)> {
        analyze(text)
            .headings
            .into_iter()
            .map(|h| (h.level, h.text))
            .collect()
    }

    fn styled(text: &str) -> Vec<(&str, Style)> {
        analyze(text)
            .spans
            .iter()
            .map(|s| (&text[s.range.clone()], s.style))
            .collect()
    }

    fn targets(text: &str) -> Vec<(LinkKind, String, Option<String>, Option<String>)> {
        analyze(text)
            .links
            .into_iter()
            .map(|l| (l.kind, l.target, l.anchor, l.alias))
            .collect()
    }

    #[test]
    fn a_heading_is_its_title_without_keyword_priority_comment_or_tags() {
        let text = "* TODO [#A] COMMENT Write the paper :work:urgent:\n** Notes\n*** DONE\n*not a heading*\n";
        let a = analyze(text);
        assert_eq!(
            headings(text),
            [
                (1, "Write the paper".to_string()),
                (2, "Notes".to_string()),
                (3, String::new())
            ]
        );
        assert_eq!(
            &text[a.headings[0].range.clone()],
            &text[..text.find('\n').unwrap()]
        );
        let tags: Vec<&str> = a.tags.iter().map(|t| &text[t.range.clone()]).collect();
        assert_eq!(tags, ["work", "urgent"]);
        assert_eq!(a.tags[0].name, "work");
        // A word that only looks like one stays in the title.
        assert_eq!(
            headings("* TODOS today 10:30:\n")[0].1,
            "TODOS today 10:30:"
        );
    }

    #[test]
    fn todo_lines_set_the_keywords_a_heading_opens_with() {
        let text =
            "#+TODO: NEXT(n) | DONE CANCELLED(c@)\n* NEXT Call\n* CANCELLED Trip\n* TODO Stays\n";
        assert_eq!(
            headings(text)
                .into_iter()
                .map(|(_, t)| t)
                .collect::<Vec<_>>(),
            ["Call", "Trip", "TODO Stays"]
        );
    }

    #[test]
    fn the_title_and_file_tags_come_from_their_keywords() {
        let a = analyze("#+title: Field notes\n#+FILETAGS: :lab:2024:\n\n* Field notes\n");
        assert_eq!(a.title.as_deref(), Some("Field notes"));
        let tags: Vec<&str> = a.tags.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(tags, ["lab", "2024"]);
        assert_eq!(analyze("* Only a heading\n").title, None);
    }

    #[test]
    fn every_link_form_is_read_as_the_index_stores_it() {
        use LinkKind::*;
        let text = "See [[file:sub/a.org::*Intro][the intro]], [[./b.md]], [[../c.pdf]], \
             [[*Plan]], [[Target]], [[https://orgmode.org][Org]], <https://example.com/x>, \
             https://plain.org/p. and [[id:1234]] [[elisp:(foo)]] [[/etc/hosts]] [[~/x.org]]\n";
        let one = |kind, target: &str, anchor: Option<&str>, alias: Option<&str>| {
            (
                kind,
                target.to_string(),
                anchor.map(str::to_string),
                alias.map(str::to_string),
            )
        };
        assert_eq!(
            targets(text),
            [
                one(Markdown, "sub/a.org", Some("Intro"), Some("the intro")),
                one(Markdown, "./b.md", None, None),
                one(Markdown, "../c.pdf", None, None),
                one(Wiki, "", Some("Plan"), None),
                one(Wiki, "", Some("Target"), None),
                one(External, "https://orgmode.org", None, Some("Org")),
                one(External, "https://example.com/x", None, None),
                one(External, "https://plain.org/p", None, None),
            ]
        );
        let a = analyze(text);
        assert_eq!(
            &text[a.links[0].range.clone()],
            "[[file:sub/a.org::*Intro][the intro]]"
        );
        assert_eq!(
            targets(r"[[file:a\[1\].org]]"),
            [one(Markdown, "a[1].org", None, None)]
        );
    }

    #[test]
    fn blocks_drawers_comments_and_code_hold_nothing() {
        let text = "* Real :tag:\n\
             #+begin_src python :results output\n\
             [[file:a.org]]\n\
             ,* Escaped heading :no:\n\
             #+end_src\n\
             #+BEGIN_EXAMPLE\n[[file:b.org]]\n#+END_EXAMPLE\n\
             :PROPERTIES:\n:LINK: [[file:c.org]]\n:END:\n\
             # [[file:d.org]]\n\
             : [[file:e.org]]\n\
             ~[[file:f.org]]~ and =[[file:g.org]]= but [[file:h.org]]\n";
        let a = analyze(text);
        assert_eq!(headings(text), [(1, "Real".to_string())]);
        assert_eq!(a.tags.len(), 1);
        assert_eq!(
            targets(text).into_iter().map(|t| t.1).collect::<Vec<_>>(),
            ["h.org"]
        );
        let spans: Vec<(&str, Style)> = styled(text)
            .into_iter()
            .filter(|(_, style)| matches!(style, Style::CodeBlock | Style::Frontmatter))
            .collect();
        assert_eq!(spans.len(), 3);
        assert!(spans[0].0.starts_with("#+begin_src") && spans[0].0.ends_with("#+end_src"));
        assert_eq!(
            spans[2],
            (
                ":PROPERTIES:\n:LINK: [[file:c.org]]\n:END:",
                Style::Frontmatter
            )
        );
        // One that never closes is plain lines, and a heading ends the search for its end.
        let open = "#+begin_src\n[[file:a.org]]\n* Next\n#+end_src\n";
        assert_eq!(targets(open).len(), 1);
        assert!(
            !analyze(open)
                .spans
                .iter()
                .any(|s| s.style == Style::CodeBlock)
        );
    }

    #[test]
    fn a_heading_is_styled_as_a_notes() {
        let text = "#+TODO: NEXT | DONE\n** NEXT [#A] Write *it* :work:\n* DONE Sent\n";
        let spans = styled(text);
        let has = |want: (&str, Style)| assert!(spans.contains(&want), "{want:?} in {spans:?}");
        has(("** NEXT [#A] Write *it* :work:", Style::Heading(2)));
        has(("** ", Style::Marker));
        has(("NEXT", Style::TaskMarker { checked: false }));
        has(("[#A]", Style::Marker));
        has(("it", Style::Strong));
        has(("work", Style::Tag));
        has(("DONE", Style::Marker));
        has(("#+TODO: NEXT | DONE", Style::Frontmatter));
        let colons = spans.iter().filter(|s| *s == &(":", Style::Marker)).count();
        assert_eq!(colons, 2);
    }

    #[test]
    fn prose_is_styled_as_a_notes() {
        let text = "- [X] done /now/ and +gone+ =v= ~c~ _under_\n  * [ ] open [[file:a.org][A]]\n\
                    1) [[*H]] <https://x.org>\n# hidden\n: fixed\n#+begin_src sh\nls\n#+end_src\n\
                    a/b/c and 10*3*2 stay plain, *not bold *, snake_case_name\n";
        let spans = styled(text);
        let has = |want: (&str, Style)| assert!(spans.contains(&want), "{want:?} in {spans:?}");
        has(("-", Style::ListMarker));
        has(("[X]", Style::TaskMarker { checked: true }));
        has(("now", Style::Emphasis));
        has(("gone", Style::Strikethrough));
        has(("v", Style::CodeInline));
        has(("c", Style::CodeInline));
        has(("under", Style::Underline));
        has(("*", Style::ListMarker));
        has(("[ ]", Style::TaskMarker { checked: false }));
        has(("[[file:a.org][A]]", Style::Link));
        has(("[[file:a.org][", Style::Marker));
        has(("1)", Style::ListMarker));
        has(("[[*H]]", Style::Link));
        has(("<https://x.org>", Style::Link));
        has(("# hidden", Style::Marker));
        has((" fixed", Style::CodeInline));
        has(("#+begin_src sh", Style::Marker));
        has(("#+end_src", Style::Marker));
        let last = text.lines().last().unwrap();
        let plain = spans
            .iter()
            .filter(|(s, _)| last.contains(*s) && s.len() > 1);
        assert_eq!(plain.count(), 0, "{spans:?}");
    }

    #[test]
    fn a_link_is_written_as_org_writes_one() {
        assert_eq!(
            link("file:a[1].org", Some("Title")),
            r"[[file:a\[1\].org][Title]]"
        );
        assert_eq!(link("https://x.org", None), "[[https://x.org]]");
        assert_eq!(link("x", Some(" ")), "[[x]]");
        assert_eq!(link("x", Some("a]]b]")), "[[x][a]\u{200B}]b]\u{200B}]]");
        let text = link("file:a.org", Some("see [1]"));
        assert_eq!(targets(&text)[0].3.as_deref(), Some("see [1]\u{200B}"));
    }

    #[test]
    fn every_range_survives_a_multibyte_file() {
        let samples = ["é", "中", "🎉", "aé中🎉"];
        let templates = [
            "* X :X:X:\n",
            "* TODO [#A] X\n",
            "#+TITLE: X\n#+FILETAGS: :X:\n",
            "[[file:X.org::*X][X]] [[X]] <https://X> https://X.org/X\n",
            "X =X= ~X~ <<X>> X\n",
            "#+begin_src X\nX\n#+end_src\n:X:\nX\n:END:\n",
            "- [X] X *X* /X/ _X_ +X+\n  * [ ] X\n1) X\n# X\n: X\n#+X\n",
        ];
        for t in templates {
            for s in samples {
                let doc = t.replace('X', s);
                let a = analyze(&doc);
                let ranges = a
                    .spans
                    .iter()
                    .map(|s| &s.range)
                    .chain(a.links.iter().map(|l| &l.range))
                    .chain(a.tags.iter().map(|t| &t.range))
                    .chain(a.headings.iter().map(|h| &h.range));
                for r in ranges {
                    assert!(
                        r.start <= r.end
                            && r.end <= doc.len()
                            && doc.is_char_boundary(r.start)
                            && doc.is_char_boundary(r.end),
                        "bad range {r:?} in {doc:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn an_anchor_lands_on_a_target_before_a_heading() {
        let text = "* Plan\nSee <<plan>> here.\n* Other\n";
        assert_eq!(
            anchor_range(text, "plan").map(|r| &text[r]),
            Some("<<plan>>")
        );
        assert_eq!(
            anchor_range(text, "Other").map(|r| &text[r]),
            Some("* Other")
        );
        assert_eq!(anchor_range(text, "missing"), None);
    }
}
