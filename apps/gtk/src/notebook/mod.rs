//! Notebook tabs: a Jupyter or marimo notebook as a column of cells, VS Code's notebook editor
//! without its kernel. `accent_core::notebook` reads and writes the file; [`NotebookTab`] keeps a
//! view per cell in step with the model, records every change on the notebook's one history
//! (`history`), and saves through the same etag gate as a note (`window`).

mod cell;
mod history;
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
use cell::CellView;
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
    presenting: Cell<bool>,
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
        model: RefCell::new(notebook),
        views: RefCell::default(),
        language,
        selected: Cell::new(None),
        history: RefCell::default(),
        pending: RefCell::default(),
        acting: Cell::new(0),
        stale: RefCell::default(),
        replaying: Cell::new(false),
        presenting: Cell::new(false),
        font: RefCell::default(),
        save: SaveState::at(etag, digest),
        autosave: Debounce::new(crate::editor::AUTOSAVE),
        monitor: RefCell::default(),
        on_history: Hook::default(),
        on_autosave: Hook::default(),
        on_banner: Hook::default(),
        on_select: Hook::default(),
        on_zoom: Hook::default(),
    });
    tab.reconcile();
    tab.scroller.set_child(Some(&clamp));
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
    pub fn key_target(&self) -> gtk::Widget {
        let views = self.views.borrow();
        let selected = self.selected.get();
        views
            .iter()
            .find(|v| Some(v.id) == selected)
            .or(views.first())
            .map_or_else(
                || self.scroller.clone().upcast(),
                |v| v.view.clone().upcast(),
            )
    }

    /// A rename landed: follow the file.
    pub fn retarget(&self, root: &Path, key: &str) {
        *self.key.borrow_mut() = key.to_string();
        *self.path.borrow_mut() = root.join(key);
        self.set_title();
        self.page
            .set_tooltip(&crate::fileops::display_path(root, key));
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
            if view.text() != source {
                self.replay(|| view.buffer.set_text(&source));
            }
            views.push(view);
        }
        for gone in old {
            self.column.remove(&gone.root);
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
    }

    fn view_of(&self, id: Option<CellId>) -> Option<Rc<CellView>> {
        let id = id?;
        self.views.borrow().iter().find(|v| v.id == id).cloned()
    }

    fn position_of(&self, id: CellId) -> Option<usize> {
        self.model.borrow().position(id)
    }

    /// The view of model cell `id`, wired to the tab.
    fn build_view(self: &Rc<Self>, id: CellId) -> Rc<CellView> {
        let view = {
            let model = self.model.borrow();
            let at = model.position(id).expect("a cell of the model");
            Rc::new(CellView::new(&model.cells()[at], self.language.as_ref()))
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
        let buffer = &view.buffer;
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
        let follow = on(|tab, id| tab.follow_caret(id));
        buffer.connect_mark_set(move |buffer, _, mark| {
            if *mark == buffer.get_insert() {
                follow();
            }
        });
        let focus = gtk::EventControllerFocus::new();
        let select = on(|tab, id| tab.select(id));
        focus.connect_enter(move |_| select());
        view.view.add_controller(focus);
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
        view.view.add_controller(keys);
        let click = gtk::GestureClick::new();
        click.set_propagation_phase(gtk::PropagationPhase::Capture);
        let select = on(|tab, id| tab.select(id));
        click.connect_pressed(move |_, _, _, _| select());
        view.card.add_controller(click);
        let act = |button: &gtk::Button, f: fn(&Rc<NotebookTab>)| {
            let weak = Rc::downgrade(self);
            button.connect_clicked(move |_| {
                if let Some(tab) = weak.upgrade() {
                    tab.select(id);
                    f(&tab);
                }
            });
        };
        act(&view.up, |tab| tab.move_cell(false));
        act(&view.down, |tab| tab.move_cell(true));
        act(&view.delete, |tab| tab.delete());
        act(&view.add_code, |tab| tab.insert(Kind::Code, true));
        act(&view.add_markdown, |tab| tab.insert(Kind::Markdown, true));
        view.more.connect_active_notify({
            let select = on(|tab, id| tab.select(id));
            move |more| {
                if more.is_active() {
                    select();
                }
            }
        });
        view
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
            && view.kind == Kind::Markdown
        {
            crate::highlight::apply(&view.buffer);
        }
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
            if let Some(view) = self.views.borrow().iter().find(|v| v.id == id) {
                let text = view.text();
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
        self.autosave.call_once(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move || tab.on_autosave.emit(&tab)
        ));
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
        let Some(view) = self.view_of(Some(id)) else {
            return;
        };
        let buffer = &view.buffer;
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
        view.view.grab_focus();
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
        let Some(view) = self.view_of(self.selected.get()) else {
            return;
        };
        let caret = view.buffer.iter_at_mark(&view.buffer.get_insert()).offset();
        let text = view.text();
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
            if let Some(view) = self.view_of(Some(new)) {
                view.buffer.place_cursor(&view.buffer.start_iter());
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
        self.history.borrow_mut().seal();
        self.on_select.emit(self);
    }

    /// Select cell `id` and give its editor the keyboard.
    fn focus_cell(self: &Rc<Self>, id: CellId) {
        self.select(id);
        if let Some(view) = self.view_of(Some(id)) {
            view.view.grab_focus();
            self.follow_caret(id);
        }
    }

    /// An arrow pressed in cell `id`: on its last row (`down`) or its first, the caret goes on
    /// into the cell beside it.
    fn leave_cell(self: &Rc<Self>, id: CellId, down: bool) -> bool {
        let Some(view) = self.view_of(Some(id)) else {
            return false;
        };
        let buffer = &view.buffer;
        let at = buffer.iter_at_mark(&buffer.get_insert());
        let edge = match down {
            true => buffer.end_iter(),
            false => buffer.start_iter(),
        };
        let row = |iter: &gtk::TextIter| view.view.iter_location(iter).y();
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
        let Some(next) = next else {
            return false;
        };
        let buffer = &next.buffer;
        let land = match down {
            true => buffer.start_iter(),
            false => buffer.end_iter(),
        };
        buffer.place_cursor(&land);
        self.focus_cell(next.id);
        true
    }

    /// Keep cell `id`'s caret in sight: the cells do not scroll themselves, the notebook does.
    fn follow_caret(self: &Rc<Self>, id: CellId) {
        let weak = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            let Some(tab) = weak.upgrade() else { return };
            let Some(view) = tab.view_of(Some(id)) else {
                return;
            };
            if !view.view.has_focus() {
                return;
            }
            let buffer = &view.buffer;
            let rect = view
                .view
                .iter_location(&buffer.iter_at_mark(&buffer.get_insert()));
            let (x, y) =
                view.view
                    .buffer_to_window_coords(gtk::TextWindowType::Widget, rect.x(), rect.y());
            let Some(content) = tab.scroller.child() else {
                return;
            };
            let Some(point) = view
                .view
                .compute_point(&content, &graphene::Point::new(x as f32, y as f32))
            else {
                return;
            };
            let adjustment = tab.scroller.vadjustment();
            let (top, bottom) = (
                f64::from(point.y()),
                f64::from(point.y()) + f64::from(rect.height()),
            );
            let margin = 24.0;
            if top < adjustment.value() {
                adjustment.set_value(top - margin);
            } else if bottom > adjustment.value() + adjustment.page_size() {
                adjustment.set_value(bottom - adjustment.page_size() + margin);
            }
        });
    }

    // ------------------------------------------------------------------------- window state

    /// Read only, the buttons and the inserters away, while presented.
    pub fn set_presenting(&self, on: bool) {
        self.presenting.set(on);
        self.head.set_visible(!on);
        for view in self.views.borrow().iter() {
            view.set_presenting(on);
        }
    }

    /// The theme, or Preferences' Images switch, changed: the editors' schemes, the markdown's
    /// colours, the pictures recoloured.
    pub fn restyle(&self) {
        for view in self.views.borrow().iter() {
            crate::editor::sync_scheme(&view.buffer);
            if view.kind == Kind::Markdown {
                crate::highlight::restyle(&view.buffer, &view.view);
            }
            view.outputs.restyle();
        }
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
        for view in self.views.borrow().iter() {
            crate::editor::refont(&view.view);
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

    /// Ctrl+Z, Ctrl+Shift+Z and Ctrl+Y over the notebook are its history's, as a diagram's
    /// canvas keys are: the cells' buffers keep no undo of their own.
    fn wire_keys(self: &Rc<Self>, host: &gtk::Box) {
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        keys.connect_key_pressed(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, key, _, state| {
                let ctrl = state.contains(gdk::ModifierType::CONTROL_MASK);
                let shift = state.contains(gdk::ModifierType::SHIFT_MASK);
                let action = match key {
                    gdk::Key::z | gdk::Key::Z if ctrl && shift => "win.notebook-redo",
                    gdk::Key::z if ctrl => "win.notebook-undo",
                    gdk::Key::y if ctrl => "win.notebook-redo",
                    _ => return glib::Propagation::Proceed,
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
        let Some(view) = self.views.borrow().get(n).cloned() else {
            return;
        };
        self.focus_cell(view.id);
        let buffer = &view.buffer;
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
        self.views
            .borrow()
            .get(n)
            .map(|v| v.text())
            .unwrap_or_default()
    }

    /// Down on cell `n`'s last line, or Up on its first, as the arrow key's controller sees it;
    /// answers where the selection is then.
    pub fn bench_step(self: &Rc<Self>, n: usize, down: bool) -> Option<usize> {
        let view = self.views.borrow().get(n).cloned()?;
        self.focus_cell(view.id);
        let buffer = &view.buffer;
        buffer.place_cursor(&match down {
            true => buffer.end_iter(),
            false => buffer.start_iter(),
        });
        self.leave_cell(view.id, down);
        self.selected_at()
    }

    /// Scroll to the `n`th screen of the notebook.
    pub fn bench_scroll(&self, n: usize) {
        let adjustment = self.scroller.vadjustment();
        adjustment.set_value(n as f64 * adjustment.page_size() * 0.9);
    }

    /// Whether no cell can be typed into and no cell button is out.
    pub fn bench_read_only(&self) -> bool {
        self.views
            .borrow()
            .iter()
            .all(|v| !v.view.is_editable() && !v.tools.is_visible() && !v.inserter.is_visible())
    }
}
