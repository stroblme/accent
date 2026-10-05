//! A draw.io diagram in a tab: the model, the page on screen, the selection, and the hooks the
//! window listens on.
//!
//! The format work is `accent-drawio`'s; the canvas (`view.rs`) paints a page and turns gestures
//! into [`Edit`]s. This is where an edit is applied — through [`DiagramTab::edit`], the one door
//! every change goes through, which is what makes each one an undo step, marks the tab dirty
//! and schedules the autosave. Its label editing is in `label`, the Properties pane's half in
//! `props`, and the canvas's menu, keys and wheel in `input`.

pub mod embed;
pub mod export;
mod geometry;
mod input;
mod label;
mod layers;
mod math;
mod paint;
mod props;
mod render;
mod tools;
mod view;
mod web;
mod window;

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use accent_core::config::{DiagramConfig, DiagramPlace};
use accent_core::fs::{Digest, Etag};
use accent_drawio::{CellId, Editor, File, Point};
use adw::prelude::*;
use gtk::{gio, glib};

use crate::editor::{SaveState, Saves};
use crate::widgets::{Debounce, Hook};
use geometry::{Sheet, Zoom};
pub use tools::Tool;
use view::{DiagramView, Edit};

type TabHook = Hook<dyn Fn(&Rc<DiagramTab>)>;
type TextHook = Hook<dyn Fn(&str)>;

pub struct DiagramTab {
    key: RefCell<String>,
    path: RefCell<PathBuf>,
    pub page: adw::TabPage,
    /// "This diagram changed on disk", while the tab holds edits the file does not.
    banner: adw::Banner,
    /// "This diagram links images from the web", while it does and the reader has not said Load.
    web_banner: adw::Banner,
    /// Whether the pictures the diagram links from the web are downloaded and drawn: the reader
    /// said Load, now or in another session (the window's `web_images`).
    web: Cell<bool>,
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
    /// The write an edit schedules, which a burst of edits shares.
    autosave: Debounce,
    /// A watch on the file itself, for a diagram from outside the vault, which no vault watcher
    /// covers. `None` for everything inside a vault, which the worker already reports on.
    monitor: RefCell<Option<gio::FileMonitor>>,
    on_zoom: TabHook,
    on_page: TabHook,
    /// Fired just before a page switch the reader asked for, so the pane can record where they
    /// were (Back).
    on_jump: TabHook,
    on_pages: TabHook,
    on_selection: TabHook,
    on_history: TabHook,
    on_autosave: TabHook,
    on_image: TabHook,
    on_banner: TabHook,
    on_options: TabHook,
    /// The web banner's Load.
    on_web: TabHook,
    /// Something to tell the reader in a toast: what was copied, or that there was nothing to
    /// paste.
    on_toast: TextHook,
}

/// A new tab of `tabs` showing `file` as it was read, at its etag and digest, where the session
/// last left it.
pub fn open(
    key: &str,
    path: &Path,
    title: &str,
    tooltip: &str,
    tabs: &adw::TabView,
    (file, etag, digest): (File, Etag, Digest),
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
    let web_banner = adw::Banner::builder()
        .title("This diagram links images from the web")
        .button_label("Load")
        .build();
    let host = gtk::Box::new(gtk::Orientation::Vertical, 0);
    host.append(&banner);
    host.append(&web_banner);
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
        web_banner,
        web: Cell::new(false),
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
        save: SaveState::at(etag, digest),
        autosave: Debounce::new(crate::editor::AUTOSAVE),
        monitor: RefCell::new(None),
        on_zoom: Hook::default(),
        on_page: Hook::default(),
        on_jump: Hook::default(),
        on_pages: Hook::default(),
        on_selection: Hook::default(),
        on_history: Hook::default(),
        on_autosave: Hook::default(),
        on_image: Hook::default(),
        on_banner: Hook::default(),
        on_options: Hook::default(),
        on_web: Hook::default(),
        on_toast: Hook::default(),
    });
    if has_math {
        tab.view.set_typesetter(math::shared());
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
            tab.on_zoom.emit(&tab)
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
            tab.on_options.emit(&tab);
        }
    ));
    tab.banner.connect_button_clicked(glib::clone!(
        #[weak]
        tab,
        move |_| tab.on_banner.emit(&tab)
    ));
    tab.web_banner.connect_button_clicked(glib::clone!(
        #[weak]
        tab,
        move |_| tab.on_web.emit(&tab)
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
        page_names(self.editor.borrow().file())
    }

    /// Show page `i`, as a jump the pane's history records.
    pub fn goto_page(self: &Rc<Self>, i: usize) {
        if i >= self.page_count() || i == self.page_index.get() {
            return;
        }
        self.on_jump.emit(self);
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
        // A layer picked on one page is not one of another's.
        self.editor.borrow_mut().set_current_layer(None);
        self.refresh();
        self.fill_props();
        match land {
            Some(top) => self.view.land(top),
            None => self.view.set_zoom(Zoom::Fit),
        }
        self.on_page.emit(self);
        self.on_selection.emit(self);
    }

    pub fn tool(&self) -> Tool {
        self.tool.get()
    }

    /// Whether the page shown has a layer new cells can go into: one neither locked nor hidden.
    pub fn can_insert(&self) -> bool {
        let editor = self.editor.borrow();
        let page = editor.page(self.page_index.get());
        page.is_ok_and(|p| p.default_parent().is_some())
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
        self.on_selection.emit(self);
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
            self.on_pages.emit(self);
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

    /// Whether the pictures the diagram links from the web are drawn.
    pub fn web_allowed(&self) -> bool {
        self.web.get()
    }

    /// Draw the pictures the diagram links from the web, or not: the window says so on opening,
    /// from what the reader said before, and when they say Load.
    pub fn allow_web(self: &Rc<Self>, on: bool) {
        self.web.set(on);
        self.view.set_web(on);
        self.sync_web();
    }

    /// The web banner shown while the diagram links pictures from the web the reader has not
    /// said Load for; once they have, whichever of them are not downloaded yet are, and the
    /// page is drawn again when they come in. Asked on opening, on Load and after every change.
    fn sync_web(self: &Rc<Self>) {
        let urls = web::urls(self.editor.borrow().file());
        self.web_banner
            .set_revealed(!urls.is_empty() && !self.web.get());
        let missing: Vec<String> = urls
            .into_iter()
            .filter(|url| !web::path(url).is_file())
            .collect();
        if !self.web.get() || missing.is_empty() {
            return;
        }
        let tab = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let (fetched, failed) = web::fetch(&missing).await;
            let Some(tab) = tab.upgrade() else { return };
            if fetched > 0 {
                tab.refresh();
            }
            if failed > 0 {
                let what = if failed == 1 { "image" } else { "images" };
                tab.toast(&format!("Cannot load {failed} {what} from the web"));
            }
        });
    }

    /// After any change to the model, an undo included.
    fn changed(self: &Rc<Self>) {
        self.save.edits.set(self.save.edits.get() + 1);
        self.save.modified.set(true);
        self.set_title();
        self.refresh();
        self.sync_web();
        self.fill_props();
        self.on_history.emit(self);
        self.on_selection.emit(self);
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
        // a scroll finishes an open label, and that is an edit that comes back here. What is
        // hidden or locked now goes too, a layer hidden or locked under it, as nothing hidden or
        // locked is picked.
        let selection = {
            let locked: std::collections::HashSet<&str> = sheet
                .scene
                .prims
                .iter()
                .filter(|p| p.locked())
                .map(|p| p.cell())
                .collect();
            let mut selection = self.selection.borrow_mut();
            selection.retain(|id| {
                let there = sheet.frame_of(id).is_some() && sheet.page.is_shown(id);
                there && !locked.contains(id.as_str())
            });
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
                self.on_zoom.emit(self);
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
            self.on_pages.emit(self);
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

    /// A page rectangle in the canvas's own coordinates.
    #[cfg(feature = "bench")]
    pub fn to_widget(&self, r: &accent_drawio::Rect) -> accent_drawio::Rect {
        self.view.to_widget(r)
    }

    #[cfg(feature = "bench")]
    pub fn scale(&self) -> f64 {
        self.view.scale()
    }

    /// Whether the web banner is out, for a drill.
    #[cfg(feature = "bench")]
    pub fn web_banner_shown(&self) -> bool {
        self.web_banner.is_revealed()
    }

    /// The web banner's Load, pressed by a drill.
    #[cfg(feature = "bench")]
    pub fn press_load(&self) {
        self.web_banner.emit_by_name::<()>("button-clicked", &[]);
    }

    /// The Layers group's rows as a drill reads them, the topmost first.
    #[cfg(feature = "bench")]
    pub fn layer_rows(&self) -> Vec<String> {
        self.props.layers().describe()
    }

    /// The Layers group's row of layer `id`.
    #[cfg(feature = "bench")]
    pub fn layer_row(&self, id: &str) -> Option<adw::EntryRow> {
        self.props.layers().row_of(id)
    }

    /// What a click on the middle of cell `id` picks.
    #[cfg(feature = "bench")]
    pub fn pick(&self, id: &str) -> Option<CellId> {
        let r = self.view.to_widget(&self.frame_of(id)?);
        self.view.cell_at(r.x + r.w / 2.0, r.y + r.h / 2.0)
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
        self.on_pages.emit(self);
        self.goto_page(index);
    }

    pub fn rename_page(self: &Rc<Self>, name: &str) {
        let name = name.to_string();
        self.edit(|e, page| e.rename_page(page, &name));
        self.on_pages.emit(self);
    }

    pub fn delete_page(self: &Rc<Self>) {
        self.edit(|e, page| e.delete_page(page));
        self.on_pages.emit(self);
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

    /// What typesets this diagram's formulas, `None` for a diagram without any.
    pub fn typesetter(&self) -> Option<Rc<math::Typesetter>> {
        self.view.typesetter()
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
    pub fn reload(self: &Rc<Self>, file: File, etag: Etag, digest: Digest) {
        let last = file.pages.len().saturating_sub(1);
        *self.editor.borrow_mut() = Editor::new(file);
        self.page_index.set(self.page_index.get().min(last));
        self.save.etag.set(Some(etag));
        self.save.digest.set(Some(digest));
        self.save.modified.set(false);
        self.save.disk_changed.set(false);
        self.banner.set_revealed(false);
        self.set_title();
        self.refresh();
        self.sync_web();
        self.fill_props();
        self.on_pages.emit(self);
        self.on_history.emit(self);
        self.on_selection.emit(self);
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

    /// Write the file a second after an edit, once for a burst of them.
    fn save_soon(self: &Rc<Self>) {
        self.autosave.call_once(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move || tab.on_autosave.emit(&tab)
        ));
    }

    pub fn connect_zoom(&self, f: impl Fn(&Rc<DiagramTab>) + 'static) {
        self.on_zoom.set(Rc::new(f));
    }

    pub fn connect_page(&self, f: impl Fn(&Rc<DiagramTab>) + 'static) {
        self.on_page.set(Rc::new(f));
    }

    pub fn connect_jump(&self, f: impl Fn(&Rc<DiagramTab>) + 'static) {
        self.on_jump.set(Rc::new(f));
    }

    pub fn connect_pages(&self, f: impl Fn(&Rc<DiagramTab>) + 'static) {
        self.on_pages.set(Rc::new(f));
    }

    pub fn connect_selection(&self, f: impl Fn(&Rc<DiagramTab>) + 'static) {
        self.on_selection.set(Rc::new(f));
    }

    pub fn connect_history(&self, f: impl Fn(&Rc<DiagramTab>) + 'static) {
        self.on_history.set(Rc::new(f));
    }

    pub fn connect_autosave(&self, f: impl Fn(&Rc<DiagramTab>) + 'static) {
        self.on_autosave.set(Rc::new(f));
    }

    /// The Image tool was picked: the window asks for a file.
    pub fn connect_image(&self, f: impl Fn(&Rc<DiagramTab>) + 'static) {
        self.on_image.set(Rc::new(f));
    }

    /// The ring's outer orbit changed how the tools draw.
    pub fn connect_options(&self, f: impl Fn(&Rc<DiagramTab>) + 'static) {
        self.on_options.set(Rc::new(f));
    }

    /// What to tell the reader in a toast.
    pub fn connect_toast(&self, f: impl Fn(&str) + 'static) {
        self.on_toast.set(Rc::new(f));
    }

    fn toast(&self, text: &str) {
        if let Some(f) = self.on_toast.get() {
            f(text);
        }
    }

    /// The web banner's Load.
    pub fn connect_web(&self, f: impl Fn(&Rc<DiagramTab>) + 'static) {
        self.on_web.set(Rc::new(f));
    }

    /// The changed-on-disk banner's button.
    pub fn connect_banner(&self, f: impl Fn(&Rc<DiagramTab>) + 'static) {
        self.on_banner.set(Rc::new(f));
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
        self.on_image.emit(self);
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

/// Whether the picture at `url` on the web has been downloaded, for a drill.
#[cfg(feature = "bench")]
pub fn web_downloaded(url: &str) -> bool {
    web::path(url).is_file()
}

/// The names of `file`'s pages as the Outline pane lists them: a page with none is "Page N".
fn page_names(file: &File) -> Vec<String> {
    file.pages
        .iter()
        .enumerate()
        .map(|(i, p)| match p.name() {
            "" => format!("Page {}", i + 1),
            name => name.to_string(),
        })
        .collect()
}

/// The page of `file` called `name`, as `![[x.drawio#name]]` and `[[x.drawio#name]]` name one.
pub fn page_named(file: &File, name: &str) -> Option<usize> {
    page_names(file).iter().position(|n| n == name.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_is_found_by_its_name_or_its_place() {
        let file = File::from_bytes(
            br#"<mxfile><diagram name="Page-1"><mxGraphModel><root/></mxGraphModel></diagram>
            <diagram><mxGraphModel><root/></mxGraphModel></diagram></mxfile>"#,
        )
        .unwrap();
        assert_eq!(page_named(&file, "Page-1"), Some(0));
        assert_eq!(page_named(&file, "Page 2"), Some(1));
        assert_eq!(page_named(&file, "Page-2"), None);
    }
}
