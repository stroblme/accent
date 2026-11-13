//! Notebook tabs: a Jupyter or marimo notebook as a column of cells, VS Code's notebook editor.
//! `accent_core::notebook` reads and writes the file; [`NotebookTab`] keeps a view per cell in
//! step with the model, records every change on the notebook's one history (`history`), runs a
//! Jupyter notebook's cells on a kernel (`kernel`), and saves through the same etag gate as a
//! note (`window`).

#[cfg(feature = "bench")]
mod bench;
mod buttons;
mod cell;
mod changes;
mod editing;
mod file;
mod find;
mod focus;
mod history;
mod kernel;
mod keys;
mod language;
mod look;
mod near;
mod operations;
mod outline;
mod output;
mod placeholder;
mod reading;
mod selection;
mod views;
mod window;

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use accent_core::fs::{Digest, Etag};
use accent_core::notebook::{CellId, Kind, Notebook};
use adw::prelude::*;
use gtk::{gdk, gio, glib};

use crate::editor::SaveState;
use crate::widgets::{Debounce, Fade, Hook};
use cell::{CellView, Tools};
use history::{Change, History};
#[cfg(feature = "bench")]
pub use output::Counts;

type TabHook = Hook<dyn Fn(&Rc<NotebookTab>)>;

pub struct NotebookTab {
    key: RefCell<String>,
    path: RefCell<PathBuf>,
    pub page: adw::TabPage,
    /// "This notebook changed outside Accent", while the tab holds edits the file does not.
    banner: adw::Banner,
    scroller: gtk::ScrolledWindow,
    /// The cells, under the strip that adds one at the top.
    column: gtk::Box,
    head: gtk::Box,
    /// The cell buttons, over the cell under the pointer, else the selected one.
    tools: Tools,
    /// The add strip of the gaps between cells, its fade in at each gap it comes to, and the cell
    /// whose gap is under the pointer.
    gap: gtk::Box,
    gap_fade: Fade,
    gap_cell: Cell<Option<CellId>>,
    /// The cell under the pointer.
    hovered: Cell<Option<CellId>>,
    /// The buttons and the add strip are to be moved once the event at hand is over.
    placing: Cell<bool>,
    /// The frame the editors near the view were last built in, and the hook that builds them.
    built_at: Cell<i64>,
    /// The cells within a screen of the view as of that frame.
    near: RefCell<Vec<Rc<CellView>>>,
    /// The cell what shows was kept still by at the last layout, the first reaching into the view
    /// or the one [`show`](Self::show) brought into it, and its top in the content.
    anchor: Cell<Option<(CellId, f64)>>,
    /// A cell whose caret is to be brought into view once its editor is laid out ([`show`]).
    ///
    /// [`show`]: Self::show
    reveal: Cell<Option<(CellId, Option<f64>)>>,
    /// The cell [`show`](Self::show) brought into view, which the view keeps still in place of the
    /// first cell in it until it is scrolled otherwise or another cell is selected, and the
    /// scroll it was left at.
    shown: Cell<Option<(CellId, f64)>>,
    /// What the last zoom keeps where it was in the view, in place of the first cell in it, until
    /// the notebook is scrolled otherwise or changed.
    zoom_anchor: RefCell<Option<near::ZoomAnchor>>,
    /// Where in the view the pointer of the Ctrl+scroll asking for the zoom under way is.
    zoom_at: Cell<Option<(f64, f64)>>,
    layout_hook: RefCell<Option<(gdk::FrameClock, glib::SignalHandlerId)>>,
    model: RefCell<Notebook>,
    /// A view per cell, in the model's order.
    views: RefCell<Vec<Rc<CellView>>>,
    /// The next [`reconcile`](Self::reconcile) fades in the cells it makes: one added, split off
    /// or converted, not one an open, a reload or the history brings (DESIGN.md, Motion).
    fade_new: Cell<bool>,
    /// The code cells' language, from the notebook's own metadata.
    language: Option<sourceview5::Language>,
    /// How wide the code cells' `[n]` column is, where the markdown cells' markers hang.
    gutter: Cell<i32>,
    /// Whether the markdown cells are spell checked: the Preferences switch a note follows.
    spellcheck: Cell<bool>,
    /// How many columns a tab takes in the cells: the Indent Width a text tab follows.
    indent: Cell<u32>,
    /// The selection whose other occurrences are painted in every cell.
    occurrence: RefCell<Option<String>>,
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
    /// A notebook's preview tab's view of it (`rendered.rs`): always read, as a presented notebook
    /// is, its markdown cells rendered, and fed the notebook as it changes ([`follow`]).
    ///
    /// [`follow`]: Self::follow
    viewer: bool,
    /// What a rendered cell's page reaches in the window, set by it.
    wiring: RefCell<Option<crate::preview::Wiring>>,
    /// The notebook's caret a viewer last followed ([`show_place_of`]), its cell and line.
    ///
    /// [`show_place_of`]: Self::show_place_of
    followed: Cell<Option<(CellId, i32)>>,
    /// The editor font and the Preview Font, `None` the system's, which the font rule is made of.
    fonts: RefCell<(Option<String>, Option<String>)>,
    /// The kernel its runs go to, and what they changed (`kernel`).
    kernel: kernel::Hold,
    /// The notebook's fonts at its zoom, one rule for all its cells.
    font: RefCell<Option<gtk::CssProvider>>,
    /// Its zoom, which its outputs' pictures and formulas are drawn at as the rule sets its text.
    zoom: Rc<Cell<f64>>,
    /// A font change recuts code-cell messages after the new text layout has landed.
    refit_after_layout: Cell<bool>,
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
    /// A cell's count changed, `[*]` as a run waits or runs and `[n]` once it is done.
    on_count: Hook<dyn Fn()>,
    /// The caret went far at once from the place handed over (`selection::leaped`).
    on_leap: Hook<dyn Fn(&gtk::TextIter)>,
    /// The find bar's query over the cells and where it matches (`find`).
    find: RefCell<find::Find>,
    /// The matches or the readout changed.
    on_matches: TabHook,
    /// A cell's caret moved.
    on_caret: TabHook,
    /// The code cells' document on the language layer, and their editors there (`language`).
    lang: language::State,
    /// Focus mode's line fade over the cells (`focus`).
    focus: focus::State,
}

impl Drop for NotebookTab {
    /// Take the font rule off the display, as a closed editor tab does: a preview tab's view is
    /// made again each time the tab comes back into sight.
    fn drop(&mut self) {
        if let (Some(display), Some(provider)) = (gdk::Display::default(), self.font.take()) {
            gtk::style_context_remove_provider_for_display(&display, &provider);
        }
    }
}

/// The tab `held` up for `key`, showing `notebook` as it was read, at its etag and digest.
pub fn open(
    key: &str,
    path: &Path,
    title: &str,
    tooltip: &str,
    held: &crate::loading::Held,
    (notebook, etag, digest): (Notebook, Etag, Digest),
) -> Rc<NotebookTab> {
    let save = SaveState::at(etag, digest);
    let tab = build(key, path, notebook, save, |host| held.fill(host), false);
    tab.page.set_title(title);
    tab.page.set_tooltip(tooltip);
    crate::doc::set_file_icon(&tab.page, key);
    tab
}

/// The read-only view of `source` in its preview tab `page`, whose child `frame` holds it: the
/// notebook as it is now, which [`NotebookTab::follow`] keeps up with.
pub fn viewer(source: &NotebookTab, page: &adw::TabPage, frame: &adw::Bin) -> Rc<NotebookTab> {
    let host = |host: &gtk::Box| {
        frame.set_child(Some(host));
        page.clone()
    };
    let save = SaveState::default();
    let tab = build(
        &source.key(),
        &source.path(),
        source.snapshot(),
        save,
        host,
        true,
    );
    tab.set_wiring(source.wiring.borrow().clone());
    let (font, preview) = source.fonts.borrow().clone();
    tab.fonts.borrow_mut().1 = preview;
    tab.set_font(font.as_deref(), source.zoom.get());
    tab
}

/// The tab for `key` at `path` showing `notebook`, its page the one `page` gives for its widget, a
/// viewer's or a notebook's own.
fn build(
    key: &str,
    path: &Path,
    notebook: Notebook,
    save: SaveState,
    page: impl FnOnce(&gtk::Box) -> adw::TabPage,
    viewer: bool,
) -> Rc<NotebookTab> {
    cell::install_css();
    let (head, add_code, add_markdown) = cell::inserter();
    let (gap, gap_code, gap_markdown) = cell::inserter();
    let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
    column.set_widget_name(&crate::editor::next_view_name());
    column.set_margin_top(12);
    column.set_margin_bottom(96);
    head.set_margin_end(cell::RAIL_ROOM);
    column.append(&head);
    // The cell buttons stand over the column, in the rail of the cell they serve (`cell::stand`).
    let overlay = gtk::Overlay::builder().child(&column).build();
    let clamp = adw::Clamp::builder()
        .maximum_size(1100)
        .tightening_threshold(900)
        .child(&overlay)
        .build();
    // The cells go in before the column is in the window, where each widget made would be
    // styled on its own as it is added.
    let scroller = gtk::ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();
    let banner = adw::Banner::builder()
        .title("This notebook changed outside Accent")
        .button_label("Resolve…")
        .build();
    let host = gtk::Box::new(gtk::Orientation::Vertical, 0);
    host.append(&banner);
    host.append(&scroller);
    let page = page(&host);

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
        gap_fade: Fade::new(&gap),
        gap,
        gap_cell: Cell::new(None),
        hovered: Cell::new(None),
        placing: Cell::new(false),
        built_at: Cell::new(-1),
        near: RefCell::default(),
        anchor: Cell::new(None),
        reveal: Cell::new(None),
        shown: Cell::new(None),
        zoom_anchor: RefCell::default(),
        zoom_at: Cell::new(None),
        layout_hook: RefCell::default(),
        model: RefCell::new(notebook),
        views: RefCell::default(),
        fade_new: Cell::new(false),
        language,
        gutter: Cell::new(0),
        spellcheck: Cell::new(false),
        indent: Cell::new(4),
        occurrence: RefCell::default(),
        selected: Cell::new(None),
        history: RefCell::default(),
        pending: RefCell::default(),
        acting: Cell::new(0),
        stale: RefCell::default(),
        replaying: Cell::new(false),
        pressing: Cell::new(false),
        presenting: Cell::new(false),
        viewer,
        wiring: RefCell::default(),
        followed: Cell::new(None),
        fonts: RefCell::default(),
        kernel: kernel::Hold::default(),
        font: RefCell::default(),
        zoom: Rc::new(Cell::new(1.0)),
        refit_after_layout: Cell::new(false),
        save,
        autosave: Debounce::new(crate::editor::AUTOSAVE),
        monitor: RefCell::default(),
        on_history: Hook::default(),
        on_autosave: Hook::default(),
        on_banner: Hook::default(),
        on_select: Hook::default(),
        on_zoom: Hook::default(),
        on_count: Hook::default(),
        on_leap: Hook::default(),
        find: RefCell::default(),
        on_matches: Hook::default(),
        on_caret: Hook::default(),
        lang: language::State::default(),
        focus: focus::State::default(),
    });
    tab.gutter.set(cell::count_width(&tab.scroller));
    tab.head.set_visible(!viewer);
    overlay.add_overlay(&tab.tools.bar);
    let rail = tab.tools.rail.clone();
    overlay.connect_get_child_position(move |overlay, bar| {
        cell::stand(overlay, bar, rail.borrow().as_ref())
    });
    tab.reconcile();
    // The notebook follows the caret itself (`follow_caret`): the viewport's own scroll to the
    // focused widget would bring a clicked cell's whole editor into view, moving what was clicked.
    let viewport = gtk::Viewport::builder()
        .scroll_to_focus(false)
        .child(&clamp)
        .build();
    tab.scroller.set_child(Some(&viewport));
    crate::widgets::smooth_wheel(&tab.scroller);
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
    tab.wire_reading();
    tab.wire_outside();
    let pointer = crate::zoom::track_pointer(&tab.scroller);
    crate::zoom::zoom_on_wheel(
        &tab.scroller,
        gtk::PropagationPhase::Capture,
        glib::clone!(
            #[weak]
            tab,
            move |out| tab.wheel_zoom(pointer.get(), out)
        ),
    );
    tab
}

impl NotebookTab {
    pub(crate) fn is_python(&self) -> bool {
        self.model.borrow().language() == "python"
    }
    pub fn key(&self) -> String {
        self.key.borrow().clone()
    }

    pub fn path(&self) -> PathBuf {
        self.path.borrow().clone()
    }

    /// The kind of the cell whose editor is `view`, which a Files row dropped with Shift writes
    /// a reference into (`refer.rs`).
    pub fn cell_kind(&self, view: &gtk::Widget) -> Option<Kind> {
        self.views
            .borrow()
            .iter()
            .find(|cell| {
                cell.editor()
                    .is_some_and(|e| e.view.upcast_ref::<gtk::Widget>() == view)
            })
            .map(|cell| cell.kind)
    }

    /// The widget the keys go to: the selected cell's editor, else the first; the column, which
    /// reads with the reading keys, in a viewer and over a rendered cell.
    pub fn key_target(self: &Rc<Self>) -> gtk::Widget {
        let first = || self.views.borrow().first().map(|v| v.id);
        let id = self.selected.get().or_else(first);
        if self.viewer || id.is_some_and(|id| self.renders(id)) {
            return self.column.clone().upcast();
        }
        match id.and_then(|id| self.editor(id)) {
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

    pub fn connect_count(&self, f: impl Fn() + 'static) {
        self.on_count.set(Rc::new(f));
    }

    pub fn connect_zoom(&self, f: impl Fn(bool) + 'static) {
        self.on_zoom.set(Rc::new(f));
    }

    pub fn connect_leap(&self, f: impl Fn(&gtk::TextIter) + 'static) {
        self.on_leap.set(Rc::new(f));
    }
}
