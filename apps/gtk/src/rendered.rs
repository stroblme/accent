//! A note's rendered preview in a tab of its own, right of the note in its pane (`Ctrl+M`), which
//! a drag takes into another pane to read the two side by side. The note is a text tab's, or a
//! terminal editor's, read as it was last saved; a notebook's preview tab is a read-only view of
//! the notebook, its markdown cells rendered (`notebook::viewer`).
//!
//! The tab holds a [`Preview`], or a notebook's view, only while it is on screen and a while after
//! ([`crate::preview::idle_for`]), so a preview tab behind another holds no page. It closes with
//! its note, and a note has one at most.

use super::*;
use crate::notebook::NotebookTab;
use crate::preview::Preview;
use crate::terminal::Term;
use accent_core::note::Syntax;
use accent_core::notebook::CellId;
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
/// last saved ([`crate::editors::Saved`]), or a notebook.
#[derive(Clone)]
pub enum Note {
    Tab(Rc<Tab>),
    Editor(Rc<Term>),
    Notebook(Rc<NotebookTab>),
}

impl Note {
    /// What `doc` holds that Toggle Preview shows a preview tab of: a note, or a notebook.
    pub fn previewed(doc: &Doc) -> Option<Note> {
        match doc {
            Doc::Notebook(nb) => Some(Note::Notebook(nb.clone())),
            doc => Note::of(doc),
        }
    }

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
            Note::Notebook(nb) => &nb.page,
        }
    }

    pub fn rel(&self) -> String {
        match self {
            Note::Tab(tab) => tab.rel(),
            Note::Editor(term) => term.file().unwrap_or_default(),
            Note::Notebook(nb) => nb.key(),
        }
    }

    /// A note's text; a notebook's view follows the notebook itself.
    fn text(&self) -> Option<String> {
        match self {
            Note::Tab(tab) => Some(tab.text()),
            Note::Editor(term) => Some(term.saved.text()),
            Note::Notebook(_) => None,
        }
    }

    /// The line accent's caret is on; a terminal editor keeps its own to itself.
    fn caret(&self) -> Option<u32> {
        match self {
            Note::Tab(tab) => Some(tab.cursor_line()),
            Note::Editor(_) | Note::Notebook(_) => None,
        }
    }

    /// Whether a click on a task's box in the preview can flip it in the note: accent's own buffer
    /// where it is editable, or a terminal editor's file; a notebook's view is read only.
    fn writable(&self) -> bool {
        match self {
            Note::Tab(tab) => tab.view.is_editable(),
            Note::Editor(_) => true,
            Note::Notebook(_) => false,
        }
    }

    /// Whether the note's own view is on screen, which its preview then follows to its caret.
    fn on_screen(&self) -> bool {
        match self {
            Note::Tab(tab) => tab.view.is_mapped(),
            Note::Editor(_) => false,
            Note::Notebook(nb) => nb.on_screen(),
        }
    }

    pub fn doc(&self) -> Doc {
        match self {
            Note::Tab(tab) => Doc::Text(tab.clone()),
            Note::Editor(term) => Doc::Terminal(term.clone()),
            Note::Notebook(nb) => Doc::Notebook(nb.clone()),
        }
    }

    fn downgrade(&self) -> Held {
        match self {
            Note::Tab(tab) => Held::Tab(Rc::downgrade(tab)),
            Note::Editor(term) => Held::Editor(Rc::downgrade(term)),
            Note::Notebook(nb) => Held::Notebook(Rc::downgrade(nb)),
        }
    }
}

/// A [`Note`], held as long as its own tab is open.
#[derive(Clone)]
enum Held {
    Tab(Weak<Tab>),
    Editor(Weak<Term>),
    Notebook(Weak<NotebookTab>),
}

impl Held {
    fn upgrade(&self) -> Option<Note> {
        match self {
            Held::Tab(tab) => tab.upgrade().map(Note::Tab),
            Held::Editor(term) => term.upgrade().map(Note::Editor),
            Held::Notebook(nb) => nb.upgrade().map(Note::Notebook),
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
    /// A notebook's read-only view, in the preview's place.
    viewer: RefCell<Option<Rc<NotebookTab>>>,
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

    /// A notebook's read-only view, while it has one.
    pub fn viewer(&self) -> Option<Rc<NotebookTab>> {
        self.viewer.borrow().clone()
    }

    /// Whether the tab is on screen, the only place anything is rendered.
    pub fn shown(&self) -> bool {
        self.frame.is_mapped()
    }

    /// What takes the keyboard: the page, which scrolls with the reading keys.
    pub fn key_target(&self) -> gtk::Widget {
        if let Some(viewer) = self.viewer() {
            return viewer.key_target();
        }
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
                rendered.viewer.take();
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
            doc => match doc.as_ref().and_then(Note::previewed) {
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
            viewer: RefCell::new(None),
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
            // A notebook's rows are its cells' (`notebook_outline_jump`).
            (None, Note::Editor(_) | Note::Notebook(_)) => {}
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
    ///
    /// A notebook's rendered cells are previews too, a presented notebook's and a preview tab's.
    pub(crate) fn each_preview(&self, mut f: impl FnMut(&Preview)) {
        if let Some(preview) = self.preview.borrow().as_ref() {
            f(preview);
        }
        for rendered in self.rendereds() {
            if let Some(preview) = rendered.preview().as_ref() {
                f(preview);
            }
        }
        for notebook in self.notebooks().iter().chain(&self.notebook_viewers()) {
            notebook.each_page(&mut f);
        }
    }

    /// Whether a notebook's preview tab is in front, over which the Code pane's kernel strip is
    /// put away and its commands greyed: the preview runs nothing, the notebook's own tab does.
    pub(crate) fn previews_notebook(&self) -> bool {
        let rendered = self.active_doc().and_then(|doc| doc.rendered().cloned());
        rendered.is_some_and(|r| matches!(r.note(), Some(Note::Notebook(_))))
    }

    /// The notebooks' preview tabs' views, those that have one.
    pub(crate) fn notebook_viewers(&self) -> Vec<Rc<NotebookTab>> {
        self.rendereds().iter().filter_map(|r| r.viewer()).collect()
    }

    /// An Outline row of notebook `nb`: in its preview tab while that is in front, as a note's
    /// preview scrolls to a heading rather than moving the note's caret, else at the caret.
    pub(crate) fn notebook_outline_jump(&self, nb: &Rc<NotebookTab>, (id, at): (CellId, i32)) {
        let viewer = (self.rendered_in(&self.pane()))
            .filter(|r| r.is_of(&nb.page))
            .and_then(|r| r.viewer());
        match viewer {
            Some(viewer) => viewer.goto(id, at),
            None => {
                self.mark();
                nb.goto(id, at)
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
        if let Some(Note::Notebook(nb)) = rendered.note() {
            if rendered.viewer.borrow().is_none() {
                let viewer = crate::notebook::viewer(&nb, &rendered.page, &rendered.frame);
                self.wire_notebook_places(&viewer);
                let weak = Rc::downgrade(self);
                viewer.connect_zoom(move |out| {
                    if let Some(app) = weak.upgrade() {
                        app.set_zoom(crate::zoom::stepped_zoom(app.zoom.get(), out));
                    }
                });
                *rendered.viewer.borrow_mut() = Some(viewer);
            }
            return self.render_rendered(rendered, true);
        }
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
            // A presented document is read only.
            let (app, held) = (Rc::downgrade(self), Rc::downgrade(rendered));
            let live = {
                let (app, held) = (app.clone(), held.clone());
                move || {
                    let note = held.upgrade().and_then(|r| r.note());
                    let presenting = app.upgrade().is_none_or(|a| a.presenting.get().is_some());
                    note.is_some_and(|note| note.writable()) && !presenting
                }
            };
            let flip = move |shown: &str, line| {
                if let (Some(app), Some(rendered)) = (app.upgrade(), held.upgrade()) {
                    app.click_box(&rendered, shown, line);
                }
            };
            preview.connect_tasks(crate::preview::Tasks {
                live: Box::new(live),
                flip: Box::new(flip),
            });
            rendered.frame.set_child(Some(preview.widget()));
            *rendered.preview.borrow_mut() = Some(preview);
        }
        self.render_rendered(rendered, true);
    }

    /// Render the note into its preview tab while that is on screen, scrolled to the note's caret
    /// if `follow`.
    pub(crate) fn render_rendered(&self, rendered: &Rendered, follow: bool) {
        let Some(note) = rendered.note().filter(|_| rendered.shown()) else {
            return;
        };
        // A notebook's view takes the notebook as it is now, and its selected cell's place.
        if let (Note::Notebook(nb), Some(viewer)) = (&note, rendered.viewer()) {
            viewer.follow(nb.snapshot(), &nb.queued());
            if follow {
                viewer.show_place_of(nb);
            }
            return;
        }
        let preview = rendered.preview.borrow();
        let (Some(preview), Some(text)) = (preview.as_ref(), note.text()) else {
            return;
        };
        preview.render(&note.rel(), &text);
        if let (true, Some(line)) = (follow, note.caret()) {
            preview.scroll_to_line(line);
        }
    }

    /// A click on the box of the task on line `line` in `rendered`'s page, which showed `shown`:
    /// the box flipped in accent's buffer as one undo step, which its save follows as a typed
    /// edit's, and the page rendered at once where it is being read; or in a terminal editor's
    /// file. Nothing where the note is no longer what the page shows, whose render is on its way.
    fn click_box(self: &Rc<Self>, rendered: &Rendered, shown: &str, line: u32) {
        let tab = match rendered.note() {
            Some(Note::Tab(tab)) => tab,
            Some(Note::Editor(term)) => return self.click_saved_box(&term, shown, line),
            Some(Note::Notebook(_)) | None => return,
        };
        let text = tab.text();
        let syntax = Syntax::of(&tab.rel());
        let Some((at, tick)) = (text == shown)
            .then(|| syntax.click_box(&text, line))
            .flatten()
        else {
            return;
        };
        let at = text[..at].chars().count() as i32;
        let buffer = &tab.buffer;
        // The new tick after the old, then the old out, so a caret either side of it stays there.
        buffer.begin_user_action();
        buffer.insert(&mut buffer.iter_at_offset(at + 1), &tick.to_string());
        buffer.delete(
            &mut buffer.iter_at_offset(at),
            &mut buffer.iter_at_offset(at + 1),
        );
        buffer.end_user_action();
        self.render_rendered(rendered, false);
    }

    /// [`click_box`](Self::click_box) in a terminal editor's file, read as it is saved: flipped
    /// where it still holds what the page shows and written over that read, its line endings as
    /// they were, then read again for the preview. The editor hears of it as of any save from
    /// outside.
    fn click_saved_box(self: &Rc<Self>, term: &Rc<Term>, shown: &str, line: u32) {
        let Some(key) = term.file() else {
            return;
        };
        let loose = doc::is_loose_key(&key);
        let vault = self.vault().filter(|_| !loose).cloned();
        let path = match loose {
            true => PathBuf::from(&key),
            false => self.root().join(&key),
        };
        let (shown, rel) = (shown.to_string(), key.clone());
        let app = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let wrote = work::attempt("change the task's box", move || {
                let read = match &vault {
                    Some(vault) => vault.read_text(&rel)?,
                    None => accent_core::fs::read_text(&path)?,
                };
                let accent_core::fs::Read::Text(read) = read else {
                    return Ok(false);
                };
                let flipped = (!read.lossy && read.text == shown)
                    .then(|| Syntax::of(&rel).click_box(&read.text, line))
                    .flatten();
                let Some((at, tick)) = flipped else {
                    return Ok(false);
                };
                let mut text = read.text;
                text.replace_range(at..at + 1, &tick.to_string());
                let disk = accent_core::fs::for_disk(&text, read.crlf, false);
                match &vault {
                    Some(vault) => vault.save(&rel, &disk, Some(read.etag))?,
                    None => accent_core::fs::write_note(&path, &disk, Some(read.etag))?,
                };
                Ok::<_, accent_api::Error>(true)
            })
            .await;
            let Some(app) = app.upgrade() else {
                return;
            };
            match wrote {
                // A save of the window's own brings no news from the watcher.
                Ok(true) => {
                    app.saved_changed(&key);
                    if let Some(git) = app.git.get() {
                        git.schedule_paths([key], false);
                    }
                }
                Ok(false) => {}
                Err(why) => app.toast(&why),
            }
        });
    }

    /// Render the preview tab of the note or notebook whose own tab is `page` [`RENDER`] after
    /// the last change. It follows the caret while the note is on screen too, and the preview
    /// does not have the keyboard; one changed behind it, as a sync does, or by a click on the
    /// page, leaves the page where it is being read.
    pub(crate) fn queue_rendered(self: &Rc<Self>, page: &adw::TabPage) {
        let Some(rendered) = self.rendered_of(page) else {
            return;
        };
        let weak = Rc::downgrade(&rendered);
        rendered.refresh.call(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move || {
                if let Some(rendered) = weak.upgrade() {
                    let follow = rendered.note().is_some_and(|note| note.on_screen())
                        && !rendered.key_target().is_focus();
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
