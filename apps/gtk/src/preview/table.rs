//! Tables in the page, the one child widget: wrapping cells in columns as wide as a browser
//! makes them ([`widths`]), scrolling sideways once their words no longer fit the column, ruled
//! and padded as the stylesheet's `th` and `td`, each column aligned as its delimiter row says,
//! the header bold and, unaligned, centred as a browser centres a `th`; a cell's text in the
//! middle of its row. A cell is a selectable label of Pango markup: the prose's inline styles,
//! and its links followed as the page's are. Copy over the page writes the table's markdown.

use super::Page;
use super::text::{Href, Object, Tags, Writer};
use super::view::PreviewView;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene, pango};
use pulldown_cmark::Alignment;
use std::cell::RefCell;

/// A rule's width, the stylesheet's.
const RULE: i32 = 1;

/// Stands in a cell's markup for the wash behind its code, which is the text's colour and so
/// changes with the theme. Escaped text never holds it: GLib writes U+0001 as `&#x1;`.
const WASH: char = '\u{1}';

/// A cell as the writer reads it: its text as Pango markup, each link an `<a href="i">` that
/// goes to `links[i]`, how deep in links the writer is, as a link in a link is not nested, and
/// whether it is in the head, a `th`.
#[derive(Default)]
pub(super) struct Cell {
    markup: String,
    links: Vec<Href>,
    open: usize,
    pub head: bool,
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

/// `markup` as a cell shows it: in `line`, the prose's line height, which a label's CSS would
/// take as a share of the font's height, not its size; its code washed in `wash`.
fn shown(markup: &str, wash: &str, line: f32) -> String {
    let markup = markup.replace(WASH, wash);
    format!("<span line_height=\"{line}\">{markup}</span>")
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

/// Each column's width out of `width`, given its narrowest and widest, `(min, max)`: a browser's
/// automatic table layout (CSS 2.1 §17.5.2.2). Each column is as wide as its widest cell where
/// they all fit; otherwise it gets its narrowest, its longest word, and a share of what is left
/// in proportion to how much more its cells would take unwrapped; with nothing left, its
/// narrowest.
fn widths(columns: &[(i32, i32)], width: i32) -> Vec<i32> {
    let least: i32 = columns.iter().map(|c| c.0).sum();
    let most: i32 = columns.iter().map(|c| c.1).sum();
    if most <= width {
        return columns.iter().map(|c| c.1).collect();
    }
    if least >= width {
        return columns.iter().map(|c| c.0).collect();
    }
    let (spare, wanted) = (i64::from(width - least), i64::from(most - least));
    // Each a share of the running total, so the rounding loses no pixel.
    let (mut want, mut given) = (0, 0);
    columns
        .iter()
        .map(|&(min, max)| {
            want += i64::from(max - min);
            let share = (spare * want / wanted) as i32 - given;
            given += share;
            min + share
        })
        .collect()
}

glib::wrapper! {
    /// The cells, a row's as tall as its tallest at its columns' [`widths`], and the rules
    /// between them, drawn on the whole pixels the cells are laid out on.
    pub struct Grid(ObjectSubclass<imp::Grid>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
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
    let look = (wash(w.page), w.page.tags.prose.line_height());
    let table = Table::new(rows, align, w.page.text_width(), look);
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
    let (wash, line) = (wash(page), page.tags.prose.line_height());
    for table in tables(page) {
        for (label, markup) in table.imp().cells.borrow().iter() {
            if markup.contains(WASH) {
                label.set_markup(&shown(markup, &wash, line));
            }
        }
    }
}

impl Table {
    /// `look` is the wash behind code and the line height.
    fn new(rows: Vec<Vec<Cell>>, align: &[Alignment], width: i32, look: (String, f32)) -> Table {
        install_css();
        // As wide as its columns, as a browser's table is, not the page: the grid's own rules,
        // the top and the left, would run on past a narrow table's last column.
        let grid: Grid = glib::Object::new();
        grid.add_css_class("accent-table");
        grid.set_halign(gtk::Align::Start);
        let mut cells = Vec::new();
        for row in rows {
            grid.imp().rows.borrow_mut().push(row.len());
            for (c, cell) in row.into_iter().enumerate() {
                let (xalign, justify) = match align.get(c) {
                    Some(Alignment::Center) => (0.5, gtk::Justification::Center),
                    Some(Alignment::Right) => (1.0, gtk::Justification::Right),
                    Some(Alignment::None) | None if cell.head => (0.5, gtk::Justification::Center),
                    _ => (0.0, gtk::Justification::Left),
                };
                let markup = cell.markup.trim().to_string();
                let label = gtk::Label::builder()
                    .use_markup(true)
                    .label(shown(&markup, &look.0, look.1))
                    .wrap(true)
                    .wrap_mode(pango::WrapMode::Word)
                    .xalign(xalign)
                    .justify(justify)
                    .selectable(true)
                    .build();
                if cell.head {
                    label.add_css_class("th");
                }
                if !cell.links.is_empty() {
                    follow(&label, cell.links);
                }
                label.set_parent(&grid);
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
    let wash = wash(page);
    let cells = tables(page).flat_map(|t| t.imp().cells.borrow().clone());
    cells
        .map(|(label, markup)| {
            let (start, end) = label.selection_bounds().unwrap_or_default();
            let text = label.text();
            let chars = text.chars().skip(start as usize);
            let selected = chars.take((end - start) as usize).collect();
            let painted = label.attributes().is_some();
            (markup.replace(WASH, &wash), selected, painted)
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

/// The padding of the stylesheet's `th, td`, in the page's font, so a zoom changes it with the
/// text: once per display. The rules are the grid's own: GTK lays a cell out with its padding
/// floored to whole pixels but draws its border around the padding unrounded, so a border would
/// straddle two pixel rows (ISSUES.md).
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
    let provider = gtk::CssProvider::new();
    provider.load_from_string(
        ".accent-table > label { padding: 0.4em 0.8em; } \
         .accent-table > label.th { font-weight: 700; }",
    );
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

    #[derive(Default)]
    pub struct Grid {
        /// How many cells each row holds, the head first; a header-less org table's head none.
        pub rows: RefCell<Vec<usize>>,
        /// The columns' widths and the rows' heights it was laid out at, each with its rule.
        layout: RefCell<(Vec<i32>, Vec<i32>)>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Grid {
        const NAME: &'static str = "AccentPreviewTableGrid";
        type Type = super::Grid;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for Grid {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl Grid {
        /// The cells, row by row.
        fn cells(&self) -> Vec<Vec<gtk::Widget>> {
            let mut child = self.obj().first_child();
            let mut cells = std::iter::from_fn(|| {
                let cell = child.take()?;
                child = cell.next_sibling();
                Some(cell)
            });
            let rows = self.rows.borrow();
            rows.iter()
                .map(|&n| cells.by_ref().take(n).collect())
                .collect()
        }

        /// Each column's narrowest and widest: its cells' longest word and whole text, and the
        /// rule after them.
        fn columns(cells: &[Vec<gtk::Widget>]) -> Vec<(i32, i32)> {
            let mut columns: Vec<(i32, i32)> = Vec::new();
            for row in cells {
                for (c, cell) in row.iter().enumerate() {
                    let (min, max, ..) = cell.measure(gtk::Orientation::Horizontal, -1);
                    let (min, max) = (min + RULE, max + RULE);
                    match columns.get_mut(c) {
                        Some(column) => *column = (column.0.max(min), column.1.max(max)),
                        None => columns.push((min, max)),
                    }
                }
            }
            columns
        }

        /// Each row's height, its cells in columns `widths` wide, and the rule under them; an
        /// empty row's none.
        fn heights(cells: &[Vec<gtk::Widget>], widths: &[i32]) -> Vec<i32> {
            let height = |cell: &gtk::Widget, &width| {
                cell.measure(gtk::Orientation::Vertical, width - RULE).1
            };
            cells
                .iter()
                .map(|row| {
                    let tallest = row.iter().zip(widths).map(|(c, w)| height(c, w)).max();
                    tallest.map_or(0, |h| h + RULE)
                })
                .collect()
        }
    }

    impl WidgetImpl for Grid {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            gtk::SizeRequestMode::HeightForWidth
        }

        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let cells = self.cells();
            let columns = Self::columns(&cells);
            match orientation {
                gtk::Orientation::Horizontal => {
                    let min = RULE + columns.iter().map(|c| c.0).sum::<i32>();
                    let max = RULE + columns.iter().map(|c| c.1).sum::<i32>();
                    (min, max, -1, -1)
                }
                _ => {
                    let width = if for_size < 0 {
                        i32::MAX
                    } else {
                        for_size - RULE
                    };
                    let widths = super::widths(&columns, width);
                    let height = RULE + Self::heights(&cells, &widths).iter().sum::<i32>();
                    (height, height, -1, -1)
                }
            }
        }

        fn size_allocate(&self, width: i32, _height: i32, _baseline: i32) {
            let cells = self.cells();
            let widths = super::widths(&Self::columns(&cells), width - RULE);
            let heights = Self::heights(&cells, &widths);
            let mut y = RULE;
            for (row, &height) in cells.iter().zip(&heights) {
                let mut x = RULE;
                for (cell, &width) in row.iter().zip(&widths) {
                    let cell_box = gtk::Allocation::new(x, y, width - RULE, height - RULE);
                    cell.size_allocate(&cell_box, -1);
                    x += width;
                }
                y += height;
            }
            self.layout.replace((widths, heights));
        }

        /// The rules the stylesheet gives `th, td`, `1px solid alpha(currentColor, 0.15)`: the
        /// table's top and left, and each cell's right and bottom.
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let ink = self.obj().color();
            let ink = ink.with_alpha(ink.alpha() * 0.15);
            let rule = |x: i32, y: i32, width: i32, height: i32| {
                let rect = graphene::Rect::new(x as f32, y as f32, width as f32, height as f32);
                snapshot.append_color(&ink, &rect);
            };
            let (widths, heights) = &*self.layout.borrow();
            rule(0, 0, RULE + widths.iter().sum::<i32>(), RULE);
            rule(0, 0, RULE, RULE + heights.iter().sum::<i32>());
            let mut y = RULE;
            for (&cells, &height) in self.rows.borrow().iter().zip(heights) {
                let mut x = RULE;
                for &width in widths.iter().take(cells) {
                    rule(x + width - RULE, y, RULE, height);
                    rule(x, y + height - RULE, width, RULE);
                    x += width;
                }
                y += height;
            }
            self.parent_snapshot(snapshot);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::widths;

    #[test]
    fn columns_share_the_width_as_a_browser_does() {
        let columns = [(10, 40), (20, 30)];
        // Room for every cell unwrapped: each column its widest.
        assert_eq!(widths(&columns, 100), [40, 30]);
        // Too little for their longest words: each its narrowest, and the table scrolls.
        assert_eq!(widths(&columns, 25), [10, 20]);
        // Between: each its narrowest, and the 30 px left shared 3:1, as 30 and 10 more would
        // show them unwrapped.
        assert_eq!(widths(&columns, 60), [32, 28]);
    }
}
