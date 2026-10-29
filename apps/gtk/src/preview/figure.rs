//! Pictures in the page: a vault image (`![[cat.png]]`), a diagram's page, a markdown or an HTML
//! image. Stage 2 P4 draws each as a paintable loaded as its tab shows the file, through the
//! asset resolver (refresh N12), with Invert Image Colours in the menu over it; until then each
//! shows its name, dimmed.

use super::native::Page;
use super::text::Writer;

pub(super) fn image(w: &mut Writer, target: &str) {
    placeholder(w, target);
}

pub(super) fn diagram(w: &mut Writer, target: &str, anchor: Option<&str>) {
    match anchor {
        Some(anchor) => placeholder(w, &format!("{target}#{anchor}")),
        None => placeholder(w, target),
    }
}

pub(super) fn markdown_image(w: &mut Writer, dest: &str, alt: &str) {
    placeholder(w, if alt.is_empty() { dest } else { alt });
}

pub(super) fn html_image(w: &mut Writer, src: &str, alt: &str) {
    placeholder(w, if alt.is_empty() { src } else { alt });
}

fn placeholder(w: &mut Writer, name: &str) {
    let page = w.page;
    w.push(&page.tags.figure);
    w.text(&format!("[{name}]"));
    w.pop();
}

/// The menu's own item over the picture at `offset`, if any.
pub(super) fn menu_item(_page: &Page, _offset: i32) -> Option<gtk::gio::MenuItem> {
    None
}

/// The pictures drawn again in the theme's look; whether that changed.
pub(super) fn restyle(_page: &Page) -> bool {
    false
}

/// Every picture loaded again, then `then`.
pub(super) fn forget_images(_page: &Page, then: impl FnOnce() + 'static) {
    then();
}

/// The pictures of `key` (a file, or a folder under it) loaded again; whether one is shown.
pub(super) fn forget_image(_page: &Page, _key: &str) -> bool {
    false
}
