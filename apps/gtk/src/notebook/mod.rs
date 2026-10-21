//! Notebook tabs: a Jupyter or marimo notebook as a column of cells, VS Code's notebook editor.
//! `accent_core::notebook` reads and writes the file; [`NotebookTab`] keeps a view per cell in
//! step with the model, records every change on the notebook's one history (`history`), runs a
//! Jupyter notebook's cells on a kernel (`kernel`), and saves through the same etag gate as a
//! note (`window`).

mod cell;
mod find;
mod history;
mod kernel;
mod language;
mod outline;
mod output;
mod window;

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Instant;

use accent_core::fs::{Digest, Etag};
use accent_core::notebook::{CellId, Edit, Kind, Notebook};
use adw::prelude::*;
use gtk::{gdk, gio, glib, graphene};

use crate::editor::{SaveState, Saves};
use crate::widgets::{Debounce, Hook};
use cell::{CellView, Editor, Tools};
use history::{Change, History, Step};
#[cfg(feature = "bench")]
pub use output::Counts;

type TabHook = Hook<dyn Fn(&Rc<NotebookTab>)>;

pub struct NotebookTab {
    key: RefCell<String>,
    path: RefCell<PathBuf>,
    pub page: adw::TabPage,
    /// "This notebook changed on disk", while the tab holds edits the file does not.
    banner: adw::Banner,
    scroller: gtk::ScrolledWindow,
    /// The cells, under the strip that adds one at the top.
    column: gtk::Box,
    head: gtk::Box,
    /// The cell buttons, over the cell under the pointer, else the selected one.
    tools: Tools,
    /// The add strip of the gaps between cells, and the cell whose gap is under the pointer.
    gap: gtk::Box,
    gap_cell: Cell<Option<CellId>>,
    /// The cell under the pointer.
    hovered: Cell<Option<CellId>>,
    /// The buttons and the add strip are to be moved once the event at hand is over.
    placing: Cell<bool>,
    /// The frame the editors near the view were last built in, and the hook that builds them.
    built_at: Cell<i64>,
    /// The first cell reaching into the view at the last layout, and its top in the content.
    anchor: Cell<Option<(CellId, f64)>>,
    /// A cell whose caret is to be brought into view once its editor is laid out ([`show`]).
    ///
    /// [`show`]: Self::show
    reveal: Cell<Option<(CellId, Option<f64>)>>,
    layout_hook: RefCell<Option<(gdk::FrameClock, glib::SignalHandlerId)>>,
    model: RefCell<Notebook>,
    /// A view per cell, in the model's order.
    views: RefCell<Vec<Rc<CellView>>>,
    /// The code cells' language, from the notebook's own metadata.
    language: Option<sourceview5::Language>,
    selected: Cell<Option<CellId>>,
    history: RefCell<History>,
    /// The cell a user action is changing, and its changes so far, until the action ends.
    pending: RefCell<Option<(CellId, Vec<Change>)>>,
    /// How deep in user actions the buffer being edited is.
    acting: Cell<u32>,
    /// Cells whose text moved ahead of the model since it was last brought up to date.
    stale: RefCell<Vec<CellId>>,
    /// A buffer is being set from the model or the history, which records nothing.
    replaying: Cell<bool>,
    /// A press in a cell is being handled: the caret it puts down is under the pointer.
    pressing: Cell<bool>,
    presenting: Cell<bool>,
    /// The kernel its runs go to, and what they changed (`kernel`).
    kernel: kernel::Hold,
    /// The notebook's fonts at its zoom, one rule for all its cells.
    font: RefCell<Option<gtk::CssProvider>>,
    pub save: SaveState,
    /// The write an edit schedules, which a burst of edits shares.
    autosave: Debounce,
    monitor: RefCell<Option<gio::FileMonitor>>,
    on_history: TabHook,
    on_autosave: TabHook,
    on_banner: TabHook,
    on_select: TabHook,
    /// Ctrl+scroll over the notebook: `true` zooms out.
    on_zoom: Hook<dyn Fn(bool)>,
    /// The find bar's query over the cells and where it matches (`find`).
    find: RefCell<find::Find>,
    /// The matches or the readout changed.
    on_matches: TabHook,
    /// A cell's caret moved.
    on_caret: TabHook,
    /// The code cells' document on the language layer, and their editors there (`language`).
    lang: language::State,
}

/// A new tab of `tabs` showing `notebook` as it was read, at its etag and digest.
pub fn open(
    key: &str,
    path: &Path,
    title: &str,
    tooltip: &str,
    tabs: &adw::TabView,
    (notebook, etag, digest): (Notebook, Etag, Digest),
) -> Rc<NotebookTab> {
    cell::install_css();
    let (head, add_code, add_markdown) = cell::inserter();
    let (gap, gap_code, gap_markdown) = cell::inserter();
    let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
    column.set_widget_name(&crate::editor::next_view_name());
    column.set_margin_top(12);
    column.set_margin_bottom(96);
    column.append(&head);
    let clamp = adw::Clamp::builder()
        .maximum_size(1100)
        .tightening_threshold(900)
        .child(&column)
        .build();
    // The cells go in before the column is in the window, where each widget made would be
    // styled on its own as it is added.
    let scroller = gtk::ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();
    let banner = adw::Banner::builder()
        .title("This notebook changed on disk")
        .button_label("Resolve…")
        .build();
    let host = gtk::Box::new(gtk::Orientation::Vertical, 0);
    host.append(&banner);
    host.append(&scroller);
    let page = tabs.append(&host);
    page.set_title(title);
    page.set_tooltip(tooltip);
    page.set_icon(Some(&gio::ThemedIcon::new("view-list-ordered-symbolic")));

    let language = crate::editor::language_for(&format!("cell{}", notebook.extension()), "");
    let tab = Rc::new(NotebookTab {
        key: RefCell::new(key.to_string()),
        path: RefCell::new(path.to_path_buf()),
        page,
        banner,
        scroller,
        column,
        head,
        tools: Tools::new(notebook.format()),
        gap,
        gap_cell: Cell::new(None),
        hovered: Cell::new(None),
        placing: Cell::new(false),
        built_at: Cell::new(-1),
        anchor: Cell::new(None),
        reveal: Cell::new(None),
        layout_hook: RefCell::default(),
        model: RefCell::new(notebook),
        views: RefCell::default(),
        language,
        selected: Cell::new(None),
        history: RefCell::default(),
        pending: RefCell::default(),
        acting: Cell::new(0),
        stale: RefCell::default(),
        replaying: Cell::new(false),
        pressing: Cell::new(false),
        presenting: Cell::new(false),
        kernel: kernel::Hold::default(),
        font: RefCell::default(),
        save: SaveState::at(etag, digest),
        autosave: Debounce::new(crate::editor::AUTOSAVE),
        monitor: RefCell::default(),
        on_history: Hook::default(),
        on_autosave: Hook::default(),
        on_banner: Hook::default(),
        on_select: Hook::default(),
        on_zoom: Hook::default(),
        find: RefCell::default(),
        on_matches: Hook::default(),
        on_caret: Hook::default(),
        lang: language::State::default(),
    });
    tab.reconcile();
    // The notebook follows the caret itself (`follow_caret`): the viewport's own scroll to the
    // focused widget would bring a clicked cell's whole editor into view, moving what was clicked.
    let viewport = gtk::Viewport::builder()
        .scroll_to_focus(false)
        .child(&clamp)
        .build();
    tab.scroller.set_child(Some(&viewport));
    add_code.connect_clicked(glib::clone!(
        #[weak]
        tab,
        move |_| tab.insert_at(0, Kind::Code)
    ));
    add_markdown.connect_clicked(glib::clone!(
        #[weak]
        tab,
        move |_| tab.insert_at(0, Kind::Markdown)
    ));
    tab.banner.connect_button_clicked(glib::clone!(
        #[weak]
        tab,
        move |_| tab.on_banner.emit(&tab)
    ));
    tab.wire_tools(&gap_code, &gap_markdown);
    tab.wire_building();
    tab.wire_keys(&host);
    crate::zoom::zoom_on_wheel(
        &tab.scroller,
        gtk::PropagationPhase::Capture,
        glib::clone!(
            #[weak]
            tab,
            move |out| {
                if let Some(f) = tab.on_zoom.get() {
                    f(out);
                }
            }
        ),
    );
    tab
}

impl NotebookTab {
    pub fn key(&self) -> String {
        self.key.borrow().clone()
    }

    pub fn path(&self) -> PathBuf {
        self.path.borrow().clone()
    }

    /// The widget the keys go to: the selected cell's editor, else the first.
    pub fn key_target(self: &Rc<Self>) -> gtk::Widget {
        let first = || self.views.borrow().first().map(|v| v.id);
        match self
            .selected
            .get()
            .or_else(first)
            .and_then(|id| self.editor(id))
        {
            Some((_, editor)) => editor.view.upcast(),
            None => self.scroller.clone().upcast(),
        }
    }

    /// A rename landed: follow the file.
    pub fn retarget(self: &Rc<Self>, root: &Path, key: &str) {
        *self.key.borrow_mut() = key.to_string();
        *self.path.borrow_mut() = root.join(key);
        self.set_title();
        self.page
            .set_tooltip(&crate::fileops::display_path(root, key));
        self.reopen_language();
    }

    fn set_title(&self) {
        let name = crate::doc::file_name(&self.key()).to_string();
        match self.save.modified.get() {
            true => self.page.set_title(&format!("• {name}")),
            false => self.page.set_title(&name),
        }
    }

    /// The status bar's kind: "Notebook · Python".
    pub fn kind_label(&self) -> String {
        let language = self.model.borrow().language().to_string();
        let mut chars = language.chars();
        let language: String = chars
            .next()
            .map(|c| c.to_uppercase().chain(chars).collect())
            .unwrap_or_default();
        match language.is_empty() {
            true => "Notebook".to_string(),
            false => format!("Notebook · {language}"),
        }
    }

    /// The status bar's count: how many cells.
    pub fn facts(&self) -> String {
        match self.model.borrow().cells().len() {
            1 => "1 cell".to_string(),
            n => format!("{n} cells"),
        }
    }

    pub fn cell_count(&self) -> usize {
        self.model.borrow().cells().len()
    }

    pub fn history(&self) -> (bool, bool) {
        let history = self.history.borrow();
        let pending = self.pending.borrow().is_some();
        (history.can_undo() || pending, history.can_redo())
    }

    // ------------------------------------------------------------------------------ the views

    /// Bring the views in line with the model: one per cell in its order, of its kind, holding
    /// its source. A view the model still has is kept, its caret and its outputs with it.
    fn reconcile(self: &Rc<Self>) {
        let mut old = self.views.take();
        let mut views = Vec::new();
        let cells: Vec<(CellId, Kind, String)> = self
            .model
            .borrow()
            .cells()
            .iter()
            .map(|c| (c.id(), c.kind(), c.source().to_string()))
            .collect();
        for (id, kind, source) in cells {
            let view = match old.iter().position(|v| v.id == id && v.kind == kind) {
                Some(i) => old.swap_remove(i),
                None => self.build_view(id),
            };
            match view.editor() {
                Some(editor) if editor.text() != source => {
                    self.replay(|| editor.buffer.set_text(&source));
                }
                Some(_) => {}
                None => view.set_source(&source),
            }
            views.push(view);
        }
        for gone in old {
            self.column.remove(&gone.root);
            self.detach_cell(gone.id);
        }
        let mut before: gtk::Widget = self.head.clone().upcast();
        for view in &views {
            if view.root.parent().is_none() {
                self.column.append(&view.root);
            }
            self.column.reorder_child_after(&view.root, Some(&before));
            before = view.root.clone().upcast();
            view.set_presenting(self.presenting.get());
            view.set_selected(Some(view.id) == self.selected.get());
        }
        *self.views.borrow_mut() = views;
        if self.view_of(self.selected.get()).is_none() {
            self.selected.set(None);
        }
        if self.view_of(self.hovered.get()).is_none() {
            self.hovered.set(None);
        }
        if self.view_of(self.gap_cell.get()).is_none() {
            self.gap_cell.set(None);
        }
        self.place_soon();
    }

    fn view_of(&self, id: Option<CellId>) -> Option<Rc<CellView>> {
        let id = id?;
        self.views.borrow().iter().find(|v| v.id == id).cloned()
    }

    fn position_of(&self, id: CellId) -> Option<usize> {
        self.model.borrow().position(id)
    }

    /// The view of model cell `id`, wired to the tab; its editor comes when it is wanted
    /// ([`editor`](Self::editor)).
    fn build_view(self: &Rc<Self>, id: CellId) -> Rc<CellView> {
        let view = {
            let model = self.model.borrow();
            let at = model.position(id).expect("a cell of the model");
            Rc::new(CellView::new(&model.cells()[at]))
        };
        let weak = Rc::downgrade(self);
        let on = move |f: fn(&Rc<NotebookTab>, CellId)| {
            let weak = weak.clone();
            move || {
                if let Some(tab) = weak.upgrade() {
                    f(&tab, id);
                }
            }
        };
        let click = gtk::GestureClick::new();
        click.set_propagation_phase(gtk::PropagationPhase::Capture);
        let press = on(|tab, id| tab.press(id));
        click.connect_pressed(move |_, _, _, _| press());
        view.card.add_controller(click);
        // The buttons follow the pointer from cell to cell, and the add strip from gap to gap.
        let hover = |widget: &gtk::Widget, enter: fn(&Rc<NotebookTab>, CellId), leave| {
            let motion = gtk::EventControllerMotion::new();
            let (enter, leave) = (on(enter), on(leave));
            motion.connect_enter(move |_, _, _| enter());
            motion.connect_leave(move |_| leave());
            widget.add_controller(motion);
        };
        hover(
            view.card.upcast_ref(),
            |tab, id| tab.hover(id, true),
            |tab, id| tab.hover(id, false),
        );
        hover(
            view.gap.upcast_ref(),
            |tab, id| tab.hover_gap(id, true),
            |tab, id| tab.hover_gap(id, false),
        );
        view
    }

    /// Cell `id`'s view and its editor, built if need be: every way into a cell's text or its
    /// caret goes through here.
    fn editor(self: &Rc<Self>, id: CellId) -> Option<(Rc<CellView>, Editor)> {
        let view = self.view_of(Some(id))?;
        let editor = match view.editor() {
            Some(editor) => editor.clone(),
            None => self.build_editor(&view),
        };
        Some((view, editor))
    }

    /// Build `view`'s editor over the model's text and wire it to the tab: the one place a
    /// cell's editor is made.
    fn build_editor(self: &Rc<Self>, view: &Rc<CellView>) -> Editor {
        let id = view.id;
        let source = {
            let model = self.model.borrow();
            let at = model.position(id);
            at.map(|at| model.cells()[at].source().to_string())
                .unwrap_or_default()
        };
        let editor = view.build_editor(&source, self.language.as_ref(), self.presenting.get());
        crate::editor::refont(&editor.view);
        let weak = Rc::downgrade(self);
        let on = move |f: fn(&Rc<NotebookTab>, CellId)| {
            let weak = weak.clone();
            move || {
                if let Some(tab) = weak.upgrade() {
                    f(&tab, id);
                }
            }
        };
        let buffer = &editor.buffer;
        buffer.connect_insert_text({
            let weak = Rc::downgrade(self);
            move |_, at, text| {
                if let Some(tab) = weak.upgrade() {
                    tab.record(id, at.offset(), "", text);
                }
            }
        });
        buffer.connect_delete_range({
            let weak = Rc::downgrade(self);
            move |buffer, start, end| {
                if let Some(tab) = weak.upgrade() {
                    let removed = buffer.text(start, end, true);
                    tab.record(id, start.offset(), &removed, "");
                }
            }
        });
        buffer.connect_begin_user_action({
            let weak = Rc::downgrade(self);
            move |_| {
                if let Some(tab) = weak.upgrade() {
                    tab.acting.set(tab.acting.get() + 1);
                }
            }
        });
        buffer.connect_end_user_action({
            let weak = Rc::downgrade(self);
            move |_| {
                if let Some(tab) = weak.upgrade() {
                    tab.acting.set(tab.acting.get().saturating_sub(1));
                    if tab.acting.get() == 0 {
                        tab.flush_pending();
                    }
                }
            }
        });
        let edited = on(|tab, id| tab.edited(id));
        buffer.connect_changed(move |_| edited());
        let follow = on(|tab, id| {
            tab.follow_caret(id);
            tab.on_caret.emit(tab);
        });
        buffer.connect_mark_set(move |buffer, _, mark| {
            if *mark == buffer.get_insert() {
                follow();
            }
        });
        let focus = gtk::EventControllerFocus::new();
        let select = on(|tab, id| tab.select(id));
        focus.connect_enter(move |_| select());
        editor.view.add_controller(focus);
        // Down on a cell's last line goes on into the next cell, Up on its first back. Ahead of
        // the view, which keeps the caret where it is there.
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        keys.connect_key_pressed({
            let weak = Rc::downgrade(self);
            move |_, key, _, state| {
                let plain = (state & gtk::accelerator_get_default_mod_mask()).is_empty();
                let down = match key {
                    gdk::Key::Down | gdk::Key::KP_Down if plain => true,
                    gdk::Key::Up | gdk::Key::KP_Up if plain => false,
                    _ => return glib::Propagation::Proceed,
                };
                match weak.upgrade().is_some_and(|tab| tab.leave_cell(id, down)) {
                    true => glib::Propagation::Stop,
                    false => glib::Propagation::Proceed,
                }
            }
        });
        editor.view.add_controller(keys);
        self.attach_cell(view, &editor);
        editor
    }

    /// Run `f`, which sets a buffer from the model or the history, recording nothing.
    fn replay(&self, f: impl FnOnce()) {
        let was = self.replaying.replace(true);
        f();
        self.replaying.set(was);
    }

    // --------------------------------------------------------------------------- recording

    /// A change to cell `id`'s text at character `at`, about to be made.
    fn record(self: &Rc<Self>, id: CellId, at: i32, removed: &str, inserted: &str) {
        if self.replaying.get() {
            return;
        }
        let change = Change {
            at,
            removed: removed.to_string(),
            inserted: inserted.to_string(),
        };
        {
            let mut pending = self.pending.borrow_mut();
            match pending.as_mut() {
                Some((cell, changes)) if *cell == id => changes.push(change),
                _ => {
                    drop(pending);
                    self.flush_pending();
                    *self.pending.borrow_mut() = Some((id, vec![change]));
                }
            }
        }
        // A change made outside a user action is a step of its own.
        if self.acting.get() == 0 {
            self.flush_pending();
        }
    }

    /// The changes of the user action that ended become a step.
    fn flush_pending(self: &Rc<Self>) {
        let Some((cell, changes)) = self.pending.take() else {
            return;
        };
        self.history
            .borrow_mut()
            .push_text(cell, changes, Instant::now());
        self.on_history.emit(self);
    }

    /// Cell `id`'s text changed.
    fn edited(self: &Rc<Self>, id: CellId) {
        if self.replaying.get() {
            return;
        }
        {
            let mut stale = self.stale.borrow_mut();
            if !stale.contains(&id) {
                stale.push(id);
            }
        }
        if let Some(view) = self.view_of(Some(id))
            && let Some(editor) = view.editor()
            && view.kind == Kind::Markdown
        {
            crate::highlight::apply(&editor.buffer);
        }
        // Typing moves the caret without setting its mark.
        self.follow_caret(id);
        self.changed();
    }

    /// Bring the model's sources up to date with what was typed.
    fn sync_model(&self) {
        let stale = self.stale.take();
        if stale.is_empty() {
            return;
        }
        let mut model = self.model.borrow_mut();
        for id in stale {
            if let Some(text) = self.view_of(Some(id)).and_then(|v| v.text()) {
                model.apply(Edit::Source { id, text });
            }
        }
    }

    /// After any change, an undo included.
    fn changed(self: &Rc<Self>) {
        self.save.edits.set(self.save.edits.get() + 1);
        self.save.modified.set(true);
        self.set_title();
        self.on_history.emit(self);
        self.find_changed();
        self.autosave.call_once(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move || tab.on_autosave.emit(&tab)
        ));
        self.language_changed();
    }

    // -------------------------------------------------------------------------- the history

    pub fn undo(self: &Rc<Self>) {
        self.walk(true);
    }

    pub fn redo(self: &Rc<Self>) {
        self.walk(false);
    }

    fn walk(self: &Rc<Self>, back: bool) {
        if self.presenting.get() {
            return;
        }
        self.flush_pending();
        let step = {
            let mut history = self.history.borrow_mut();
            match back {
                true => history.take_undo(),
                false => history.take_redo(),
            }
        };
        let Some(step) = step else {
            return;
        };
        let step = match step {
            Step::Text { cell, changes } => {
                self.replay_text(cell, &changes, back);
                Step::Text { cell, changes }
            }
            Step::Cells(edits) => {
                self.sync_model();
                let inverse = self.model.borrow_mut().apply_all(edits);
                self.reconcile();
                self.redraw_outputs(&inverse);
                Step::Cells(inverse)
            }
        };
        {
            let mut history = self.history.borrow_mut();
            match back {
                true => history.undone(step),
                false => history.redone(step),
            }
        }
        self.changed();
    }

    /// Walk a text step back or forward in its cell's buffer, the caret where it ends.
    fn replay_text(self: &Rc<Self>, id: CellId, changes: &[Change], back: bool) {
        let Some((view, editor)) = self.editor(id) else {
            return;
        };
        let buffer = &editor.buffer;
        let len = |s: &str| s.chars().count() as i32;
        let mut caret = 0;
        self.replay(|| {
            let mut apply = |change: &Change| {
                let (gone, put) = match back {
                    true => (&change.inserted, &change.removed),
                    false => (&change.removed, &change.inserted),
                };
                let (mut start, mut end) = (
                    buffer.iter_at_offset(change.at),
                    buffer.iter_at_offset(change.at + len(gone)),
                );
                buffer.delete(&mut start, &mut end);
                buffer.insert(&mut buffer.iter_at_offset(change.at), put);
                caret = change.at + len(put);
            };
            match back {
                true => changes.iter().rev().for_each(&mut apply),
                false => changes.iter().for_each(&mut apply),
            }
        });
        buffer.place_cursor(&buffer.iter_at_offset(caret));
        {
            let mut stale = self.stale.borrow_mut();
            if !stale.contains(&id) {
                stale.push(id);
            }
        }
        if view.kind == Kind::Markdown {
            crate::highlight::apply(buffer);
        }
        self.select(id);
        editor.view.grab_focus();
    }

    // ---------------------------------------------------------------------- cell operations

    /// Make a cell operation through the model, on the history as one step.
    fn operate(self: &Rc<Self>, op: impl FnOnce(&mut Notebook) -> Option<Vec<Edit>>) -> bool {
        if self.presenting.get() {
            return false;
        }
        self.flush_pending();
        self.sync_model();
        let undo = op(&mut self.model.borrow_mut());
        let Some(undo) = undo.filter(|u| !u.is_empty()) else {
            return false;
        };
        self.history.borrow_mut().push_cells(undo);
        self.reconcile();
        self.changed();
        true
    }

    /// The selected cell's place, if a cell is selected.
    fn selected_at(&self) -> Option<usize> {
        self.position_of(self.selected.get()?)
    }

    /// A cell of `kind` below the selected one or above it; at the end with none selected.
    pub fn insert(self: &Rc<Self>, kind: Kind, below: bool) {
        let at = match (self.selected_at(), below) {
            (Some(at), true) => at + 1,
            (Some(at), false) => at,
            (None, _) => self.cell_count(),
        };
        self.insert_at(at, kind);
    }

    fn insert_at(self: &Rc<Self>, at: usize, kind: Kind) {
        let mut new = None;
        self.operate(|nb| {
            let cell = nb.new_cell(kind, "")?;
            new = Some(cell.id());
            Some(nb.apply_all(vec![Edit::Insert { at, cell }]))
        });
        if let Some(id) = new {
            self.focus_cell(id);
        }
    }

    pub fn delete(self: &Rc<Self>) {
        let Some(id) = self.selected.get() else {
            return;
        };
        let at = self.position_of(id).unwrap_or(0);
        if self.operate(|nb| Some(nb.apply_all(vec![Edit::Remove { id }]))) {
            let next = {
                let model = self.model.borrow();
                let cells = model.cells();
                cells.get(at).or(cells.last()).map(|c| c.id())
            };
            match next {
                Some(next) => self.focus_cell(next),
                None => self.selected.set(None),
            }
        }
    }

    pub fn move_cell(self: &Rc<Self>, down: bool) {
        let (Some(id), Some(at)) = (self.selected.get(), self.selected_at()) else {
            return;
        };
        let to = match down {
            true => at + 1,
            false => match at.checked_sub(1) {
                Some(to) => to,
                None => return,
            },
        };
        if self.operate(|nb| Some(nb.apply_all(vec![Edit::Move { id, to }]))) {
            self.focus_cell(id);
        }
    }

    pub fn convert(self: &Rc<Self>, kind: Kind) {
        let Some(id) = self.selected.get() else {
            return;
        };
        if self.operate(|nb| nb.convert(id, kind)) {
            self.focus_cell(id);
        }
    }

    /// Split the selected cell at its caret.
    pub fn split(self: &Rc<Self>) {
        let Some((view, editor)) = self.selected.get().and_then(|id| self.editor(id)) else {
            return;
        };
        let caret = editor
            .buffer
            .iter_at_mark(&editor.buffer.get_insert())
            .offset();
        let text = editor.text();
        let at = text
            .char_indices()
            .nth(caret as usize)
            .map_or(text.len(), |(at, _)| at);
        let id = view.id;
        let mut new = None;
        self.operate(|nb| {
            let undo = nb.split(id, at)?;
            new = nb
                .position(id)
                .and_then(|p| nb.cells().get(p + 1))
                .map(|c| c.id());
            Some(undo)
        });
        if let Some(new) = new {
            self.focus_cell(new);
            if let Some((_, editor)) = self.editor(new) {
                editor.buffer.place_cursor(&editor.buffer.start_iter());
            }
        }
    }

    /// Join the selected cell with the one below it.
    pub fn join(self: &Rc<Self>) {
        let Some(id) = self.selected.get() else {
            return;
        };
        if self.operate(|nb| nb.join(id)) {
            self.focus_cell(id);
        }
    }

    // -------------------------------------------------------------------- selection, caret

    fn select(self: &Rc<Self>, id: CellId) {
        if self.selected.get() == Some(id) {
            return;
        }
        if let Some(old) = self.view_of(self.selected.get()) {
            old.set_selected(false);
        }
        if let Some(view) = self.view_of(Some(id)) {
            view.set_selected(true);
        }
        self.selected.set(Some(id));
        self.complete_in(id);
        self.place_soon();
        self.history.borrow_mut().seal();
        self.on_select.emit(self);
    }

    /// A press in cell `id`, ahead of its editor's: the cell is selected, and the caret the editor
    /// puts down is not followed, being where the pointer is.
    fn press(self: &Rc<Self>, id: CellId) {
        self.select(id);
        self.pressing.set(true);
        let weak = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            if let Some(tab) = weak.upgrade() {
                tab.pressing.set(false);
            }
        });
        // A cell pressed before its editor was built: the editor takes the keyboard.
        if self.view_of(Some(id)).is_some_and(|v| v.editor().is_none()) {
            self.focus_cell(id);
        }
    }

    /// Select cell `id` and give its editor the keyboard.
    fn focus_cell(self: &Rc<Self>, id: CellId) {
        self.select(id);
        if let Some((_, editor)) = self.editor(id) {
            editor.view.grab_focus();
            self.follow_caret(id);
        }
    }

    /// An arrow pressed in cell `id`: on its last row (`down`) or its first, the caret goes on
    /// into the cell beside it.
    fn leave_cell(self: &Rc<Self>, id: CellId, down: bool) -> bool {
        // The arrows walk the completion popup's rows while it is up.
        if self.popup_shown(id) {
            return false;
        }
        let Some((_, editor)) = self.editor(id) else {
            return false;
        };
        let buffer = &editor.buffer;
        let at = buffer.iter_at_mark(&buffer.get_insert());
        let edge = match down {
            true => buffer.end_iter(),
            false => buffer.start_iter(),
        };
        let row = |iter: &gtk::TextIter| editor.view.iter_location(iter).y();
        row(&at) == row(&edge) && self.step_cell(id, down)
    }

    /// Past the end of cell `id`, into the next cell's first line, or the previous cell's last.
    fn step_cell(self: &Rc<Self>, id: CellId, down: bool) -> bool {
        let views = self.views.borrow().clone();
        let Some(at) = views.iter().position(|v| v.id == id) else {
            return false;
        };
        let next = match down {
            true => views.get(at + 1),
            false => at.checked_sub(1).and_then(|at| views.get(at)),
        };
        let Some((_, editor)) = next.and_then(|next| self.editor(next.id)) else {
            return false;
        };
        let buffer = &editor.buffer;
        let land = match down {
            true => buffer.start_iter(),
            false => buffer.end_iter(),
        };
        buffer.place_cursor(&land);
        if let Some(next) = next {
            self.focus_cell(next.id);
        }
        true
    }

    /// Keep cell `id`'s caret in sight: the cells do not scroll themselves, the notebook does.
    fn follow_caret(self: &Rc<Self>, id: CellId) {
        if self.pressing.get() {
            return;
        }
        let weak = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            let Some(tab) = weak.upgrade() else { return };
            let Some(editor) = tab.view_of(Some(id)).and_then(|v| v.editor().cloned()) else {
                return;
            };
            if editor.view.has_focus() {
                tab.scroll_to_caret(&editor, None);
            }
        });
    }

    /// Scroll so `editor`'s caret is `align` of the way down the view, or, with none, by as little
    /// as keeps it a margin inside.
    fn scroll_to_caret(&self, editor: &Editor, align: Option<f64>) {
        let Some((top, bottom)) = self.caret_at(editor) else {
            return;
        };
        let adjustment = self.scroller.vadjustment();
        let (value, page, margin) = (adjustment.value(), adjustment.page_size(), 24.0);
        match align {
            Some(align) => adjustment.set_value(top - align * page),
            None if top < value => adjustment.set_value(top - margin),
            None if bottom > value + page => adjustment.set_value(bottom - page + margin),
            None => {}
        }
    }

    /// Bring cell `id`'s caret into view as [`scroll_to_caret`](Self::scroll_to_caret) does, once
    /// its editor is laid out: one built just now has no place to measure yet, and the keyboard
    /// may be elsewhere (the find bar), which [`follow_caret`](Self::follow_caret) waits for.
    fn show(&self, id: CellId, align: Option<f64>) {
        self.reveal.set(Some((id, align)));
        self.scroller.queue_allocate();
    }

    /// The scroll [`show`](Self::show) asked for, in the first layout that has its editor in place.
    fn show_pending(&self) {
        let Some((id, align)) = self.reveal.get() else {
            return;
        };
        match self.view_of(Some(id)).and_then(|v| v.editor().cloned()) {
            Some(editor) if editor.view.height() == 0 => {}
            Some(editor) => {
                self.reveal.set(None);
                self.scroll_to_caret(&editor, align);
            }
            None => self.reveal.set(None),
        }
    }

    /// The caret at character `at` of cell `id`, the keyboard in it, a quarter down the view:
    /// where Go to Cell and an Outline row land.
    pub fn goto(self: &Rc<Self>, id: CellId, at: i32) {
        self.focus_cell(id);
        if let Some((_, editor)) = self.editor(id) {
            editor
                .buffer
                .place_cursor(&editor.buffer.iter_at_offset(at));
        }
        self.show(id, Some(0.25));
    }

    /// The top and bottom of `view`'s caret in the scrolled content, where the scroll's value is
    /// measured, so a scroll not yet laid out does not move it.
    fn caret_at(&self, editor: &Editor) -> Option<(f64, f64)> {
        let buffer = &editor.buffer;
        let rect = editor
            .view
            .iter_location(&buffer.iter_at_mark(&buffer.get_insert()));
        let (x, y) =
            editor
                .view
                .buffer_to_window_coords(gtk::TextWindowType::Widget, rect.x(), rect.y());
        let point = editor
            .view
            .compute_point(&self.content()?, &graphene::Point::new(x as f32, y as f32))?;
        let top = f64::from(point.y());
        Some((top, top + f64::from(rect.height())))
    }

    /// The scrolled content, in whose coordinates the scroll's value is measured: the scroller's
    /// child is the viewport, and the content is the viewport's.
    fn content(&self) -> Option<gtk::Widget> {
        self.scroller
            .child()
            .and_downcast::<gtk::Viewport>()?
            .child()
    }

    // ---------------------------------------------------- building editors, keeping the view

    /// After each layout of the window, what shows stays put as cells change height
    /// ([`keep_anchor`](Self::keep_anchor)) and editors are built as their cells near the view
    /// ([`build_near`](Self::build_near)).
    fn wire_building(self: &Rc<Self>) {
        let hook = |tab: &Rc<NotebookTab>| {
            let Some(clock) = tab.scroller.frame_clock() else {
                return;
            };
            // After the window's own handler, whose allocations it reads.
            let weak = Rc::downgrade(tab);
            let id = clock.connect_local("layout", true, move |values| {
                let clock = values[0].get::<gdk::FrameClock>().ok()?;
                weak.upgrade()?.laid_out(&clock);
                None
            });
            tab.layout_hook.replace(Some((clock, id)));
        };
        // The first tab of a pane is realised as it is added.
        if self.scroller.is_realized() {
            hook(self);
        }
        self.scroller.connect_realize(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move |_| hook(&tab)
        ));
        self.scroller.connect_unrealize(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move |_| {
                if let Some((clock, id)) = tab.layout_hook.take() {
                    clock.disconnect(id);
                }
            }
        ));
    }

    /// The window was laid out. What it lays out again before the frame is drawn, after a scroll
    /// or an editor built here, comes back through here.
    fn laid_out(self: &Rc<Self>, clock: &gdk::FrameClock) {
        if !self.scroller.is_mapped() {
            return;
        }
        self.keep_anchor();
        self.show_pending();
        self.build_near(clock);
        self.anchor.set(self.top_cell());
    }

    /// Build the editors of the cells within a screen of what shows, once a frame after the
    /// window is laid out, so the frame lays them out again and draws no cell scrolled to without
    /// its editor.
    fn build_near(self: &Rc<Self>, clock: &gdk::FrameClock) {
        let frame = clock.frame_counter();
        if self.built_at.replace(frame) == frame {
            return;
        }
        for view in self.near() {
            if view.editor().is_none() {
                self.build_editor(&view);
            }
        }
    }

    /// The views of the cells within a screen above and below what shows, in order.
    fn near(&self) -> Vec<Rc<CellView>> {
        let adjustment = self.scroller.vadjustment();
        let page = adjustment.page_size();
        let Some(content) = self.content().filter(|_| page > 0.0) else {
            return Vec::new();
        };
        let (top, bottom) = (adjustment.value() - page, adjustment.value() + 2.0 * page);
        let mut near = Vec::new();
        for view in self.views.borrow().iter() {
            let Some(bounds) = view.root.compute_bounds(&content) else {
                continue;
            };
            if f64::from(bounds.y()) > bottom {
                break;
            }
            if f64::from(bounds.y() + bounds.height()) >= top {
                near.push(view.clone());
            }
        }
        near
    }

    /// Keep what shows where it is when the cells above it change height, an editor built in its
    /// placeholder's place or a picture decoded: the first cell in view moved in the content
    /// since the last layout, so the view moves as far. Not at the top, where what is added
    /// above shows.
    fn keep_anchor(&self) {
        let adjustment = self.scroller.vadjustment();
        let value = adjustment.value();
        let (Some((id, was)), Some(content)) = (self.anchor.get(), self.content()) else {
            return;
        };
        let now = self
            .view_of(Some(id))
            .and_then(|v| v.root.compute_bounds(&content));
        if let Some(now) = now.map(|b| f64::from(b.y()))
            && now != was
            && value > 0.0
        {
            adjustment.set_value(value + now - was);
        }
    }

    /// The first cell reaching into the view, and its top in the content.
    fn top_cell(&self) -> Option<(CellId, f64)> {
        let value = self.scroller.vadjustment().value();
        let content = self.content()?;
        self.views.borrow().iter().find_map(|v| {
            let bounds = v.root.compute_bounds(&content)?;
            let top = f64::from(bounds.y());
            (top + f64::from(bounds.height()) > value).then_some((v.id, top))
        })
    }

    // ------------------------------------------------------------ the buttons and the strips

    /// The shared buttons act on the cell they are over, and the gaps' add strip after the cell
    /// it is under, selecting that cell first.
    fn wire_tools(self: &Rc<Self>, add_code: &gtk::Button, add_markdown: &gtk::Button) {
        let act = |button: &gtk::Button, f: fn(&Rc<NotebookTab>)| {
            let weak = Rc::downgrade(self);
            button.connect_clicked(move |_| {
                if let Some(tab) = weak.upgrade()
                    && let Some(id) = tab.tools.cell.get()
                {
                    tab.select(id);
                    f(&tab);
                }
            });
        };
        // Run Cell is the window's, whose vault the kernel runs where.
        act(&self.tools.run, |tab| {
            let _ = tab.scroller.activate_action("win.notebook-run-cell", None);
        });
        act(&self.tools.up, |tab| tab.move_cell(false));
        act(&self.tools.down, |tab| tab.move_cell(true));
        act(&self.tools.delete, |tab| tab.delete());
        self.tools.more.connect_active_notify(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move |more| {
                if more.is_active()
                    && let Some(id) = tab.tools.cell.get()
                {
                    tab.select(id);
                }
            }
        ));
        let add = |button: &gtk::Button, kind: Kind| {
            let weak = Rc::downgrade(self);
            button.connect_clicked(move |_| {
                if let Some(tab) = weak.upgrade()
                    && let Some(id) = tab.gap_cell.get()
                {
                    tab.select(id);
                    tab.insert(kind, true);
                }
            });
        };
        add(add_code, Kind::Code);
        add(add_markdown, Kind::Markdown);
    }

    /// The pointer came onto cell `id`'s card (`on`) or left it: the buttons go with it.
    fn hover(self: &Rc<Self>, id: CellId, on: bool) {
        match on {
            true => self.hovered.set(Some(id)),
            false if self.hovered.get() == Some(id) => self.hovered.set(None),
            false => return,
        }
        self.place_soon();
    }

    /// The pointer came into cell `id`'s gap (`on`) or left it: the add strip goes with it.
    fn hover_gap(self: &Rc<Self>, id: CellId, on: bool) {
        match on {
            true => self.gap_cell.set(Some(id)),
            false if self.gap_cell.get() == Some(id) => self.gap_cell.set(None),
            false => return,
        }
        self.place_soon();
    }

    /// Move the buttons and the add strip where the pointer and the selection say, once the
    /// event at hand is over: a placeholder leaving the tree under the pointer sends crossing
    /// events, and moving widgets about from inside those crashed GTK.
    fn place_soon(self: &Rc<Self>) {
        if self.placing.replace(true) {
            return;
        }
        let weak = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            if let Some(tab) = weak.upgrade() {
                tab.placing.set(false);
                tab.place();
            }
        });
    }

    /// The buttons over the cell under the pointer, else the selected one, and the add strip in
    /// the gap under the pointer; neither while presented.
    fn place(&self) {
        let shown = |id: Option<CellId>| match self.presenting.get() {
            true => None,
            false => self.view_of(id),
        };
        let tools = shown(self.hovered.get().or(self.selected.get()));
        self.tools.place(tools.as_deref());
        let gap = shown(self.gap_cell.get());
        let target = gap.as_ref().map(|v| v.gap.clone().upcast::<gtk::Widget>());
        if self.gap.parent() != target {
            if let Some(old) = self.gap.parent().and_downcast::<gtk::Overlay>() {
                old.remove_overlay(&self.gap);
            }
            if let Some(view) = gap {
                view.gap.add_overlay(&self.gap);
            }
        }
    }

    // ------------------------------------------------------------------------- window state

    /// Read only, the buttons and the inserters away, while presented.
    pub fn set_presenting(self: &Rc<Self>, on: bool) {
        self.presenting.set(on);
        self.head.set_visible(!on);
        for view in self.views.borrow().iter() {
            view.set_presenting(on);
        }
        self.place_soon();
    }

    /// The theme, or Preferences' Images switch, changed: the editors' schemes, the markdown's
    /// colours, the pictures recoloured.
    pub fn restyle(&self) {
        for view in self.views.borrow().iter() {
            if let Some(editor) = view.editor() {
                crate::editor::sync_scheme(&editor.buffer);
                if view.kind == Kind::Markdown {
                    crate::highlight::restyle(&editor.buffer, &editor.view);
                }
            }
            view.outputs.restyle();
        }
        self.restyle_language();
    }

    /// The editor font and the document zoom, one rule for every cell.
    pub fn set_font(&self, font: Option<&str>, zoom: f64) {
        let Some(display) = gdk::Display::default() else {
            return;
        };
        if let Some(old) = self.font.borrow_mut().take() {
            gtk::style_context_remove_provider_for_display(&display, &old);
        }
        let name = self.column.widget_name();
        let doc = font
            .filter(|f| !f.is_empty())
            .map_or_else(crate::editor::default_font, str::to_string);
        let mono = crate::platform::monospace_font();
        let css = [
            crate::editor::font_css(&doc, &format!("#{name} .accent-doc"), zoom),
            crate::editor::font_css(&mono, &format!("#{name} .accent-code"), zoom),
            crate::editor::font_css(&mono, &format!("#{name} .notebook-output"), zoom),
        ]
        .join(" ");
        let provider = gtk::CssProvider::new();
        provider.load_from_string(&css);
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        *self.font.borrow_mut() = Some(provider);
        for editor in self.views.borrow().iter().filter_map(|v| v.editor()) {
            crate::editor::refont(&editor.view);
        }
    }

    /// What the drill counts: code cells, markdown cells, stored outputs, and how they show.
    #[cfg(feature = "bench")]
    pub fn counts(&self) -> (usize, usize, usize, Counts) {
        let mut sum = Counts::default();
        let (mut code, mut markdown) = (0, 0);
        let outputs = self
            .model
            .borrow()
            .cells()
            .iter()
            .map(|c| c.outputs().len())
            .sum();
        for view in self.views.borrow().iter() {
            match view.kind {
                Kind::Code => code += 1,
                Kind::Markdown => markdown += 1,
                Kind::Raw => {}
            }
            let c = view.outputs.counts();
            sum.pictures += c.pictures;
            sum.recoloured += c.recoloured;
            sum.tables += c.tables;
            sum.latex += c.latex;
            sum.typeset += c.typeset;
            sum.ansi += c.ansi;
            sum.placeholders += c.placeholders;
        }
        (code, markdown, outputs, sum)
    }

    /// What the file should hold now, the cells' editors read into the model first.
    pub fn text(&self) -> String {
        self.sync_model();
        self.model.borrow().to_text()
    }

    /// A write landed: the tab is clean at `etag`.
    pub fn mark_clean(&self, etag: Etag) {
        self.save.etag.set(Some(etag));
        self.save.modified.set(false);
        self.set_title();
    }

    /// The edits are given up on (a tab closing without saving them).
    pub fn discard(&self) {
        self.save.modified.set(false);
        self.set_title();
    }

    /// Read the file again into a clean tab: the history starts over, as a note's does.
    pub fn reload(self: &Rc<Self>, notebook: Notebook, etag: Etag, digest: Digest) {
        *self.model.borrow_mut() = notebook;
        self.stale.take();
        self.pending.take();
        *self.history.borrow_mut() = History::default();
        // Every cell is new to the model read, so every view is built again.
        for view in self.views.take() {
            self.column.remove(&view.root);
            self.detach_cell(view.id);
        }
        self.selected.set(None);
        self.reconcile();
        self.save.etag.set(Some(etag));
        self.save.digest.set(Some(digest));
        self.save.modified.set(false);
        self.save.disk_changed.set(false);
        self.banner.set_revealed(false);
        self.set_title();
        self.on_history.emit(self);
        self.find_changed();
        self.language_changed();
    }

    /// The file moved under edits nobody has saved: the banner holds the question.
    pub fn show_changed(&self) {
        self.save.disk_changed.set(true);
        self.banner.set_revealed(true);
    }

    pub fn clear_changed(&self) {
        self.save.disk_changed.set(false);
        self.banner.set_revealed(false);
    }

    /// Watch the file behind this tab and call `f` when someone else writes it.
    pub fn watch_file(self: &Rc<Self>, f: impl Fn(&Rc<NotebookTab>) + 'static) {
        *self.monitor.borrow_mut() = crate::editor::watch_file(
            &self.path(),
            glib::clone!(
                #[weak(rename_to = tab)]
                self,
                move || f(&tab)
            ),
        );
    }

    /// The notebook's own keys ([`own_key`]), as a diagram's canvas's are, heard ahead of the
    /// cells' editors.
    fn wire_keys(self: &Rc<Self>, host: &gtk::Box) {
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        keys.connect_key_pressed(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, key, _, state| {
                let Some(action) = own_key(key, state) else {
                    return glib::Propagation::Proceed;
                };
                let _ = tab.scroller.activate_action(action, None);
                glib::Propagation::Stop
            }
        ));
        host.add_controller(keys);
    }

    pub fn connect_history(&self, f: impl Fn(&Rc<NotebookTab>) + 'static) {
        self.on_history.set(Rc::new(f));
    }

    pub fn connect_autosave(&self, f: impl Fn(&Rc<NotebookTab>) + 'static) {
        self.on_autosave.set(Rc::new(f));
    }

    pub fn connect_banner(&self, f: impl Fn(&Rc<NotebookTab>) + 'static) {
        self.on_banner.set(Rc::new(f));
    }

    pub fn connect_select(&self, f: impl Fn(&Rc<NotebookTab>) + 'static) {
        self.on_select.set(Rc::new(f));
    }

    pub fn connect_zoom(&self, f: impl Fn(bool) + 'static) {
        self.on_zoom.set(Rc::new(f));
    }
}

/// The command a key pressed over the notebook is the chord of (DESIGN.md, Keyboard › Notebook).
/// Undo and Redo are its one history's, the cells' buffers keeping none, and the cell chords take
/// GtkTextView's `Ctrl+Shift+Up` / `Down`, which extend the selection by a paragraph, in a cell.
fn own_key(key: gdk::Key, state: gdk::ModifierType) -> Option<&'static str> {
    use gdk::ModifierType as M;
    let mods = state & (M::CONTROL_MASK | M::SHIFT_MASK | M::ALT_MASK | M::SUPER_MASK);
    let (ctrl, shift, alt) = (M::CONTROL_MASK, M::SHIFT_MASK, M::ALT_MASK);
    Some(match key {
        gdk::Key::z | gdk::Key::Z if mods == ctrl => "win.notebook-undo",
        gdk::Key::z | gdk::Key::Z if mods == ctrl | shift => "win.notebook-redo",
        gdk::Key::y | gdk::Key::Y if mods == ctrl => "win.notebook-redo",
        gdk::Key::Return | gdk::Key::KP_Enter if mods == alt => "win.notebook-insert-below",
        gdk::Key::Return | gdk::Key::KP_Enter if mods == alt | shift => "win.notebook-insert-above",
        gdk::Key::Return | gdk::Key::KP_Enter if mods == shift => "win.notebook-run-advance",
        gdk::Key::minus | gdk::Key::underscore if mods == ctrl | shift => "win.notebook-split",
        gdk::Key::Up | gdk::Key::KP_Up if mods == ctrl | shift => "win.notebook-move-up",
        gdk::Key::Down | gdk::Key::KP_Down if mods == ctrl | shift => "win.notebook-move-down",
        _ => return None,
    })
}

impl Saves for NotebookTab {
    fn save_state(&self) -> &SaveState {
        &self.save
    }

    fn key(&self) -> String {
        NotebookTab::key(self)
    }

    fn path(&self) -> PathBuf {
        NotebookTab::path(self)
    }

    fn for_disk(&self) -> String {
        self.text()
    }
}

/// What `ACCENT_BENCH_NOTEBOOK` drives and reads.
#[cfg(feature = "bench")]
impl NotebookTab {
    /// Type `text` at the end of cell `n`, a key at a time.
    pub fn bench_type(self: &Rc<Self>, n: usize, text: &str) {
        let Some(id) = self.views.borrow().get(n).map(|v| v.id) else {
            return;
        };
        self.focus_cell(id);
        let Some((_, editor)) = self.editor(id) else {
            return;
        };
        let buffer = &editor.buffer;
        for c in text.chars() {
            buffer.begin_user_action();
            buffer.insert(&mut buffer.end_iter(), &c.to_string());
            buffer.end_user_action();
        }
    }

    /// Select cell `n` and give it the keyboard.
    pub fn bench_select(self: &Rc<Self>, n: usize) {
        let id = self.views.borrow().get(n).map(|v| v.id);
        if let Some(id) = id {
            self.focus_cell(id);
        }
    }

    pub fn bench_source(&self, n: usize) -> String {
        let view = self.views.borrow().get(n).cloned();
        view.map(|v| self.source_of(&v)).unwrap_or_default()
    }

    /// The text of `view`'s cell: its editor's, or the model's while it has none.
    fn source_of(&self, view: &CellView) -> String {
        view.text().unwrap_or_else(|| {
            let model = self.model.borrow();
            let at = model.position(view.id);
            at.map(|at| model.cells()[at].source().to_string())
                .unwrap_or_default()
        })
    }

    /// How many cells have their editor built.
    pub fn bench_built(&self) -> usize {
        self.views
            .borrow()
            .iter()
            .filter(|v| v.editor().is_some())
            .count()
    }

    /// Down on cell `n`'s last line, or Up on its first, as the arrow key's controller sees it;
    /// answers where the selection is then.
    pub fn bench_step(self: &Rc<Self>, n: usize, down: bool) -> Option<usize> {
        let id = self.views.borrow().get(n)?.id;
        self.focus_cell(id);
        let (_, editor) = self.editor(id)?;
        let buffer = &editor.buffer;
        buffer.place_cursor(&match down {
            true => buffer.end_iter(),
            false => buffer.start_iter(),
        });
        self.leave_cell(id, down);
        self.selected_at()
    }

    /// Cell `n`'s first line scrolled half under the view's bottom edge and pressed there, as a
    /// click goes: the press, the keyboard, the caret put down. Answers the scroll before it.
    pub fn bench_click(self: &Rc<Self>, n: usize) -> Option<f64> {
        let id = self.views.borrow().get(n)?.id;
        self.press(id);
        let (_, editor) = self.editor(id)?;
        let buffer = &editor.buffer;
        buffer.place_cursor(&buffer.start_iter());
        let (top, bottom) = self.caret_at(&editor)?;
        let adjustment = self.scroller.vadjustment();
        adjustment.set_value((top + bottom) / 2.0 - adjustment.page_size());
        let before = adjustment.value();
        editor.view.grab_focus();
        Some(before)
    }

    /// Down on cell `n`'s last line with that line just inside the view's bottom edge, so the
    /// caret goes on into the next cell out of sight.
    pub fn bench_down_at_edge(self: &Rc<Self>, n: usize) {
        let Some(id) = self.views.borrow().get(n).map(|v| v.id) else {
            return;
        };
        self.focus_cell(id);
        let Some((_, editor)) = self.editor(id) else {
            return;
        };
        editor.buffer.place_cursor(&editor.buffer.end_iter());
        if let Some((_, bottom)) = self.caret_at(&editor) {
            let adjustment = self.scroller.vadjustment();
            adjustment.set_value(bottom - adjustment.page_size());
        }
        self.leave_cell(id, true);
    }

    /// How far above the view's bottom edge the selected cell's caret is.
    pub fn bench_caret_above_edge(&self) -> Option<f64> {
        let editor = self.view_of(self.selected.get())?.editor()?.clone();
        let (_, bottom) = self.caret_at(&editor)?;
        let adjustment = self.scroller.vadjustment();
        Some(adjustment.value() + adjustment.page_size() - bottom)
    }

    pub fn bench_scrolled(&self) -> f64 {
        self.scroller.vadjustment().value()
    }

    /// Scroll by `pages` of the view, up for a negative count; answers how far it went up.
    pub fn bench_scroll_by(&self, pages: f64) -> f64 {
        let adjustment = self.scroller.vadjustment();
        let before = adjustment.value();
        adjustment.set_value(before + pages * adjustment.page_size());
        before - adjustment.value()
    }

    /// The first cell reaching into the view, and where its top is in the view.
    pub fn bench_top_cell(&self) -> Option<(usize, f64)> {
        let value = self.scroller.vadjustment().value();
        let content = self.content()?;
        let views = self.views.borrow();
        views.iter().enumerate().find_map(|(n, v)| {
            let bounds = v.root.compute_bounds(&content)?;
            let top = f64::from(bounds.y());
            (top + f64::from(bounds.height()) > value).then_some((n, top - value))
        })
    }

    /// Where cell `n`'s top is in the view.
    pub fn bench_cell_y(&self, n: usize) -> Option<f64> {
        let bounds = self
            .views
            .borrow()
            .get(n)?
            .root
            .compute_bounds(&self.content()?)?;
        Some(f64::from(bounds.y()) - self.scroller.vadjustment().value())
    }

    /// Scroll to the `n`th screen of the notebook.
    pub fn bench_scroll(&self, n: usize) {
        let adjustment = self.scroller.vadjustment();
        adjustment.set_value(n as f64 * adjustment.page_size() * 0.9);
    }

    /// Whether no cell can be typed into and no cell button or add strip is out.
    pub fn bench_read_only(&self) -> bool {
        let views = self.views.borrow();
        views
            .iter()
            .all(|v| v.editor().is_none_or(|e| !e.view.is_editable()))
            && self.tools.bar.parent().is_none()
            && self.gap.parent().is_none()
            && !self.head.is_visible()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gdk::ModifierType as M;

    #[test]
    fn the_notebook_keys_name_their_commands() {
        let (ctrl, shift, alt) = (M::CONTROL_MASK, M::SHIFT_MASK, M::ALT_MASK);
        let cases = [
            (gdk::Key::z, ctrl, Some("win.notebook-undo")),
            (gdk::Key::Z, ctrl | shift, Some("win.notebook-redo")),
            (gdk::Key::y, ctrl, Some("win.notebook-redo")),
            (gdk::Key::Return, alt, Some("win.notebook-insert-below")),
            (gdk::Key::KP_Enter, alt, Some("win.notebook-insert-below")),
            (
                gdk::Key::Return,
                alt | shift,
                Some("win.notebook-insert-above"),
            ),
            (gdk::Key::Return, shift, Some("win.notebook-run-advance")),
            (gdk::Key::KP_Enter, shift, Some("win.notebook-run-advance")),
            // Shift turns the minus key into an underscore on most layouts.
            (
                gdk::Key::underscore,
                ctrl | shift,
                Some("win.notebook-split"),
            ),
            (gdk::Key::minus, ctrl | shift, Some("win.notebook-split")),
            (gdk::Key::Up, ctrl | shift, Some("win.notebook-move-up")),
            (gdk::Key::Down, ctrl | shift, Some("win.notebook-move-down")),
            // Caps Lock changes nothing.
            (gdk::Key::Z, ctrl | M::LOCK_MASK, Some("win.notebook-undo")),
            (
                gdk::Key::Up,
                ctrl | shift | M::LOCK_MASK,
                Some("win.notebook-move-up"),
            ),
            // Chords the editor and the window keep.
            (gdk::Key::Return, M::empty(), None),
            (gdk::Key::Return, ctrl, None),
            (gdk::Key::Return, ctrl | shift, None),
            (gdk::Key::minus, ctrl, None),
            (gdk::Key::Up, ctrl, None),
            (gdk::Key::Up, alt, None),
            (gdk::Key::Up, alt | shift, None),
            (gdk::Key::z, ctrl | alt, None),
        ];
        for (key, state, action) in cases {
            assert_eq!(own_key(key, state), action, "{key:?} {state:?}");
        }
    }
}
