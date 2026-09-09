//! The rows more than one pane draws the same way.

use crate::widgets::{label_factory, row_text};
use gtk::pango;
use gtk::prelude::*;

/// A `GtkListView` of plain strings — references and the files carrying a tag are the same row.
pub(super) fn path_list(
    model: &gtk::StringList,
    on_activate: impl Fn(&str) + 'static,
) -> gtk::ListView {
    let factory = label_factory(pango::EllipsizeMode::Middle, |label, item| {
        if let Some(text) = row_text(item) {
            label.set_text(&text);
        }
    });

    let view = gtk::ListView::new(
        Some(gtk::SingleSelection::new(Some(model.clone()))),
        Some(factory),
    );
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
