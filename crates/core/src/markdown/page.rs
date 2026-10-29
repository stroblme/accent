//! A note as the blocks its page shows: pulldown-cmark's events as accent rewrites them, with a
//! typed node wherever accent shows something of its own (a wikilink, a `#tag`, a formula, a
//! conflict side), cut into top-level blocks with the bytes and lines each is written on.
//!
//! The page is the same for every reader: [`super::to_html`] and [`super::to_html_file`] write
//! it out, each by its own rules, and the native preview draws it, redrawing only the blocks an
//! edit changed. [`parts`], [`cut`], [`math`] and [`picture`] know no markdown, so another syntax
//! (org) builds the same blocks.

use super::blocks::block_ids;
use super::frontmatter::scan_tags;
use super::html::mathml;
use super::links::{heading_named, is_image, percent_decode, slugs, split_anchor};
use super::options;
use super::spans::list_marker;
use super::task::in_progress;
use crate::conflict;
use pulldown_cmark::{CowStr, Event, LinkType, Options, Parser, Tag as Cm, TagEnd};
use std::collections::HashMap;
use std::ops::Range;

/// A top-level element of a page: a paragraph, heading, list, quote, table, code block, rule,
/// HTML block or the front matter, or a conflict block whole.
#[derive(Debug, Clone, PartialEq)]
pub struct Block<'a> {
    /// The note's bytes it is written in.
    pub bytes: Range<usize>,
    /// The lines those bytes are on, 1-based, end exclusive.
    pub lines: Range<u32>,
    /// What it shows, in order. A line in it counts from `lines.start`, so a block an edit above
    /// only moved has equal nodes: equal nodes show the same.
    pub nodes: Vec<Node<'a>>,
}

/// A side of a conflict block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Current,
    Base,
    Incoming,
}

/// One step of a page: a pulldown-cmark event, or what accent shows of its own. A node that
/// opens something (a wikilink, a callout, a conflict side) is closed by a later one.
#[derive(Debug, Clone, PartialEq)]
pub enum Node<'a> {
    /// As pulldown-cmark's HTML writer takes it, with each heading's `id` and each link to a
    /// heading by its text pointed at that `id`.
    Event(Event<'a>),
    /// The start of the element it follows, on line `at` of its block, and that element's id,
    /// whole (`^para`): what the preview scrolls by and an in-note link lands on. [`cut`] sets
    /// `at` from the node's bytes.
    Line {
        at: u32,
        id: Option<String>,
    },
    /// A link to a vault file and an anchor in it, `[[target#anchor]]`; its text follows, up to
    /// a [`Node::LinkEnd`].
    WikiLink {
        target: String,
        anchor: Option<String>,
    },
    /// `![[target#anchor]]` of a file that is no picture: a link to it, closed as a wikilink is.
    Embed {
        target: String,
        anchor: Option<String>,
    },
    /// The end of a [`Node::WikiLink`] or [`Node::Embed`].
    LinkEnd,
    /// A `#tag` as written, `name` without its `#`: a link to the tag's notes.
    Tag {
        name: String,
        text: CowStr<'a>,
    },
    /// A picture in the vault, by its vault path ([`picture`]).
    Image(String),
    /// A diagram in the vault, by its vault path, shown as its page `anchor` names, else its
    /// first ([`picture`]).
    Diagram {
        target: String,
        anchor: Option<String>,
    },
    /// A formula and its MathML ([`math`]); one that does not parse stays its event.
    Math {
        tex: CowStr<'a>,
        display: bool,
        mathml: String,
    },
    /// A list item that is a task, in place of its `Start(Item)`.
    TaskItem,
    /// An in-progress task's box, `[/]`, which pulldown-cmark reads as text.
    TaskDoing,
    /// A callout, `> [!kind]`, in place of its quote's `Start(BlockQuote)`; `kind` lowercased.
    Callout {
        kind: String,
    },
    /// A callout's title, as plain text, first inside it.
    CalloutTitle(String),
    /// Underlined text (org's `_text_`), up to a [`Node::UnderlineEnd`].
    Underline,
    UnderlineEnd,
    /// A conflict block: its sides top to bottom, up to a [`Node::ConflictEnd`].
    Conflict,
    /// A side of a conflict block, under its marker's label on line `at` of the block; its
    /// lines follow, up to a [`Node::SideEnd`].
    Side {
        side: Side,
        label: String,
        at: u32,
    },
    SideEnd,
    ConflictEnd,
}

impl Block<'_> {
    /// The block owning all it holds, to outlive the note's text.
    pub fn into_static(self) -> Block<'static> {
        Block {
            bytes: self.bytes,
            lines: self.lines,
            nodes: self.nodes.into_iter().map(Node::into_static).collect(),
        }
    }
}

impl Node<'_> {
    fn into_static(self) -> Node<'static> {
        match self {
            Node::Event(ev) => Node::Event(ev.into_static()),
            Node::Line { at, id } => Node::Line { at, id },
            Node::WikiLink { target, anchor } => Node::WikiLink { target, anchor },
            Node::Embed { target, anchor } => Node::Embed { target, anchor },
            Node::LinkEnd => Node::LinkEnd,
            Node::Tag { name, text } => Node::Tag {
                name,
                text: text.into_static(),
            },
            Node::Image(target) => Node::Image(target),
            Node::Diagram { target, anchor } => Node::Diagram { target, anchor },
            Node::Math {
                tex,
                display,
                mathml,
            } => Node::Math {
                tex: tex.into_static(),
                display,
                mathml,
            },
            Node::TaskItem => Node::TaskItem,
            Node::TaskDoing => Node::TaskDoing,
            Node::Callout { kind } => Node::Callout { kind },
            Node::CalloutTitle(title) => Node::CalloutTitle(title),
            Node::Underline => Node::Underline,
            Node::UnderlineEnd => Node::UnderlineEnd,
            Node::Conflict => Node::Conflict,
            Node::Side { side, label, at } => Node::Side { side, label, at },
            Node::SideEnd => Node::SideEnd,
            Node::ConflictEnd => Node::ConflictEnd,
        }
    }

    /// How the node changes the depth of what is open: 1 for one that opens, -1 for one that
    /// closes, 0 for any other.
    fn depth(&self) -> isize {
        match self {
            Node::Event(Event::Start(_))
            | Node::WikiLink { .. }
            | Node::Embed { .. }
            | Node::TaskItem
            | Node::Callout { .. }
            | Node::Underline
            | Node::Conflict
            | Node::Side { .. } => 1,
            Node::Event(Event::End(_))
            | Node::LinkEnd
            | Node::UnderlineEnd
            | Node::SideEnd
            | Node::ConflictEnd => -1,
            _ => 0,
        }
    }
}

/// The page of a markdown note: the same for the preview, an exported file and the native
/// preview, whose writers each apply their own rules.
///
/// A heading's `id` is its [`slugs`] anchor, and a link naming a heading by its text points at
/// that `id`. A block id is a [`Node::Line`]'s, and gone from the text. A conflict block shows
/// its sides without git's markers, which markdown would misread; each stretch around and inside
/// it is parsed on its own with the whole note's [`definitions`], so reference links and
/// footnotes still find theirs across it.
pub fn page(text: &str) -> Vec<Block<'_>> {
    let mut page = Builder {
        defs: definitions(text),
        ..Builder::default()
    };
    for part in parts(text) {
        match part {
            Part::Stretch(r) => page.markdown(text, r),
            Part::Node(node, bytes) => page.nodes.push((node, bytes)),
        }
    }
    cut(text, page.nodes, &page.headings, &page.anchors)
}

/// A stretch of a note to read in its syntax, or a node of a conflict block, with its bytes.
pub(crate) enum Part {
    Stretch(Range<usize>),
    Node(Node<'static>, Range<usize>),
}

/// `text` in the order its page shows it: the stretches between conflict blocks, and each block
/// as its sides top to bottom, each side's lines a stretch under its marker's label. A side
/// starts where the marker above it is, so that is its label's line; the incoming side is named
/// by the marker below it, the `>>>>>>>` line.
pub(crate) fn parts(text: &str) -> Vec<Part> {
    let mut out = Vec::new();
    let mut at = 0;
    for block in conflict::blocks(text) {
        out.push(Part::Stretch(at..block.range.start));
        out.push(Part::Node(Node::Conflict, block.range.clone()));
        let m = block.markers();
        let (split, end) = (&m[m.len() - 2], &m[m.len() - 1]);
        let sides = [
            Some((Side::Current, &m[0], &m[0], block.ours.clone())),
            block
                .base
                .clone()
                .map(|base| (Side::Base, &m[1], &m[1], base)),
            Some((Side::Incoming, end, split, block.theirs.clone())),
        ];
        for (side, named, top, lines) in sides.into_iter().flatten() {
            let label = conflict::label(text, named.clone()).to_string();
            out.push(Part::Node(Node::Side { side, label, at: 0 }, top.clone()));
            out.push(Part::Stretch(lines));
            out.push(Part::Node(Node::SideEnd, block.range.clone()));
        }
        out.push(Part::Node(Node::ConflictEnd, block.range.clone()));
        at = block.range.end;
    }
    out.push(Part::Stretch(at..text.len()));
    out
}

/// A page's top-level blocks out of `nodes`, each with the bytes of `text` it stands for: a
/// block is a node opening at depth 0 up to the node closing it, or a node on its own there.
///
/// Every heading takes the `id` [`slugs`] gives its text, a link to `#text` the `id` of a heading
/// so named, and every [`Node::Line`] and [`Node::Side`] the line its bytes start on. `headings`
/// holds each heading's place in `nodes` and its text, `anchors` each in-page link's place.
pub(crate) fn cut<'a>(
    text: &str,
    mut nodes: Vec<(Node<'a>, Range<usize>)>,
    headings: &[(usize, String)],
    anchors: &[usize],
) -> Vec<Block<'a>> {
    let texts: Vec<&str> = headings.iter().map(|(_, h)| h.as_str()).collect();
    let ids = slugs(texts.iter().copied());
    // A link to a heading of the page goes to its `id`, the only anchor a page has, even when it
    // names the heading by its text, as the editor lets it.
    for &at in anchors {
        if let Node::Event(Event::Start(Cm::Link { dest_url, .. })) = &mut nodes[at].0
            && let Some(i) = heading_named(&texts, &percent_decode(&dest_url[1..]))
        {
            *dest_url = format!("#{}", ids[i]).into();
        }
    }
    for ((at, _), slug) in headings.iter().zip(ids) {
        if let Node::Event(Event::Start(Cm::Heading { id, .. })) = &mut nodes[*at].0 {
            *id = Some(slug.into());
        }
    }
    let newlines: Vec<usize> = text.match_indices('\n').map(|(i, _)| i).collect();
    let line = |at: usize| newlines.partition_point(|&n| n < at) as u32 + 1;
    let mut blocks = Vec::new();
    let mut open: Option<Block> = None;
    let mut depth = 0usize;
    for (mut node, bytes) in nodes {
        let block = open.get_or_insert_with(|| {
            let last = bytes.end.saturating_sub(1).max(bytes.start);
            Block {
                lines: line(bytes.start)..line(last) + 1,
                bytes: bytes.clone(),
                nodes: Vec::new(),
            }
        });
        if let Node::Line { at, .. } | Node::Side { at, .. } = &mut node {
            *at = line(bytes.start) - block.lines.start;
        }
        depth = depth.saturating_add_signed(node.depth());
        block.nodes.push(node);
        if depth == 0 {
            blocks.extend(open.take());
        }
    }
    blocks.extend(open);
    blocks
}

/// The node of a formula: its MathML, or, if its LaTeX does not parse, its event, so a typo
/// shows the source as written rather than nothing.
pub(crate) fn math(tex: CowStr<'_>, display: bool) -> Node<'_> {
    match mathml(&tex, display) {
        Some(mathml) => Node::Math {
            tex,
            display,
            mathml,
        },
        None if display => Node::Event(Event::DisplayMath(tex)),
        None => Node::Event(Event::InlineMath(tex)),
    }
}

/// What an embed of the vault file `target` shows in place: the picture, a diagram's page
/// `anchor` names, or `None` for a file that is neither.
pub(crate) fn picture(target: &str, anchor: Option<&str>) -> Option<Node<'static>> {
    if is_image(target) {
        Some(Node::Image(target.to_string()))
    } else if crate::path::is_diagram(target) {
        Some(Node::Diagram {
            target: target.to_string(),
            anchor: anchor.map(str::to_string),
        })
    } else {
        None
    }
}

/// Blocks that carry a source-line marker. Everything the editor can put a cursor in starts at
/// one of these, which is all the preview needs to scroll along.
fn is_block_start(ev: &Event) -> bool {
    matches!(
        ev,
        Event::Start(
            Cm::Paragraph
                | Cm::Heading { .. }
                | Cm::Item
                | Cm::BlockQuote(_)
                | Cm::CodeBlock(_)
                | Cm::Table(_)
                | Cm::HtmlBlock
        )
    )
}

/// The callout a quote starting at `src[at..]` opens: its type lowercased, its title, and the
/// bytes of its `[!type]` line, newline included. `None` for a plain quote. A `+` or `-` fold
/// marker is read past: a callout always shows open.
fn callout(src: &str, at: usize) -> Option<(String, String, Range<usize>)> {
    let line = src[at..].split('\n').next().unwrap_or_default();
    let head = line.strip_prefix('>')?.trim_start_matches([' ', '\t']);
    let (kind, rest) = head.strip_prefix("[!")?.split_once(']')?;
    let named = kind.starts_with(|c: char| c.is_ascii_alphabetic())
        && kind
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if !named {
        return None;
    }
    let kind = kind.to_ascii_lowercase();
    let title = match rest.strip_prefix(['+', '-']).unwrap_or(rest).trim() {
        "" => {
            let mut t = kind.clone();
            t[..1].make_ascii_uppercase();
            t
        }
        t => t.to_string(),
    };
    let start = at + line.len() - head.len();
    Some((kind, title, start..(at + line.len() + 1).min(src.len())))
}

/// Every reference and footnote definition of a note with a conflict block, as written, each
/// after a blank line; empty for any other note. A stretch parsed on its own knows only the
/// definitions in it, so [`page`] parses each with these after it.
fn definitions(text: &str) -> String {
    let Some(blank) = conflict::blank_markers(text) else {
        return String::new();
    };
    let parser = Parser::new_ext(&blank, options());
    let mut defs: Vec<Range<usize>> = parser
        .reference_definitions()
        .iter()
        .map(|(_, d)| d.span.clone())
        .collect();
    defs.extend(
        parser.into_offset_iter().filter_map(|(ev, r)| {
            matches!(ev, Event::Start(Cm::FootnoteDefinition(_))).then_some(r)
        }),
    );
    defs.into_iter()
        .map(|r| format!("\n\n{}", &text[r]))
        .collect()
}

/// What [`page`] has gathered so far, a stretch of the note at a time.
#[derive(Default)]
struct Builder<'a> {
    /// Every node so far, with the bytes of the note it stands for.
    nodes: Vec<(Node<'a>, Range<usize>)>,
    /// Each heading's place in `nodes` and its text, gathered the way `analyze` gathers
    /// `Heading::text` — every text and code event inside, an embed's too — so the `id` is the
    /// anchor the editor resolves.
    headings: Vec<(usize, String)>,
    /// The place in `nodes` of each link to `#…` on the page itself.
    anchors: Vec<usize>,
    /// The note's [`definitions`], parsed after every stretch.
    defs: String,
}

impl<'a> Builder<'a> {
    fn push(&mut self, node: Node<'a>, bytes: Range<usize>) {
        self.nodes.push((node, bytes));
    }

    /// Make the item being read a task, so the preview draws its box in place of the bullet. The
    /// box is the first thing in its item, so the nearest item start is its own.
    fn task_item(&mut self) {
        let item = |(n, _): &(Node, Range<usize>)| matches!(n, Node::Event(Event::Start(Cm::Item)));
        if let Some(li) = self.nodes.iter().rposition(item) {
            self.nodes[li].0 = Node::TaskItem;
        }
    }

    /// A text run of `src` at `r`, written at `bytes` of the note, with its `#tag`s as
    /// [`Node::Tag`]s. They are found in the source by the rule the index reads tags with, so a
    /// run the parser rewrote (an escape, an entity) is left as it is; one whose block id was cut
    /// off its end is still the source up to there.
    fn tagged(&mut self, src: &'a str, r: Range<usize>, t: CowStr<'a>, bytes: Range<usize>) {
        let shown = r.start..r.start + t.len();
        let mut tags = Vec::new();
        if src.get(shown.clone()) == Some(&*t) {
            scan_tags(src, &shown, &mut tags, &mut Vec::new());
        }
        if tags.is_empty() {
            return self.push(Node::Event(Event::Text(t)), bytes);
        }
        let mut at = shown.start;
        for tag in tags {
            if at < tag.range.start {
                let before = Event::Text(src[at..tag.range.start].into());
                self.push(Node::Event(before), bytes.clone());
            }
            let text = src[tag.range.clone()].into();
            self.push(
                Node::Tag {
                    name: tag.name,
                    text,
                },
                bytes.clone(),
            );
            at = tag.range.end;
        }
        if at < shown.end {
            self.push(Node::Event(Event::Text(src[at..shown.end].into())), bytes);
        }
    }

    /// The markdown of `text[part]`, parsed as a note of its own.
    fn markdown(&mut self, text: &'a str, part: Range<usize>) {
        let src = &text[part.clone()];
        let mut opts = options();
        // Front matter opens the note, never a stretch after a conflict block.
        if part.start > 0 {
            opts.remove(Options::ENABLE_YAML_STYLE_METADATA_BLOCKS);
        }
        let blocks = block_ids(src);
        // Asked of every event, so indexed once: a block start's id (the first, if two name it),
        // and the marker inside a text, found by halving, as `block_ids` gives them in order.
        let mut ids = HashMap::new();
        for b in &blocks {
            ids.entry(b.start).or_insert(b.id.as_str());
        }
        let marker_in = |r: &Range<usize>| {
            let at = blocks.partition_point(|b| b.marker.start < r.start);
            blocks.get(at).filter(|b| r.contains(&b.marker.start))
        };
        // Whether each open link and embed is a node of ours rather than the parser's event.
        let mut link_ours: Vec<bool> = Vec::new();
        let mut image_ours: Vec<bool> = Vec::new();
        let mut skip = 0usize;
        // The `[/]` the item being read opens with, and once its box is in, the bytes it covered.
        let mut doing: Option<Range<usize>> = None;
        let mut dropped = 0..0;
        let mut in_heading = false;
        // Where a text run is never a tag, as `analyze` reads them: code and the front matter.
        let mut in_code = false;
        let mut in_meta = false;
        // The `[!type]` line of the callout being read, its newline included: its events go.
        let mut header = 0..0;

        // The definitions go on after the stretch, where the parser finds them; nothing of them
        // is shown, their events all starting past its end.
        let joined;
        let events: Box<dyn Iterator<Item = (Event<'a>, Range<usize>)>> = if self.defs.is_empty() {
            Box::new(Parser::new_ext(src, opts).into_offset_iter())
        } else {
            joined = format!("{src}{}", self.defs);
            Box::new(
                Parser::new_ext(&joined, opts)
                    .into_offset_iter()
                    .filter(|(_, r)| r.start < src.len())
                    .map(|(ev, r)| (ev.into_static(), r)),
            )
        };
        let mut events = events.peekable();
        while let Some((ev, r)) = events.next() {
            if header.contains(&r.start) && r.end <= header.end {
                continue;
            }
            match &ev {
                Event::Start(Cm::Heading { .. }) => {
                    in_heading = true;
                    self.headings.push((self.nodes.len(), String::new()));
                }
                Event::End(TagEnd::Heading(_)) => in_heading = false,
                Event::Start(Cm::CodeBlock(_)) => in_code = true,
                Event::End(TagEnd::CodeBlock) => in_code = false,
                Event::Start(Cm::MetadataBlock(_)) => in_meta = true,
                Event::End(TagEnd::MetadataBlock(_)) => in_meta = false,
                Event::Text(t) | Event::Code(t) if in_heading => {
                    if let Some((_, h)) = self.headings.last_mut() {
                        h.push_str(t);
                    }
                }
                _ => {}
            }
            if skip > 0 {
                match ev {
                    Event::Start(Cm::Image { .. }) => skip += 1,
                    Event::End(TagEnd::Image) => skip -= 1,
                    _ => {}
                }
                continue;
            }
            // The note's bytes of the event, short of the definitions after the stretch.
            let bytes = part.start + r.start..part.start + r.end.min(src.len());
            let line = is_block_start(&ev).then(|| {
                let id = ids.get(&r.start).map(|id| format!("^{id}"));
                (Node::Line { at: 0, id }, bytes.clone())
            });
            // A callout's header is its quote's first paragraph line, which the title replaces,
            // after the quote's line.
            if matches!(ev, Event::Start(Cm::BlockQuote(_)))
                && let Some((kind, title, header_line)) = callout(src, r.start)
                && let Some((Event::Start(Cm::Paragraph), p)) = events.peek()
                && p.start == header_line.start
            {
                header = header_line;
                self.push(Node::Callout { kind }, bytes.clone());
                self.nodes.extend(line);
                self.push(Node::CalloutTitle(title), bytes);
                continue;
            }
            // The text a block's id ends goes on without it.
            let ev = match (ev, marker_in(&r)) {
                (Event::Text(t), Some(b)) => {
                    let shown = t.strip_suffix(&src[b.marker.clone()]).unwrap_or(&t);
                    Event::Text(shown.trim_end().to_string().into())
                }
                (ev, _) => ev,
            };
            match ev {
                Event::Start(Cm::Link {
                    link_type: LinkType::WikiLink { .. },
                    ref dest_url,
                    ..
                }) => {
                    let (target, anchor) = split_anchor(dest_url);
                    let link = Node::WikiLink {
                        target: target.to_string(),
                        anchor: anchor.map(str::to_string),
                    };
                    self.push(link, bytes);
                    link_ours.push(true);
                }
                Event::Start(Cm::Link { ref dest_url, .. }) => {
                    if dest_url.starts_with('#') {
                        self.anchors.push(self.nodes.len());
                    }
                    link_ours.push(false);
                    self.push(Node::Event(ev), bytes);
                }
                Event::End(TagEnd::Link) => match link_ours.pop() {
                    Some(true) => self.push(Node::LinkEnd, bytes),
                    _ => self.push(Node::Event(ev), bytes),
                },
                Event::Start(Cm::Image {
                    link_type: LinkType::WikiLink { .. },
                    ref dest_url,
                    ..
                }) => {
                    let (target, anchor) = split_anchor(dest_url);
                    if let Some(shown) = picture(target, anchor) {
                        self.push(shown, bytes);
                        skip = 1;
                    } else {
                        let embed = Node::Embed {
                            target: target.to_string(),
                            anchor: anchor.map(str::to_string),
                        };
                        self.push(embed, bytes);
                        image_ours.push(true);
                    }
                }
                Event::Start(Cm::Image { .. }) => {
                    image_ours.push(false);
                    self.push(Node::Event(ev), bytes);
                }
                Event::End(TagEnd::Image) => match image_ours.pop() {
                    Some(true) => self.push(Node::LinkEnd, bytes),
                    _ => self.push(Node::Event(ev), bytes),
                },
                Event::InlineMath(tex) => self.push(math(tex, false), bytes),
                Event::DisplayMath(tex) => self.push(math(tex, true), bytes),
                Event::TaskListMarker(_) => {
                    self.task_item();
                    self.push(Node::Event(ev), bytes);
                }
                Event::Start(Cm::Item) => {
                    doing = list_marker(src, &r).and_then(|m| in_progress(src, m.end));
                    self.push(Node::Event(ev), bytes);
                }
                // `[/]` is three runs of text to pulldown-cmark: a box of its own takes the first
                // one's place, and the other two and the space after them go, as the parser drops
                // a task's own brackets.
                Event::Text(_) if doing.as_ref().is_some_and(|d| d.start == r.start) => {
                    dropped = doing.take().unwrap_or_default();
                    self.task_item();
                    self.push(Node::TaskDoing, bytes);
                }
                Event::Text(_) if dropped.contains(&r.start) => {}
                Event::Text(t) if !dropped.is_empty() && dropped.end == r.start => {
                    dropped = 0..0;
                    let shown = t.trim_start().to_string();
                    self.tagged(src, r.end - shown.len()..r.end, shown.into(), bytes);
                }
                Event::Text(t)
                    if !in_code && !in_meta && link_ours.is_empty() && image_ours.is_empty() =>
                {
                    self.tagged(src, r, t, bytes)
                }
                _ => self.push(Node::Event(ev), bytes),
            }
            self.nodes.extend(line);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each top-level element is a block of the bytes and lines it is written on; a line inside
    /// one counts from its first, a conflict side's label too.
    #[test]
    fn page_cuts_top_level_blocks_with_their_lines() {
        let text = "# Title\n\nSome prose. ^p\n\n- a\n- b\n\n***\n\n\
                    <<<<<<< HEAD\nours\n=======\ntheirs\n>>>>>>> side\nafter";
        let blocks = page(text);
        let lines: Vec<Range<u32>> = blocks.iter().map(|b| b.lines.clone()).collect();
        // A list's bytes run on over the blank line after it, as pulldown-cmark gives them.
        assert_eq!(lines, [1..2, 3..4, 5..8, 8..9, 10..15, 15..16]);
        assert_eq!(&text[blocks[2].bytes.clone()], "- a\n- b\n\n");
        assert_eq!(&text[blocks[5].bytes.clone()], "after");
        let marks = |b: &Block| -> Vec<(u32, Option<String>)> {
            b.nodes
                .iter()
                .filter_map(|n| match n {
                    Node::Line { at, id } => Some((*at, id.clone())),
                    Node::Side { at, .. } => Some((*at, None)),
                    _ => None,
                })
                .collect()
        };
        assert_eq!(marks(&blocks[1]), [(0, Some("^p".into()))]);
        assert_eq!(marks(&blocks[2]), [(0, None), (1, None)]);
        assert_eq!(
            marks(&blocks[4]),
            [(0, None), (1, None), (2, None), (3, None)]
        );
    }

    /// A block an edit above only moved shows the same, so the native preview keeps it.
    #[test]
    fn a_block_below_an_edit_compares_equal() {
        let tail = "## Head\n\n- [ ] task #tag and [[Note]] $x$\n";
        let before = page(tail);
        let after = format!("Intro,\nnow on two lines.\n\n{tail}");
        let after = page(&after);
        assert_eq!(after.len(), before.len() + 1);
        for (a, b) in after[1..].iter().zip(&before) {
            assert_eq!(a.nodes, b.nodes);
            assert_eq!(a.lines.start, b.lines.start + 3);
        }
    }
}
