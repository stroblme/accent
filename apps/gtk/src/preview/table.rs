//! Tables in the page, the one child widget: wrapping cells in a grid that scrolls sideways once
//! its words no longer fit the column, ruled and padded as the stylesheet's `th` and `td`, each
//! column aligned as its delimiter row says, the header bold and, unaligned, centred as a
//! browser centres a `th`; a cell's text in the middle of its row. A cell's text is plain and
//! selectable; Copy over the page writes the table's markdown.

use super::native::Page;
use super::text::{Object, Writer};
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, pango};
use pulldown_cmark::Alignment;
use std::cell::Cell;

glib::wrapper! {
    /// GtkTextView gives an anchored child its minimum size, so the table says the text's width
    /// is its minimum, and its height is the grid's at that width (spike, gate G4).
    pub struct Table(ObjectSubclass<imp::Table>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

/// `rows` (the header first) at the writer, on a line of its own, column `i` aligned as `align[i]`
/// says.
pub(super) fn insert(w: &mut Writer, rows: Vec<Vec<String>>, align: &[Alignment]) {
    let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
    let label = format!("table {}x{columns}", rows.len());
    let object = Object::new(label, w.source.trim_end().to_string());
    let table = Table::new(&rows, align, w.page.text_width());
    w.widget(&table, object);
}

/// Lay every table out again for the page's text width, after a resize or a zoom.
pub(super) fn set_width(page: &Page) {
    let width = page.text_width();
    let mut child = page.view.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        if let Some(table) = widget.downcast_ref::<Table>() {
            table.set_width(width);
        }
    }
}

impl Table {
    fn new(rows: &[Vec<String>], align: &[Alignment], width: i32) -> Table {
        install_css();
        let grid = gtk::Grid::builder().css_classes(["accent-table"]).build();
        for (r, row) in rows.iter().enumerate() {
            for (c, cell) in row.iter().enumerate() {
                let (xalign, justify) = match align.get(c) {
                    Some(Alignment::Center) => (0.5, gtk::Justification::Center),
                    Some(Alignment::Right) => (1.0, gtk::Justification::Right),
                    Some(Alignment::None) | None if r == 0 => (0.5, gtk::Justification::Center),
                    _ => (0.0, gtk::Justification::Left),
                };
                let label = gtk::Label::builder()
                    .label(cell.trim())
                    .wrap(true)
                    .wrap_mode(pango::WrapMode::Word)
                    .xalign(xalign)
                    .justify(justify)
                    .selectable(true)
                    .build();
                if r == 0 {
                    label.add_css_class("th");
                }
                grid.attach(&label, c as i32, r as i32, 1, 1);
            }
        }
        let scroller = gtk::ScrolledWindow::builder()
            .vscrollbar_policy(gtk::PolicyType::Never)
            .child(&grid)
            .build();
        let table: Table = glib::Object::new();
        table.imp().width.set(width);
        scroller.set_parent(&table);
        table
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

/// The rules and padding of the stylesheet's `th, td`, in the text's colour and the page's
/// font, so a theme or a zoom changes them with the text: once per display.
fn install_css() {
    thread_local! {
        static DONE: Cell<bool> = const { Cell::new(false) };
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
        pub width: Cell<i32>,
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
