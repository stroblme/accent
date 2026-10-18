//! The rows more than one pane draws the same way.

use crate::widgets::row_text;
use gtk::pango;
use gtk::prelude::*;

/// One section: a header that folds it, with its title and a count, over its body.
pub(super) struct Section {
    pub(super) root: gtk::Expander,
    pub(super) title: gtk::Label,
    pub(super) count: gtk::Label,
}

impl Section {
    pub(super) fn new(title: &str, body: &impl IsA<gtk::Widget>) -> Section {
        // At the rows' size, as every section's and group's title is (DESIGN.md, Typography).
        let title = gtk::Label::builder().label(title).xalign(0.0).build();
        title.add_css_class("heading");
        let count = gtk::Label::builder().visible(false).build();
        count.add_css_class("dim-label");
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        header.append(&title);
        header.append(&count);
        let root = gtk::Expander::builder()
            .label_widget(&header)
            .child(body)
            .expanded(true)
            .build();
        root.add_css_class("accent-section");
        Section { root, title, count }
    }

    pub(super) fn set_title(&self, title: &str) {
        self.title.set_text(title);
    }

    /// How many rows the section holds. `None` and none at all show no number, so neither an
    /// answer still coming nor an empty one reads as "0".
    pub(super) fn set_count(&self, n: Option<usize>) {
        let n = n.filter(|n| *n > 0);
        self.count.set_visible(n.is_some());
        if let Some(n) = n {
            self.count.set_text(&n.to_string());
        }
    }

    /// Shown and unfolded.
    pub(super) fn is_open(&self) -> bool {
        self.root.is_visible() && self.root.is_expanded()
    }
}

/// A `GtkListView` of plain strings — references and the files carrying a tag are the same row,
/// led by the icon `icon` gives the row's text, as every file list's rows are.
pub(super) fn path_list(
    model: &gtk::StringList,
    icon: fn(&str) -> &'static str,
    on_activate: impl Fn(&str) + 'static,
) -> gtk::ListView {
    let factory = crate::widgets::factory(
        |_| {
            let label = gtk::Label::builder()
                .xalign(0.0)
                .ellipsize(pango::EllipsizeMode::Middle)
                .build();
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            row.append(&gtk::Image::new());
            row.append(&label);
            row
        },
        move |row: &gtk::Box, item| {
            let (Some(text), Some(image), Some(label)) = (
                row_text(item),
                row.first_child().and_downcast::<gtk::Image>(),
                row.last_child().and_downcast::<gtk::Label>(),
            ) else {
                return;
            };
            image.set_icon_name(Some(icon(&text)));
            label.set_text(&text);
        },
    );

    let view = gtk::ListView::new(None::<gtk::SingleSelection>, Some(factory));
    crate::widgets::set_model(&view, &gtk::SingleSelection::new(Some(model.clone())));
    view.add_css_class("navigation-sidebar");
    // Backlinks and the files under a tag are result lists too, and open on one click like the
    // rest of them.
    view.set_single_click_activate(true);
    view.connect_activate(move |view, pos| {
        if let Some(s) = view
            .model()
            .and_then(|m| m.item(pos))
            .and_downcast::<gtk::StringObject>()
        {
            on_activate(&s.string());
        }
    });
    view
}
