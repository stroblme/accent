//! A file's tab while its document is on its way (DESIGN.md, Loading): the tab is up at once and
//! blank, shows the loading page once the open has taken [`OPENING`], and then the document, or
//! why it did not come.
//!
//! The tab's page holds a [`holder`], a stack of those four. A note, a diagram or a notebook is
//! read before its tab can be built, so its tab is a [`Held`] until the read lands; an image's tab
//! is built at once and holds its picture back until the picture is decoded, and a PDF's stack
//! shows the same loading and failure pages under the same names.

use adw::prelude::*;
use gtk::glib;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

/// How long an open runs before its tab shows the loading page, as long as the sidebar's
/// progress bar waits: a quick open, a local note or a cached copy, never flashes it.
pub(crate) const OPENING: Duration = Duration::from_millis(160);

/// How many times a holder has put its loading page up, which a quick open never does.
#[cfg(feature = "bench")]
pub(crate) static SHOWN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

const BLANK: &str = "blank";
/// The loading page's name in a holder, and in a PDF's stack, which shows it the same way.
pub(crate) const LOADING: &str = "loading";
const DOC: &str = "doc";
/// The failure page's name, in a holder or a PDF's stack.
pub(crate) const FAILED: &str = "failed";

/// The page a tab shows while its file is on its way: a spinner, centred, and nothing else (user
/// decision 2026-11-07).
pub(crate) fn page() -> adw::Spinner {
    adw::Spinner::builder()
        .width_request(32)
        .height_request(32)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .build()
}

/// The child of a tab whose document is not in yet: blank, then the loading [`page`] once
/// [`OPENING`] has passed, crossfading in.
pub(crate) fn holder() -> gtk::Stack {
    let holder = gtk::Stack::builder()
        .transition_type(gtk::StackTransitionType::Crossfade)
        .transition_duration(crate::widgets::FADE_MS)
        .build();
    holder.add_named(&adw::Bin::new(), Some(BLANK));
    holder.add_named(&page(), Some(LOADING));
    glib::timeout_add_local_once(
        OPENING,
        glib::clone!(
            #[weak]
            holder,
            move || {
                if holder.visible_child_name().as_deref() == Some(BLANK) {
                    #[cfg(feature = "bench")]
                    SHOWN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    holder.set_visible_child_name(LOADING);
                }
            }
        ),
    );
    holder
}

/// Put `content` in `holder` as its document, without showing it yet.
pub(crate) fn put(holder: &gtk::Stack, content: &impl IsA<gtk::Widget>) {
    if let Some(old) = holder.child_by_name(DOC) {
        holder.remove(&old);
    }
    holder.add_named(content, Some(DOC));
}

/// Show `holder`'s document: crossfading from the loading or the failure page, at once from the
/// blank page, which only a quick open shows and a newly opened tab fades in from already.
pub(crate) fn show(holder: &gtk::Stack) {
    let transition = match holder.visible_child_name().as_deref() {
        Some(BLANK) => gtk::StackTransitionType::None,
        _ => gtk::StackTransitionType::Crossfade,
    };
    holder.set_visible_child_full(DOC, transition);
}

/// [`put`] and [`show`] at once.
pub(crate) fn fill(holder: &gtk::Stack, content: &impl IsA<gtk::Widget>) {
    put(holder, content);
    show(holder);
}

/// Say in `holder` why `name` did not come, with Retry: a transfer that failed is tried again as
/// a failed upload is (`App::retry_upload`).
pub(crate) fn fail(holder: &gtk::Stack, name: &str, why: &str, retry: impl Fn() + 'static) {
    failed(holder, &format!("Cannot Open {name}"), why, retry);
}

/// [`fail`] under `title`, for what failed other than an open: a preview's page.
pub(crate) fn failed(holder: &gtk::Stack, title: &str, why: &str, retry: impl Fn() + 'static) {
    let button = gtk::Button::builder()
        .label("Retry")
        .halign(gtk::Align::Center)
        .css_classes(["pill"])
        .build();
    button.connect_clicked(move |_| retry());
    let page = adw::StatusPage::builder()
        .icon_name("dialog-warning-symbolic")
        .title(title)
        .description(why)
        .child(&button)
        .build();
    if let Some(old) = holder.child_by_name(FAILED) {
        holder.remove(&old);
    }
    holder.add_named(&page, Some(FAILED));
    holder.set_visible_child_name(FAILED);
}

/// Back from the failure page to the loading page for another try, at once: the reader asked
/// for it, or the link came back.
pub(crate) fn again(holder: &gtk::Stack) {
    if holder.visible_child_name().as_deref() == Some(FAILED) {
        holder.set_visible_child_name(LOADING);
    }
}

/// `why`, the end of "Cannot <what>: <why>", ended as a status page's sentence is. Its first
/// letter stays as it is: a reason may begin with a path or a method's name (`read_text timed
/// out`).
pub(crate) fn sentence(why: &str) -> String {
    match why.ends_with(['.', '!', '?']) {
        true => why.to_string(),
        false => format!("{why}."),
    }
}

/// The holder `page` shows its document in, if it was built with one.
pub(crate) fn holder_of(page: &adw::TabPage) -> Option<gtk::Stack> {
    page.child()
        .downcast::<gtk::Stack>()
        .ok()
        .filter(|stack| stack.child_by_name(DOC).is_some())
}

/// The widget showing `page`'s document: what its holder holds, else the page's own child.
pub(crate) fn content(page: &adw::TabPage) -> gtk::Widget {
    holder_of(page)
        .and_then(|holder| holder.child_by_name(DOC))
        .unwrap_or_else(|| page.child())
}

/// A file's tab before its document is in: its page, up in its pane at once, and the key it is
/// opening. A failed read leaves it in place, saying why, until it is tried again.
pub struct Held {
    key: RefCell<String>,
    pub page: adw::TabPage,
    holder: gtk::Stack,
    failed: Cell<bool>,
}

impl Held {
    /// A new tab of `tabs` for `key`, titled and with the icon its file will have.
    pub fn new(key: &str, tabs: &adw::TabView, tooltip: &str) -> Rc<Held> {
        let holder = holder();
        let page = tabs.append(&holder);
        page.set_title(crate::doc::file_name(key));
        page.set_tooltip(tooltip);
        crate::doc::set_file_icon(&page, key);
        Rc::new(Held {
            key: RefCell::new(key.to_string()),
            page,
            holder,
            failed: Cell::new(false),
        })
    }

    pub fn key(&self) -> String {
        self.key.borrow().clone()
    }

    /// A rename landed while the file was on its way.
    pub fn retarget(&self, key: &str) {
        *self.key.borrow_mut() = key.to_string();
    }

    /// Put the document in, in place of the loading page, and hand back the page it is in.
    pub fn fill(&self, content: &impl IsA<gtk::Widget>) -> adw::TabPage {
        fill(&self.holder, content);
        self.page.clone()
    }

    /// The read failed for `why`: say so, with Retry running `retry`.
    pub fn fail(&self, why: &str, retry: impl Fn() + 'static) {
        self.failed.set(true);
        fail(&self.holder, crate::doc::file_name(&self.key()), why, retry);
    }

    /// Whether the read failed and nothing is on its way.
    pub fn failed(&self) -> bool {
        self.failed.get()
    }

    /// Another read is on its way.
    pub fn again(&self) {
        self.failed.set(false);
        again(&self.holder);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reason_ends_as_a_sentence() {
        assert_eq!(sentence("read_text timed out"), "read_text timed out.");
        assert_eq!(
            sentence("/v/a.md: not a regular file."),
            "/v/a.md: not a regular file."
        );
    }
}
