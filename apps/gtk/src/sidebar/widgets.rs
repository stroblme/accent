//! The rows more than one pane draws the same way.

use crate::widgets::row_text;
use gtk::pango;
use gtk::prelude::*;

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
