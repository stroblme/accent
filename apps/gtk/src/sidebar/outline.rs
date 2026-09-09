//! The Outline pane: a document's headings, or a PDF's bookmarks, as rows that jump.

use crate::widgets::{label_factory, row_text, scroller, status_page};
use adw::prelude::*;
use gtk::pango;

/// The pane's icon, and the one its empty states are drawn with.
pub(super) const ICON: &str = "view-list-bullet-symbolic";

/// How far each heading level is indented in the Outline pane, on the 6/12/18 spacing scale.
const INDENT: i32 = 12;

/// What the Outline pane says with nothing to outline.
pub(super) fn empty() -> gtk::Widget {
    status_page(ICON, "No Outline", "Open a note to see its headings.").upcast()
}

/// A sentence in the Outline pane's own shape, for a tab that has no outline to give.
pub fn outline_note(title: &str, body: &str) -> gtk::Widget {
    status_page(ICON, title, body).upcast()
}

/// An outline as rows that jump: `(level, text, where a click goes)`.
///
/// Generic in what a row jumps to, because the two callers mean different things by it: a text
/// tab's symbols carry a position in the buffer and a PDF's bookmarks carry a page number.
///
/// Indented by level rather than nested in a tree: an outline is read top to bottom, and an
/// expander per row would hide exactly what the pane exists to show.
pub fn outline_list<T: Copy + 'static>(
    rows: &[(u8, String, T)],
    on_jump: impl Fn(T) + 'static,
) -> gtk::Widget {
    let texts: Vec<&str> = rows.iter().map(|(_, text, _)| text.as_str()).collect();
    let model = gtk::StringList::new(&texts);
    let levels: Vec<u8> = rows.iter().map(|(level, _, _)| *level).collect();
    let targets: Vec<T> = rows.iter().map(|(_, _, at)| *at).collect();

    let factory = label_factory(pango::EllipsizeMode::End, move |label, item| {
        if let Some(text) = row_text(item) {
            label.set_text(&text);
            let level = levels.get(item.position() as usize).copied().unwrap_or(1);
            label.set_margin_start(INDENT * i32::from(level.saturating_sub(1)));
        }
    });

    let view = gtk::ListView::new(Some(gtk::SingleSelection::new(Some(model))), Some(factory));
    view.add_css_class("navigation-sidebar");
    view.set_single_click_activate(true);
    view.connect_activate(move |_, row| {
        if let Some(at) = targets.get(row as usize) {
            on_jump(*at);
        }
    });
    scroller(&view).upcast()
}
