//! A page's blocks written into the preview's buffer: the tags its text is set in, the boxes of
//! task items, the raw HTML a note holds unwrapped into text, and per block the facts the view
//! reads back: where its lines start, its anchors, its links and the source of each object.

use super::{Laid, Page};
use super::{figure, formula, table};
use accent_core::markdown::{Node, Side};
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene, gsk, pango};
use pulldown_cmark::{Alignment, CodeBlockKind, Event, Tag as Cm, TagEnd};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;

// -------------------------------------------------------------------------------------- tags

/// The tags a page is set in, one table per preview. Made in order of priority: a later one wins
/// where two set the same property.
pub(super) struct Tags {
    pub prose: gtk::TextTag,
    pub gap: gtk::TextTag,
    /// A paragraph's last line inside a quote, an item, a footnote or a callout: the
    /// stylesheet's `p` margin below it, where a block's own last line has the gap.
    pub para: gtk::TextTag,
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
    /// A code block's first and last lines, which hold the room of its box's padding and margin.
    pub fence_top: gtk::TextTag,
    pub fence_bottom: gtk::TextTag,
    pub caption: gtk::TextTag,
    pub quote: gtk::TextTag,
    pub callout: gtk::TextTag,
    pub callout_title: gtk::TextTag,
    /// A callout's last line: the tint's padding below its text, then the gap after the block.
    pub callout_end: gtk::TextTag,
    /// An item's lines at each depth, at the item's text.
    pub lists: [gtk::TextTag; 6],
    /// An item's first line per depth and marker, made as one is met ([`size_hang`]).
    pub hangs: RefCell<HashMap<(usize, String), gtk::TextTag>>,
    pub taskdone: gtk::TextTag,
    pub dt: gtk::TextTag,
    pub dd: gtk::TextTag,
    pub rule: gtk::TextTag,
    pub display: gtk::TextTag,
    /// Org's `#+begin_center` block.
    pub center: gtk::TextTag,
    /// A display formula's line right under another's, the margin between them collapsed.
    pub collapsed: gtk::TextTag,
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
/// The room inside a code block's box around its text, in pixels, as the stylesheet's `pre`.
pub(super) const PAD: i32 = 12;

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
            prose: tag("prose"),
            gap: tag("gap"),
            para: tag("para"),
            void: set("void", &|t| t.set_scale(0.05)),
            headings: std::array::from_fn(|i| {
                set(&format!("h{}", i + 1), &|t| {
                    t.set_scale(crate::highlight::HEADING_SCALES[i]);
                    t.set_weight(700);
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
            }),
            fence_top: tag("fence-top"),
            fence_bottom: tag("fence-bottom"),
            caption: set("caption", &|t| t.set_scale(0.85)),
            quote: set("quote", &|t| t.set_style(pango::Style::Italic)),
            callout: tag("callout"),
            callout_title: set("callout-title", &|t| t.set_weight(700)),
            callout_end: tag("callout-end"),
            lists: std::array::from_fn(|i| tag(&format!("list{}", i + 1))),
            hangs: RefCell::default(),
            taskdone: tag("taskdone"),
            dt: set("dt", &|t| t.set_weight(700)),
            dd: tag("dd"),
            rule: tag("rule"),
            display: set("display", &|t| {
                t.set_justification(gtk::Justification::Center)
            }),
            center: set("center", &|t| {
                t.set_justification(gtk::Justification::Center)
            }),
            collapsed: set("collapsed", &|t| t.set_pixels_above_lines(0)),
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

    /// The sizes measured in the font's pixels, `px` at the zoom, in the font of `context`, and
    /// the page's gutter, which a tag's margin replaces rather than adds to.
    pub(super) fn resize(&self, px: f64, gutter: i32, context: &pango::Context) {
        let em = |n: f64| (n * px).round() as i32;
        // The stylesheet's line height is a share of the font's size, Pango's of the font's
        // height, its ascent and descent (1.16 em in DejaVu Sans Mono): `css` em as Pango's.
        let mut font = context.font_description().unwrap_or_default();
        font.set_absolute_size(px * f64::from(pango::SCALE));
        let metrics = context.metrics(Some(&font), None);
        let height = f64::from(metrics.ascent() + metrics.descent()) / f64::from(pango::SCALE);
        let line = |css: f64| (css * px / height) as f32;
        self.prose.set_line_height(line(1.6));
        self.codeblock.set_line_height(line(1.6));
        for tag in &self.headings {
            tag.set_line_height(line(1.25));
        }
        self.gap.set_pixels_below_lines(em(0.8));
        self.para.set_pixels_below_lines(em(0.8));
        for (tag, scale) in self.headings.iter().zip(crate::highlight::HEADING_SCALES) {
            tag.set_pixels_above_lines(em(1.2 * scale));
            tag.set_pixels_below_lines(em(0.4 * scale));
        }
        let inset = gutter + PAD;
        // The stylesheet's `pre`: PAD inside its box and its font's 1 em outside, which takes the
        // place of the gap the block before leaves.
        self.fence_top.set_pixels_above_lines(em(MONO - 0.8) + PAD);
        self.fence_bottom.set_pixels_below_lines(em(MONO) + PAD);
        self.codeblock.set_left_margin(inset);
        self.codeblock.set_right_margin(inset);
        self.quote.set_left_margin(inset + 3);
        self.callout.set_left_margin(inset + 3);
        self.callout.set_right_margin(inset);
        // The stylesheet's callout, a flow root: a paragraph's 0.8 em, the title's too, stays
        // inside the tint, above the title and below the last line.
        self.callout_title.set_pixels_above_lines(em(0.8));
        self.callout_title.set_pixels_below_lines(em(0.8));
        self.callout_end.set_pixels_below_lines(em(0.8) * 2);
        for (i, list) in self.lists.iter().enumerate() {
            list.set_left_margin(list_text(px, gutter, i + 1));
        }
        self.dd.set_left_margin(gutter + em(1.4));
        self.rule.set_pixels_above_lines(12);
        self.rule.set_pixels_below_lines(12);
        // 1.2em above, less the gap the block before leaves, as a browser collapses the two.
        self.display.set_pixels_above_lines(em(0.4));
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

/// Where a list item's text starts at `depth` (from 1), every line of the item there: the
/// stylesheet's `ul, ol`, 1.4 em further in each depth.
fn list_text(px: f64, gutter: i32, depth: usize) -> i32 {
    gutter + (1.4 * depth as f64 * px).round() as i32
}

/// What a task item's first line leads with, as the buffer holds it: its box and a space.
const TASK_MARKER: &str = "\u{fffc} ";

/// Size `hang`, the first line of an item at `depth` led by `marker`: it starts the marker's
/// width before the item's text, GTK setting the lines it wraps onto in by the indent, under the
/// text, so the text after the marker starts there too as a browser's does. The marker is
/// measured in the prose's font at its size, a box as its glyph is drawn, and rounded to whole
/// pixels, which a margin is.
fn size_hang(page: &Page, hang: &gtk::TextTag, depth: usize, marker: &str) {
    let px = page.px.get();
    let context = page.view.pango_context();
    let mut font = context.font_description().unwrap_or_default();
    font.set_absolute_size(px * f64::from(pango::SCALE));
    let layout = pango::Layout::new(&context);
    layout.set_font_description(Some(&font));
    layout.set_text(&marker.replace('\u{fffc}', ""));
    let mut width = f64::from(layout.extents().1.width()) / f64::from(pango::SCALE);
    if marker.contains('\u{fffc}') {
        width += f64::from(Mark::Task(Task::Open).size(px).0);
    }
    let width = width.round() as i32;
    let text = list_text(px, page.view.left_margin(), depth);
    hang.set_left_margin((text - width).max(0));
    hang.set_indent(-width);
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

// -------------------------------------------------------------------------------------- glyphs

/// A task item's box: open, in progress (half filled) or done (filled and ticked).
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum Task {
    #[default]
    Open,
    Doing,
    Done,
}

/// The icon a callout of type `kind` leads its title with: Obsidian's glyph for each type it
/// names, as the icon theme draws it, and a note's for a type it does not. Adwaita has no flame,
/// bug or quotation mark: a tip has its star, a bug the Debug Console's bug and a quote a glyph
/// of the app's own.
pub(crate) fn callout_icon(kind: &str) -> &'static str {
    match kind {
        "abstract" | "summary" | "tldr" => "edit-paste-symbolic",
        "info" => "help-about-symbolic",
        "todo" => "checkbox-checked-symbolic",
        "tip" | "hint" | "important" => "starred-symbolic",
        "success" | "check" | "done" => "object-select-symbolic",
        "question" | "help" | "faq" => "dialog-question-symbolic",
        "warning" | "caution" | "attention" => "dialog-warning-symbolic",
        "failure" | "fail" | "missing" => "process-stop-symbolic",
        "danger" | "error" => "dialog-error-symbolic",
        "bug" => "dap-console-symbolic",
        "example" => "view-list-bullet-symbolic",
        "quote" | "cite" => "callout-quote-symbolic",
        _ => "document-edit-symbolic",
    }
}

/// What a [`Glyph`] draws.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    /// A task's box, one paintable for all three (refresh T4), as large as the font's `x` is
    /// tall plus a little, in the accent faded as a disabled checkbox is.
    Task(Task),
    /// A callout's icon, its type's symbolic icon by its name in the theme, as tall as the
    /// font's size, in the accent its title is in.
    Icon(&'static str),
}

impl Default for Mark {
    fn default() -> Mark {
        Mark::Task(Task::Open)
    }
}

impl Mark {
    /// Its side and how far it reaches below the baseline at the prose's size `px`: a box
    /// centred on the `x`, an icon on the capitals.
    fn size(self, px: f64) -> (i32, i32) {
        let (side, descent) = match self {
            Mark::Task(_) => (0.8, 0.1),
            Mark::Icon(_) => (1.0, 0.15),
        };
        ((side * px).round() as i32, (descent * px).round() as i32)
    }
}

glib::wrapper! {
    /// A task's box or a callout's icon in the text, sized from the prose's size, so a zoom
    /// resizes it in place ([`resize`]).
    pub struct Glyph(ObjectSubclass<glyph::Glyph>) @implements gdk::Paintable;
}

impl Glyph {
    fn new(mark: Mark, px: f64, ink: Rc<Ink>) -> Glyph {
        let glyph: Glyph = glib::Object::new();
        glyph.imp().mark.set(mark);
        let _ = glyph.imp().ink.set(ink);
        glyph.fit(px);
        glyph
    }

    /// Size it for the prose's size `px`; how far it reached below the baseline before, and
    /// how far now.
    fn fit(&self, px: f64) -> (i32, i32) {
        let imp = self.imp();
        let (side, descent) = imp.mark.get().size(px);
        imp.side.set(side);
        (imp.descent.replace(descent), descent)
    }
}

/// Every glyph sized again for the prose's size at the zoom, lowered anew where that changed, and
/// every list marker's hang measured again.
pub(super) fn resize(page: &Page) {
    for ((depth, marker), hang) in page.tags.hangs.borrow().iter() {
        size_hang(page, hang, *depth, marker);
    }
    let (buffer, px) = (&page.buffer, page.px.get());
    let mut from = 0;
    while let Some((start, end)) =
        buffer
            .iter_at_offset(from)
            .forward_search("\u{fffc}", gtk::TextSearchFlags::empty(), None)
    {
        from = end.offset();
        let Some(glyph) = start.paintable().and_downcast::<Glyph>() else {
            continue;
        };
        let (before, after) = glyph.fit(px);
        if after != before {
            if before != 0 {
                buffer.remove_tag(&page.rise(before), &start, &end);
            }
            if after != 0 {
                buffer.apply_tag(&page.rise(after), &start, &end);
            }
        }
        glyph.invalidate_size();
    }
}

mod glyph {
    use super::*;
    use std::cell::OnceCell;

    #[derive(Default)]
    pub struct Glyph {
        pub mark: Cell<Mark>,
        pub side: Cell<i32>,
        pub descent: Cell<i32>,
        pub ink: OnceCell<Rc<Ink>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Glyph {
        const NAME: &'static str = "AccentPreviewGlyph";
        type Type = super::Glyph;
        type Interfaces = (gdk::Paintable,);
    }

    impl ObjectImpl for Glyph {}

    impl PaintableImpl for Glyph {
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
            match self.mark.get() {
                Mark::Task(task) => task_box(snapshot, task, width, height, ink),
                Mark::Icon(name) => {
                    let Some(display) = gdk::Display::default() else {
                        return;
                    };
                    let icon = gtk::IconTheme::for_display(&display).lookup_icon(
                        name,
                        &[],
                        width as i32,
                        1,
                        gtk::TextDirection::None,
                        gtk::IconLookupFlags::empty(),
                    );
                    icon.snapshot_symbolic(snapshot, width, height, &[ink.accent.get()]);
                }
            }
        }
    }

    fn task_box(snapshot: &gtk::Snapshot, task: Task, width: f64, height: f64, ink: &Ink) {
        let (w, h) = (width as f32, height as f32);
        let accent = crate::theme::at(ink.accent.get(), 0.5);
        let line = (w / 7.0).max(1.5);
        let rect = graphene::Rect::new(0.0, 0.0, w, h);
        let corner = graphene::Size::new(w / 4.0, w / 4.0);
        let shape = gsk::RoundedRect::new(rect, corner, corner, corner, corner);
        match task {
            Task::Done => {
                snapshot.push_rounded_clip(&shape);
                snapshot.append_color(&accent, &rect);
                snapshot.pop();
                let tick = gsk::PathBuilder::new();
                tick.move_to(w * 0.25, h * 0.52);
                tick.line_to(w * 0.43, h * 0.7);
                tick.line_to(w * 0.76, h * 0.32);
                snapshot.append_stroke(&tick.to_path(), &gsk::Stroke::new(line), &ink.page.get());
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
/// notes, or a markdown link's destination, resolved against the note's folder (`Page::route`).
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
        items: Vec::new(),
        links: Vec::new(),
        image: None,
        meta: false,
        rows: None,
        depth: 0,
        align: Vec::new(),
        html: None,
        code: None,
        callout: String::new(),
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

/// Take the margin above the line `at` starts, a display formula's, away where the line before
/// is one too: a browser collapses the two margins into one. Again where a patch changed the
/// block before.
pub(super) fn collapse_margin(page: &Page, at: &gtk::TextIter) {
    let display = &page.tags.display;
    let mut before = *at;
    let under = at.has_tag(display) && before.backward_line() && before.has_tag(display);
    let mut end = *at;
    end.forward_line();
    match under {
        true => page.buffer.apply_tag(&page.tags.collapsed, at, &end),
        false => page.buffer.remove_tag(&page.tags.collapsed, at, &end),
    }
}

/// A list item being written.
struct OpenItem {
    /// Where its first line starts, from the block's start.
    start: i32,
    /// What leads that line, as the buffer holds it: a bullet, a number or a task's box, then a
    /// space.
    marker: String,
    /// Whether it is a done task, whose text is dimmed.
    done: bool,
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
    items: Vec<OpenItem>,
    links: Vec<(i32, Href)>,
    /// A markdown image being read: its destination and alt text.
    image: Option<(String, String)>,
    meta: bool,
    rows: Option<Vec<Vec<table::Cell>>>,
    /// How many tags were open where the table started: those after are a cell's own styles.
    depth: usize,
    align: Vec<Alignment>,
    html: Option<String>,
    code: Option<(String, i32)>,
    /// The type of the callout whose title comes next.
    callout: String,
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
        if let Some((_, alt)) = &mut self.image {
            alt.push_str(text);
            return;
        }
        if let Some(cell) = self.rows.as_mut().and_then(|r| r.last_mut()?.last_mut()) {
            let open = self.tags.get(self.depth..).unwrap_or_default();
            cell.text(&self.page.tags, open, text);
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
        self.page.attach(widget.upcast_ref(), &anchor);
        self.facts.objects.push(object);
        self.line_start = false;
        self.newline();
    }

    /// Whether the writer is inside a table cell, which holds text alone.
    pub fn in_cell(&self) -> bool {
        self.rows.is_some()
    }

    /// The table cell being written.
    fn cell(&mut self) -> Option<&mut table::Cell> {
        self.rows.as_mut()?.last_mut()?.last_mut()
    }

    pub fn open_link(&mut self, href: Href) {
        if let Some(cell) = self.cell() {
            cell.open_link(href.clone());
        }
        self.links.push((self.offset(), href));
        let page = self.page;
        self.push(&page.tags.link);
    }

    pub fn close_link(&mut self) {
        if let Some(cell) = self.cell() {
            cell.close_link();
        }
        self.pop();
        if let Some((start, href)) = self.links.pop()
            && start < self.offset()
        {
            self.facts.links.push((start..self.offset(), href));
        }
    }

    /// An item's start; a task's box is written by [`Writer::task`].
    fn item(&mut self, task: bool) {
        self.newline();
        let depth = self.lists.len().clamp(1, 6);
        let page = self.page;
        self.push(&page.tags.lists[depth - 1]);
        let start = self.offset();
        let marker = match (task, self.lists.last_mut()) {
            (true, _) => TASK_MARKER.to_string(),
            (false, Some(Some(n))) => {
                *n += 1;
                format!("{}. ", *n - 1)
            }
            _ => "• ".to_string(),
        };
        if !task {
            self.text(&marker);
        }
        let item = OpenItem {
            start,
            marker,
            done: false,
        };
        self.items.push(item);
    }

    /// `mark` and a space after it.
    fn glyph(&mut self, mark: Mark, object: Object) {
        let glyph = Glyph::new(mark, self.page.px.get(), self.page.ink.clone());
        let descent = glyph.imp().descent.get();
        self.paintable(&glyph, descent, object);
        self.text(" ");
    }

    /// A callout's icon before its title; Copy writes the `[!type]` it stands for.
    fn icon(&mut self, kind: &str) {
        let name = callout_icon(kind);
        let object = Object::new(format!("icon {name}"), format!("[!{kind}]"));
        self.glyph(Mark::Icon(name), object);
    }

    fn task(&mut self, task: Task) {
        let source = match task {
            Task::Open => "[ ]",
            Task::Doing => "[/]",
            Task::Done => "[x]",
        };
        let object = Object::new(format!("box {source}"), source.to_string());
        self.glyph(Mark::Task(task), object);
        if task == Task::Done {
            let page = self.page;
            self.push(&page.tags.taskdone);
            if let Some(item) = self.items.last_mut() {
                item.done = true;
            }
        }
    }

    /// Tag the line just ended, its newline included.
    fn tag_line(&self, tag: &gtk::TextTag) {
        let mut start = *self.at;
        start.backward_char();
        start.set_line_offset(0);
        self.page.buffer.apply_tag(tag, &start, self.at);
    }

    /// `item`'s first line, the one its marker hangs in, in the hang of its depth and marker.
    fn hang(&self, item: OpenItem) {
        let depth = self.lists.len().clamp(1, 6);
        let page = self.page;
        let buffer = &page.buffer;
        let hang = (page.tags.hangs.borrow_mut())
            .entry((depth, item.marker))
            .or_insert_with_key(|(_, marker)| {
                let hang = gtk::TextTag::new(None);
                buffer.tag_table().add(&hang);
                size_hang(page, &hang, depth, marker);
                hang
            })
            .clone();
        let first = buffer.iter_at_offset(self.start + item.start);
        let mut end = first;
        end.forward_line();
        buffer.apply_tag(&hang, &first, &end);
    }

    /// The room of a code block's box above its first line, from `start`, and below its last.
    fn fence(&mut self, start: i32) {
        if self.offset() == start {
            return;
        }
        let (buffer, tags) = (&self.page.buffer, &self.page.tags);
        let first = buffer.iter_at_offset(self.start + start);
        let mut end = first;
        end.forward_line();
        buffer.apply_tag(&tags.fence_top, &first, &end);
        let mut last = *self.at;
        last.backward_char();
        last.set_line_offset(0);
        buffer.apply_tag(&tags.fence_bottom, &last, self.at);
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
                if let Some(cell) = self.cell() {
                    cell.open_link(Href::Tag(name.clone()));
                }
                self.push(&tags.hashtag);
                self.text(text);
                self.pop();
                match self.cell() {
                    Some(cell) => cell.close_link(),
                    None => {
                        let range = start..self.offset();
                        self.facts.links.push((range, Href::Tag(name.clone())));
                    }
                }
            }
            Node::Image(target) => figure::image(self, target),
            Node::Diagram { target, anchor } => figure::diagram(self, target, anchor.as_deref()),
            Node::Math {
                tex,
                display,
                mathml,
            } => formula::insert(self, tex, mathml, *display),
            Node::TaskItem => self.item(true),
            Node::TaskDoing => self.task(Task::Doing),
            Node::Callout { kind } => {
                self.push(&tags.callout);
                self.callout = kind.clone();
            }
            Node::CalloutTitle(title) => {
                self.push(&tags.callout_title);
                let kind = std::mem::take(&mut self.callout);
                self.icon(&kind);
                self.text(title);
                self.pop();
                self.newline();
            }
            Node::Underline => self.push(&tags.underline),
            Node::UnderlineEnd => self.pop(),
            Node::Center => self.push(&tags.center),
            Node::CenterEnd => self.pop(),
            Node::Keyword { text, done } => {
                self.push(if *done { &tags.done } else { &tags.todo });
                self.text(text);
                self.pop();
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
            Event::Start(Cm::Table(align)) => {
                self.rows = Some(Vec::new());
                self.depth = self.tags.len();
                self.align = align.clone();
            }
            Event::Start(Cm::TableHead | Cm::TableRow) => {
                if let Some(rows) = &mut self.rows {
                    rows.push(Vec::new());
                }
            }
            Event::Start(Cm::TableCell) => {
                if let Some(row) = self.rows.as_mut().and_then(|r| r.last_mut()) {
                    row.push(table::Cell::default());
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
            // The tags still open are the blocks the paragraph is in.
            Event::End(TagEnd::Paragraph) => {
                self.newline();
                if !self.tags.is_empty() {
                    self.tag_line(&tags.para);
                }
            }
            Event::End(TagEnd::BlockQuote(_)) if self.tags.last() == Some(&tags.callout) => {
                self.pop();
                self.newline();
                self.tag_line(&tags.callout_end);
            }
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
                    self.fence(start);
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
                let item = self.items.pop();
                if item.as_ref().is_some_and(|item| item.done) {
                    self.pop();
                }
                self.pop();
                self.newline();
                if let Some(item) = item {
                    self.hang(item);
                }
            }
            // Every row read so far is the head's, which in org may be several.
            Event::End(TagEnd::TableHead) => {
                for cell in self.rows.iter_mut().flatten().flatten() {
                    cell.head = true;
                }
            }
            Event::End(TagEnd::Table) => {
                if let Some(rows) = self.rows.take() {
                    let align = std::mem::take(&mut self.align);
                    table::insert(self, rows, &align);
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
