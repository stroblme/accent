//! A page's blocks written into the preview's buffer: the tags its text is set in, the boxes of
//! task items, the raw HTML a note holds unwrapped into text, and per block the facts the view
//! reads back: where its lines start, its anchors, its links and the source of each object.

use super::native::{Laid, Page};
use super::{figure, formula, table};
use accent_core::markdown::{Node, Side};
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene, gsk, pango};
use pulldown_cmark::{CodeBlockKind, Event, Tag as Cm, TagEnd};
use std::cell::Cell;
use std::ops::Range;
use std::rc::Rc;

// -------------------------------------------------------------------------------------- tags

/// The tags a page is set in, one table per preview. Made in order of priority: a later one wins
/// where two set the same property.
pub(super) struct Tags {
    pub prose: gtk::TextTag,
    pub gap: gtk::TextTag,
    /// The line a block that shows nothing still takes, as thin as it goes.
    pub void: gtk::TextTag,
    pub headings: [gtk::TextTag; 6],
    pub em: gtk::TextTag,
    pub strong: gtk::TextTag,
    pub strike: gtk::TextTag,
    pub underline: gtk::TextTag,
    pub sub: gtk::TextTag,
    pub sup: gtk::TextTag,
    pub code: gtk::TextTag,
    pub kbd: gtk::TextTag,
    pub codeblock: gtk::TextTag,
    pub caption: gtk::TextTag,
    pub quote: gtk::TextTag,
    pub callout: gtk::TextTag,
    pub callout_title: gtk::TextTag,
    pub lists: [gtk::TextTag; 6],
    pub taskdone: gtk::TextTag,
    pub dt: gtk::TextTag,
    pub dd: gtk::TextTag,
    pub rule: gtk::TextTag,
    pub display: gtk::TextTag,
    pub math: gtk::TextTag,
    pub html: gtk::TextTag,
    pub figure: gtk::TextTag,
    pub fnref: gtk::TextTag,
    pub fndef: gtk::TextTag,
    pub summary: gtk::TextTag,
    pub todo: gtk::TextTag,
    pub done: gtk::TextTag,
    pub link: gtk::TextTag,
    pub hashtag: gtk::TextTag,
    /// Each conflict side's body and the caption over it: current, base, incoming.
    pub sides: [(gtk::TextTag, gtk::TextTag); 3],
}

/// The share of the font size a code block's text is set at, as the stylesheet's `pre`.
const MONO: f64 = 0.92;

impl Tags {
    pub(super) fn new(table: &gtk::TextTagTable) -> Tags {
        let tag = |name: &str| {
            let tag = gtk::TextTag::new(Some(name));
            table.add(&tag);
            tag
        };
        let set = |name: &str, f: &dyn Fn(&gtk::TextTag)| {
            let t = tag(name);
            f(&t);
            t
        };
        Tags {
            prose: set("prose", &|t| t.set_line_height(1.6)),
            gap: tag("gap"),
            void: set("void", &|t| t.set_scale(0.05)),
            headings: std::array::from_fn(|i| {
                set(&format!("h{}", i + 1), &|t| {
                    t.set_scale(crate::highlight::HEADING_SCALES[i]);
                    t.set_weight(700);
                    t.set_line_height(1.25);
                })
            }),
            em: set("em", &|t| t.set_style(pango::Style::Italic)),
            strong: set("strong", &|t| t.set_weight(700)),
            strike: set("strike", &|t| t.set_strikethrough(true)),
            underline: set("underline", &|t| t.set_underline(pango::Underline::Single)),
            sub: set("sub", &|t| t.set_scale(0.8)),
            sup: set("sup", &|t| t.set_scale(0.8)),
            code: set("code", &|t| {
                t.set_family(Some("monospace"));
                t.set_scale(MONO);
            }),
            kbd: set("kbd", &|t| {
                t.set_family(Some("monospace"));
                t.set_scale(0.85);
            }),
            codeblock: set("codeblock", &|t| {
                t.set_family(Some("monospace"));
                t.set_scale(MONO);
                t.set_wrap_mode(gtk::WrapMode::Char);
                t.set_line_height(1.25);
            }),
            caption: set("caption", &|t| t.set_scale(0.85)),
            quote: set("quote", &|t| t.set_style(pango::Style::Italic)),
            callout: tag("callout"),
            callout_title: set("callout-title", &|t| t.set_weight(700)),
            lists: std::array::from_fn(|i| tag(&format!("list{}", i + 1))),
            taskdone: tag("taskdone"),
            dt: set("dt", &|t| t.set_weight(700)),
            dd: tag("dd"),
            rule: tag("rule"),
            display: set("display", &|t| {
                t.set_justification(gtk::Justification::Center)
            }),
            math: set("math", &|t| t.set_family(Some("monospace"))),
            html: set("html", &|t| t.set_family(Some("monospace"))),
            figure: tag("figure"),
            fnref: set("fnref", &|t| t.set_scale(0.8)),
            fndef: set("fndef", &|t| t.set_scale(0.9)),
            summary: set("summary", &|t| t.set_weight(700)),
            todo: set("todo", &|t| t.set_weight(700)),
            done: tag("done"),
            link: set("link", &|t| t.set_underline(pango::Underline::Single)),
            hashtag: tag("tag"),
            sides: ["current", "base", "incoming"].map(|side| {
                let label = set(&format!("conflict-{side}-label"), &|t| {
                    t.set_weight(700);
                    t.set_scale(0.85);
                });
                (tag(&format!("conflict-{side}")), label)
            }),
        }
    }

    /// The sizes measured in the font's pixels, `px` at the zoom, and the page's gutter, which a
    /// tag's margin replaces rather than adds to.
    pub(super) fn resize(&self, px: f64, gutter: i32) {
        let em = |n: f64| (n * px).round() as i32;
        self.gap.set_pixels_below_lines(em(0.8));
        for (tag, scale) in self.headings.iter().zip(crate::highlight::HEADING_SCALES) {
            tag.set_pixels_above_lines(em(1.2 * scale));
            tag.set_pixels_below_lines(em(0.4 * scale));
        }
        let inset = gutter + 12;
        self.codeblock.set_left_margin(inset);
        self.codeblock.set_right_margin(inset);
        self.quote.set_left_margin(inset + 3);
        self.callout.set_left_margin(inset + 3);
        self.callout.set_right_margin(inset);
        for (i, tag) in self.lists.iter().enumerate() {
            tag.set_left_margin(gutter + em(1.4 * (i + 1) as f64));
            tag.set_indent(-em(1.2));
        }
        self.dd.set_left_margin(gutter + em(1.4));
        self.rule.set_pixels_above_lines(12);
        self.rule.set_pixels_below_lines(12);
        self.display.set_pixels_above_lines(em(1.2));
        self.display.set_pixels_below_lines(em(1.2));
        self.sub.set_rise(-em(0.3) * pango::SCALE);
        self.sup.set_rise(em(0.4) * pango::SCALE);
        self.fnref.set_rise(em(0.4) * pango::SCALE);
    }

    /// The colours, from the text's `fg` on `page` and the accent: the stylesheet's.
    pub(super) fn restyle(&self, ink: &Ink) {
        let (fg, page, accent) = (ink.fg.get(), ink.page.get(), ink.accent.get());
        let dim = |alpha: f32| crate::highlight::dim(fg, page, alpha);
        let surface = crate::theme::at(fg, 0.07);
        self.prose.set_foreground_rgba(Some(&fg));
        for tag in [&self.link, &self.hashtag, &self.callout_title, &self.todo] {
            tag.set_foreground_rgba(Some(&accent));
        }
        self.fnref.set_foreground_rgba(Some(&accent));
        self.quote.set_foreground_rgba(Some(&dim(0.6)));
        self.callout.set_foreground_rgba(Some(&fg));
        for tag in [&self.taskdone, &self.done, &self.caption, &self.figure] {
            tag.set_foreground_rgba(Some(&dim(0.55)));
        }
        self.html.set_foreground_rgba(Some(&dim(0.4)));
        for tag in [&self.code, &self.kbd] {
            tag.set_background_rgba(Some(&surface));
        }
        let tints = crate::conflict::tints(fg, page);
        for ((body, label), (tint, head)) in self.sides.iter().zip(tints) {
            body.set_paragraph_background_rgba(Some(&tint));
            label.set_paragraph_background_rgba(Some(&crate::theme::over(head, page)));
        }
    }
}

/// The colours drawn by hand, by the view and by the boxes and formulas in the text, which a
/// restyle changes in place.
pub struct Ink {
    pub fg: Cell<gdk::RGBA>,
    pub page: Cell<gdk::RGBA>,
    pub accent: Cell<gdk::RGBA>,
}

impl Default for Ink {
    fn default() -> Ink {
        Ink {
            fg: Cell::new(gdk::RGBA::BLACK),
            page: Cell::new(gdk::RGBA::WHITE),
            accent: Cell::new(gdk::RGBA::BLUE),
        }
    }
}

// ---------------------------------------------------------------------------------- task boxes

/// A task item's box: open, in progress (half filled) or done (filled and ticked).
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum Task {
    #[default]
    Open,
    Doing,
    Done,
}

glib::wrapper! {
    /// A task's box, one paintable for all three (refresh T4), as large as the font's `x` is
    /// tall plus a little, in the accent faded as a disabled checkbox is.
    pub struct TaskBox(ObjectSubclass<boxes::TaskBox>) @implements gdk::Paintable;
}

impl TaskBox {
    fn new(task: Task, side: i32, ink: Rc<Ink>) -> TaskBox {
        let paintable: TaskBox = glib::Object::new();
        paintable.imp().task.set(task);
        paintable.imp().side.set(side);
        let _ = paintable.imp().ink.set(ink);
        paintable
    }
}

mod boxes {
    use super::*;
    use std::cell::OnceCell;

    #[derive(Default)]
    pub struct TaskBox {
        pub task: Cell<Task>,
        pub side: Cell<i32>,
        pub ink: OnceCell<Rc<Ink>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TaskBox {
        const NAME: &'static str = "AccentPreviewTaskBox";
        type Type = super::TaskBox;
        type Interfaces = (gdk::Paintable,);
    }

    impl ObjectImpl for TaskBox {}

    impl PaintableImpl for TaskBox {
        fn intrinsic_width(&self) -> i32 {
            self.side.get()
        }

        fn intrinsic_height(&self) -> i32 {
            self.side.get()
        }

        fn snapshot(&self, snapshot: &gdk::Snapshot, width: f64, height: f64) {
            let (Some(snapshot), Some(ink)) =
                (snapshot.downcast_ref::<gtk::Snapshot>(), self.ink.get())
            else {
                return;
            };
            let (w, h) = (width as f32, height as f32);
            let accent = crate::theme::at(ink.accent.get(), 0.5);
            let line = (w / 7.0).max(1.5);
            let rect = graphene::Rect::new(0.0, 0.0, w, h);
            let corner = graphene::Size::new(w / 4.0, w / 4.0);
            let shape = gsk::RoundedRect::new(rect, corner, corner, corner, corner);
            match self.task.get() {
                Task::Done => {
                    snapshot.push_rounded_clip(&shape);
                    snapshot.append_color(&accent, &rect);
                    snapshot.pop();
                    let tick = gsk::PathBuilder::new();
                    tick.move_to(w * 0.25, h * 0.52);
                    tick.line_to(w * 0.43, h * 0.7);
                    tick.line_to(w * 0.76, h * 0.32);
                    snapshot.append_stroke(
                        &tick.to_path(),
                        &gsk::Stroke::new(line),
                        &ink.page.get(),
                    );
                }
                task => {
                    if task == Task::Doing {
                        snapshot.push_rounded_clip(&shape);
                        snapshot.append_color(&accent, &graphene::Rect::new(0.0, 0.0, w / 2.0, h));
                        snapshot.pop();
                    }
                    snapshot.append_border(&shape, &[line; 4], &[accent; 4]);
                }
            }
        }
    }
}

// --------------------------------------------------------------------------------- raw HTML

/// What a run of raw HTML in a note shows (user decision 2026-10-22: unwrap). A comment, a
/// `<style>` or `<script>` show nothing; `<br>` breaks the line; a few tags style their text;
/// `<img>` is a picture, `<iframe>` and `<video>` a link to their source; every other tag is
/// dropped and its content shown, a block-level one breaking the paragraph.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Piece {
    Text(String),
    Break,
    Paragraph,
    Open(Style),
    Close(Style),
    Image { src: String, alt: String },
    Link { href: String, label: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Style {
    Sub,
    Sup,
    Kbd,
    Strong,
    Emphasis,
    Underline,
    /// A `<details>` block's `<summary>`: a bold line, the block always open (GtkTextView cannot
    /// hide text safely, ISSUES.md).
    Summary,
}

/// Tags that start a block of their own, so their start and end break the paragraph.
const BLOCKS: [&str; 20] = [
    "p",
    "div",
    "center",
    "details",
    "section",
    "article",
    "header",
    "footer",
    "figure",
    "figcaption",
    "ul",
    "ol",
    "li",
    "table",
    "tr",
    "blockquote",
    "pre",
    "h1",
    "h2",
    "h3",
];

pub(crate) fn classify(html: &str) -> Vec<Piece> {
    let mut out = Vec::new();
    let mut rest = html;
    let mut text = String::new();
    let flush = |text: &mut String, out: &mut Vec<Piece>| {
        if !text.is_empty() {
            out.push(Piece::Text(collapse(&decode(text))));
            text.clear();
        }
    };
    while let Some(at) = rest.find('<') {
        text.push_str(&rest[..at]);
        rest = &rest[at..];
        if let Some(after) = rest.strip_prefix("<!--") {
            flush(&mut text, &mut out);
            rest = after.split_once("-->").map_or("", |(_, r)| r);
            continue;
        }
        let Some((tag, after)) = parse_tag(rest) else {
            text.push('<');
            rest = &rest[1..];
            continue;
        };
        flush(&mut text, &mut out);
        rest = after;
        let name = tag.name.as_str();
        match (name, tag.closing) {
            ("style" | "script", false) => {
                let end = format!("</{name}");
                let lower = rest.to_ascii_lowercase();
                rest = match lower.find(&end) {
                    Some(i) => rest[i..].split_once('>').map_or("", |(_, r)| r),
                    None => "",
                };
            }
            ("br", _) => out.push(Piece::Break),
            ("img", false) => out.push(Piece::Image {
                src: tag.attr("src").unwrap_or_default(),
                alt: tag.attr("alt").unwrap_or_default(),
            }),
            ("iframe" | "video", false) => {
                if let Some(src) = tag.attr("src") {
                    let label = src.trim_end_matches('/').rsplit('/').next().unwrap_or(&src);
                    out.push(Piece::Link {
                        label: label.to_string(),
                        href: src.clone(),
                    });
                }
            }
            ("summary", closing) => out.push(match closing {
                false => Piece::Open(Style::Summary),
                true => Piece::Close(Style::Summary),
            }),
            (name, closing) => {
                let style = match name {
                    "sub" => Some(Style::Sub),
                    "sup" => Some(Style::Sup),
                    "kbd" => Some(Style::Kbd),
                    "b" | "strong" => Some(Style::Strong),
                    "i" | "em" => Some(Style::Emphasis),
                    "u" | "ins" => Some(Style::Underline),
                    _ => None,
                };
                match style {
                    Some(style) if closing => out.push(Piece::Close(style)),
                    Some(style) => out.push(Piece::Open(style)),
                    None if BLOCKS.contains(&name) => out.push(Piece::Paragraph),
                    None => {}
                }
            }
        }
    }
    text.push_str(rest);
    flush(&mut text, &mut out);
    out
}

struct HtmlTag {
    name: String,
    closing: bool,
    attrs: String,
}

impl HtmlTag {
    /// The value of attribute `name`, quoted either way or bare, entities decoded.
    fn attr(&self, name: &str) -> Option<String> {
        let lower = self.attrs.to_ascii_lowercase();
        let mut from = 0;
        while let Some(i) = lower[from..].find(name).map(|i| i + from) {
            from = i + name.len();
            let before = lower[..i].chars().next_back();
            let after = lower[from..].trim_start();
            if before.is_some_and(|c| !c.is_whitespace()) || !after.starts_with('=') {
                continue;
            }
            let value = self.attrs[self.attrs.len() - after.len() + 1..].trim_start();
            let value = match value.chars().next() {
                Some(q @ ('"' | '\'')) => value[1..].split(q).next().unwrap_or(""),
                _ => value
                    .split(|c: char| c.is_whitespace() || c == '>')
                    .next()
                    .unwrap_or(""),
            };
            return Some(decode(value));
        }
        None
    }
}

/// The tag `s` opens with, and what follows it; `None` where `<` starts no tag.
fn parse_tag(s: &str) -> Option<(HtmlTag, &str)> {
    let body = &s[1..];
    let (closing, body) = match body.strip_prefix('/') {
        Some(b) => (true, b),
        None => (false, body),
    };
    let name_len = body
        .find(|c: char| !c.is_ascii_alphanumeric())
        .unwrap_or(body.len());
    if name_len == 0 || !body.starts_with(|c: char| c.is_ascii_alphabetic()) {
        return None;
    }
    let end = body.find('>')?;
    let tag = HtmlTag {
        name: body[..name_len].to_ascii_lowercase(),
        closing,
        attrs: body[name_len..end].trim_end_matches('/').to_string(),
    };
    Some((tag, &body[end + 1..]))
}

/// Whitespace as HTML shows it: any run of it one space.
fn collapse(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for word in text.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    if text.starts_with(char::is_whitespace) && !out.is_empty() {
        out.insert(0, ' ');
    }
    if text.ends_with(char::is_whitespace) && !out.ends_with(' ') {
        out.push(' ');
    }
    out
}

/// The common entities, and numeric ones.
fn decode(text: &str) -> String {
    if !text.contains('&') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let Some(end) = rest.find(';').filter(|&e| e < 10) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let name = &rest[1..end];
        let ch = match name {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some('\u{a0}'),
            _ => name
                .strip_prefix("#x")
                .or_else(|| name.strip_prefix("#X"))
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| name.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match ch {
            Some(ch) => {
                out.push(ch);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

// ------------------------------------------------------------------------------------ writer

/// Where a link in the page goes: a vault file or a wikilink target with its anchor, a tag's
/// notes, or a markdown link's destination, which [`super::native`] resolves as WebKit did.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Href {
    Open(String),
    Tag(String),
    Url(String),
}

/// Something in the text that is no text, a U+FFFC: what the drill calls it, and what Copy
/// writes in its place.
pub(super) struct Object {
    #[cfg(feature = "bench")]
    pub label: String,
    pub source: String,
}

impl Object {
    /// `label` is what a drill prints for it.
    pub fn new(label: String, source: String) -> Object {
        #[cfg(not(feature = "bench"))]
        drop(label);
        Object {
            #[cfg(feature = "bench")]
            label,
            source,
        }
    }
}

/// What the view reads back of a block, each place a character offset from the block's start.
#[derive(Default)]
pub(super) struct Facts {
    /// The start of each element on line `at` of the block, as [`Node::Line`] gives it.
    pub lines: Vec<(u32, i32)>,
    /// Heading ids, block ids (`^para`), footnote definitions (`fn:label`), org targets.
    pub ids: Vec<(String, i32)>,
    pub links: Vec<(Range<i32>, Href)>,
    /// In the order they stand in the text.
    pub objects: Vec<Object>,
}

/// Write `laid` at `at`, ending on a line of its own, its source `source` (the note's bytes of
/// it); its start mark and its facts.
pub(super) fn write(
    page: &Page,
    at: &mut gtk::TextIter,
    laid: &Laid,
    source: &str,
) -> (gtk::TextMark, Facts) {
    let start = at.offset();
    let mut writer = Writer {
        page,
        start,
        at,
        source,
        notes: laid.notes.iter(),
        tags: Vec::new(),
        lists: Vec::new(),
        done: Vec::new(),
        links: Vec::new(),
        image: None,
        meta: false,
        rows: None,
        html: None,
        code: None,
        line_start: true,
        facts: Facts::default(),
    };
    for node in &laid.block.nodes {
        writer.node(node);
    }
    writer.newline();
    let facts = std::mem::take(&mut writer.facts);
    let buffer = &page.buffer;
    // Text of its own however empty the block (front matter), or its mark, which text put in at
    // it goes before, would slide past the next block's text.
    if at.offset() == start {
        buffer.insert_with_tags(at, "\n", &[&page.tags.void]);
    } else {
        let mut last = *at;
        last.backward_char();
        last.set_line_offset(0);
        buffer.apply_tag(&page.tags.gap, &last, at);
    }
    let first = buffer.iter_at_offset(start);
    buffer.apply_tag(&page.tags.prose, &first, at);
    (buffer.create_mark(None, &first, false), facts)
}

/// A block's nodes into the buffer; the hooks of figures, formulas and tables write through it.
pub(super) struct Writer<'a> {
    pub page: &'a Page,
    start: i32,
    pub at: &'a mut gtk::TextIter,
    /// The note's markdown of the block, which Copy writes for a table.
    pub source: &'a str,
    notes: std::slice::Iter<'a, usize>,
    pub tags: Vec<gtk::TextTag>,
    lists: Vec<Option<u64>>,
    /// Per open item, whether it is a done task, whose text is dimmed.
    done: Vec<bool>,
    links: Vec<(i32, Href)>,
    /// A markdown image being read: its destination and alt text.
    image: Option<(String, String)>,
    meta: bool,
    rows: Option<Vec<Vec<String>>>,
    html: Option<String>,
    code: Option<(String, i32)>,
    line_start: bool,
    facts: Facts,
}

impl Writer<'_> {
    /// Where the writer is, from the block's start.
    pub fn offset(&self) -> i32 {
        self.at.offset() - self.start
    }

    pub fn push(&mut self, tag: &gtk::TextTag) {
        self.tags.push(tag.clone());
    }

    pub fn pop(&mut self) {
        self.tags.pop();
    }

    pub fn text(&mut self, text: &str) {
        if let Some(cell) = self.rows.as_mut().and_then(|r| r.last_mut()?.last_mut()) {
            cell.push_str(text);
            return;
        }
        if let Some((_, alt)) = &mut self.image {
            alt.push_str(text);
            return;
        }
        if self.meta || text.is_empty() {
            return;
        }
        let tags: Vec<&gtk::TextTag> = self.tags.iter().collect();
        // A U+FFFC of the note's own would be taken for an object.
        let text = text.replace('\u{fffc}', "\u{fffd}");
        self.page.buffer.insert_with_tags(self.at, &text, &tags);
        self.line_start = text.ends_with('\n');
    }

    pub fn newline(&mut self) {
        if !self.line_start && self.rows.is_none() && self.image.is_none() {
            self.text("\n");
        }
    }

    /// `paintable` in the text, lowered `descent` pixels so its baseline is the line's.
    pub fn paintable(
        &mut self,
        paintable: &impl IsA<gdk::Paintable>,
        descent: i32,
        object: Object,
    ) {
        let buffer = &self.page.buffer;
        buffer.insert_paintable(self.at, paintable);
        let mut start = *self.at;
        start.backward_char();
        for tag in &self.tags {
            buffer.apply_tag(tag, &start, self.at);
        }
        if descent != 0 {
            buffer.apply_tag(&self.page.rise(descent), &start, self.at);
        }
        self.facts.objects.push(object);
        self.line_start = false;
    }

    /// `widget` on a line of its own.
    pub fn widget(&mut self, widget: &impl IsA<gtk::Widget>, object: Object) {
        self.newline();
        let anchor = self.page.buffer.create_child_anchor(self.at);
        self.page.view.add_child_at_anchor(widget, &anchor);
        self.facts.objects.push(object);
        self.line_start = false;
        self.newline();
    }

    /// Whether the writer is inside a table cell, whose content is plain text.
    pub fn in_cell(&self) -> bool {
        self.rows.is_some()
    }

    fn open_link(&mut self, href: Href) {
        self.links.push((self.offset(), href));
        let page = self.page;
        self.push(&page.tags.link);
    }

    fn close_link(&mut self) {
        self.pop();
        if let Some((start, href)) = self.links.pop()
            && start < self.offset()
        {
            self.facts.links.push((start..self.offset(), href));
        }
    }

    fn item(&mut self, task: bool) {
        self.newline();
        let depth = self.lists.len().clamp(1, 6);
        let page = self.page;
        self.push(&page.tags.lists[depth - 1]);
        self.done.push(false);
        if !task {
            let bullet = match self.lists.last_mut() {
                Some(Some(n)) => {
                    *n += 1;
                    format!("{}. ", *n - 1)
                }
                _ => "• ".to_string(),
            };
            self.text(&bullet);
        }
    }

    fn task(&mut self, task: Task) {
        let px = self.page.px.get();
        let side = (0.8 * px).round() as i32;
        let source = match task {
            Task::Open => "[ ]",
            Task::Doing => "[/]",
            Task::Done => "[x]",
        };
        let paintable = TaskBox::new(task, side, self.page.ink.clone());
        let object = Object::new(format!("box {source}"), source.to_string());
        self.paintable(&paintable, (0.1 * px).round() as i32, object);
        self.text(" ");
        if task == Task::Done {
            let page = self.page;
            self.push(&page.tags.taskdone);
            if let Some(done) = self.done.last_mut() {
                *done = true;
            }
        }
    }

    fn html(&mut self, html: &str) {
        let page = self.page;
        let tags = &page.tags;
        for piece in classify(html) {
            match piece {
                Piece::Text(t) => {
                    let t = if self.line_start { t.trim_start() } else { &t };
                    self.text(t);
                }
                Piece::Break => self.text("\u{2028}"),
                Piece::Paragraph => self.newline(),
                Piece::Open(Style::Summary) => {
                    self.newline();
                    self.push(&tags.summary);
                    self.text("▾ ");
                }
                Piece::Open(style) => self.push(style_tag(tags, style)),
                Piece::Close(style) => {
                    let tag = style_tag(tags, style);
                    if let Some(i) = self.tags.iter().rposition(|t| t == tag) {
                        self.tags.remove(i);
                    }
                    if style == Style::Summary {
                        self.newline();
                    }
                }
                Piece::Image { src, alt } => figure::html_image(self, &src, &alt),
                Piece::Link { href, label } => {
                    self.open_link(Href::Url(href));
                    self.text(&label);
                    self.close_link();
                }
            }
        }
    }

    fn node(&mut self, node: &Node) {
        let page = self.page;
        let tags = &page.tags;
        match node {
            Node::Event(event) => self.event(event),
            Node::Line { at, id } => {
                self.facts.lines.push((*at, self.offset()));
                if let Some(id) = id {
                    self.facts.ids.push((id.clone(), self.offset()));
                }
            }
            Node::WikiLink { target, anchor } | Node::Embed { target, anchor } => {
                let href = match anchor {
                    Some(anchor) => format!("{target}#{anchor}"),
                    None => target.clone(),
                };
                self.open_link(Href::Open(href));
            }
            Node::LinkEnd => self.close_link(),
            Node::Tag { name, text } => {
                let start = self.offset();
                self.push(&tags.hashtag);
                self.text(text);
                self.pop();
                if !self.in_cell() {
                    let range = start..self.offset();
                    self.facts.links.push((range, Href::Tag(name.clone())));
                }
            }
            Node::Image(target) => figure::image(self, target),
            Node::Diagram { target, anchor } => figure::diagram(self, target, anchor.as_deref()),
            Node::Math { tex, display, .. } => formula::insert(self, tex, *display),
            Node::TaskItem => self.item(true),
            Node::TaskDoing => self.task(Task::Doing),
            Node::Callout { .. } => self.push(&tags.callout),
            Node::CalloutTitle(title) => {
                self.push(&tags.callout_title);
                self.text(title);
                self.pop();
                self.newline();
            }
            Node::Underline => self.push(&tags.underline),
            Node::UnderlineEnd => self.pop(),
            Node::Keyword { text, done } => {
                self.push(if *done { &tags.done } else { &tags.todo });
                self.text(text);
                self.pop();
                self.text(" ");
            }
            Node::Anchor(name) => self.facts.ids.push((name.clone(), self.offset())),
            Node::Conflict | Node::ConflictEnd => self.newline(),
            Node::Side { side, label, at } => {
                let (body, caption) = &tags.sides[match side {
                    Side::Current => 0,
                    Side::Base => 1,
                    Side::Incoming => 2,
                }];
                self.newline();
                self.facts.lines.push((*at, self.offset()));
                self.push(body);
                self.push(caption);
                self.text(label);
                self.pop();
                self.newline();
            }
            Node::SideEnd => {
                self.newline();
                self.pop();
            }
        }
    }

    fn event(&mut self, event: &Event) {
        let page = self.page;
        let tags = &page.tags;
        match event {
            Event::Start(Cm::Heading { level, id, .. }) => {
                if let Some(id) = id {
                    self.facts.ids.push((id.to_string(), self.offset()));
                }
                self.push(&tags.headings[*level as usize - 1]);
            }
            Event::Start(Cm::Emphasis) => self.push(&tags.em),
            Event::Start(Cm::Strong) => self.push(&tags.strong),
            Event::Start(Cm::Strikethrough) => self.push(&tags.strike),
            Event::Start(Cm::Superscript) => self.push(&tags.sup),
            Event::Start(Cm::Subscript) => self.push(&tags.sub),
            Event::Start(Cm::BlockQuote(_)) => self.push(&tags.quote),
            Event::Start(Cm::Link { dest_url, .. }) => {
                self.open_link(Href::Url(dest_url.to_string()))
            }
            Event::Start(Cm::Image { dest_url, .. }) => {
                self.image = Some((dest_url.to_string(), String::new()));
            }
            Event::Start(Cm::CodeBlock(kind)) => {
                self.newline();
                let lang = match kind {
                    CodeBlockKind::Fenced(info) => info.split_whitespace().next().unwrap_or(""),
                    CodeBlockKind::Indented => "",
                };
                // Mermaid is not drawn (user decision 2026-10-22): its source, under a caption.
                if lang.eq_ignore_ascii_case("mermaid") {
                    self.push(&tags.caption);
                    self.text("Mermaid diagram\n");
                    self.pop();
                }
                self.push(&tags.codeblock);
                self.code = Some((lang.to_string(), self.offset()));
            }
            Event::Start(Cm::HtmlBlock) => self.html = Some(String::new()),
            Event::Start(Cm::List(first)) => {
                self.newline();
                self.lists.push(*first);
            }
            Event::Start(Cm::Item) => self.item(false),
            Event::Start(Cm::Table(_)) => self.rows = Some(Vec::new()),
            Event::Start(Cm::TableHead | Cm::TableRow) => {
                if let Some(rows) = &mut self.rows {
                    rows.push(Vec::new());
                }
            }
            Event::Start(Cm::TableCell) => {
                if let Some(row) = self.rows.as_mut().and_then(|r| r.last_mut()) {
                    row.push(String::new());
                }
            }
            Event::Start(Cm::FootnoteDefinition(label)) => {
                self.newline();
                self.facts.ids.push((format!("fn:{label}"), self.offset()));
                self.push(&tags.fndef);
                let n = self.notes.next().copied().unwrap_or(0);
                self.text(&format!("{n}. "));
            }
            Event::Start(Cm::MetadataBlock(_)) => self.meta = true,
            Event::Start(Cm::DefinitionListTitle) => self.push(&tags.dt),
            Event::Start(Cm::DefinitionListDefinition) => self.push(&tags.dd),
            Event::Start(_) => {}
            Event::End(TagEnd::Paragraph) => self.newline(),
            Event::End(
                TagEnd::Heading(_)
                | TagEnd::BlockQuote(_)
                | TagEnd::FootnoteDefinition
                | TagEnd::DefinitionListTitle
                | TagEnd::DefinitionListDefinition,
            ) => {
                self.pop();
                self.newline();
            }
            Event::End(
                TagEnd::Emphasis
                | TagEnd::Strong
                | TagEnd::Strikethrough
                | TagEnd::Superscript
                | TagEnd::Subscript,
            ) => self.pop(),
            Event::End(TagEnd::Link) => self.close_link(),
            Event::End(TagEnd::Image) => {
                if let Some((dest, alt)) = self.image.take() {
                    figure::markdown_image(self, &dest, &alt);
                }
            }
            Event::End(TagEnd::CodeBlock) => {
                self.newline();
                self.pop();
                if let Some((lang, start)) = self.code.take() {
                    super::code::fence(self, &lang, start..self.offset());
                }
            }
            Event::End(TagEnd::HtmlBlock) => {
                if let Some(html) = self.html.take() {
                    self.html(&html);
                }
                self.newline();
            }
            Event::End(TagEnd::List(_)) => {
                self.lists.pop();
            }
            Event::End(TagEnd::Item) => {
                if self.done.pop() == Some(true) {
                    self.pop();
                }
                self.pop();
                self.newline();
            }
            Event::End(TagEnd::Table) => {
                if let Some(rows) = self.rows.take() {
                    table::insert(self, rows);
                }
            }
            Event::End(TagEnd::MetadataBlock(_)) => self.meta = false,
            Event::End(_) => {}
            Event::Text(t) => self.text(t),
            Event::Code(t) => {
                self.push(&tags.code);
                self.text(t);
                self.pop();
            }
            // A formula that does not parse: its source, as written.
            Event::InlineMath(tex) => {
                self.push(&tags.math);
                self.text(&format!("${tex}$"));
                self.pop();
            }
            Event::DisplayMath(tex) => {
                self.push(&tags.math);
                self.text(&format!("$${tex}$$"));
                self.pop();
            }
            Event::Html(t) => match &mut self.html {
                Some(html) => html.push_str(t),
                None => self.html(t),
            },
            Event::InlineHtml(t) => self.html(t),
            Event::FootnoteReference(label) => {
                let n = self.notes.next().copied().unwrap_or(0);
                self.open_link(Href::Url(format!("#fn:{label}")));
                self.push(&tags.fnref);
                self.text(&format!("[{n}]"));
                self.pop();
                self.close_link();
            }
            Event::SoftBreak => self.text(" "),
            Event::HardBreak => self.text("\u{2028}"),
            Event::Rule => {
                self.newline();
                self.push(&tags.rule);
                self.text("\n");
                self.pop();
            }
            Event::TaskListMarker(done) => self.task(if *done { Task::Done } else { Task::Open }),
        }
    }
}

fn style_tag(tags: &Tags, style: Style) -> &gtk::TextTag {
    match style {
        Style::Sub => &tags.sub,
        Style::Sup => &tags.sup,
        Style::Kbd => &tags.kbd,
        Style::Strong => &tags.strong,
        Style::Emphasis => &tags.em,
        Style::Underline => &tags.underline,
        Style::Summary => &tags.summary,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str) -> Piece {
        Piece::Text(s.to_string())
    }

    #[test]
    fn raw_html_is_unwrapped_and_comments_and_styles_go() {
        assert_eq!(classify("<!-- a\nnote -->"), vec![]);
        assert_eq!(
            classify("<div align=\"center\">\nHello <span>there</span>\n</div>"),
            vec![
                Piece::Paragraph,
                text(" Hello "),
                text("there"),
                text(" "),
                Piece::Paragraph
            ]
        );
        assert_eq!(
            classify("<style>p { color: red }</style>after"),
            vec![text("after")]
        );
        assert_eq!(classify("<SCRIPT>x<y</script>"), vec![]);
        assert_eq!(classify("a<br>b"), vec![text("a"), Piece::Break, text("b")]);
        assert_eq!(classify("a < b &amp; c"), vec![text("a < b & c")]);
    }

    #[test]
    fn raw_html_styles_pictures_and_links() {
        assert_eq!(
            classify("H<sub>2</sub>O"),
            vec![
                text("H"),
                Piece::Open(Style::Sub),
                text("2"),
                Piece::Close(Style::Sub),
                text("O")
            ]
        );
        assert_eq!(
            classify("<img alt='a cat' src=\"cat.png\"/>"),
            vec![Piece::Image {
                src: "cat.png".into(),
                alt: "a cat".into()
            }]
        );
        assert_eq!(
            classify("<iframe src=\"https://example.org/v/clip\"></iframe>"),
            vec![Piece::Link {
                href: "https://example.org/v/clip".into(),
                label: "clip".into()
            }]
        );
        assert_eq!(
            classify("<details><summary>More</summary>"),
            vec![
                Piece::Paragraph,
                Piece::Open(Style::Summary),
                text("More"),
                Piece::Close(Style::Summary)
            ]
        );
    }
}
