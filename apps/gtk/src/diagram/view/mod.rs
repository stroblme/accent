//! The canvas a diagram is drawn and edited on: a scrollable widget painting one page's display
//! list, with the selection, its handles and whatever a drag is doing drawn over it.
//!
//! It never changes the diagram. A gesture ends in an [`Edit`] handed to the tab, which applies
//! it to the model and hands back a new [`Sheet`]; so one gesture is one undo step, and the
//! widget is a picture of the page plus a pointer. The scrolling is [`crate::scrollable`]'s.

mod drag;
mod overlay;
mod preview;

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use accent_drawio::{CellId, Color, Point, Rect};
use adw::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene, gsk};

use super::geometry::{self, DRAG_SLOP, End, Frame, GAP, HANDLE, Sheet, TOLERANCE, Zoom};
use super::paint::{self, Cache, Tint};
use super::tools::Tool;
use crate::theme;
use drag::Drag;
use preview::Preview;

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
    /// A new edge; an end with no cell dangles at its point, and one pinned to a connection
    /// point carries its constraint.
    Connect {
        source: End,
        target: End,
    },
    /// Edit this cell's label.
    Label(CellId),
    /// Turn a shape to `degrees`.
    Rotate {
        id: CellId,
        degrees: f64,
    },
    /// Put the source end (else the target end) of edge `id` on `end`.
    End {
        id: CellId,
        source: bool,
        end: End,
    },
    /// Put the label of edge `id` at `at`, in page units.
    LabelAt {
        id: CellId,
        at: Point,
    },
    /// Give edge `id` these waypoints, in page units.
    Points {
        id: CellId,
        points: Vec<Point>,
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
        // A preview is of the page before; a drag still under way starts one of this page.
        imp.preview.take();
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

    /// Paint the page in `tint`'s colours from now on: its labels are laid out again, their run
    /// colours being in their layouts, and a drag's preview is built again; the page itself, its
    /// formulas and its pictures stay as they are.
    pub fn set_tint(&self, tint: Tint) {
        let imp = self.imp();
        if imp.tint.replace(tint) != tint {
            imp.cache.forget();
            imp.preview.take();
            self.queue_draw();
        }
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

    /// A page to read, not to edit, as a presented one is: a drag moves the page, and no click
    /// selects, moves, draws or opens a label.
    pub fn set_read_only(&self, on: bool) {
        self.imp().read_only.set(on);
        self.set_cursor_from_name(None);
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
        self.imp().scroll.scroll()
    }

    pub fn set_scroll(&self, at: (f64, f64)) {
        self.imp().scroll.set_scroll(at);
    }

    /// Whether the drawing goes on below the view (`down`) or above it. The last [`GAP`] is the
    /// margin round it, nothing to read: a stroke on the page's edge, which widens the drawing a
    /// pixel past a fitted page, does not take a notch of its own.
    pub fn has_room(&self, down: bool) -> bool {
        let Some(v) = self.imp().scroll.v() else {
            return false;
        };
        match down {
            true => v.value() + v.page_size() < v.upper() - GAP,
            false => v.value() > v.lower() + GAP,
        }
    }

    /// Scroll to the top of the drawing, or to its bottom, at the zoom it is at.
    pub fn land(&self, top: bool) {
        if let Some(v) = self.imp().scroll.v() {
            v.set_value(match top {
                true => v.lower(),
                false => v.upper() - v.page_size(),
            });
        }
    }

    /// Where the diagram is left: the zoom and scroll to come back to. Applied once the widget
    /// has a size, which is when a scroll position means anything.
    pub fn restore(&self, zoom: Zoom, scroll: (f64, f64)) {
        self.imp().zoom.set(zoom);
        self.imp().pending_scroll.set(Some(scroll));
        self.relayout();
    }

    /// The cell a secondary click at widget `(x, y)` is about: the selected one under it, else
    /// the one a click there would select (`Sheet::pick`); `None` over empty page.
    pub fn cell_at(&self, x: f64, y: f64) -> Option<CellId> {
        let sheet = self.sheet()?;
        let tolerance = TOLERANCE / self.scale();
        let pick = sheet.pick(self.page_at(x, y), tolerance, &self.selection())?;
        Some(pick.held.unwrap_or(pick.cell))
    }

    /// Where draw.io puts what is pasted without a place of its own (`Graph.getInsertPoint`): a
    /// grid step in from the top-left of what is on screen, on the grid, never off the page's
    /// top or left.
    pub fn insert_point(&self, grid: f64) -> Point {
        let at = self.page_at(0.0, 0.0);
        let snap = |v: f64| (v.max(0.0) / grid + 1.0).round() * grid;
        Point::new(snap(at.x), snap(at.y))
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
    #[cfg(feature = "bench")]
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
        let c = self.imp().frame.get().to_content(sheet.shown().centre());
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
            Zoom::Fit => {
                let shown = sheet.shown();
                geometry::fit_scale((shown.w, shown.h), (w, h))
            }
            Zoom::Scale(s) => geometry::clamp_scale(s),
        };
        let (frame, size) = geometry::frame(sheet.extent, scale, (w, h));
        if imp.frame.replace(frame).scale != frame.scale {
            imp.cache.forget();
        }
        imp.scroll.configure(size, (w, h));
        if !imp.laid_out.replace(true) && imp.pending_scroll.get().is_none() {
            self.centre_page();
        }
        if let Some(scroll) = imp.pending_scroll.take() {
            self.set_scroll(scroll);
        }
        self.queue_draw();
    }
}

/// Where two rectangles overlap, if they do.
fn intersection(a: &Rect, b: &Rect) -> Option<Rect> {
    let (x, y) = (a.x.max(b.x), a.y.max(b.y));
    let (r, bottom) = (a.right().min(b.right()), a.bottom().min(b.bottom()));
    (r > x && bottom > y).then(|| Rect::new(x, y, r - x, bottom - y))
}

mod imp {
    use super::*;
    use crate::scrollable::Adjustments;

    type OnEdit = Box<dyn Fn(Edit)>;
    type OnZoom = Box<dyn Fn()>;

    #[derive(glib::Properties)]
    #[properties(wrapper_type = super::DiagramView)]
    pub struct DiagramView {
        #[property(name = "hadjustment", type = Option<gtk::Adjustment>, get = |v: &Self| v.scroll.h(), set = Self::adopt_h, nullable, override_interface = gtk::Scrollable)]
        #[property(name = "vadjustment", type = Option<gtk::Adjustment>, get = |v: &Self| v.scroll.v(), set = Self::adopt_v, nullable, override_interface = gtk::Scrollable)]
        pub scroll: Adjustments,
        #[property(get, set, override_interface = gtk::Scrollable, builder(gtk::ScrollablePolicy::Minimum))]
        pub hscroll_policy: Cell<gtk::ScrollablePolicy>,
        #[property(get, set, override_interface = gtk::Scrollable, builder(gtk::ScrollablePolicy::Minimum))]
        pub vscroll_policy: Cell<gtk::ScrollablePolicy>,
        pub sheet: RefCell<Option<Rc<Sheet>>>,
        pub selection: RefCell<Vec<CellId>>,
        pub zoom: Cell<Zoom>,
        pub frame: Cell<Frame>,
        /// Whether the widget has had a size yet: the first layout centres the page.
        pub laid_out: Cell<bool>,
        pub pending_scroll: Cell<Option<(f64, f64)>>,
        pub tool: Cell<Tool>,
        pub panning: Cell<bool>,
        pub read_only: Cell<bool>,
        pub(super) drag: RefCell<Option<Drag>>,
        pub(super) preview: RefCell<Option<Preview>>,
        /// Whether the drag under way has gone past [`DRAG_SLOP`].
        pub moved: Cell<bool>,
        /// Where the pointer is in the drag under way, in page units.
        pub pointer: Cell<Point>,
        /// Alt held in the drag under way: no snapping to the grid.
        pub free: Cell<bool>,
        pub cache: Cache,
        /// The colours the page is painted in.
        pub tint: Cell<Tint>,
        /// Where labels with formulas are typeset, for a diagram that has any.
        pub typesetter: RefCell<Option<Rc<super::super::math::Typesetter>>>,
        pub on_edit: RefCell<Option<OnEdit>>,
        pub on_zoom: RefCell<Option<OnZoom>>,
    }

    // `gtk::ScrollablePolicy` has no `Default`, so the struct spells its own out.
    impl Default for DiagramView {
        fn default() -> Self {
            DiagramView {
                scroll: Adjustments::default(),
                hscroll_policy: Cell::new(gtk::ScrollablePolicy::Minimum),
                vscroll_policy: Cell::new(gtk::ScrollablePolicy::Minimum),
                sheet: RefCell::new(None),
                selection: RefCell::new(Vec::new()),
                zoom: Cell::new(Zoom::Fit),
                frame: Cell::new(Frame::default()),
                laid_out: Cell::new(false),
                pending_scroll: Cell::new(None),
                tool: Cell::new(Tool::Select),
                panning: Cell::new(false),
                read_only: Cell::new(false),
                drag: RefCell::new(None),
                preview: RefCell::new(None),
                moved: Cell::new(false),
                pointer: Cell::new(Point::default()),
                free: Cell::new(false),
                cache: Cache::default(),
                tint: Cell::new(Tint::FILE),
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
            self.scroll
                .adopt(&*self.obj(), 0, adjustment, |v| v.queue_draw());
        }

        fn adopt_v(&self, adjustment: Option<gtk::Adjustment>) {
            self.scroll
                .adopt(&*self.obj(), 1, adjustment, |v| v.queue_draw());
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

            crate::zoom::zoom_on_pinch(
                &obj,
                glib::clone!(
                    #[weak]
                    obj,
                    #[upgrade_or]
                    1.0,
                    move || obj.scale()
                ),
                glib::clone!(
                    #[weak]
                    obj,
                    move |zoom, at| {
                        let zoom = Zoom::Scale(geometry::clamp_scale(zoom));
                        if obj.zoom() != zoom {
                            obj.zoom_around(zoom, Some(at));
                        }
                    }
                ),
            );

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
                    imp.preview.take();
                    obj.queue_draw();
                }
            ));
            obj.add_controller(drag.clone());

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
            // group, not the group. Grouped with the drag, which claims every press: a gesture
            // in a group of its own would have each press denied and never count to two.
            let click = gtk::GestureClick::new();
            click.connect_pressed(glib::clone!(
                #[weak]
                obj,
                move |_, n, x, y| {
                    if n != 2 || obj.imp().read_only.get() {
                        return;
                    }
                    let Some(sheet) = obj.sheet() else { return };
                    let p = obj.page_at(x, y);
                    // ponytail: a label found by its painted text only when nothing else is
                    // under the pointer; walking both in one paint order is the upgrade if a
                    // label over a shape should win.
                    let hit = sheet
                        .scene
                        .hit(p, TOLERANCE / obj.scale())
                        .or_else(|| obj.imp().cache.label_at(&sheet.scene.prims, p));
                    // After the press is done with: the drag that shares it takes the keyboard for
                    // the canvas, and would take it from an editor opened here.
                    if let Some(id) = hit.map(str::to_string) {
                        glib::idle_add_local_once(move || obj.emit(Edit::Label(id)));
                    }
                }
            ));
            obj.add_controller(click.clone());
            click.group_with(&drag);
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

            // The sheet, or with none the whole canvas in the page's colour, as draw.io's.
            let tint = self.tint.get();
            let paper = tint.colour(sheet.scene.background.unwrap_or(Color::WHITE));
            match sheet.page_rect {
                Some(r) => {
                    let page = paint::grect(&frame.rect(&r));
                    snapshot.append_color(&paint::rgba(paper), &page);
                    let edge = theme::at(obj.color(), theme::PAGE_EDGE_ALPHA);
                    snapshot.append_border(
                        &gsk::RoundedRect::from_rect(page, 0.0),
                        &[1.0; 4],
                        &[edge; 4],
                    );
                }
                None => {
                    let canvas = graphene::Rect::new(sx as f32, sy as f32, w as f32, h as f32);
                    snapshot.append_color(&paint::rgba(paper), &canvas);
                }
            }

            // Only what is on screen, a prim's box being known from when the page was shown.
            let near = frame.to_page(Point::new(sx, sy));
            let visible = Rect::new(near.x, near.y, w / frame.scale, h / frame.scale);
            // The grid, while a move, a resize or a drawing is under way.
            let placing = matches!(
                self.drag.borrow().as_ref(),
                Some(Drag::Move { .. } | Drag::Resize { .. } | Drag::Draw { .. })
            );
            if let Some(step) = sheet.grid.filter(|_| placing && self.moved.get())
                && let Some(area) = intersection(&visible, &sheet.page_rect.unwrap_or(visible))
            {
                paint::grid(snapshot, &frame, area, step, paper);
            }

            // The page as a drag under way would leave it, or as it is.
            obj.update_preview(&sheet);
            let preview = self.preview.borrow();
            let (prims, bounds, cache) = match preview.as_ref() {
                Some(Preview::Live(live)) => (&live.scene.prims, &live.bounds, &live.cache),
                _ => (&sheet.scene.prims, &sheet.bounds, &self.cache),
            };
            let typesetter = self.typesetter.borrow();
            for (i, prim) in prims.iter().enumerate() {
                if bounds[i].intersects(&visible) {
                    paint::prim(
                        snapshot,
                        obj.upcast_ref(),
                        i,
                        prim,
                        &frame,
                        cache,
                        typesetter.as_ref(),
                        tint,
                    );
                }
            }
            obj.paint_overlays(snapshot, &sheet, &frame);
            snapshot.restore();
        }
    }

    impl ScrollableImpl for DiagramView {}
}
