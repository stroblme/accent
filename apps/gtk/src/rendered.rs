//! A note's rendered preview in a tab of its own, right of the note in its pane (`Ctrl+M`), which
//! a drag takes into another pane to read the two side by side.
//!
//! The tab holds a [`Preview`] only while it is on screen and a while after
//! ([`crate::preview::idle_for`]), so a preview tab behind another costs no web process. It closes
//! with its note, and a note has one at most.

use super::*;
use crate::preview::Preview;
use std::cell::Ref;
use std::rc::Weak;

/// What a preview tab's key starts with, the note's own key after it: what the session keeps it
/// by, and never a path.
const PREFIX: &str = "preview:";

/// The tab's icon, a presentation's.
const ICON: &str = "x-office-presentation-symbolic";

/// The key of `note`'s preview tab.
pub fn key_of(note: &str) -> String {
    format!("{PREFIX}{note}")
}

/// The note a preview tab's `key` renders, or `None` for any other key.
pub fn note_of(key: &str) -> Option<&str> {
    key.strip_prefix(PREFIX)
}

/// `doc`, or the note it renders for a preview tab: what stands for a preview tab where a file is
/// asked for.
pub fn note_for(doc: Doc) -> Option<Doc> {
    match doc {
        Doc::Rendered(rendered) => rendered.note().map(Doc::Text),
        doc => Some(doc),
    }
}

/// Where a note's preview tab goes in its pane, the note being at `note`: right of it, but past
/// the `pinned` tabs at the start of the bar, being no pinned tab itself.
pub fn beside(note: usize, pinned: usize) -> usize {
    (note + 1).max(pinned)
}

pub struct Rendered {
    pub page: adw::TabPage,
    note: Weak<Tab>,
    /// The handler on the note's tab that names this one after it.
    named: Option<glib::SignalHandlerId>,
    /// The page's child, which holds the preview while there is one.
    frame: adw::Bin,
    preview: RefCell<Option<Preview>>,
    /// The preview's release, from when the tab went out of sight.
    idle: widgets::Debounce,
    /// The render after the note was edited ([`RENDER`]).
    refresh: widgets::Debounce,
}

impl Rendered {
    pub fn key(&self) -> String {
        key_of(&self.note().map(|note| note.rel()).unwrap_or_default())
    }

    /// The note it renders, open as long as this tab is.
    pub fn note(&self) -> Option<Rc<Tab>> {
        self.note.upgrade()
    }

    pub fn is_of(&self, note: &Rc<Tab>) -> bool {
        std::ptr::eq(self.note.as_ptr(), Rc::as_ptr(note))
    }

    pub fn preview(&self) -> Ref<'_, Option<Preview>> {
        self.preview.borrow()
    }

    /// Whether the tab is on screen, the only place anything is rendered.
    pub fn shown(&self) -> bool {
        self.frame.is_mapped()
    }

    /// What takes the keyboard: the page, which scrolls with the reading keys.
    pub fn key_target(&self) -> gtk::Widget {
        match self.preview.borrow().as_ref() {
            Some(preview) => preview.widget().clone(),
            None => self.frame.clone().upcast(),
        }
    }

    pub fn scroll_to_line(&self, line: u32) {
        if let Some(preview) = self.preview.borrow().as_ref().filter(|_| self.shown()) {
            preview.scroll_to_line(line);
        }
    }

    /// Out of sight: the preview and its web process go after [`crate::preview::idle_for`].
    fn release_soon(self: &Rc<Self>) {
        let rendered = Rc::downgrade(self);
        self.idle.call_once(move || {
            if let Some(rendered) = rendered.upgrade() {
                rendered.frame.set_child(gtk::Widget::NONE);
                rendered.preview.take();
            }
        });
    }
}

impl Drop for Rendered {
    fn drop(&mut self) {
        if let (Some(note), Some(named)) = (self.note.upgrade(), self.named.take()) {
            note.page.disconnect(named);
        }
    }
}

impl App {
    /// Toggle Preview: from a markdown note to its preview tab, opened right of it first if it is
    /// not open, and from a preview tab back to its note, the keyboard going along either way.
    pub(crate) fn toggle_preview(self: &Rc<Self>) {
        let page = match self.active_doc() {
            Some(Doc::Text(tab)) if tab.flavour().is_markdown() => match self.rendered_of(&tab) {
                Some(rendered) => rendered.page.clone(),
                None => self.open_rendered(&tab, Opened::Kept).page.clone(),
            },
            Some(Doc::Rendered(rendered)) => match rendered.note() {
                Some(note) => note.page.clone(),
                None => return,
            },
            _ => return,
        };
        self.reveal_page(&page);
        if let Some(pane) = self.pane_of(&page) {
            self.focus_document(&pane);
        }
    }

    /// Open `note`'s preview tab right of the note, keeping the note, which a click down the
    /// Files tree would otherwise replace and take its preview with it. A restored one goes where
    /// the session put it ([`App::tabs_for`]).
    pub(crate) fn open_rendered(self: &Rc<Self>, note: &Rc<Tab>, how: Opened) -> Rc<Rendered> {
        let frame = adw::Bin::new();
        let page = match self.pane_of(&note.page).filter(|_| how != Opened::Restored) {
            Some(pane) => {
                let pages = pane.pages();
                let at = pages.iter().position(|p| p == &note.page).unwrap_or(0);
                let pinned = pages.iter().filter(|p| self.is_pinned(p)).count();
                pane.tabs.insert(&frame, beside(at, pinned) as i32)
            }
            None => self.tabs_for(&key_of(&note.rel())).append(&frame),
        };
        page.set_icon(Some(&gio::ThemedIcon::new(ICON)));
        // The note's file name, which its own title adds an unsaved dot and a comparison to;
        // asked again whenever that changes, a rename included.
        let title = |note: &Tab, page: &adw::TabPage| page.set_title(doc::file_name(&note.rel()));
        title(note, &page);
        let named = note.page.connect_title_notify(glib::clone!(
            #[weak]
            page,
            #[weak]
            note,
            move |_| title(&note, &page)
        ));
        note.page
            .bind_property("tooltip", &page, "tooltip")
            .sync_create()
            .build();
        let rendered = Rc::new(Rendered {
            page,
            note: Rc::downgrade(note),
            named: Some(named),
            frame,
            preview: RefCell::new(None),
            idle: widgets::Debounce::new(crate::preview::idle_for()),
            refresh: widgets::Debounce::new(RENDER),
        });
        rendered.frame.connect_map(glib::clone!(
            #[weak(rename_to = app)]
            self,
            #[weak]
            rendered,
            move |_| app.show_rendered(&rendered)
        ));
        rendered.frame.connect_unmap(glib::clone!(
            #[weak]
            rendered,
            move |_| rendered.release_soon()
        ));
        // The first page of an empty pane is on screen as it is added, before the handlers were.
        if rendered.shown() {
            self.show_rendered(&rendered);
        }
        self.docs.borrow_mut().push(Doc::Rendered(rendered.clone()));
        self.select_new_page(&rendered.page, how);
        self.mark_opened(&rendered.page, how);
        self.promote(&note.page);
        self.save_session_soon();
        rendered
    }

    /// The document the side panes speak for: the one in front, a preview tab's note for a
    /// preview tab (user decision 2026-10-28), so Info, Outline and References read the same over
    /// either.
    pub(crate) fn side_doc(&self) -> Option<Doc> {
        self.active_doc().and_then(note_for)
    }

    /// Whether the side panes speak for `tab`: it is in front, or its preview is.
    pub(crate) fn speaks_for(&self, tab: &Rc<Tab>) -> bool {
        self.side_doc()
            .and_then(|doc| doc.tab().cloned())
            .is_some_and(|side| Rc::ptr_eq(&side, tab))
    }

    /// Show `tab`'s Outline row at `at`: scrolled to in its preview while that is in front, else
    /// at the caret in the note.
    pub(crate) fn outline_jump(&self, tab: &Rc<Tab>, at: accent_api::Pos) {
        match self.rendered_in(&self.pane()).filter(|r| r.is_of(tab)) {
            Some(rendered) => rendered.scroll_to_line(at.line + 1),
            None => tab.goto_pos(at),
        }
    }

    /// Every preview tab of the window.
    pub(crate) fn rendereds(&self) -> Vec<Rc<Rendered>> {
        self.docs
            .borrow()
            .iter()
            .filter_map(|d| d.rendered().cloned())
            .collect()
    }

    /// `note`'s preview tab, if it has one open.
    pub(crate) fn rendered_of(&self, note: &Rc<Tab>) -> Option<Rc<Rendered>> {
        self.rendereds().into_iter().find(|r| r.is_of(note))
    }

    /// The preview tab in front of `pane`, if that is one.
    pub(crate) fn rendered_in(&self, pane: &Pane) -> Option<Rc<Rendered>> {
        self.doc_of(pane)?.rendered().cloned()
    }

    /// Every preview of the window: the presented note's, and each preview tab's that has one.
    pub(crate) fn each_preview(&self, mut f: impl FnMut(&Preview)) {
        if let Some(preview) = self.preview.borrow().as_ref() {
            f(preview);
        }
        for rendered in self.rendereds() {
            if let Some(preview) = rendered.preview().as_ref() {
                f(preview);
            }
        }
    }

    /// Close `note`'s preview tab with the note. Picked to show in the note's place as that
    /// closes, it hands its place on to the tab used before either.
    pub(crate) fn close_rendered(&self, note: &Rc<Tab>) {
        let Some(rendered) = self.rendered_of(note) else {
            return;
        };
        let Some(pane) = self.pane_of(&rendered.page) else {
            return;
        };
        if pane.tabs.selected_page().as_ref() == Some(&rendered.page)
            && let Some(next) = pane
                .recent()
                .into_iter()
                .find(|p| p != &rendered.page && p != &note.page)
        {
            pane.tabs.set_selected_page(&next);
        }
        pane.tabs.close_page(&rendered.page);
    }

    /// The tab came on screen: build its preview if it was let go, and render the note at its
    /// caret, as any render does.
    fn show_rendered(self: &Rc<Self>, rendered: &Rc<Rendered>) {
        rendered.idle.cancel();
        if rendered.preview.borrow().is_none() {
            let (found, lost) = (Rc::downgrade(rendered), Rc::downgrade(rendered));
            let preview = self.new_preview(
                glib::clone!(
                    #[weak(rename_to = app)]
                    self,
                    move |label| {
                        let pane = found.upgrade().and_then(|r| app.pane_of(&r.page));
                        if let Some(pane) = pane {
                            pane.find.set_matches_text(label);
                        }
                    }
                ),
                glib::clone!(
                    #[weak(rename_to = app)]
                    self,
                    move || {
                        if let Some(rendered) = lost.upgrade() {
                            app.render_rendered(&rendered, true);
                        }
                    }
                ),
            );
            rendered.frame.set_child(Some(preview.widget()));
            *rendered.preview.borrow_mut() = Some(preview);
        }
        self.render_rendered(rendered, true);
    }

    /// Render the note into its preview tab while that is on screen, scrolled to the note's caret
    /// if `follow`.
    pub(crate) fn render_rendered(&self, rendered: &Rendered, follow: bool) {
        let preview = rendered.preview.borrow();
        let (Some(preview), Some(note)) = (preview.as_ref(), rendered.note()) else {
            return;
        };
        if !rendered.shown() {
            return;
        }
        preview.render(&note.rel(), &note.text());
        if follow {
            preview.scroll_to_line(note.cursor_line());
        }
    }

    /// Render `note`'s preview tab [`RENDER`] after the last edit. It follows the caret while the
    /// note is on screen too; one changed behind it, as a sync does, leaves the page where it is
    /// being read.
    pub(crate) fn queue_rendered(self: &Rc<Self>, note: &Rc<Tab>) {
        let Some(rendered) = self.rendered_of(note) else {
            return;
        };
        let weak = Rc::downgrade(&rendered);
        rendered.refresh.call(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move || {
                if let Some(rendered) = weak.upgrade() {
                    let follow = rendered.note().is_some_and(|n| n.view.is_mapped());
                    app.render_rendered(&rendered, follow);
                }
            }
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_preview_key_names_its_note_and_no_file() {
        let key = key_of("Inbox/note.md");
        assert_eq!(note_of(&key), Some("Inbox/note.md"));
        assert_eq!(note_of("Inbox/note.md"), None);
        assert!(!doc::is_loose_key(&key_of("/home/me/loose.md")));
    }

    /// Right of the note, past the pinned tabs where the note is one of them.
    #[test]
    fn a_preview_tab_opens_right_of_its_note() {
        assert_eq!(beside(0, 0), 1);
        assert_eq!(beside(2, 1), 3);
        assert_eq!(beside(0, 3), 3);
    }
}
