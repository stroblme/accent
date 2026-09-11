//! A draw.io diagram in a tab: the model, the page on screen, the selection, and the hooks the
//! window listens on.
//!
//! The format work is `accent-drawio`'s; the canvas (`view.rs`) paints a page and turns gestures
//! into [`Edit`]s. This is where an edit is applied — through [`DiagramTab::edit`], the one door
//! every change goes through, which is what makes each one an undo step, marks the tab dirty
//! and schedules the autosave.

mod geometry;
mod label;
mod math;
mod paint;
mod props;
mod tools;
mod view;
mod window;

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use accent_core::config::DiagramPlace;
use accent_core::fs::Etag;
use accent_drawio::{CellId, Editor, File, Point};
use adw::prelude::*;
use gtk::{gdk, gio, glib};

use crate::editor::{SaveState, Saves};
use geometry::{Sheet, Zoom};
pub use tools::{Options, Tool};
use view::{DiagramView, Edit};

/// How long after the last edit the file is written: a note's autosave.
const AUTOSAVE: std::time::Duration = std::time::Duration::from_secs(1);

type Hook = RefCell<Option<Rc<dyn Fn(&Rc<DiagramTab>)>>>;

/// Whether `text` is a draw.io diagram whatever its name says: an `.xml` file draw.io wrote.
pub fn sniff(text: &str) -> bool {
    let body = text.trim_start_matches('\u{feff}').trim_start();
    let body = match body.strip_prefix("<?xml") {
        Some(rest) => rest.split_once("?>").map_or("", |(_, r)| r).trim_start(),
        None => body,
    };
    body.starts_with("<mxfile") || body.starts_with("<mxGraphModel")
}

pub struct DiagramTab {
    key: RefCell<String>,
    path: RefCell<PathBuf>,
    pub page: adw::TabPage,
    /// "This diagram changed on disk", while the tab holds edits the file does not.
    banner: adw::Banner,
    /// The canvas's overlay: the ring and the label editor float over the page in it.
    overlay: gtk::Overlay,
    view: DiagramView,
    ring: Rc<tools::DiagramRing>,
    /// The Properties pane's content for this diagram, which the sidebar shows while it is in
    /// front.
    props: Rc<props::Props>,
    /// The label being edited, while one is.
    label: RefCell<Option<Rc<label::LabelEditor>>>,
    spellcheck: Cell<bool>,
    /// The editor font preference, which the label editor writes in.
    font: RefCell<Option<String>>,
    /// Whether the ring is out. A diagram's own, and out when it opens: a diagram is a surface
    /// to draw on, where a PDF is one to read.
    ring_shown: Cell<bool>,
    editor: RefCell<Editor>,
    page_index: Cell<usize>,
    selection: RefCell<Vec<CellId>>,
    tool: Cell<Tool>,
    options: Cell<Options>,
    /// What a note's tab keeps about its file, so the save path is the same one (`save.rs`).
    pub save: SaveState,
    save_pending: Cell<bool>,
    /// A watch on the file itself, for a diagram from outside the vault, which no vault watcher
    /// covers. `None` for everything inside a vault, which the worker already reports on.
    monitor: RefCell<Option<gio::FileMonitor>>,
    on_zoom: Hook,
    on_page: Hook,
    /// Fired just before a page switch the reader asked for, so the pane can record where they
    /// were (Back).
    on_jump: Hook,
    on_pages: Hook,
    on_selection: Hook,
    on_history: Hook,
    on_autosave: Hook,
    on_image: Hook,
    on_banner: Hook,
}

/// A new tab of `tabs` showing `file` as it was read, at its etag, where the session last left
/// it.
pub fn open(
    key: &str,
    path: &Path,
    title: &str,
    tooltip: &str,
    tabs: &adw::TabView,
    (file, etag): (File, Etag),
    place: DiagramPlace,
) -> Rc<DiagramTab> {
    let view = DiagramView::new();
    let scroller = gtk::ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .child(&view)
        .build();
    let overlay = gtk::Overlay::builder().child(&scroller).build();
    let ring = tools::DiagramRing::new();
    overlay.add_overlay(ring.widget());
    let banner = adw::Banner::builder()
        .title("This diagram changed on disk")
        .button_label("Resolve…")
        .build();
    let host = gtk::Box::new(gtk::Orientation::Vertical, 0);
    host.append(&banner);
    host.append(&overlay);

    let page = tabs.append(&host);
    page.set_title(title);
    page.set_tooltip(tooltip);
    page.set_icon(Some(&gio::ThemedIcon::new(
        "applications-graphics-symbolic",
    )));

    let pages = file.pages.len();
    // A WebKit view is a process; only a diagram that has formulas pays for one.
    let has_math = file.pages.iter().any(|p| p.model_attr("math") == Some("1"));
    let tab = Rc::new(DiagramTab {
        key: RefCell::new(key.to_string()),
        path: RefCell::new(path.to_path_buf()),
        page,
        banner,
        overlay,
        view,
        ring,
        props: props::Props::new(),
        label: RefCell::new(None),
        spellcheck: Cell::new(false),
        font: RefCell::new(None),
        ring_shown: Cell::new(true),
        editor: RefCell::new(Editor::new(file)),
        page_index: Cell::new(place.page.min(pages.saturating_sub(1))),
        selection: RefCell::new(Vec::new()),
        tool: Cell::new(Tool::Select),
        options: Cell::new(Options::default()),
        save: SaveState::at(etag),
        save_pending: Cell::new(false),
        monitor: RefCell::new(None),
        on_zoom: RefCell::new(None),
        on_page: RefCell::new(None),
        on_jump: RefCell::new(None),
        on_pages: RefCell::new(None),
        on_selection: RefCell::new(None),
        on_history: RefCell::new(None),
        on_autosave: RefCell::new(None),
        on_image: RefCell::new(None),
        on_banner: RefCell::new(None),
    });
    if has_math {
        tab.view.set_typesetter(math::Typesetter::new(&tab.overlay));
    }
    let zoom = place.zoom.map_or(Zoom::Fit, Zoom::Scale);
    tab.view.restore(zoom, (place.x, place.y));
    tab.refresh();
    tab.fill_props();
    tab.view.connect_edit(glib::clone!(
        #[weak]
        tab,
        move |edit| tab.apply(edit)
    ));
    tab.view.connect_zoom(glib::clone!(
        #[weak]
        tab,
        move || {
            tab.finish_label();
            tab.emit(&tab.on_zoom)
        }
    ));
    // The label editor sits at a place on screen; a scroll would leave it over another cell.
    // ponytail: finishing on a scroll is the simple answer; moving the editor with the page is
    // the upgrade if it gets in the way.
    for adjustment in [scroller.hadjustment(), scroller.vadjustment()] {
        adjustment.connect_value_changed(glib::clone!(
            #[weak]
            tab,
            move |_| tab.finish_label()
        ));
    }
    tab.props.connect_change(glib::clone!(
        #[weak]
        tab,
        move |change| tab.apply_property(change)
    ));
    tab.ring.connect_options(glib::clone!(
        #[weak]
        tab,
        move |options| tab.options.set(options)
    ));
    tab.banner.connect_button_clicked(glib::clone!(
        #[weak]
        tab,
        move |_| tab.emit(&tab.on_banner)
    ));
    tab.wire_keys();
    tab
}

impl DiagramTab {
    pub fn key(&self) -> String {
        self.key.borrow().clone()
    }

    pub fn path(&self) -> PathBuf {
        self.path.borrow().clone()
    }

    /// The widget the keys go to, and what a window hands the keyboard to when this tab moves.
    pub fn key_target(&self) -> gtk::Widget {
        self.view.clone().upcast()
    }

    pub fn place(&self) -> DiagramPlace {
        let (x, y) = self.view.scroll();
        DiagramPlace {
            page: self.page_index.get(),
            zoom: match self.view.zoom() {
                Zoom::Fit => None,
                Zoom::Scale(s) => Some(s),
            },
            x,
            y,
        }
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
        // The dot a dirty note's tab wears (editor/mod.rs), so one symbol means one thing.
        match self.save.modified.get() {
            true => self.page.set_title(&format!("• {name}")),
            false => self.page.set_title(&name),
        }
    }

    pub fn page_count(&self) -> usize {
        self.editor.borrow().file().pages.len()
    }

    pub fn page_index(&self) -> usize {
        self.page_index.get()
    }

    pub fn page_names(&self) -> Vec<String> {
        let editor = self.editor.borrow();
        editor
            .file()
            .pages
            .iter()
            .enumerate()
            .map(|(i, p)| match p.name() {
                "" => format!("Page {}", i + 1),
                name => name.to_string(),
            })
            .collect()
    }

    /// Show page `i`, as a jump the pane's history records.
    pub fn goto_page(self: &Rc<Self>, i: usize) {
        if i >= self.page_count() || i == self.page_index.get() {
            return;
        }
        self.emit(&self.on_jump);
        self.show_page(i);
    }

    /// The next or previous page, as reading rather than a jump.
    pub fn step_page(self: &Rc<Self>, forward: bool) {
        let i = self.page_index.get();
        let next = match forward {
            true => (i + 1 < self.page_count()).then_some(i + 1),
            false => i.checked_sub(1),
        };
        if let Some(next) = next {
            self.show_page(next);
        }
    }

    /// Go to page `i` without a history entry: Back itself, and the paging keys.
    pub fn show_page(self: &Rc<Self>, i: usize) {
        if i >= self.page_count() {
            return;
        }
        self.finish_label();
        self.page_index.set(i);
        self.selection.borrow_mut().clear();
        self.refresh();
        self.fill_props();
        self.view.set_zoom(Zoom::Fit);
        self.emit(&self.on_page);
        self.emit(&self.on_selection);
    }

    pub fn tool(&self) -> Tool {
        self.tool.get()
    }

    pub fn set_tool(&self, tool: Tool) {
        self.tool.set(tool);
        self.view.set_tool(tool);
        self.ring.set_tool(tool);
    }

    pub fn ring_shown(&self) -> bool {
        self.ring_shown.get()
    }

    /// Put the ring out or away, where the window last had one; putting it away puts the tool
    /// down with it.
    pub fn show_ring(&self, shown: bool, at: Option<(f64, f64)>) {
        self.ring_shown.set(shown);
        if !shown {
            self.ring.set_visible(false, at);
            return self.set_tool(Tool::Select);
        }
        // The ring finds its corner from the canvas's width, which a tab that has only just
        // opened does not have yet: it comes out on the first frame that has one.
        let ring = self.ring.clone();
        self.overlay.add_tick_callback(move |overlay, _| {
            if overlay.width() == 0 {
                return glib::ControlFlow::Continue;
            }
            ring.set_visible(true, at);
            glib::ControlFlow::Break
        });
    }

    /// Where the reader dragged the ring, for the next tab's to open at.
    pub fn ring_at(&self) -> Option<(f64, f64)> {
        self.ring.at()
    }

    pub fn selection(&self) -> Vec<CellId> {
        self.selection.borrow().clone()
    }

    pub fn has_selection(&self) -> bool {
        !self.selection.borrow().is_empty()
    }

    pub fn select(self: &Rc<Self>, ids: Vec<CellId>) {
        self.view.set_selection(&ids);
        *self.selection.borrow_mut() = ids;
        self.fill_props();
        self.emit(&self.on_selection);
    }

    /// What the Properties pane shows for this diagram.
    pub fn properties(&self) -> gtk::Widget {
        self.props.widget().clone()
    }

    /// Put the selection's look, or the page's, into the Properties pane.
    fn fill_props(&self) {
        let target = {
            let editor = self.editor.borrow();
            let Ok(page) = editor.page(self.page_index.get()) else {
                return;
            };
            let selection = self.selection.borrow();
            let cells: Vec<&accent_drawio::Cell> =
                selection.iter().filter_map(|id| page.cell(id)).collect();
            match cells.first() {
                Some(first) => props::Target::Cells {
                    style: first.style.resolve(first.edge),
                    raw: first.style.to_string(),
                    count: cells.len(),
                    vertices: cells.iter().any(|c| c.vertex),
                    edges: cells.iter().any(|c| c.edge),
                },
                None => props::Target::Page {
                    name: page.name().to_string(),
                    size: page.size(),
                    background: page.background(),
                },
            }
        };
        self.props.fill(&target);
    }

    /// A row of the Properties pane changed.
    fn apply_property(self: &Rc<Self>, change: props::Change) {
        let ids = self.selection();
        match change {
            props::Change::Style(pairs) if !ids.is_empty() => {
                let pairs: Vec<(&str, Option<&str>)> =
                    pairs.iter().map(|(k, v)| (*k, v.as_deref())).collect();
                self.edit(|e, page| e.set_styles(page, &ids, &pairs));
            }
            props::Change::Raw(style) => {
                if let [id] = ids.as_slice() {
                    self.edit(|e, page| e.set_style_string(page, id, &style));
                }
            }
            props::Change::PageAttr(key, value) => {
                self.edit(|e, page| e.set_page_attr(page, key, value.as_deref()));
            }
            props::Change::PageName(name) if !name.trim().is_empty() => {
                self.rename_page(name.trim());
            }
            _ => {}
        }
    }

    pub fn select_all(self: &Rc<Self>) {
        let ids = self.view.sheet().map(|s| s.top_level()).unwrap_or_default();
        self.select(ids);
    }

    pub fn history(&self) -> (bool, bool) {
        let editor = self.editor.borrow();
        (editor.can_undo(), editor.can_redo())
    }

    pub fn undo(self: &Rc<Self>) {
        self.walk(true);
    }

    pub fn redo(self: &Rc<Self>) {
        self.walk(false);
    }

    fn walk(self: &Rc<Self>, back: bool) {
        let walked = {
            let mut editor = self.editor.borrow_mut();
            match back {
                true => editor.undo(),
                false => editor.redo(),
            }
        };
        if walked {
            // An undo may have taken a page away, or brought one back.
            let last = self.page_count().saturating_sub(1);
            if self.page_index.get() > last {
                self.page_index.set(last);
            }
            self.changed();
            self.emit(&self.on_pages);
        }
    }

    /// Apply one change to the model: the one door every edit goes through. A change the model
    /// refuses leaves nothing behind but a log line.
    pub fn edit(
        self: &Rc<Self>,
        what: impl FnOnce(&mut Editor, usize) -> Result<(), accent_drawio::Error>,
    ) {
        let done = what(&mut self.editor.borrow_mut(), self.page_index.get());
        match done {
            Ok(()) => self.changed(),
            Err(e) => tracing::info!("diagram edit refused: {e}"),
        }
    }

    /// After any change to the model, an undo included.
    fn changed(self: &Rc<Self>) {
        self.save.edits.set(self.save.edits.get() + 1);
        self.save.modified.set(true);
        self.set_title();
        self.refresh();
        self.fill_props();
        self.emit(&self.on_history);
        self.emit(&self.on_selection);
        self.save_soon();
    }

    /// Put the page back on the canvas, and let go of anything selected that no longer exists.
    fn refresh(&self) {
        let sheet = {
            let editor = self.editor.borrow();
            match editor.page(self.page_index.get()) {
                Ok(page) => Sheet::of(page),
                Err(_) => Sheet::default(),
            }
        };
        // Let go of the selection before the canvas hears of it: showing a page can scroll it,
        // a scroll finishes an open label, and that is an edit that comes back here.
        let selection = {
            let mut selection = self.selection.borrow_mut();
            selection.retain(|id| sheet.frame_of(id).is_some());
            selection.clone()
        };
        self.view.set_selection(&selection);
        self.view.show(sheet);
    }

    /// What a gesture on the canvas asked for.
    fn apply(self: &Rc<Self>, edit: Edit) {
        match edit {
            Edit::Select(ids) => self.select(ids),
            Edit::Move { ids, delta } => {
                self.edit(|e, page| e.move_cells(page, &ids, delta.x, delta.y));
            }
            Edit::Resize { id, rect } => self.edit(|e, page| e.resize(page, &id, rect)),
            Edit::Add { tool, rect } => {
                let style = tool.style(&self.options.get());
                let mut added = None;
                self.edit(|e, page| {
                    added = Some(e.add_vertex(page, None, rect, &style, tool.label())?);
                    Ok(())
                });
                self.set_tool(Tool::Select);
                self.emit(&self.on_zoom);
                if let Some(id) = added {
                    self.select(vec![id]);
                    // A text box is made to be written in.
                    if tool == Tool::Text {
                        self.edit_label();
                    }
                }
            }
            Edit::Connect { source, target } => {
                let style = Tool::Connector.style(&self.options.get());
                let style = accent_drawio::presets::constrained(
                    &style,
                    source.2.as_ref(),
                    target.2.as_ref(),
                );
                let mut added = None;
                self.edit(|e, page| {
                    let (s, t) = (
                        (source.0.as_deref(), source.1),
                        (target.0.as_deref(), target.1),
                    );
                    added = Some(e.add_edge(page, s, t, &style)?);
                    Ok(())
                });
                if let Some(id) = added {
                    self.select(vec![id]);
                }
            }
            Edit::Label(id) => {
                self.select(vec![id]);
                self.edit_label();
            }
        }
    }

    pub fn delete(self: &Rc<Self>) {
        let ids = self.selection();
        if !ids.is_empty() {
            self.edit(|e, page| e.delete(page, &ids));
        }
    }

    pub fn duplicate(self: &Rc<Self>) {
        let ids = self.selection();
        if ids.is_empty() {
            return;
        }
        let mut copies = Vec::new();
        self.edit(|e, page| {
            copies = e.duplicate(page, &ids)?;
            Ok(())
        });
        self.select(copies);
    }

    /// Move the selection by `dx`, `dy` page units: the arrow keys.
    pub fn nudge(self: &Rc<Self>, dx: f64, dy: f64) {
        let ids = self.selection();
        if !ids.is_empty() {
            self.edit(|e, page| e.move_cells(page, &ids, dx, dy));
        }
    }

    pub fn reorder(self: &Rc<Self>, z: accent_drawio::ZOrder) {
        let ids = self.selection();
        if !ids.is_empty() {
            self.edit(|e, page| e.reorder(page, &ids, z));
        }
    }

    /// Edit the label of the one selected cell, in the note editor over it.
    pub fn edit_label(self: &Rc<Self>) {
        let selection = self.selection();
        let [id] = selection.as_slice() else {
            return;
        };
        let id = id.clone();
        self.finish_label();
        let Some(sheet) = self.view.sheet() else {
            return;
        };
        let markdown = {
            let editor = self.editor.borrow();
            let Ok(page) = editor.page(self.page_index.get()) else {
                return;
            };
            let Some(cell) = page.cell(&id) else { return };
            accent_drawio::label::to_markdown(cell.label(), cell.is_html())
        };
        // Where the label is drawn, or where it will be for a cell that has none yet: halfway
        // along an edge, over the whole of a shape.
        let (mut place, size) = sheet
            .scene
            .prims
            .iter()
            .find_map(|p| match p {
                accent_drawio::Prim::Text {
                    cell, rect, font, ..
                } if *cell == id => Some((*rect, font.size)),
                _ => None,
            })
            .or_else(|| {
                let m = sheet.edge_middle(&id)?;
                Some((accent_drawio::Rect::new(m.x, m.y, 0.0, 0.0), 11.0))
            })
            .unwrap_or_else(|| (sheet.frame_of(&id).unwrap_or_default(), 12.0));
        if let Some(r) = sheet.rect(&id).filter(|_| place.w < 1.0 || place.h < 1.0) {
            place = r;
        }
        self.view.reveal(&place);
        let at = self.view.to_widget(&place);
        let editor = label::LabelEditor::open(
            &self.overlay,
            id,
            &markdown,
            at,
            size * self.view.scale(),
            self.font.borrow().as_deref(),
            self.spellcheck.get(),
        );
        let focus = gtk::EventControllerFocus::new();
        focus.connect_leave(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move |_| tab.finish_label()
        ));
        editor.view().add_controller(focus);
        label::wire_keys(
            &editor,
            glib::clone!(
                #[weak(rename_to = tab)]
                self,
                move || tab.finish_label()
            ),
        );
        *self.label.borrow_mut() = Some(editor);
    }

    /// Put the label editor away, writing what was typed into the cell. Safe to call twice:
    /// taking the editor off the canvas moves the focus, which calls it again.
    pub fn finish_label(self: &Rc<Self>) {
        let Some(editor) = self.label.borrow_mut().take() else {
            return;
        };
        let typed = editor.changed();
        editor.close(&self.overlay);
        if let Some(markdown) = typed {
            let id = editor.cell.clone();
            self.edit(|e, page| e.set_label_markdown(page, &id, &markdown));
        }
        // Not when another label opened meanwhile: a double click on a second label finishes
        // the first and opens the second in one turn, and taking the focus back would finish
        // that one too.
        let tab = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            if let Some(tab) = tab.upgrade().filter(|t| t.label.borrow().is_none()) {
                tab.view.grab_focus();
            }
        });
    }

    /// Typing over the one selected shape: its label is edited with `typed` in place of what it
    /// said, as draw.io does. `false` when there is nothing to type into.
    fn type_into_label(self: &Rc<Self>, typed: Option<char>) -> bool {
        let Some(c) = typed.filter(|c| !c.is_control()) else {
            return false;
        };
        let one = match self.selection.borrow().as_slice() {
            [id] => self.view.sheet().is_some_and(|s| !s.is_pinned(id)),
            _ => false,
        };
        if !one {
            return false;
        }
        self.edit_label();
        let Some(editor) = self.label.borrow().clone() else {
            return false;
        };
        editor.type_text(&c.to_string());
        true
    }

    /// The cell whose label is being edited, if one is.
    pub fn editing_label(&self) -> Option<CellId> {
        self.label.borrow().as_ref().map(|e| e.cell.clone())
    }

    /// `Ctrl+Return` in the label editor, which the window's accelerator took first: finish the
    /// label. `false` when no label is being edited here.
    pub fn commit_label(self: &Rc<Self>) -> bool {
        let editing = self.label.borrow().as_ref().is_some_and(|e| e.has_focus());
        if editing {
            self.finish_label();
        }
        editing
    }

    /// A page rectangle in the canvas's own coordinates.
    pub fn to_widget(&self, r: &accent_drawio::Rect) -> accent_drawio::Rect {
        self.view.to_widget(r)
    }

    pub fn scale(&self) -> f64 {
        self.view.scale()
    }

    /// Whether formulas are still being typeset.
    pub fn typesetting(&self) -> bool {
        self.view.typesetting()
    }

    /// A cell's frame on the page: what the selection box is drawn around.
    pub fn frame_of(&self, id: &str) -> Option<accent_drawio::Rect> {
        self.view.sheet()?.frame_of(id)
    }

    /// A cell's label as the label editor would show it.
    pub fn label_markdown(&self, id: &str) -> Option<String> {
        let editor = self.editor.borrow();
        let cell = editor.page(self.page_index.get()).ok()?.cell(id)?;
        Some(accent_drawio::label::to_markdown(
            cell.label(),
            cell.is_html(),
        ))
    }

    pub fn set_spellcheck(&self, on: bool) {
        self.spellcheck.set(on);
    }

    pub fn set_font(&self, font: Option<&str>) {
        *self.font.borrow_mut() = font.map(str::to_string);
    }

    pub fn add_page(self: &Rc<Self>) {
        let name = format!("Page-{}", self.page_count() + 1);
        let mut index = 0;
        self.edit(|e, _| {
            index = e.add_page(&name);
            Ok(())
        });
        self.emit(&self.on_pages);
        self.goto_page(index);
    }

    pub fn rename_page(self: &Rc<Self>, name: &str) {
        let name = name.to_string();
        self.edit(|e, page| e.rename_page(page, &name));
        self.emit(&self.on_pages);
    }

    pub fn delete_page(self: &Rc<Self>) {
        self.edit(|e, page| e.delete_page(page));
        self.emit(&self.on_pages);
    }

    /// Zoom a step in or out around the middle of the view.
    pub fn zoom_step(&self, out: bool) {
        self.view.zoom_step(out, None);
    }

    pub fn fit_page(&self) {
        self.view.set_zoom(Zoom::Fit);
    }

    pub fn zoom_label(&self) -> String {
        self.view.zoom_label()
    }

    /// The status bar's count slot: where the reader is, and the tool in hand.
    pub fn facts(&self) -> Option<String> {
        let page = crate::statusbar::page_label(self.page_index.get(), self.page_count())?;
        match self.tool.get() {
            Tool::Select => Some(page),
            tool => Some(format!(
                "{page} · {}",
                crate::actions::label_of(tool.action())
            )),
        }
    }

    /// What the file should hold now.
    pub fn text(&self) -> String {
        self.editor.borrow().file().to_xml()
    }

    /// A write of the model landed: the tab is clean at `etag`.
    pub fn mark_clean(&self, etag: Etag) {
        self.save.etag.set(Some(etag));
        self.save.modified.set(false);
        self.editor.borrow_mut().mark_saved();
        self.set_title();
    }

    /// The edits are given up on (a tab closing without saving them).
    pub fn discard(&self) {
        self.save.modified.set(false);
        self.set_title();
    }

    /// The file changed on disk and the tab holds no edits: read it again, staying on the same
    /// page and keeping whatever of the selection is still there.
    pub fn reload(self: &Rc<Self>, file: File, etag: Etag) {
        let last = file.pages.len().saturating_sub(1);
        *self.editor.borrow_mut() = Editor::new(file);
        self.page_index.set(self.page_index.get().min(last));
        self.save.etag.set(Some(etag));
        self.save.modified.set(false);
        self.save.disk_changed.set(false);
        self.banner.set_revealed(false);
        self.set_title();
        self.refresh();
        self.fill_props();
        self.emit(&self.on_pages);
        self.emit(&self.on_history);
        self.emit(&self.on_selection);
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

    /// Watch the file behind this tab and call `f` when someone else writes it
    /// (`editor::watch_file`).
    pub fn watch_file(self: &Rc<Self>, f: impl Fn(&Rc<DiagramTab>) + 'static) {
        *self.monitor.borrow_mut() = crate::editor::watch_file(
            &self.path(),
            glib::clone!(
                #[weak(rename_to = tab)]
                self,
                move || f(&tab)
            ),
        );
    }

    /// Write the file a moment after the last edit, as a note's autosave does.
    fn save_soon(self: &Rc<Self>) {
        if self.save_pending.replace(true) {
            return;
        }
        glib::timeout_add_local_once(
            AUTOSAVE,
            glib::clone!(
                #[weak(rename_to = tab)]
                self,
                move || {
                    tab.save_pending.set(false);
                    tab.emit(&tab.on_autosave);
                }
            ),
        );
    }

    /// Fire a window action from the canvas's own keys (the PDF tab's `run`).
    fn run(&self, action: &str) {
        let _ = self.view.activate_action(action, None);
    }

    /// The canvas's keys. Undo, Select All and Delete belong to whatever has the keyboard, so
    /// they are the canvas's own and fire the window's actions rather than being accelerators
    /// (the PDF doctrine, `actions.rs`).
    fn wire_keys(self: &Rc<Self>) {
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, key, _, state| {
                let ctrl = state.contains(gdk::ModifierType::CONTROL_MASK);
                let alt = state.contains(gdk::ModifierType::ALT_MASK);
                let shift = state.contains(gdk::ModifierType::SHIFT_MASK);
                let step = if shift { 10.0 } else { 1.0 };
                match key {
                    gdk::Key::z | gdk::Key::Z if ctrl && shift => tab.run("win.diagram-redo"),
                    gdk::Key::z if ctrl => tab.run("win.diagram-undo"),
                    gdk::Key::y if ctrl => tab.run("win.diagram-redo"),
                    gdk::Key::a if ctrl => tab.run("win.diagram-select-all"),
                    gdk::Key::Delete | gdk::Key::BackSpace => tab.run("win.diagram-delete"),
                    gdk::Key::Return | gdk::Key::KP_Enter if !ctrl => {
                        tab.run("win.diagram-edit-label")
                    }
                    gdk::Key::Page_Down if !ctrl => tab.run("win.diagram-next-page"),
                    gdk::Key::Page_Up if !ctrl => tab.run("win.diagram-previous-page"),
                    gdk::Key::Left if !ctrl => tab.nudge(-step, 0.0),
                    gdk::Key::Right if !ctrl => tab.nudge(step, 0.0),
                    gdk::Key::Up if !ctrl => tab.nudge(0.0, -step),
                    gdk::Key::Down if !ctrl => tab.nudge(0.0, step),
                    gdk::Key::space if !ctrl => tab.view.set_panning(true),
                    gdk::Key::Escape if tab.tool.get() != Tool::Select => {
                        tab.run("win.diagram-select")
                    }
                    gdk::Key::Escape if tab.has_selection() => tab.select(Vec::new()),
                    _ if !ctrl && !alt && tab.type_into_label(key.to_unicode()) => {}
                    _ => return glib::Propagation::Proceed,
                }
                glib::Propagation::Stop
            }
        ));
        keys.connect_key_released(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move |_, key, _, _| {
                if key == gdk::Key::space {
                    tab.view.set_panning(false);
                }
            }
        ));
        self.view.add_controller(keys);
        // A key held while the canvas lost the keyboard never comes up here.
        let focus = gtk::EventControllerFocus::new();
        focus.connect_leave(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move |_| tab.view.set_panning(false)
        ));
        self.view.add_controller(focus);
    }

    fn emit(self: &Rc<Self>, hook: &Hook) {
        let f = hook.borrow().clone();
        if let Some(f) = f {
            f(self);
        }
    }

    pub fn connect_zoom(&self, f: impl Fn(&Rc<DiagramTab>) + 'static) {
        *self.on_zoom.borrow_mut() = Some(Rc::new(f));
    }

    pub fn connect_page(&self, f: impl Fn(&Rc<DiagramTab>) + 'static) {
        *self.on_page.borrow_mut() = Some(Rc::new(f));
    }

    pub fn connect_jump(&self, f: impl Fn(&Rc<DiagramTab>) + 'static) {
        *self.on_jump.borrow_mut() = Some(Rc::new(f));
    }

    pub fn connect_pages(&self, f: impl Fn(&Rc<DiagramTab>) + 'static) {
        *self.on_pages.borrow_mut() = Some(Rc::new(f));
    }

    pub fn connect_selection(&self, f: impl Fn(&Rc<DiagramTab>) + 'static) {
        *self.on_selection.borrow_mut() = Some(Rc::new(f));
    }

    pub fn connect_history(&self, f: impl Fn(&Rc<DiagramTab>) + 'static) {
        *self.on_history.borrow_mut() = Some(Rc::new(f));
    }

    pub fn connect_autosave(&self, f: impl Fn(&Rc<DiagramTab>) + 'static) {
        *self.on_autosave.borrow_mut() = Some(Rc::new(f));
    }

    /// The Image tool was picked: the window asks for a file.
    pub fn connect_image(&self, f: impl Fn(&Rc<DiagramTab>) + 'static) {
        *self.on_image.borrow_mut() = Some(Rc::new(f));
    }

    /// The changed-on-disk banner's button.
    pub fn connect_banner(&self, f: impl Fn(&Rc<DiagramTab>) + 'static) {
        *self.on_banner.borrow_mut() = Some(Rc::new(f));
    }

    /// Embed a picture the window read, at the middle of what is on screen, no wider than 400.
    pub fn add_image(self: &Rc<Self>, mime: &str, bytes: &[u8], size: (f64, f64)) {
        let (w, h) = size;
        let s = (400.0 / w.max(1.0)).min(1.0);
        let (w, h) = (w * s, h * s);
        let (vw, vh) = (f64::from(self.view.width()), f64::from(self.view.height()));
        let (sx, sy) = self.view.scroll();
        let scale = self.view.scale();
        let centre = Point::new((sx + vw / 2.0) / scale, (sy + vh / 2.0) / scale);
        let centre = self
            .view
            .sheet()
            .map(|_| self.view_page_point(sx + vw / 2.0, sy + vh / 2.0))
            .unwrap_or(centre);
        let rect = accent_drawio::Rect::new(centre.x - w / 2.0, centre.y - h / 2.0, w, h);
        let style = accent_drawio::presets::image(mime, bytes);
        let mut added = None;
        self.edit(|e, page| {
            added = Some(e.add_vertex(page, None, rect, &style, "")?);
            Ok(())
        });
        self.set_tool(Tool::Select);
        if let Some(id) = added {
            self.select(vec![id]);
        }
    }

    fn view_page_point(&self, cx: f64, cy: f64) -> Point {
        let r = self
            .view
            .to_widget(&accent_drawio::Rect::new(0.0, 0.0, 1.0, 1.0));
        let (sx, sy) = self.view.scroll();
        let scale = self.view.scale();
        Point::new((cx - sx - r.x) / scale, (cy - sy - r.y) / scale)
    }

    /// The Image tool: nothing to drag, the window's file dialog does the rest.
    pub fn ask_image(self: &Rc<Self>) {
        self.emit(&self.on_image);
    }
}

impl Saves for DiagramTab {
    fn save_state(&self) -> &SaveState {
        &self.save
    }

    fn key(&self) -> String {
        DiagramTab::key(self)
    }

    fn path(&self) -> PathBuf {
        DiagramTab::path(self)
    }

    fn for_disk(&self) -> String {
        self.text()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_diagram_is_known_by_its_first_element() {
        assert!(super::sniff("<mxfile host=\"x\">"));
        assert!(super::sniff(
            "\u{feff}<?xml version=\"1.0\"?>\n  <mxGraphModel>"
        ));
        assert!(!super::sniff("<?xml version=\"1.0\"?><svg/>"));
        assert!(!super::sniff("mxfile"));
    }
}
