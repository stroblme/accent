//! The canvas a diagram is drawn and edited on: a scrollable widget painting one page's display
//! list, with the selection, its handles and whatever a drag is doing drawn over it.
//!
//! It never changes the diagram. A gesture ends in an [`Edit`] handed to the tab, which applies
//! it to the model and hands back a new [`Sheet`]; so one gesture is one undo step, and the
//! widget is a picture of the page plus a pointer. The scrollable skeleton is the PDF view's.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use accent_drawio::{CellId, Color, Point, Rect};
use adw::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene, gsk};

use super::geometry::{
    self, DEFAULT_SIZE, DRAG_SLOP, Frame, HANDLE, Handle, Sheet, TOLERANCE, Zoom,
};
use super::paint::{self, Cache};
use super::tools::Tool;
use crate::theme;

/// What a gesture on the canvas asks of the diagram.
#[derive(Debug, Clone, PartialEq)]
pub enum Edit {
    /// Replace the selection.
    Select(Vec<CellId>),
    Move {
        ids: Vec<CellId>,
        delta: Point,
    },
    Resize {
        id: CellId,
        rect: Rect,
    },
    /// A new shape or text box at `rect`.
    Add {
        tool: Tool,
        rect: Rect,
    },
    /// A new edge; an end with no cell dangles at its point.
    Connect {
        source: (Option<CellId>, Point),
        target: (Option<CellId>, Point),
    },
    /// Edit this cell's label.
    Label(CellId),
}

/// A drag under way, in page units.
#[derive(Debug, Clone)]
enum Drag {
    /// Moving `ids`, whose frames start at `origin`; a release that did not move selects
    /// `click` instead, when there is one (a click into a selected group).
    Move {
        from: Point,
        ids: Vec<CellId>,
        click: Option<CellId>,
        origin: Point,
    },
    Resize {
        from: Point,
        id: CellId,
        handle: Handle,
        rect: Rect,
    },
    Band {
        from: Point,
        add: bool,
    },
    Draw {
        tool: Tool,
        from: Point,
    },
    Connect {
        source: Option<CellId>,
        from: Point,
    },
    /// Dragging the page itself, from this scroll position.
    Pan {
        scroll: (f64, f64),
    },
}

glib::wrapper! {
    pub struct DiagramView(ObjectSubclass<imp::DiagramView>)
        @extends gtk::Widget,
        @implements gtk::Scrollable, gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for DiagramView {
    fn default() -> Self {
        Self::new()
    }
}

impl DiagramView {
    pub fn new() -> DiagramView {
        let view: DiagramView = glib::Object::new();
        // Bubble phase, ahead of the scrolled window's own controller, as the PDF view does.
        crate::zoom::zoom_on_wheel(
            &view,
            gtk::PropagationPhase::Bubble,
            glib::clone!(
                #[weak]
                view,
                move |out, at| view.zoom_step(out, at)
            ),
        );
        view
    }

    /// Put a page on the canvas: a new display list after an edit, a page switch or a reload.
    /// What is on screen stays where it is, even when the drawing grew past the edge it had.
    pub fn show(&self, sheet: Sheet) {
        let imp = self.imp();
        let before = imp.frame.get();
        imp.cache.forget();
        *imp.sheet.borrow_mut() = Some(Rc::new(sheet));
        self.relayout();
        let after = imp.frame.get();
        if imp.laid_out.get() && before.scale == after.scale {
            let (x, y) = self.scroll();
            self.set_scroll((x + after.x - before.x, y + after.y - before.y));
        }
        self.queue_draw();
    }

    pub fn sheet(&self) -> Option<Rc<Sheet>> {
        self.imp().sheet.borrow().clone()
    }

    pub fn set_selection(&self, ids: &[CellId]) {
        *self.imp().selection.borrow_mut() = ids.to_vec();
        self.queue_draw();
    }

    pub fn selection(&self) -> Vec<CellId> {
        self.imp().selection.borrow().clone()
    }

    pub fn set_tool(&self, tool: Tool) {
        self.imp().tool.set(tool);
        self.set_cursor_from_name(None);
    }

    /// Space held: a drag moves the page rather than what is on it.
    pub fn set_panning(&self, on: bool) {
        self.imp().panning.set(on);
        self.set_cursor_from_name(on.then_some("grab"));
    }

    pub fn zoom(&self) -> Zoom {
        self.imp().zoom.get()
    }

    /// The scale on screen, whatever the zoom mode.
    pub fn scale(&self) -> f64 {
        self.imp().frame.get().scale
    }

    /// Fit the page, or zoom to a scale keeping the middle of the view where it is. Fitting
    /// also brings the page to the middle, which is what a reset is for.
    pub fn set_zoom(&self, zoom: Zoom) {
        let (w, h) = (f64::from(self.width()), f64::from(self.height()));
        match zoom {
            Zoom::Fit => {
                self.imp().zoom.set(zoom);
                self.relayout();
                self.centre_page();
                self.zoomed();
            }
            Zoom::Scale(_) => self.zoom_around(zoom, Some((w / 2.0, h / 2.0))),
        }
    }

    /// One zoom step in or out, keeping what is under `at` (widget coordinates) in place.
    pub fn zoom_step(&self, out: bool, at: Option<(f64, f64)>) {
        let from = self.scale();
        let to = geometry::clamp_scale(crate::zoom::stepped_zoom(from, out));
        self.zoom_around(Zoom::Scale(to), at);
    }

    fn zoom_around(&self, zoom: Zoom, at: Option<(f64, f64)>) {
        let (w, h) = (f64::from(self.width()), f64::from(self.height()));
        let (x, y) = at.unwrap_or((w / 2.0, h / 2.0));
        let under = self.page_at(x, y);
        self.imp().zoom.set(zoom);
        self.relayout();
        let c = self.imp().frame.get().to_content(under);
        self.set_scroll((c.x - x, c.y - y));
        self.zoomed();
    }

    /// The readout for the status bar: `Fit` or a percentage.
    pub fn zoom_label(&self) -> String {
        match self.zoom() {
            Zoom::Fit => "Fit".to_string(),
            Zoom::Scale(s) => format!("{:.0} %", s * 100.0),
        }
    }

    pub fn scroll(&self) -> (f64, f64) {
        let value = |a: Option<gtk::Adjustment>| a.map_or(0.0, |a| a.value());
        (value(self.hadjustment()), value(self.vadjustment()))
    }

    pub fn set_scroll(&self, (x, y): (f64, f64)) {
        if let Some(a) = self.hadjustment() {
            a.set_value(x);
        }
        if let Some(a) = self.vadjustment() {
            a.set_value(y);
        }
    }

    /// Where the diagram is left: the zoom and scroll to come back to. Applied once the widget
    /// has a size, which is when a scroll position means anything.
    pub fn restore(&self, zoom: Zoom, scroll: (f64, f64)) {
        self.imp().zoom.set(zoom);
        self.imp().pending_scroll.set(Some(scroll));
        self.relayout();
    }

    /// A page rectangle in widget coordinates.
    pub fn to_widget(&self, r: &Rect) -> Rect {
        let c = self.imp().frame.get().rect(r);
        let (sx, sy) = self.scroll();
        c.translate(-sx, -sy)
    }

    /// Scroll just enough to have `r` (page units) on screen.
    pub fn reveal(&self, r: &Rect) {
        let c = self.imp().frame.get().rect(r);
        let (w, h) = (f64::from(self.width()), f64::from(self.height()));
        let (mut x, mut y) = self.scroll();
        x = x.min(c.x - HANDLE).max(c.right() + HANDLE - w);
        y = y.min(c.y - HANDLE).max(c.bottom() + HANDLE - h);
        self.set_scroll((x, y));
    }

    /// Typeset labels with formulas here, and paint again whenever it has finished some.
    pub fn set_typesetter(&self, typesetter: Rc<super::math::Typesetter>) {
        let weak = self.downgrade();
        typesetter.connect_ready(move || {
            if let Some(view) = weak.upgrade() {
                view.queue_draw();
            }
        });
        *self.imp().typesetter.borrow_mut() = Some(typesetter);
    }

    /// Whether formulas are still being typeset: the drill waits for them.
    pub fn typesetting(&self) -> bool {
        self.imp()
            .typesetter
            .borrow()
            .as_ref()
            .is_some_and(|t| t.busy())
    }

    pub fn connect_edit(&self, f: impl Fn(Edit) + 'static) {
        *self.imp().on_edit.borrow_mut() = Some(Box::new(f));
    }

    pub fn connect_zoom(&self, f: impl Fn() + 'static) {
        *self.imp().on_zoom.borrow_mut() = Some(Box::new(f));
    }

    fn emit(&self, edit: Edit) {
        if let Some(f) = self.imp().on_edit.borrow().as_ref() {
            f(edit);
        }
    }

    fn zoomed(&self) {
        self.imp().cache.forget();
        self.queue_draw();
        if let Some(f) = self.imp().on_zoom.borrow().as_ref() {
            f();
        }
    }

    /// The page point under a widget coordinate.
    fn page_at(&self, x: f64, y: f64) -> Point {
        let (sx, sy) = self.scroll();
        self.imp().frame.get().to_page(Point::new(x + sx, y + sy))
    }

    fn centre_page(&self) {
        let Some(sheet) = self.sheet() else { return };
        let (pw, ph) = sheet.scene.page_size;
        let c = self
            .imp()
            .frame
            .get()
            .to_content(Point::new(pw / 2.0, ph / 2.0));
        let (w, h) = (f64::from(self.width()), f64::from(self.height()));
        self.set_scroll((c.x - w / 2.0, c.y - h / 2.0));
    }

    /// Work out the frame for the current size and zoom, and tell the scrollbars.
    fn relayout(&self) {
        let imp = self.imp();
        let (w, h) = (f64::from(self.width()), f64::from(self.height()));
        let Some(sheet) = self.sheet() else { return };
        if w <= 1.0 || h <= 1.0 {
            return;
        }
        let scale = match imp.zoom.get() {
            Zoom::Fit => geometry::fit_scale(sheet.scene.page_size, (w, h)),
            Zoom::Scale(s) => geometry::clamp_scale(s),
        };
        let (frame, size) = geometry::frame(sheet.extent, scale, (w, h));
        if imp.frame.replace(frame).scale != frame.scale {
            imp.cache.forget();
        }
        configure(self.hadjustment(), size.0, w);
        configure(self.vadjustment(), size.1, h);
        if !imp.laid_out.replace(true) && imp.pending_scroll.get().is_none() {
            self.centre_page();
        }
        if let Some(scroll) = imp.pending_scroll.take() {
            self.set_scroll(scroll);
        }
        self.queue_draw();
    }

    /// Where a press at widget `(x, y)` starts a drag, if it starts one.
    fn start_drag(&self, x: f64, y: f64, shift: bool) -> Option<Drag> {
        let imp = self.imp();
        let sheet = self.sheet()?;
        let frame = imp.frame.get();
        let p = self.page_at(x, y);
        if imp.panning.get() {
            return Some(Drag::Pan {
                scroll: self.scroll(),
            });
        }
        let selection = self.selection();
        let tolerance = TOLERANCE / frame.scale;
        match imp.tool.get() {
            Tool::Select => {
                if let [id] = selection.as_slice()
                    && let Some(rect) = sheet.rect(id).filter(|_| !sheet.is_pinned(id))
                {
                    let content = frame.to_content(p);
                    if let Some(handle) = geometry::handle_at(&frame.rect(&rect), content, HANDLE) {
                        return Some(Drag::Resize {
                            from: p,
                            id: id.clone(),
                            handle,
                            rect,
                        });
                    }
                }
                let Some(pick) = sheet.pick(p, tolerance, &selection) else {
                    if !shift {
                        self.emit(Edit::Select(Vec::new()));
                    }
                    return Some(Drag::Band {
                        from: p,
                        add: shift,
                    });
                };
                if shift {
                    let toggled = pick.held.clone().unwrap_or(pick.cell.clone());
                    let mut next = selection.clone();
                    match next.iter().position(|s| *s == toggled) {
                        Some(i) => {
                            next.remove(i);
                        }
                        None => next.push(pick.cell),
                    }
                    self.emit(Edit::Select(next));
                    return None;
                }
                let (ids, click) = match &pick.held {
                    Some(held) => (selection, (pick.cell != *held).then_some(pick.cell)),
                    None => {
                        self.emit(Edit::Select(vec![pick.cell.clone()]));
                        (vec![pick.cell], None)
                    }
                };
                let ids: Vec<CellId> = ids.into_iter().filter(|id| !sheet.is_pinned(id)).collect();
                let origin = ids
                    .iter()
                    .filter_map(|id| sheet.frame_of(id))
                    .reduce(|a, b| a.union(&b))
                    .map_or(p, |r| Point::new(r.x, r.y));
                Some(Drag::Move {
                    from: p,
                    ids,
                    click,
                    origin,
                })
            }
            tool if tool.draws_box() => Some(Drag::Draw { tool, from: p }),
            Tool::Connector => Some(Drag::Connect {
                source: sheet.vertex_at(p, tolerance),
                from: p,
            }),
            _ => None,
        }
    }

    /// A move of the selection from `from` to `to`, on the grid unless `free`.
    fn move_delta(&self, from: Point, to: Point, origin: Point, free: bool) -> Point {
        let raw = Point::new(to.x - from.x, to.y - from.y);
        match (free, self.sheet()) {
            (false, Some(sheet)) => geometry::snap_move(origin, raw, sheet.grid),
            _ => raw,
        }
    }

    fn grid(&self, free: bool) -> Option<f64> {
        self.sheet().filter(|_| !free).map(|s| s.grid)
    }

    /// What a drag that ends at widget `(x, y)` asks for.
    fn end_drag(&self, drag: Drag, x: f64, y: f64, moved: bool, free: bool) {
        let p = self.page_at(x, y);
        let Some(sheet) = self.sheet() else { return };
        let snap = |q: Point| match self.grid(free) {
            Some(g) => Point::new(geometry::snap(q.x, g), geometry::snap(q.y, g)),
            None => q,
        };
        match drag {
            Drag::Move {
                from,
                ids,
                click,
                origin,
            } => match (moved, click) {
                (true, _) if !ids.is_empty() => self.emit(Edit::Move {
                    ids,
                    delta: self.move_delta(from, p, origin, free),
                }),
                (false, Some(cell)) => self.emit(Edit::Select(vec![cell])),
                _ => {}
            },
            Drag::Resize {
                from,
                id,
                handle,
                rect,
            } if moved => {
                let delta = Point::new(p.x - from.x, p.y - from.y);
                let rect = geometry::resize_by(&rect, handle, delta, self.grid(free));
                self.emit(Edit::Resize { id, rect });
            }
            Drag::Band { from, add } if moved => {
                let mut ids = sheet.band(Rect::from_corners(from, p));
                if add {
                    let mut all = self.selection();
                    all.extend(
                        ids.into_iter()
                            .filter(|id| !all.contains(id))
                            .collect::<Vec<_>>(),
                    );
                    ids = all;
                }
                self.emit(Edit::Select(ids));
            }
            Drag::Draw { tool, from } => {
                let rect = match moved {
                    true => Rect::from_corners(snap(from), snap(p)),
                    false => {
                        let c = snap(from);
                        let (w, h) = DEFAULT_SIZE;
                        Rect::new(c.x - w / 2.0, c.y - h / 2.0, w, h)
                    }
                };
                if rect.w >= 1.0 && rect.h >= 1.0 {
                    self.emit(Edit::Add { tool, rect });
                }
            }
            Drag::Connect { source, from } if moved => {
                let tolerance = TOLERANCE / self.scale();
                let target = sheet
                    .vertex_at(p, tolerance)
                    .filter(|t| source.as_ref() != Some(t));
                self.emit(Edit::Connect {
                    source: (source, from),
                    target: (target, p),
                });
            }
            _ => {}
        }
    }

    /// The pointer over the canvas: say with the cursor what a press there would do.
    fn hover(&self, x: f64, y: f64) {
        let imp = self.imp();
        if imp.panning.get() || imp.drag.borrow().is_some() {
            return;
        }
        let Some(sheet) = self.sheet() else { return };
        let frame = imp.frame.get();
        let p = self.page_at(x, y);
        let name = match imp.tool.get() {
            Tool::Select => {
                let selection = self.selection();
                let handle = match selection.as_slice() {
                    [id] if !sheet.is_pinned(id) => sheet.rect(id).and_then(|r| {
                        geometry::handle_at(&frame.rect(&r), frame.to_content(p), HANDLE)
                    }),
                    _ => None,
                };
                match handle {
                    Some(h) => Some(h.cursor()),
                    None => sheet.scene.hit(p, TOLERANCE / frame.scale).map(|_| "move"),
                }
            }
            Tool::Image => None,
            _ => Some("crosshair"),
        };
        self.set_cursor_from_name(name);
    }

    /// The selection, its handles and whatever a drag is doing, over the page.
    fn paint_overlays(&self, snapshot: &gtk::Snapshot, sheet: &Sheet, frame: &Frame) {
        let imp = self.imp();
        let accent = theme::accent();
        let outline = |r: &Rect| {
            snapshot.append_border(
                &gsk::RoundedRect::from_rect(paint::grect(r), 0.0),
                &[1.0; 4],
                &[accent; 4],
            );
        };
        let drag = imp.drag.borrow().clone();
        let pointer = imp.pointer.get();
        let free = imp.free.get();
        let selection = imp.selection.borrow().clone();
        let moving = matches!(drag, Some(Drag::Move { .. })) && imp.moved.get();
        for id in &selection {
            if let Some(r) = sheet.frame_of(id) {
                outline(&frame.rect(&r));
            }
        }
        if let [id] = selection.as_slice()
            && let Some(r) = sheet.rect(id)
            && !sheet.is_pinned(id)
            && !moving
        {
            let r = frame.rect(&r);
            for h in Handle::ALL {
                let at = h.at(&r);
                let square = Rect::new(at.x - HANDLE / 2.0, at.y - HANDLE / 2.0, HANDLE, HANDLE);
                snapshot.append_color(&accent, &paint::grect(&square));
            }
        }
        let Some(drag) = drag.filter(|_| imp.moved.get()) else {
            return;
        };
        match drag {
            Drag::Move {
                from, ids, origin, ..
            } => {
                let d = self.move_delta(from, pointer, origin, free);
                snapshot.save();
                snapshot.translate(&graphene::Point::new(
                    (d.x * frame.scale) as f32,
                    (d.y * frame.scale) as f32,
                ));
                snapshot.push_opacity(f64::from(theme::GHOST_ALPHA));
                for (i, prim) in sheet.scene.prims.iter().enumerate() {
                    if ids.iter().any(|id| sheet.is_within(prim.cell(), id)) {
                        let typesetter = imp.typesetter.borrow();
                        paint::prim(
                            snapshot,
                            self.upcast_ref(),
                            i,
                            prim,
                            frame,
                            &imp.cache,
                            typesetter.as_ref(),
                        );
                    }
                }
                snapshot.pop();
                snapshot.restore();
            }
            Drag::Resize {
                from, handle, rect, ..
            } => {
                let delta = Point::new(pointer.x - from.x, pointer.y - from.y);
                outline(&frame.rect(&geometry::resize_by(&rect, handle, delta, self.grid(free))));
            }
            Drag::Band { from, .. } => {
                let r = frame.rect(&Rect::from_corners(from, pointer));
                snapshot.append_color(
                    &theme::at(accent, theme::HIGHLIGHT_ALPHA),
                    &paint::grect(&r),
                );
                outline(&r);
            }
            Drag::Draw { tool, from } => {
                let r = frame.rect(&Rect::from_corners(from, pointer));
                let builder = gsk::PathBuilder::new();
                match tool {
                    Tool::Ellipse => builder.add_rounded_rect(&gsk::RoundedRect::from_rect(
                        paint::grect(&r),
                        (r.w.min(r.h) / 2.0) as f32,
                    )),
                    _ => builder.add_rect(&paint::grect(&r)),
                }
                snapshot.append_stroke(&builder.to_path(), &gsk::Stroke::new(1.0), &accent);
            }
            Drag::Connect { from, .. } => {
                let (a, b) = (frame.to_content(from), frame.to_content(pointer));
                let builder = gsk::PathBuilder::new();
                builder.move_to(a.x as f32, a.y as f32);
                builder.line_to(b.x as f32, b.y as f32);
                snapshot.append_stroke(&builder.to_path(), &gsk::Stroke::new(1.0), &accent);
            }
            Drag::Pan { .. } => {}
        }
    }
}

/// Point an adjustment at a content size, keeping it where it is scrolled to. The upper bound is
/// never below the page size, which GTK asserts on (the PDF view's `configure`).
fn configure(adjustment: Option<gtk::Adjustment>, upper: f64, page: f64) {
    let Some(adjustment) = adjustment else {
        return;
    };
    let value = adjustment.value().min((upper - page).max(0.0));
    adjustment.configure(value, 0.0, upper.max(page), page * 0.1, page * 0.9, page);
}

mod imp {
    use super::*;

    type OnEdit = Box<dyn Fn(Edit)>;
    type OnZoom = Box<dyn Fn()>;

    #[derive(glib::Properties)]
    #[properties(wrapper_type = super::DiagramView)]
    pub struct DiagramView {
        #[property(get, set = Self::adopt_h, nullable, override_interface = gtk::Scrollable)]
        pub hadjustment: RefCell<Option<gtk::Adjustment>>,
        #[property(get, set = Self::adopt_v, nullable, override_interface = gtk::Scrollable)]
        pub vadjustment: RefCell<Option<gtk::Adjustment>>,
        #[property(get, set, override_interface = gtk::Scrollable, builder(gtk::ScrollablePolicy::Minimum))]
        pub hscroll_policy: Cell<gtk::ScrollablePolicy>,
        #[property(get, set, override_interface = gtk::Scrollable, builder(gtk::ScrollablePolicy::Minimum))]
        pub vscroll_policy: Cell<gtk::ScrollablePolicy>,
        pub adj_handlers: RefCell<[Option<glib::SignalHandlerId>; 2]>,
        pub sheet: RefCell<Option<Rc<Sheet>>>,
        pub selection: RefCell<Vec<CellId>>,
        pub zoom: Cell<Zoom>,
        pub frame: Cell<Frame>,
        /// Whether the widget has had a size yet: the first layout centres the page.
        pub laid_out: Cell<bool>,
        pub pending_scroll: Cell<Option<(f64, f64)>>,
        pub tool: Cell<Tool>,
        pub panning: Cell<bool>,
        pub(super) drag: RefCell<Option<Drag>>,
        /// Whether the drag under way has gone past [`DRAG_SLOP`].
        pub moved: Cell<bool>,
        /// Where the pointer is in the drag under way, in page units.
        pub pointer: Cell<Point>,
        /// Alt held in the drag under way: no snapping to the grid.
        pub free: Cell<bool>,
        pub cache: Cache,
        /// Where labels with formulas are typeset, for a diagram that has any.
        pub typesetter: RefCell<Option<Rc<super::super::math::Typesetter>>>,
        pub on_edit: RefCell<Option<OnEdit>>,
        pub on_zoom: RefCell<Option<OnZoom>>,
    }

    // `gtk::ScrollablePolicy` has no `Default`, so the struct spells its own out.
    impl Default for DiagramView {
        fn default() -> Self {
            DiagramView {
                hadjustment: RefCell::new(None),
                vadjustment: RefCell::new(None),
                hscroll_policy: Cell::new(gtk::ScrollablePolicy::Minimum),
                vscroll_policy: Cell::new(gtk::ScrollablePolicy::Minimum),
                adj_handlers: RefCell::new([None, None]),
                sheet: RefCell::new(None),
                selection: RefCell::new(Vec::new()),
                zoom: Cell::new(Zoom::Fit),
                frame: Cell::new(Frame::default()),
                laid_out: Cell::new(false),
                pending_scroll: Cell::new(None),
                tool: Cell::new(Tool::Select),
                panning: Cell::new(false),
                drag: RefCell::new(None),
                moved: Cell::new(false),
                pointer: Cell::new(Point::default()),
                free: Cell::new(false),
                cache: Cache::default(),
                typesetter: RefCell::new(None),
                on_edit: RefCell::new(None),
                on_zoom: RefCell::new(None),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for DiagramView {
        const NAME: &'static str = "AccentDiagramView";
        type Type = super::DiagramView;
        type ParentType = gtk::Widget;
        type Interfaces = (gtk::Scrollable,);
    }

    impl DiagramView {
        fn adopt_h(&self, adjustment: Option<gtk::Adjustment>) {
            self.adopt(0, adjustment);
        }

        fn adopt_v(&self, adjustment: Option<gtk::Adjustment>) {
            self.adopt(1, adjustment);
        }

        /// Follow an adjustment: redraw when it moves, and drop the handler on the old one.
        fn adopt(&self, slot: usize, adjustment: Option<gtk::Adjustment>) {
            let obj = self.obj().clone();
            let old = match slot {
                0 => self.hadjustment.replace(adjustment.clone()),
                _ => self.vadjustment.replace(adjustment.clone()),
            };
            if let (Some(old), Some(id)) = (old, self.adj_handlers.borrow_mut()[slot].take()) {
                old.disconnect(id);
            }
            if let Some(adjustment) = adjustment {
                let id = adjustment.connect_value_changed(move |_| obj.queue_draw());
                self.adj_handlers.borrow_mut()[slot] = Some(id);
            }
            self.obj().queue_allocate();
        }
    }

    #[glib::derived_properties]
    impl ObjectImpl for DiagramView {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj().clone();
            obj.set_overflow(gtk::Overflow::Hidden);
            obj.set_focusable(true);
            obj.set_hexpand(true);
            obj.set_vexpand(true);

            let pinch = gtk::GestureZoom::new();
            pinch.connect_scale_changed(glib::clone!(
                #[weak]
                obj,
                move |gesture, scale| {
                    if !(0.9..1.1).contains(&scale) {
                        obj.zoom_step(scale < 1.0, gesture.bounding_box_center());
                    }
                }
            ));
            obj.add_controller(pinch);

            let motion = gtk::EventControllerMotion::new();
            motion.connect_motion(glib::clone!(
                #[weak]
                obj,
                move |_, x, y| obj.hover(x, y)
            ));
            obj.add_controller(motion);

            // The primary button selects, moves, resizes and draws; see `start_drag`. The
            // sequence is claimed on the press: every drag here is the canvas's, and letting the
            // scrolled window have the first pixels would scroll the page under the hand.
            let drag = gtk::GestureDrag::new();
            drag.connect_drag_begin(glib::clone!(
                #[weak]
                obj,
                move |gesture, x, y| {
                    obj.grab_focus();
                    let state = gesture.current_event_state();
                    let started =
                        obj.start_drag(x, y, state.contains(gdk::ModifierType::SHIFT_MASK));
                    let imp = obj.imp();
                    imp.moved.set(false);
                    imp.pointer.set(obj.page_at(x, y));
                    if started.is_some() {
                        gesture.set_state(gtk::EventSequenceState::Claimed);
                    }
                    *imp.drag.borrow_mut() = started;
                }
            ));
            drag.connect_drag_update(glib::clone!(
                #[weak]
                obj,
                move |gesture, dx, dy| {
                    let imp = obj.imp();
                    let Some((x0, y0)) = gesture.start_point() else {
                        return;
                    };
                    if dx.hypot(dy) > DRAG_SLOP {
                        imp.moved.set(true);
                    }
                    imp.free.set(
                        gesture
                            .current_event_state()
                            .contains(gdk::ModifierType::ALT_MASK),
                    );
                    let pan = match imp.drag.borrow().as_ref() {
                        Some(Drag::Pan { scroll }) => Some(*scroll),
                        _ => None,
                    };
                    match pan {
                        Some((sx, sy)) => obj.set_scroll((sx - dx, sy - dy)),
                        None => imp.pointer.set(obj.page_at(x0 + dx, y0 + dy)),
                    }
                    obj.queue_draw();
                }
            ));
            drag.connect_drag_end(glib::clone!(
                #[weak]
                obj,
                move |gesture, dx, dy| {
                    let imp = obj.imp();
                    let Some(drag) = imp.drag.borrow_mut().take() else {
                        return;
                    };
                    let Some((x0, y0)) = gesture.start_point() else {
                        return;
                    };
                    let moved = imp.moved.replace(false) || dx.hypot(dy) > DRAG_SLOP;
                    let free = gesture
                        .current_event_state()
                        .contains(gdk::ModifierType::ALT_MASK);
                    obj.end_drag(drag, x0 + dx, y0 + dy, moved, free);
                    obj.queue_draw();
                }
            ));
            obj.add_controller(drag);

            // The middle button pans, as it does in draw.io.
            let pan = gtk::GestureDrag::builder().button(2).build();
            pan.connect_drag_begin(glib::clone!(
                #[weak]
                obj,
                move |_, _, _| {
                    *obj.imp().drag.borrow_mut() = Some(Drag::Pan {
                        scroll: obj.scroll(),
                    });
                    obj.set_cursor_from_name(Some("grabbing"));
                }
            ));
            pan.connect_drag_update(glib::clone!(
                #[weak]
                obj,
                move |_, dx, dy| {
                    let scroll = match obj.imp().drag.borrow().as_ref() {
                        Some(Drag::Pan { scroll }) => *scroll,
                        _ => return,
                    };
                    obj.set_scroll((scroll.0 - dx, scroll.1 - dy));
                }
            ));
            pan.connect_drag_end(glib::clone!(
                #[weak]
                obj,
                move |_, _, _| {
                    obj.imp().drag.borrow_mut().take();
                    obj.set_cursor_from_name(None);
                }
            ));
            obj.add_controller(pan);

            // A double click edits the label of what is under it, innermost first: a shape in a
            // group, not the group.
            let click = gtk::GestureClick::new();
            click.connect_pressed(glib::clone!(
                #[weak]
                obj,
                move |_, n, x, y| {
                    if n != 2 {
                        return;
                    }
                    let Some(sheet) = obj.sheet() else { return };
                    let p = obj.page_at(x, y);
                    if let Some(id) = sheet.scene.hit(p, TOLERANCE / obj.scale()) {
                        obj.emit(Edit::Label(id.to_string()));
                    }
                }
            ));
            obj.add_controller(click);
        }
    }

    impl WidgetImpl for DiagramView {
        /// The content decides its own size, and the scrolled window gives it what it has.
        fn measure(&self, _orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            (0, 0, -1, -1)
        }

        fn size_allocate(&self, _width: i32, _height: i32, _baseline: i32) {
            self.obj().relayout();
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();
            let Some(sheet) = obj.sheet() else { return };
            let frame = self.frame.get();
            let (sx, sy) = obj.scroll();
            let (w, h) = (f64::from(obj.width()), f64::from(obj.height()));
            snapshot.save();
            snapshot.translate(&graphene::Point::new(-sx as f32, -sy as f32));

            let (pw, ph) = sheet.scene.page_size;
            let page = paint::grect(&frame.rect(&Rect::new(0.0, 0.0, pw, ph)));
            let paper = sheet.scene.background.unwrap_or(Color::WHITE);
            snapshot.append_color(&paint::rgba(paper), &page);
            let edge = theme::at(obj.color(), theme::PAGE_EDGE_ALPHA);
            snapshot.append_border(
                &gsk::RoundedRect::from_rect(page, 0.0),
                &[1.0; 4],
                &[edge; 4],
            );

            // Only what is on screen, a prim's box being known from when the page was shown.
            let near = frame.to_page(Point::new(sx, sy));
            let visible = Rect::new(near.x, near.y, w / frame.scale, h / frame.scale);
            for (i, prim) in sheet.scene.prims.iter().enumerate() {
                if sheet.bounds[i].intersects(&visible) {
                    let typesetter = self.typesetter.borrow();
                    paint::prim(
                        snapshot,
                        obj.upcast_ref(),
                        i,
                        prim,
                        &frame,
                        &self.cache,
                        typesetter.as_ref(),
                    );
                }
            }
            obj.paint_overlays(snapshot, &sheet, &frame);
            snapshot.restore();
        }
    }

    impl ScrollableImpl for DiagramView {}
}
