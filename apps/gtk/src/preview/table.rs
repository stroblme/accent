//! Tables in the page, the one child widget: wrapping cells in a grid that scrolls sideways once
//! its words no longer fit the column, ruled and padded as the stylesheet's `th` and `td`, each
//! column aligned as its delimiter row says, the header bold and, unaligned, centred as a
//! browser centres a `th`; a cell's text in the middle of its row. A cell is a selectable label
//! of Pango markup: the prose's inline styles, and its links followed as the page's are. Copy
//! over the page writes the table's markdown.

use super::native::Page;
use super::text::{Href, Object, Tags, Writer};
use super::view::PreviewView;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, pango};
use pulldown_cmark::Alignment;
use std::cell::RefCell;

/// Stands in a cell's markup for the wash behind its code, which is the text's colour and so
/// changes with the theme. Escaped text never holds it: GLib writes U+0001 as `&#x1;`.
const WASH: char = '\u{1}';

/// A cell as the writer reads it: its text as Pango markup, each link an `<a href="i">` that
/// goes to `links[i]`, and how deep in links the writer is, as a link in a link is not nested.
#[derive(Default)]
pub(super) struct Cell {
    markup: String,
    links: Vec<Href>,
    open: usize,
}

impl Cell {
    /// `text` in the styles the tags opened inside the cell, `open`, give it.
    pub fn text(&mut self, tags: &Tags, open: &[gtk::TextTag], text: &str) {
        let spans: Vec<(&str, &str)> = open.iter().filter_map(|t| span(tags, t)).collect();
        for (start, _) in &spans {
            self.markup.push_str(start);
        }
        self.markup.push_str(&glib::markup_escape_text(text));
        for (_, end) in spans.iter().rev() {
            self.markup.push_str(end);
        }
    }

    pub fn open_link(&mut self, href: Href) {
        self.open += 1;
        if self.open == 1 {
            self.markup
                .push_str(&format!("<a href=\"{}\">", self.links.len()));
            self.links.push(href);
        }
    }

    pub fn close_link(&mut self) {
        if self.open == 1 {
            self.markup.push_str("</a>");
        }
        self.open = self.open.saturating_sub(1);
    }
}

/// The markup that gives text `tag`'s look in a label, its start and its end. A link's is the
/// `<a>` around it; a tag is one without the underline, as in the prose.
fn span(tags: &Tags, tag: &gtk::TextTag) -> Option<(&'static str, &'static str)> {
    // `\u{1}` is [`WASH`], filled in as the cell is shown.
    const CODE: &str = "<span font_family=\"monospace\" size=\"92%\" \u{1}>";
    let looks = [
        (&tags.em, "<i>", "</i>"),
        (&tags.strong, "<b>", "</b>"),
        (&tags.strike, "<s>", "</s>"),
        (&tags.underline, "<u>", "</u>"),
        (&tags.sub, "<sub>", "</sub>"),
        (&tags.sup, "<sup>", "</sup>"),
        (&tags.fnref, "<sup>", "</sup>"),
        (&tags.code, CODE, "</span>"),
        (&tags.kbd, CODE, "</span>"),
        (&tags.math, "<tt>", "</tt>"),
        (&tags.html, "<tt>", "</tt>"),
        (&tags.figure, "<span fgalpha=\"55%\">", "</span>"),
        (&tags.hashtag, "<span underline=\"none\">", "</span>"),
    ];
    looks
        .into_iter()
        .find(|(t, ..)| *t == tag)
        .map(|(_, start, end)| (start, end))
}

/// The wash behind code, as the `code` tag's: the text's colour at 0.07.
fn wash(page: &Page) -> String {
    let fg = page.ink.fg.get();
    let byte = |c: f32| (c.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!(
        "bgcolor=\"#{:02x}{:02x}{:02x}\" bgalpha=\"7%\"",
        byte(fg.red()),
        byte(fg.green()),
        byte(fg.blue())
    )
}

glib::wrapper! {
    /// GtkTextView gives an anchored child its minimum size, so the table says the text's width
    /// is its minimum, and its height is the grid's at that width (spike, gate G4).
    pub struct Table(ObjectSubclass<imp::Table>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

/// `rows` (the header first) at the writer, on a line of its own, column `i` aligned as `align[i]`
/// says.
pub(super) fn insert(w: &mut Writer, rows: Vec<Vec<Cell>>, align: &[Alignment]) {
    let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
    let label = format!("table {}x{columns}", rows.len());
    let object = Object::new(label, w.source.trim_end().to_string());
    let table = Table::new(rows, align, w.page.text_width(), &wash(w.page));
    w.widget(&table, object);
}

/// The page's tables.
fn tables(page: &Page) -> impl Iterator<Item = Table> {
    let mut child = page.view.first_child();
    std::iter::from_fn(move || {
        while let Some(widget) = child.take() {
            child = widget.next_sibling();
            if let Ok(table) = widget.downcast::<Table>() {
                return Some(table);
            }
        }
        None
    })
}

/// Lay every table out again for the page's text width, after a resize or a zoom.
pub(super) fn set_width(page: &Page) {
    let width = page.text_width();
    for table in tables(page) {
        table.set_width(width);
    }
}

/// The code in the cells washed in the theme's text colour.
pub(super) fn restyle(page: &Page) {
    let wash = wash(page);
    for table in tables(page) {
        for (label, markup) in table.imp().cells.borrow().iter() {
            if markup.contains(WASH) {
                label.set_markup(&markup.replace(WASH, &wash));
            }
        }
    }
}

impl Table {
    fn new(rows: Vec<Vec<Cell>>, align: &[Alignment], width: i32, wash: &str) -> Table {
        install_css();
        let grid = gtk::Grid::builder().css_classes(["accent-table"]).build();
        let mut cells = Vec::new();
        for (r, row) in rows.into_iter().enumerate() {
            for (c, cell) in row.into_iter().enumerate() {
                let (xalign, justify) = match align.get(c) {
                    Some(Alignment::Center) => (0.5, gtk::Justification::Center),
                    Some(Alignment::Right) => (1.0, gtk::Justification::Right),
                    Some(Alignment::None) | None if r == 0 => (0.5, gtk::Justification::Center),
                    _ => (0.0, gtk::Justification::Left),
                };
                let markup = cell.markup.trim().to_string();
                let label = gtk::Label::builder()
                    .use_markup(true)
                    .label(markup.replace(WASH, wash))
                    .wrap(true)
                    .wrap_mode(pango::WrapMode::Word)
                    .xalign(xalign)
                    .justify(justify)
                    .selectable(true)
                    .build();
                if r == 0 {
                    label.add_css_class("th");
                }
                if !cell.links.is_empty() {
                    follow(&label, cell.links);
                }
                grid.attach(&label, c as i32, r as i32, 1, 1);
                cells.push((label, markup));
            }
        }
        let scroller = gtk::ScrolledWindow::builder()
            .vscrollbar_policy(gtk::PolicyType::Never)
            .child(&grid)
            .build();
        let table: Table = glib::Object::new();
        table.imp().width.set(width);
        table.imp().cells.replace(cells);
        scroller.set_parent(&table);
        table
    }

    /// Its cells, row by row.
    pub(super) fn labels(&self) -> Vec<gtk::Label> {
        let cells = self.imp().cells.borrow();
        cells.iter().map(|(label, _)| label.clone()).collect()
    }

    fn set_width(&self, width: i32) {
        if self.imp().width.replace(width) != width {
            self.queue_resize();
        }
    }

    fn scroller(&self) -> Option<gtk::ScrolledWindow> {
        self.first_child().and_downcast()
    }
}

/// Each cell's markup, the text selected in it and whether matches are painted in it.
#[cfg(feature = "bench")]
pub(super) fn cells(page: &Page) -> Vec<(String, String, bool)> {
    let labels = tables(page).flat_map(|t| t.labels());
    labels
        .map(|label| {
            let (start, end) = label.selection_bounds().unwrap_or_default();
            let text = label.text();
            let chars = text.chars().skip(start as usize);
            let selected = chars.take((end - start) as usize).collect();
            let painted = label.attributes().is_some();
            (label.label().to_string(), selected, painted)
        })
        .collect()
}

/// A click on a link in `label` goes where the page sends the same link.
fn follow(label: &gtk::Label, links: Vec<Href>) {
    label.connect_activate_link(move |label, uri| {
        let href = uri.parse::<usize>().ok().and_then(|i| links.get(i));
        let view = label.ancestor(PreviewView::static_type());
        if let (Some(href), Some(view)) = (href, view.and_downcast::<PreviewView>()) {
            view.follow(href);
        }
        glib::Propagation::Stop
    });
}

/// The rules and padding of the stylesheet's `th, td`, in the text's colour and the page's
/// font, so a theme or a zoom changes them with the text: once per display.
fn install_css() {
    thread_local! {
        static DONE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    let Some(display) = gdk::Display::default() else {
        return;
    };
    if DONE.replace(true) {
        return;
    }
    let rule = "1px solid alpha(currentColor, 0.15)";
    let provider = gtk::CssProvider::new();
    provider.load_from_string(&format!(
        "grid.accent-table {{ border-top: {rule}; border-left: {rule}; }} \
         grid.accent-table > label {{ padding: 0.4em 0.8em; line-height: 1.6; \
           border-right: {rule}; border-bottom: {rule}; }} \
         grid.accent-table > label.th {{ font-weight: 700; }}"
    ));
    gtk::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Table {
        pub width: std::cell::Cell<i32>,
        /// Each cell's label and its markup, the code's wash still to fill in.
        pub cells: RefCell<Vec<(gtk::Label, String)>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Table {
        const NAME: &'static str = "AccentPreviewTable";
        type Type = super::Table;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for Table {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for Table {
        fn measure(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            let width = self.width.get();
            let Some(content) = self.obj().scroller().and_then(|s| s.child()) else {
                return (0, 0, -1, -1);
            };
            match orientation {
                gtk::Orientation::Horizontal => (width, width, -1, -1),
                _ => {
                    let least = content.measure(gtk::Orientation::Horizontal, -1).0;
                    let height = content.measure(orientation, width.max(least)).1;
                    (height, height, -1, -1)
                }
            }
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            if let Some(scroller) = self.obj().scroller() {
                scroller.measure(gtk::Orientation::Horizontal, -1);
                scroller.allocate(width, height, baseline, None);
            }
        }
    }
}
