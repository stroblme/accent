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

use accent_core::config::{DiagramConfig, DiagramPlace};
use accent_core::fs::Etag;
use accent_drawio::{CellId, Editor, File, Point};
use adw::prelude::*;
use gtk::{gdk, gio, glib};

use crate::editor::{SaveState, Saves};
use geometry::{Overshoot, Sheet, Zoom};
pub use tools::Tool;
use view::{DiagramView, Edit};

/// How long after the last edit the file is written: a note's autosave.
const AUTOSAVE: std::time::Duration = std::time::Duration::from_secs(1);

type Hook = RefCell<Option<Rc<dyn Fn(&Rc<DiagramTab>)>>>;

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
    /// The frame callback waiting to put the ring out, while one is.
    ring_tick: RefCell<Option<gtk::TickCallbackId>>,
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
    /// Where the diagram was left while presentation shows it, `None` otherwise.
    presenting: Cell<Option<DiagramPlace>>,
    /// Invert Diagram Colours: painted as the other half of the theme would, as an inverted PDF
    /// is. This tab's alone, and kept nowhere.
    inverted: Cell<bool>,
    editor: RefCell<Editor>,
    page_index: Cell<usize>,
    selection: RefCell<Vec<CellId>>,
    tool: Cell<Tool>,
    /// How the tools draw: the config's `[diagram]`, which the ring's outer orbit changes.
    options: Cell<DiagramConfig>,
    /// The text last pasted or copied here and how many times it has been pasted since: each
    /// paste of the same lands a grid step further (`Graph.lastPasteXml`, `pasteCounter`).
    pasted: RefCell<(Option<String>, u32)>,
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
    on_options: Hook,
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
    // Select is in hand from the start, and its button says so.
    ring.set_tool(Tool::Select);
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
        ring_tick: RefCell::new(None),
        props: props::Props::new(),
        label: RefCell::new(None),
        spellcheck: Cell::new(false),
        font: RefCell::new(None),
        ring_shown: Cell::new(true),
        presenting: Cell::new(None),
        inverted: Cell::new(false),
        editor: RefCell::new(Editor::new(file)),
        page_index: Cell::new(place.page.min(pages.saturating_sub(1))),
        selection: RefCell::new(Vec::new()),
        tool: Cell::new(Tool::Select),
        options: Cell::new(DiagramConfig::default()),
        pasted: RefCell::new((None, 0)),
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
        on_options: RefCell::new(None),
    });
    if has_math {
        tab.view.set_typesetter(math::Typesetter::new(&tab.overlay));
    }
    let zoom = place.zoom.map_or(Zoom::Fit, Zoom::Scale);
    tab.view.restore(zoom, (place.x, place.y));
    tab.restyle();
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
            tab.place_label();
            tab.emit(&tab.on_zoom)
        }
    ));
    // The label editor sits at a place on screen: the page moving under it takes it along, and
    // the overlay clips it where the label leaves the canvas, so neither a scroll nor a zoom
    // finishes the edit or leaves the editor over another cell.
    for adjustment in [scroller.hadjustment(), scroller.vadjustment()] {
        adjustment.connect_value_changed(glib::clone!(
            #[weak]
            tab,
            move |_| tab.place_label()
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
        move |options| {
            tab.options.set(options);
            tab.emit(&tab.on_options);
        }
    ));
    tab.banner.connect_button_clicked(glib::clone!(
        #[weak]
        tab,
        move |_| tab.emit(&tab.on_banner)
    ));
    tab.wire_keys();
    tab.wire_wheel();
    tab.wire_menu();
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

    /// Show cell `id` of page `page`: the page as a jump, the cell selected and scrolled into
    /// view. Where a search hit on a label lands.
    pub fn reveal_cell(self: &Rc<Self>, page: usize, id: &str) {
        self.goto_page(page);
        self.select(vec![id.to_string()]);
        if let Some(frame) = self.view.sheet().and_then(|sheet| sheet.frame_of(id)) {
            self.view.reveal(&frame);
        }
    }

    /// The next or previous page, as reading rather than a jump.
    pub fn step_page(self: &Rc<Self>, forward: bool) {
        if let Some(next) = self.next_page(forward) {
            self.show_page(next);
        }
    }

    fn next_page(&self, forward: bool) -> Option<usize> {
        let i = self.page_index.get();
        match forward {
            true => (i + 1 < self.page_count()).then_some(i + 1),
            false => i.checked_sub(1),
        }
    }

    /// The next or previous page, for a wheel pushed on past the edge of this one: read on as a
    /// continuous PDF reads, at the zoom it was read at, onto the top of the next page or the
    /// bottom of the one before; while presenting, fitted as every page shown then is.
    fn turn_page(self: &Rc<Self>, forward: bool) {
        if let Some(next) = self.next_page(forward) {
            let land = self.presenting.get().is_none().then_some(forward);
            self.show_page_landing(next, land);
        }
    }

    /// Go to page `i` without a history entry: Back itself, and the paging keys.
    pub fn show_page(self: &Rc<Self>, i: usize) {
        self.show_page_landing(i, None);
    }

    /// [`show_page`](Self::show_page), fitted, or with `land` at the zoom it is at, on the top of
    /// the page (`Some(true)`) or its bottom.
    fn show_page_landing(self: &Rc<Self>, i: usize, land: Option<bool>) {
        if i >= self.page_count() {
            return;
        }
        self.finish_label();
        self.page_index.set(i);
        self.selection.borrow_mut().clear();
        self.refresh();
        self.fill_props();
        match land {
            Some(top) => self.view.land(top),
            None => self.view.set_zoom(Zoom::Fit),
        }
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

    pub fn options(&self) -> DiagramConfig {
        self.options.get()
    }

    /// Draw with `options`: the config's, on opening and whenever a diagram's ring changed them.
    pub fn set_options(&self, options: DiagramConfig) {
        self.options.set(options);
        self.ring.set_options(options);
    }

    pub fn ring_shown(&self) -> bool {
        self.ring_shown.get()
    }

    /// Put the ring out or away, where the window last had one; putting it away puts the tool
    /// down with it. While presenting it stays away, and comes out as asked afterwards.
    pub fn show_ring(&self, shown: bool, at: Option<(f64, f64)>) {
        self.ring_shown.set(shown);
        // One waiting at a time: every tab switch asks again, and a ring put away before its
        // first frame must stay away.
        if let Some(tick) = self.ring_tick.take() {
            tick.remove();
        }
        if !shown || self.presenting.get().is_some() {
            self.ring.set_visible(false, at);
            return self.set_tool(Tool::Select);
        }
        // The ring finds its corner from the canvas's width, which a tab that has only just
        // opened does not have yet: it comes out on the first frame that has one.
        let ring = Rc::downgrade(&self.ring);
        let tick = self.overlay.add_tick_callback(move |overlay, _| {
            if overlay.width() == 0 {
                return glib::ControlFlow::Continue;
            }
            if let Some(ring) = ring.upgrade() {
                ring.set_visible(true, at);
            }
            glib::ControlFlow::Break
        });
        self.ring_tick.replace(Some(tick));
    }

    /// Where the reader dragged the ring, for the next tab's to open at.
    pub fn ring_at(&self) -> Option<(f64, f64)> {
        self.ring.at()
    }

    /// Presentation shows the page as a slide: fitted, the label being edited finished, nothing
    /// selected, the ring away with the tool in hand, and read only, so a click never draws on it
    /// or moves it. Leaving gives back the zoom, and the scroll on the page it began on, and the
    /// ring as it was, `at` where the window had it; the tool stays down.
    pub fn set_presenting(self: &Rc<Self>, on: bool, at: Option<(f64, f64)>) {
        match (on, self.presenting.get()) {
            (true, None) => {
                self.presenting.set(Some(self.place()));
                self.finish_label();
                self.select(Vec::new());
                self.show_ring(self.ring_shown.get(), at);
                self.view.set_read_only(true);
                self.view.set_zoom(Zoom::Fit);
            }
            (false, Some(before)) => {
                self.presenting.set(None);
                self.view.set_read_only(false);
                self.view
                    .set_zoom(before.zoom.map_or(Zoom::Fit, Zoom::Scale));
                if before.page == self.page_index.get() {
                    self.view.set_scroll((before.x, before.y));
                }
                self.show_ring(self.ring_shown.get(), at);
            }
            _ => {}
        }
    }

    /// Paint the page in the theme's colours, the other half's while inverted: the PDF's rule
    /// (`PdfTab::restyle`). Only the canvas changes; the file, the model and the Properties pane
    /// keep the file's colours, so the tab never becomes dirty.
    pub fn restyle(&self) {
        let dark = adw::StyleManager::default().is_dark() != self.inverted.get();
        self.view
            .set_tint(paint::Tint::onto(crate::theme::page_colours(dark)));
    }

    pub fn toggle_invert(&self) {
        self.inverted.set(!self.inverted.get());
        self.restyle();
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
                    fills: cells.iter().any(|c| c.takes_fill()),
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
            let index = self.page_index.get();
            match editor.page(index) {
                Ok(page) => Sheet::of(page, &shown(index, editor.file().pages().len())),
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
            Edit::Move { ids, delta } => self.move_cells(&ids, delta.x, delta.y),
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
            Edit::Points { id, points } => self.edit(|e, page| e.set_points(page, &id, &points)),
            Edit::LabelAt { id, at } => {
                let shown = self.view.sheet().unwrap_or_default();
                let route = shown.scene.route(&id).unwrap_or_default();
                self.edit(|e, page| e.move_label(page, &id, route, at));
            }
            Edit::End { id, source, end } => self.edit(|e, page| {
                let on = (end.0.as_deref(), end.1);
                e.set_end(page, &id, source, on, end.2.as_ref())
            }),
            Edit::Rotate { id, degrees } => {
                let value = props::rotation(degrees);
                self.edit(|e, page| e.set_style(page, &[id], "rotation", value.as_deref()));
            }
        }
    }

    /// Delete the selection as draw.io's Delete does: the edges on it stay, let go of it where
    /// they are drawn.
    pub fn delete(self: &Rc<Self>) {
        let ids = self.selection();
        if !ids.is_empty() {
            let shown = self.view.sheet().unwrap_or_default();
            self.edit(|e, page| e.remove(page, &ids, &shown.scene));
        }
    }

    /// Delete the selection with every edge on it, as draw.io's Delete All (`Ctrl+Delete`).
    pub fn delete_all(self: &Rc<Self>) {
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

    /// Group the selection, and select the group (draw.io's Group, `Ctrl+G`).
    pub fn group(self: &Rc<Self>) {
        let ids = self.selection();
        let mut group = None;
        self.edit(|e, page| {
            group = e.group(page, &ids)?;
            Ok(())
        });
        if let Some(group) = group {
            self.select(vec![group]);
        }
    }

    /// Ungroup the selection, and select what it let go of.
    pub fn ungroup(self: &Rc<Self>) {
        let ids = self.selection();
        let mut chosen = Vec::new();
        self.edit(|e, page| {
            chosen = e.ungroup(page, &ids)?;
            Ok(())
        });
        if !chosen.is_empty() {
            self.select(chosen);
        }
    }

    /// Copy the selection as draw.io's Copy does: its XML, as text (`clipboard::copy`). The
    /// next paste of it lands a grid step down and right, as draw.io's does.
    pub fn copy(self: &Rc<Self>) {
        let ids = self.selection();
        if ids.is_empty() {
            return;
        }
        let shown = self.view.sheet().unwrap_or_default();
        let xml = {
            let editor = self.editor.borrow();
            let Ok(page) = editor.page(self.page_index.get()) else {
                return;
            };
            accent_drawio::clipboard::copy(page, &ids, &shown.scene)
        };
        self.view.clipboard().set_text(&xml);
        self.pasted.replace((Some(xml), 0));
    }

    /// Cut the selection as draw.io's Cut does: copied, then removed, the edges on it let go
    /// of it rather than going with it. The next paste lands where the cells were.
    pub fn cut(self: &Rc<Self>) {
        let ids = self.selection();
        if ids.is_empty() {
            return;
        }
        self.copy();
        self.pasted.replace((None, 0));
        let shown = self.view.sheet().unwrap_or_default();
        self.edit(|e, page| e.remove(page, &ids, &shown.scene));
    }

    /// Paste text read off the clipboard as draw.io's Paste does (`EditorUi.pasteXml`): a
    /// diagram in it as its cells, anything else as a text cell at the top-left of the view;
    /// pasting the same text again lands a grid step further each time. Selects what it
    /// pasted.
    pub fn paste_text(self: &Rc<Self>, text: &str) {
        let grid = {
            let editor = self.editor.borrow();
            let page = editor.page(self.page_index.get());
            page.map_or(10.0, |p| p.grid_size())
        };
        let steps = {
            let mut pasted = self.pasted.borrow_mut();
            match pasted.0.as_deref() == Some(text) {
                true => pasted.1 += 1,
                false => *pasted = (Some(text.to_string()), 0),
            }
            pasted.1
        };
        let d = f64::from(steps) * grid;
        let mut chosen = Vec::new();
        let pages = accent_drawio::clipboard::pages_in(text);
        let count = self.page_count();
        match pages.is_empty() {
            false => self.edit(|e, page| {
                chosen = e.paste(page, &pages, d, d)?;
                Ok(())
            }),
            true => {
                let at = self.view.insert_point(grid);
                let (w, h) = text_size(&self.view, text);
                let rect = accent_drawio::Rect::new(at.x + d, at.y + d, w + grid, h + grid);
                let label = accent_drawio::clipboard::text_label(text);
                self.edit(|e, page| {
                    let style = "text;whiteSpace=wrap;html=1;";
                    chosen = vec![e.add_vertex(page, None, rect, style, &label)?];
                    Ok(())
                });
            }
        }
        if self.page_count() != count {
            self.emit(&self.on_pages);
        }
        if !chosen.is_empty() {
            self.select(chosen);
        }
    }

    /// Move the selection by `dx`, `dy` page units: the arrow keys.
    pub fn nudge(self: &Rc<Self>, dx: f64, dy: f64) {
        let ids = self.selection();
        if !ids.is_empty() {
            self.move_cells(&ids, dx, dy);
        }
    }

    /// Move `ids` by `dx`, `dy`, an edge letting go of a shape where the canvas draws its end.
    fn move_cells(self: &Rc<Self>, ids: &[CellId], dx: f64, dy: f64) {
        let shown = self.view.sheet().unwrap_or_default();
        self.edit(|e, page| e.move_cells(page, ids, dx, dy, &shown.scene));
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
        let editor = label::LabelEditor::open(
            &self.overlay,
            id,
            &markdown,
            place,
            size,
            self.font.borrow().as_deref(),
            self.spellcheck.get(),
        );
        *self.label.borrow_mut() = Some(editor.clone());
        self.place_label();
        // A click outside finishes the label, from an idle rather than the handler: leaving is
        // GTK moving the focus, and taking the editor off the canvas meanwhile left GTK walking
        // up from a widget that was gone, over and over (the freeze). Only the editor that left:
        // another may have opened by the time the idle runs.
        let later = {
            let (tab, left) = (Rc::downgrade(self), Rc::downgrade(&editor));
            move || {
                let (tab, left) = (tab.clone(), left.clone());
                glib::idle_add_local_once(move || {
                    let Some(tab) = tab.upgrade() else { return };
                    let open = tab.label.borrow().as_ref().map(Rc::downgrade);
                    if open.is_some_and(|open| open.ptr_eq(&left)) {
                        tab.finish_label();
                    }
                });
            }
        };
        let focus = gtk::EventControllerFocus::new();
        focus.connect_leave({
            let later = later.clone();
            move |_| later()
        });
        editor.view().add_controller(focus);
        label::wire_keys(
            &editor,
            glib::clone!(
                #[weak(rename_to = tab)]
                self,
                move || tab.finish_label()
            ),
        );
        // Space that takes no keyboard moves no focus, so the press on it is heard on the window
        // instead. Wired now that the editor is in place, and taken off again when it closes.
        label::wire_press(&editor, later);
        // Only now: a focus controller hears the focus leave only if it saw it come in, so a
        // click outside finishes the label only when the grab comes after the wiring.
        editor.focus();
    }

    /// Put the label editor back over its cell: where it opens, and again whenever the canvas
    /// scrolls or zooms under it. Nothing when no label is being edited.
    fn place_label(&self) {
        let Some(editor) = self.label.borrow().clone() else {
            return;
        };
        editor.place_at(
            self.view.to_widget(&editor.place),
            editor.font_size * self.view.scale(),
        );
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
    #[cfg(feature = "bench")]
    pub fn editing_label(&self) -> Option<CellId> {
        self.label.borrow().as_ref().map(|e| e.cell.clone())
    }

    /// Where the label editor sits on the canvas, how big its text is there and whether it holds
    /// the keyboard, for the drills that move the page under it.
    #[cfg(feature = "bench")]
    pub fn label_at(&self) -> Option<(f64, f64, f64, bool)> {
        let editor = self.label.borrow().clone()?;
        let (x, y, px) = editor.at();
        Some((x, y, px, editor.has_focus()))
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
    #[cfg(feature = "bench")]
    pub fn to_widget(&self, r: &accent_drawio::Rect) -> accent_drawio::Rect {
        self.view.to_widget(r)
    }

    #[cfg(feature = "bench")]
    pub fn scale(&self) -> f64 {
        self.view.scale()
    }

    /// What the Properties pane's Fill row says, for a drill.
    #[cfg(feature = "bench")]
    pub fn props_fill(&self) -> String {
        self.props.fill_value()
    }

    /// Whether the ring is on screen, which presentation keeps it from being.
    #[cfg(feature = "bench")]
    pub fn ring_visible(&self) -> bool {
        self.ring.widget().is_visible()
    }

    /// One frame of a move of `ids` by `delta` on the canvas, timed (`DiagramView::bench_move`).
    #[cfg(feature = "bench")]
    pub fn bench_move(&self, ids: &[CellId], delta: Option<Point>) -> (f64, f64) {
        self.view.bench_move(ids, delta)
    }

    /// Whether formulas are still being typeset.
    #[cfg(feature = "bench")]
    pub fn typesetting(&self) -> bool {
        self.view.typesetting()
    }

    /// A cell's frame on the page: what the selection box is drawn around.
    #[cfg(feature = "bench")]
    pub fn frame_of(&self, id: &str) -> Option<accent_drawio::Rect> {
        self.view.sheet()?.frame_of(id)
    }

    /// A cell's label as the label editor would show it.
    #[cfg(feature = "bench")]
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

    /// The model as it stands, for a save's worker to serialise: a clone costs a fraction of
    /// [`text`](Self::text), which the main thread would otherwise wait on.
    pub fn file(&self) -> File {
        self.editor.borrow().file().clone()
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

    /// The canvas's own menu on a secondary click, as a PDF page has one: the cell under the
    /// pointer is selected first unless it is already, and empty page lets the selection go, as
    /// draw.io's `mxPopupMenuHandler` does.
    fn wire_menu(self: &Rc<Self>) {
        let secondary = gtk::GestureClick::builder()
            .button(gdk::BUTTON_SECONDARY)
            .build();
        secondary.connect_pressed(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            // Every entry edits, and a presented page is read only.
            move |_, _, x, y| {
                if tab.presenting.get().is_none() {
                    tab.menu_at(x, y);
                }
            }
        ));
        self.view.add_controller(secondary);
    }

    /// The menu under the pointer, at widget `(x, y)`: the clipboard and Duplicate and Delete,
    /// Group or Ungroup where they apply, the order, and Edit Label for one cell; over nothing
    /// selected, Paste alone. Window actions, so the palette lists them and they can be rebound.
    fn menu_at(self: &Rc<Self>, x: f64, y: f64) {
        match self.view.cell_at(x, y) {
            Some(cell) if !self.selection.borrow().contains(&cell) => self.select(vec![cell]),
            Some(_) => {}
            None => self.select(Vec::new()),
        }
        let ids = self.selection();
        let menu = gio::Menu::new();
        let section = |actions: &[&str]| {
            let part = gio::Menu::new();
            for action in actions {
                part.append(Some(crate::actions::label_of(action)), Some(action));
            }
            if part.n_items() > 0 {
                menu.append_section(None, &part);
            }
        };
        if ids.is_empty() {
            section(&["win.diagram-paste"]);
        } else {
            section(&[
                "win.diagram-cut",
                "win.diagram-copy",
                "win.diagram-paste",
                "win.diagram-duplicate",
                "win.diagram-delete",
            ]);
            let groups = {
                let editor = self.editor.borrow();
                let page = editor.page(self.page_index.get());
                let group =
                    |id: &CellId| page.as_ref().is_ok_and(|p| p.children(id).next().is_some());
                ids.iter().any(group)
            };
            let mut grouping = Vec::new();
            if ids.len() > 1 {
                grouping.push("win.diagram-group");
            }
            if groups {
                grouping.push("win.diagram-ungroup");
            }
            section(&grouping);
            section(&["win.diagram-to-front", "win.diagram-to-back"]);
            if ids.len() == 1 {
                section(&["win.diagram-edit-label"]);
            }
        }
        // Parented to the tab's box, not the canvas, which allocates itself: a popover on it
        // would never be presented again (DESIGN.md, States).
        let Some(host) = self.page.child().downcast::<gtk::Box>().ok() else {
            return;
        };
        let at = gtk::graphene::Point::new(x as f32, y as f32);
        let at = self.view.compute_point(&host, &at).unwrap_or(at);
        let anchor = gdk::Rectangle::new(at.x() as i32, at.y() as i32, 1, 1);
        crate::widgets::popup_menu(&host, &menu, Some(anchor));
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
                let presenting = tab.presenting.get().is_some();
                match key {
                    // A presented page is a slide, which Space, the arrows and the paging keys
                    // read on through, as they do a presented PDF, and nothing edits: a chord goes
                    // on to the window, and any other key that reaches the canvas does nothing,
                    // an arrow included, which would move the keyboard off the page.
                    gdk::Key::space if presenting && shift => tab.run("win.diagram-previous-page"),
                    gdk::Key::space | gdk::Key::Right | gdk::Key::Page_Down
                        if presenting && !ctrl =>
                    {
                        tab.run("win.diagram-next-page")
                    }
                    gdk::Key::Left | gdk::Key::Page_Up if presenting && !ctrl => {
                        tab.run("win.diagram-previous-page")
                    }
                    _ if presenting && (ctrl || alt) => return glib::Propagation::Proceed,
                    _ if presenting => {}
                    gdk::Key::z | gdk::Key::Z if ctrl && shift => tab.run("win.diagram-redo"),
                    gdk::Key::z if ctrl => tab.run("win.diagram-undo"),
                    gdk::Key::y if ctrl => tab.run("win.diagram-redo"),
                    gdk::Key::a if ctrl => tab.run("win.diagram-select-all"),
                    gdk::Key::c if ctrl => tab.run("win.diagram-copy"),
                    gdk::Key::x if ctrl => tab.run("win.diagram-cut"),
                    gdk::Key::v if ctrl => tab.run("win.diagram-paste"),
                    gdk::Key::Delete | gdk::Key::BackSpace if ctrl => {
                        tab.run("win.diagram-delete-all")
                    }
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

    /// A plain wheel pushed on past the top or bottom of the page turns it ([`Overshoot`]). Ahead
    /// of the scrolled window, which scrolls whatever this passes on.
    fn wire_wheel(self: &Rc<Self>) {
        let overshoot = Rc::new(Cell::new(Overshoot::default()));
        let wheel = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
        wheel.connect_scroll(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            #[strong]
            overshoot,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |wheel, dx, dy| {
                // Ctrl zooms (`zoom::zoom_on_wheel`), and Shift and a sideways swipe scroll across.
                let held = gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::SHIFT_MASK;
                if wheel.current_event_state().intersects(held) || dx.abs() >= dy.abs() {
                    return glib::Propagation::Proceed;
                }
                let room = tab.view.has_room(dy > 0.0);
                let ahead = tab.next_page(dy > 0.0).is_some();
                let swipe = wheel.unit() == gdk::ScrollUnit::Surface;
                let mut push = overshoot.get();
                if let Some(forward) = push.scroll(dy, room, swipe) {
                    tab.turn_page(forward);
                }
                overshoot.set(push);
                // At the edge only the margin is left for the scrolled window to scroll, and a
                // swipe it saw begin would be its own to the end (its `smooth_scroll`), never
                // reaching here again: the push is kept while there is a page to turn to.
                match room || !ahead {
                    true => glib::Propagation::Proceed,
                    false => glib::Propagation::Stop,
                }
            }
        ));
        // The fingers left the touchpad: the next swipe may turn the page again.
        wheel.connect_scroll_end(move |_| overshoot.set(Overshoot::default()));
        self.view.add_controller(wheel);
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

    /// The ring's outer orbit changed how the tools draw.
    pub fn connect_options(&self, f: impl Fn(&Rc<DiagramTab>) + 'static) {
        *self.on_options.borrow_mut() = Some(Rc::new(f));
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

/// The size `text` takes as a pasted text cell's label, as Pango lays it out in draw.io's
/// default font, wrapped at draw.io's widest pasted text (`EditorUi.maxTextWidth`), a little
/// room around it (`Graph.updateCellSize`).
fn text_size(widget: &impl IsA<gtk::Widget>, text: &str) -> (f64, f64) {
    /// Pasted text wider than this wraps (`EditorUi.maxTextWidth`).
    const WIDEST: i32 = 520;
    /// draw.io's `spacing` on each side of a label.
    const SPACING: f64 = 2.0;
    let layout = widget.create_pango_layout(Some(text));
    let mut font = gtk::pango::FontDescription::from_string("Helvetica");
    font.set_absolute_size(12.0 * f64::from(gtk::pango::SCALE));
    layout.set_font_description(Some(&font));
    if layout.pixel_size().0 > WIDEST {
        layout.set_width(WIDEST * gtk::pango::SCALE);
        layout.set_wrap(gtk::pango::WrapMode::WordChar);
    }
    let (w, h) = layout.pixel_size();
    (f64::from(w) + 2.0 * SPACING, f64::from(h) + 2.0 * SPACING)
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

/// Page `index` of `pages` shown now: what labels with placeholders fill `%pagenumber%` and
/// `%date%` in from.
fn shown(index: usize, pages: usize) -> accent_drawio::Context {
    let now = chrono::Local::now();
    accent_drawio::Context {
        page: index,
        pages,
        now: Some(accent_drawio::Now {
            unix_ms: now.timestamp_millis(),
            offset_minutes: now.offset().local_minus_utc() / 60,
        }),
    }
}
