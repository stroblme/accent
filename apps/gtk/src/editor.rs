//! One editor tab: a `sourceview5::View` on a note, plus its etag and debounced re-highlight.

use crate::highlight;
use accent_core::fs::{self, Etag};
use adw::prelude::*;
use gtk::glib;
use sourceview5::prelude::*;
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

/// Re-analysing on every keystroke would be wasteful; ~150 ms after the last one is invisible.
const DEBOUNCE: Duration = Duration::from_millis(150);

pub struct Tab {
    pub rel: String,
    pub path: PathBuf,
    pub view: sourceview5::View,
    pub buffer: sourceview5::Buffer,
    pub page: adw::TabPage,
    pub etag: Cell<Option<Etag>>,
    pub modified: Cell<bool>,
    /// Set while we replace the buffer text ourselves, so `changed` does not mark it dirty.
    loading: Cell<bool>,
    debounce: RefCell<Option<glib::SourceId>>,
}

/// Open `rel` from `vault` in a new tab of `tabs`.
pub fn open(vault: &Path, rel: &str, tabs: &adw::TabView) -> std::io::Result<Rc<Tab>> {
    let path = vault.join(rel);
    let (text, etag) = fs::read_note(&path)?;

    let buffer = sourceview5::Buffer::new(None);
    highlight::install_tags(&buffer);
    buffer.set_text(&text);
    buffer.set_highlight_matching_brackets(false);
    sync_scheme(&buffer);

    let view = sourceview5::View::new();
    view.set_buffer(Some(&buffer));
    view.set_monospace(false);
    view.add_css_class("accent-doc");
    view.set_wrap_mode(gtk::WrapMode::WordChar);
    view.set_show_line_numbers(false);
    // Apostrophe-like page: generous side gutters, room to breathe at the ends.
    view.set_left_margin(48);
    view.set_right_margin(48);
    view.set_top_margin(24);
    view.set_bottom_margin(96);
    view.set_pixels_above_lines(2);
    view.set_pixels_below_lines(2);

    let scroller = gtk::ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .child(&view)
        .build();
    let page = tabs.append(&scroller);
    page.set_title(title_of(rel));

    let tab = Rc::new(Tab {
        rel: rel.to_string(),
        path,
        view: view.clone(),
        buffer: buffer.clone(),
        page,
        etag: Cell::new(Some(etag)),
        modified: Cell::new(false),
        loading: Cell::new(false),
        debounce: RefCell::new(None),
    });

    highlight::apply(&buffer);
    // `view.color()` only resolves the theme foreground once the widget is mapped. A tab added to
    // the visible TabView is mapped by `append` above, so restyle now *and* on every later map
    // (a background tab is only mapped when it is first selected).
    highlight::restyle(&buffer, &view);
    view.connect_map(glib::clone!(
        #[strong]
        buffer,
        move |view| highlight::restyle(&buffer, view)
    ));

    buffer.connect_changed(glib::clone!(
        #[strong]
        tab,
        move |_| tab.on_changed()
    ));
    Ok(tab)
}

/// GtkSourceView paints its background from its own style scheme, so unlike every other widget in
/// the window it has to be told about dark mode explicitly.
fn sync_scheme(buffer: &sourceview5::Buffer) {
    let id = if adw::StyleManager::default().is_dark() {
        "Adwaita-dark"
    } else {
        "Adwaita"
    };
    let scheme = sourceview5::StyleSchemeManager::default().scheme(id);
    buffer.set_style_scheme(scheme.as_ref());
}

fn title_of(rel: &str) -> &str {
    rel.rsplit('/')
        .next()
        .map(|n| n.strip_suffix(".md").unwrap_or(n))
        .unwrap_or(rel)
}

impl Tab {
    /// ponytail: `Tab` and its buffer closure hold each other, so a closed tab's `Rc` never drops.
    /// One leaked struct per opened note for the lifetime of the process; break the cycle with a
    /// weak ref if long sessions ever show up in the memory profile.
    fn on_changed(self: &Rc<Self>) {
        if self.loading.get() {
            return;
        }
        if !self.modified.replace(true) {
            self.page.set_title(&format!("• {}", title_of(&self.rel)));
        }
        if let Some(id) = self.debounce.borrow_mut().take() {
            id.remove();
        }
        let id = glib::timeout_add_local_once(
            DEBOUNCE,
            glib::clone!(
                #[strong(rename_to = tab)]
                self,
                move || {
                    *tab.debounce.borrow_mut() = None;
                    highlight::apply(&tab.buffer);
                }
            ),
        );
        *self.debounce.borrow_mut() = Some(id);
    }

    pub fn text(&self) -> String {
        let (s, e) = self.buffer.bounds();
        self.buffer.text(&s, &e, true).to_string()
    }

    /// Replace the buffer with `text` without marking the tab dirty.
    pub fn set_text(&self, text: &str) {
        self.loading.set(true);
        self.buffer.set_text(text);
        self.loading.set(false);
        highlight::apply(&self.buffer);
    }

    pub fn mark_clean(&self, etag: Etag) {
        self.etag.set(Some(etag));
        self.modified.set(false);
        self.page.set_title(title_of(&self.rel));
    }

    /// Re-read the note from disk, discarding local edits.
    pub fn reload(&self) -> std::io::Result<()> {
        let (text, etag) = fs::read_note(&self.path)?;
        self.set_text(&text);
        self.mark_clean(etag);
        Ok(())
    }

    pub fn restyle(&self) {
        sync_scheme(&self.buffer);
        highlight::restyle(&self.buffer, &self.view);
    }
}
