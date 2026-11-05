//! A note's rendered preview in a tab of its own, right of the note in its pane (`Ctrl+M`), which
//! a drag takes into another pane to read the two side by side. The note is a text tab's, or a
//! terminal editor's, read as it was last saved.
//!
//! The tab holds a [`Preview`] only while it is on screen and a while after
//! ([`crate::preview::idle_for`]), so a preview tab behind another holds no page. It closes with
//! its note, and a note has one at most.

use super::*;
use crate::preview::Preview;
use crate::terminal::Term;
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
        Doc::Rendered(rendered) => rendered.note().map(|note| note.doc()),
        doc => Some(doc),
    }
}

/// What a preview tab renders: a note in accent's own tab, or one a terminal editor has open, as
/// last saved ([`crate::editors::Saved`]).
#[derive(Clone)]
pub enum Note {
    Tab(Rc<Tab>),
    Editor(Rc<Term>),
}

impl Note {
    /// The note `doc` holds, which Toggle Preview renders.
    pub fn of(doc: &Doc) -> Option<Note> {
        match doc {
            Doc::Text(tab) if tab.flavour().is_note() => Some(Note::Tab(tab.clone())),
            Doc::Terminal(term)
                if term
                    .file()
                    .is_some_and(|f| doc::kind_of(&f) == doc::Kind::Note) =>
            {
                Some(Note::Editor(term.clone()))
            }
            _ => None,
        }
    }

    /// The note's own tab.
    pub fn page(&self) -> &adw::TabPage {
        match self {
            Note::Tab(tab) => &tab.page,
            Note::Editor(term) => &term.page,
        }
    }

    pub fn rel(&self) -> String {
        match self {
            Note::Tab(tab) => tab.rel(),
            Note::Editor(term) => term.file().unwrap_or_default(),
        }
    }

    fn text(&self) -> String {
        match self {
            Note::Tab(tab) => tab.text(),
            Note::Editor(term) => term.saved.text(),
        }
    }

    /// The line accent's caret is on; a terminal editor keeps its own to itself.
    fn caret(&self) -> Option<u32> {
        match self {
            Note::Tab(tab) => Some(tab.cursor_line()),
            Note::Editor(_) => None,
        }
    }

    pub fn doc(&self) -> Doc {
        match self {
            Note::Tab(tab) => Doc::Text(tab.clone()),
            Note::Editor(term) => Doc::Terminal(term.clone()),
        }
    }

    fn downgrade(&self) -> Held {
        match self {
            Note::Tab(tab) => Held::Tab(Rc::downgrade(tab)),
            Note::Editor(term) => Held::Editor(Rc::downgrade(term)),
        }
    }
}

/// A [`Note`], held as long as its own tab is open.
#[derive(Clone)]
enum Held {
    Tab(Weak<Tab>),
    Editor(Weak<Term>),
}

impl Held {
    fn upgrade(&self) -> Option<Note> {
        match self {
            Held::Tab(tab) => tab.upgrade().map(Note::Tab),
            Held::Editor(term) => term.upgrade().map(Note::Editor),
        }
    }
}

/// Where a note's preview tab goes in its pane, the note being at `note`: right of it, but past
/// the `pinned` tabs at the start of the bar, being no pinned tab itself.
pub fn beside(note: usize, pinned: usize) -> usize {
    (note + 1).max(pinned)
}

pub struct Rendered {
    pub page: adw::TabPage,
    note: Held,
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
    pub fn note(&self) -> Option<Note> {
        self.note.upgrade()
    }

    /// Whether it renders the note whose own tab is `page`.
    pub fn is_of(&self, page: &adw::TabPage) -> bool {
        self.note().is_some_and(|note| note.page() == page)
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
            Some(preview) => preview.focus_target().clone(),
            None => self.frame.clone().upcast(),
        }
    }

    pub fn scroll_to_line(&self, line: u32) {
        if let Some(preview) = self.preview.borrow().as_ref().filter(|_| self.shown()) {
            preview.scroll_to_line(line);
        }
    }

    /// Out of sight: the preview, its page and pictures, go after [`crate::preview::idle_for`].
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
            note.page().disconnect(named);
        }
    }
}

impl App {
    /// Toggle Preview: from a note to its preview tab, opened right of it first if it is not open,
    /// and from a preview tab back to its note, the keyboard going along either way.
    pub(crate) fn toggle_preview(self: &Rc<Self>) {
        let page = match self.active_doc() {
            Some(Doc::Rendered(rendered)) => match rendered.note() {
                Some(note) => note.page().clone(),
                None => return,
            },
            doc => match doc.as_ref().and_then(Note::of) {
                Some(note) => match self.rendered_of(note.page()) {
                    Some(rendered) => rendered.page.clone(),
                    None => self.open_rendered(&note, Opened::Kept).page.clone(),
                },
                None => return,
            },
        };
        self.reveal_page(&page);
        if let Some(pane) = self.pane_of(&page) {
            self.focus_document(&pane);
        }
    }

    /// Open `note`'s preview tab right of the note, keeping the note, which a click down the
    /// Files tree would otherwise replace and take its preview with it. A restored one goes where
    /// the session put it ([`App::tabs_for`]).
    pub(crate) fn open_rendered(self: &Rc<Self>, note: &Note, how: Opened) -> Rc<Rendered> {
        let frame = adw::Bin::new();
        let page = match self
            .pane_of(note.page())
            .filter(|_| how != Opened::Restored)
        {
            Some(pane) => {
                let pages = pane.pages();
                let at = pages.iter().position(|p| p == note.page()).unwrap_or(0);
                let pinned = pages.iter().filter(|p| self.is_pinned(p)).count();
                pane.tabs.insert(&frame, beside(at, pinned) as i32)
            }
            None => self.tabs_for(&key_of(&note.rel())).append(&frame),
        };
        page.set_icon(Some(&gio::ThemedIcon::new(ICON)));
        // The note's file name, which its own title adds an unsaved dot and a comparison to;
        // asked again whenever that changes, a rename included.
        page.set_title(doc::file_name(&note.rel()));
        let held = note.downgrade();
        let named = note.page().connect_title_notify(glib::clone!(
            #[weak]
            page,
            #[strong]
            held,
            move |_| {
                if let Some(note) = held.upgrade() {
                    page.set_title(doc::file_name(&note.rel()));
                }
            }
        ));
        note.page()
            .bind_property("tooltip", &page, "tooltip")
            .sync_create()
            .build();
        let rendered = Rc::new(Rendered {
            page,
            note: note.downgrade(),
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
        self.promote(note.page());
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

    /// Show `note`'s Outline row at `at`: scrolled to in its preview while that is in front, else
    /// at the caret in a text tab. A terminal editor's cursor is the editor's.
    pub(crate) fn outline_jump(&self, note: &Note, at: accent_api::Pos) {
        let preview = self
            .rendered_in(&self.pane())
            .filter(|r| r.is_of(note.page()));
        match (preview, note) {
            (Some(rendered), _) => rendered.scroll_to_line(at.line + 1),
            (None, Note::Tab(tab)) => {
                self.mark();
                tab.goto_pos(at)
            }
            (None, Note::Editor(_)) => {}
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

    /// The preview tab of the note whose own tab is `note`, if it has one open.
    pub(crate) fn rendered_of(&self, note: &adw::TabPage) -> Option<Rc<Rendered>> {
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

    /// Close the preview tab of the note whose own tab `note` is closing. Picked to show in the
    /// note's place as that closes, it hands its place on to the tab used before either.
    pub(crate) fn close_rendered(&self, note: &adw::TabPage) {
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
                .find(|p| p != &rendered.page && p != note)
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
            let found = Rc::downgrade(rendered);
            let preview = self.new_preview(glib::clone!(
                #[weak(rename_to = app)]
                self,
                move |label| {
                    let pane = found.upgrade().and_then(|r| app.pane_of(&r.page));
                    if let Some(pane) = pane {
                        pane.find.set_matches_text(label);
                    }
                }
            ));
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
        if let (true, Some(line)) = (follow, note.caret()) {
            preview.scroll_to_line(line);
        }
    }

    /// Render `note`'s preview tab [`RENDER`] after the last edit. It follows the caret while the
    /// note is on screen too; one changed behind it, as a sync does, leaves the page where it is
    /// being read.
    pub(crate) fn queue_rendered(self: &Rc<Self>, note: &Rc<Tab>) {
        let Some(rendered) = self.rendered_of(&note.page) else {
            return;
        };
        let weak = Rc::downgrade(&rendered);
        rendered.refresh.call(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move || {
                if let Some(rendered) = weak.upgrade() {
                    let follow =
                        matches!(rendered.note(), Some(Note::Tab(note)) if note.view.is_mapped());
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
