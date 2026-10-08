//! Markdown analysis: one pulldown-cmark pass -> styling spans, links, tags, headings, HTML.
//! All ranges are byte offsets into the input text. Consumers (GTK, Compose) only apply spans.
//!
//! `analyze` lives here with the types it fills; the helpers it calls sit by concern — `spans`
//! reads delimiters back from the source, `links` classifies and rewrites targets, `frontmatter`
//! scans tags and the YAML block, `html` renders the preview.
//!
//! `table` stands apart: it lays out a pipe table as the editor's Tab and Enter keep it.

mod blocks;
mod frontmatter;
mod html;
mod links;
mod spans;
mod table;

pub use blocks::{BlockId, block_ids};
pub use html::{math_errors, mathml, to_html};
pub use links::{
    Repaged, anchor_range, heading_for, is_image, is_url, link_key, path_keys, path_link_keys,
    pdf_anchor, percent_decode, percent_encode, repage_links, rewrite_moved, section, section_end,
    slugs, strip_ext,
};
pub use table::{TableEdit, TableKey, table_key};

use frontmatter::{frontmatter, scan_tags};
use links::pending;
use pulldown_cmark::{CodeBlockKind, Event, LinkType, Options, Parser, Tag as Cm, TagEnd};
use serde::{Deserialize, Serialize};
use spans::*;
use std::ops::Range;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Analysis {
    pub spans: Vec<Span>,
    pub links: Vec<Link>,
    pub tags: Vec<Tag>,
    pub headings: Vec<Heading>,
    /// First H1, else frontmatter `title:`, else None (caller falls back to file stem).
    pub title: Option<String>,
    /// Frontmatter `aliases:` (or `alias:`): the other names Obsidian finds the note by.
    pub aliases: Vec<String>,
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

/// The one set of parser options, shared by [`analyze`] and [`to_html`] so the preview cannot
/// disagree with the highlighting about what a note says.
///
/// `ENABLE_HEADING_ATTRIBUTES` is deliberately absent: pulldown-cmark 0.13.4 panics on some
/// setext headings while parsing an attribute block, and `{#custom-id}` after a heading is a
/// pulldown-cmark extension nothing here uses. It renders as literal text instead.
fn options() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_MATH
        | Options::ENABLE_WIKILINKS
        | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS
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
    // Git's conflict markers are not markdown: read as it, `=======` makes the current side a
    // heading and `>>>>>>>` a quote. Blanked, each side reads as the prose it is.
    let blank = crate::conflict::blank_markers(text);
    let text = blank.as_deref().unwrap_or(text);
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
                wrapped(&mut a.spans, text, r, Style::CodeInline, b"`", n);
                if let Some(h) = heading.as_mut() {
                    h.2.push_str(t);
                }
                if let Some(l) = open.last_mut() {
                    l.text.push_str(t);
                }
            }
            Event::InlineMath(_) => wrapped(&mut a.spans, text, r, Style::Math, b"$", 1),
            Event::DisplayMath(_) => wrapped(&mut a.spans, text, r, Style::Math, b"$", 2),
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
                if let Some(m) = meta_close(text, &r) {
                    a.spans.push(sp(m, Style::Marker));
                }
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
                    wiki_markers(&mut a.spans, text, &r, b"[[");
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
                    wiki_markers(&mut a.spans, text, &r, b"![[");
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

/// Shorthands the submodules' tests share.
#[cfg(test)]
pub(super) mod testing {
    use super::*;

    pub fn link(text: &str, i: usize) -> Link {
        analyze(text).links[i].clone()
    }

    pub fn names(text: &str) -> Vec<String> {
        analyze(text).tags.into_iter().map(|t| t.name).collect()
    }

    /// The bytes a span actually covers — every offset assertion goes through this.
    pub fn at<'a>(text: &'a str, s: &Span) -> &'a str {
        &text[s.range.clone()]
    }

    pub fn spans_of(a: &Analysis, style: Style) -> Vec<Range<usize>> {
        a.spans
            .iter()
            .filter(|s| s.style == style)
            .map(|s| s.range.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conflict_markers_are_not_read_as_markdown() {
        // Read as markdown, `=======` makes the two lines above it a heading and `>>>>>>>` a
        // quote seven deep.
        let t = "<<<<<<< HEAD\nours *here*\n=======\ntheirs\n>>>>>>> side\n";
        let a = analyze(t);
        assert!(a.headings.is_empty() && a.title.is_none());
        let styles: Vec<Style> = a.spans.iter().map(|s| s.style).collect();
        assert_eq!(styles, [Style::Emphasis, Style::Marker, Style::Marker]);
        assert_eq!(testing::at(t, &a.spans[0]), "here");
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

    /// Every range `analyze` reports is a byte range into the source, and consumers slice them
    /// directly — uniffi hands them to Android as they are, and `&text[range]` panics on a
    /// boundary that cuts a character in half. So: every construct, crossed with a character of
    /// each UTF-8 width.
    #[test]
    fn every_span_boundary_survives_a_multibyte_note() {
        let samples = ["é", "中", "🎉", "aé中🎉"];
        let templates = [
            "[[X]]",
            "[[X|X]]",
            "![[X]]",
            "[[X#X|X]]",
            "**X**",
            "*X*",
            "__X__",
            "~~X~~",
            "`X`",
            "``X``",
            "$X$",
            "$$X$$",
            // A math run pulldown-cmark reports as starting inside the alias, not at its `$`.
            "[[$|X$]]",
            "[[X|$X$]]",
            // An embed whose event range pulldown-cmark starts past its own `![[`.
            "![[# :![[X|X)]]&.%)]]",
            "# X\n",
            "- X\n",
            "1. X\n",
            "> X\n",
            "#X\n",
            "[X](X)",
            "```X\nX\n```\n",
            "---\ntitle: X\ntags: [X, X]\n---\n\n# X\n\n- [ ] X `X` **X** [[X|X]] #X\n",
            // A frontmatter block the parser ends short of its own delimiter: the closing
            // marker used to be the range's last three bytes, whatever they happened to be.
            "1. ---\n\tX\n---",
        ];
        for t in templates {
            for s in samples {
                let doc = t.replace('X', s);
                let a = analyze(&doc);
                for span in &a.spans {
                    assert!(
                        span.range.start <= span.range.end
                            && span.range.end <= doc.len()
                            && doc.is_char_boundary(span.range.start)
                            && doc.is_char_boundary(span.range.end),
                        "bad span {span:?} in {doc:?}"
                    );
                }
                for l in &a.links {
                    assert!(
                        doc.is_char_boundary(l.range.start) && doc.is_char_boundary(l.range.end),
                        "bad link {l:?} in {doc:?}"
                    );
                }
                for g in &a.tags {
                    assert!(
                        doc.is_char_boundary(g.range.start) && doc.is_char_boundary(g.range.end),
                        "bad tag {g:?} in {doc:?}"
                    );
                }
            }
        }
    }

    /// A hyphen, a space, `[`, a space, `]`, a space, a backslash, a newline, a tab, a hyphen:
    /// ten bytes that made pulldown-cmark slice `6..5` inside its heading-attribute block while
    /// looking at the setext heading the tab-indented `-` opens. Dropping
    /// `Options::ENABLE_HEADING_ATTRIBUTES` is what keeps it out of that code; a note holding
    /// these bytes used to take the window down.
    #[test]
    fn a_backslash_before_a_tabbed_setext_rule_does_not_panic() {
        let doc = "- [ ] \\\n\t-";
        assert_eq!(doc.len(), 10);
        analyze(doc);
        to_html(doc);
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
