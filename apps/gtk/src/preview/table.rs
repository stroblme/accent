//! Tables in the page, the one child widget: wrapping cells in a grid that scrolls sideways once
//! its words no longer fit the column. Stage 2 P4 adds borders, alignment, links in cells and
//! their markup.

use super::native::Page;
use super::text::{Object, Writer};
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{glib, pango};
use std::cell::Cell;

glib::wrapper! {
    /// GtkTextView gives an anchored child its minimum size, so the table says the text's width
    /// is its minimum, and its height is the grid's at that width (spike, gate G4).
    pub struct Table(ObjectSubclass<imp::Table>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

/// `rows` (the header first) at the writer, on a line of its own.
pub(super) fn insert(w: &mut Writer, rows: Vec<Vec<String>>) {
    let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
    let label = format!("table {}x{columns}", rows.len());
    let object = Object::new(label, w.source.trim_end().to_string());
    let table = Table::new(&rows, w.page.text_width());
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
    fn new(rows: &[Vec<String>], width: i32) -> Table {
        let grid = gtk::Grid::builder()
            .column_spacing(18)
            .row_spacing(4)
            .build();
        for (r, row) in rows.iter().enumerate() {
            for (c, cell) in row.iter().enumerate() {
                let label = gtk::Label::builder()
                    .label(cell.trim())
                    .wrap(true)
                    .wrap_mode(pango::WrapMode::Word)
                    .xalign(0.0)
                    .yalign(0.0)
                    .selectable(true)
                    .build();
                if r == 0 {
                    label.add_css_class("heading");
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
