//! The graph's canvas (DESIGN.md, Graph): every node and link drawn with GSK each frame, a path
//! per colour, culled to what is on screen, and the input that moves round it.
//!
//! The canvas owns the [`Graph`] and where each of its nodes is. Every change to what it shows
//! goes through [`GraphView::apply`], so the nodes and their places stay in step; their places
//! come from the force layout, worked out off the main loop.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use accent_core::graph::layout::Layout as Force;
use adw::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene, gsk, pango};

use super::camera::{self, Camera};
use super::model::{self, Changes, Graph, Paint};
use crate::theme;

/// Names show for every node on screen from this zoom up, and below it only round the lit node,
/// the file in front and the node the keyboard is on.
const LABEL_ZOOM: f64 = 0.6;
/// The most names drawn in a frame, a whole vault zoomed in included.
const LABELS_MAX: usize = 400;
/// The widest a name is drawn before it is ellipsized, in pixels.
const LABEL_WIDTH: i32 = 160;
/// How far a wheel notch pans, in pixels.
const NOTCH: f64 = 64.0;
/// The most steps the layout takes to settle, which a cold layout of any size settles well
/// within (300 steps).
const MAX_SETTLE: usize = 1000;

/// A press and whether it is still a click: where it went down and the node under it.
#[derive(Clone, Copy)]
pub struct Press {
    at: [f64; 2],
    node: Option<usize>,
    slop: f64,
}

/// What a press on a node asks of the tab: open its file, and keep it for a double click.
type Open = dyn Fn(usize, bool);

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct GraphView {
        pub graph: RefCell<Graph>,
        /// Where each node is, in the layout's units, in the graph's order.
        pub at: RefCell<Vec<[f32; 2]>>,
        pub camera: Cell<Camera>,
        /// The node under the pointer.
        pub hover: Cell<Option<usize>>,
        /// The file in front of the window, drawn in the accent, by key: it is no node until the
        /// answer holding it lands.
        pub front: RefCell<Option<String>>,
        /// Counts the layouts asked for, so only the last one's places land.
        pub generation: Cell<u64>,
        /// Each name laid out once, by the node's key, until the style changes.
        pub labels: RefCell<HashMap<String, pango::Layout>>,
        pub press: Cell<Option<Press>>,
        /// The pan a wheel notch eases towards: from where to where.
        pub easing: Cell<([f64; 2], [f64; 2])>,
        pub wheel: RefCell<Option<adw::TimedAnimation>>,
        pub open: RefCell<Option<Rc<Open>>>,
        pub zoomed: RefCell<Option<Rc<dyn Fn()>>>,
        /// Whether the places shown are the settled layout of what is shown, for the drill.
        #[cfg(feature = "bench")]
        pub settled: Cell<bool>,
        /// How long each frame took to draw, for the drill.
        #[cfg(feature = "bench")]
        pub painted: RefCell<Vec<f64>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for GraphView {
        const NAME: &'static str = "AccentGraph";
        type Type = super::GraphView;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for GraphView {}

    impl WidgetImpl for GraphView {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            #[cfg(feature = "bench")]
            let t = std::time::Instant::now();
            self.obj().paint(snapshot);
            #[cfg(feature = "bench")]
            self.painted
                .borrow_mut()
                .push(t.elapsed().as_secs_f64() * 1000.0);
        }
    }
}

glib::wrapper! {
    /// The canvas the Graph tab shows its nodes on.
    pub struct GraphView(ObjectSubclass<imp::GraphView>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl GraphView {
    pub fn new() -> GraphView {
        let view: GraphView = glib::Object::builder()
            .property("accessible-role", gtk::AccessibleRole::Group)
            .property("focusable", true)
            .property("hexpand", true)
            .property("vexpand", true)
            .build();
        view.update_property(&[gtk::accessible::Property::Label("Graph")]);
        view.wire_pointer();
        view.wire_wheel();
        view
    }

    pub fn graph(&self) -> std::cell::Ref<'_, Graph> {
        self.imp().graph.borrow()
    }

    pub fn graph_mut(&self) -> std::cell::RefMut<'_, Graph> {
        self.imp().graph.borrow_mut()
    }

    /// What a press on a node does: `keep` for the second press of a double click.
    pub fn connect_open(&self, f: impl Fn(usize, bool) + 'static) {
        self.imp().open.replace(Some(Rc::new(f)));
    }

    /// Hear a pan or a zoom, for the zoom readout.
    pub fn connect_zoomed(&self, f: impl Fn() + 'static) {
        self.imp().zoomed.replace(Some(Rc::new(f)));
    }

    /// Show what `changes` change, the nodes staying in place and the new ones placed beside the
    /// node they slide out of, then lay the graph out again.
    pub fn apply(&self, changes: &Changes) {
        let imp = self.imp();
        {
            let mut at = imp.at.borrow_mut();
            for &i in &changes.removed {
                at.swap_remove(i);
            }
            for (_, near) in &changes.added {
                let near = near.and_then(|n| at.get(n).copied());
                at.push(near.unwrap_or_default());
            }
        }
        imp.graph.borrow_mut().apply(changes);
        imp.hover.set(None);
        #[cfg(feature = "bench")]
        imp.settled.set(false);
        self.lay_out();
        self.queue_draw();
    }

    /// Work the layout out off the main loop, from scratch, and show it once settled.
    fn lay_out(&self) {
        let imp = self.imp();
        let generation = imp.generation.get() + 1;
        imp.generation.set(generation);
        let (n, links) = {
            let graph = imp.graph.borrow();
            (graph.nodes.len(), graph.links())
        };
        let view = self.downgrade();
        glib::spawn_future_local(async move {
            let at = crate::work::off_thread("graph layout", move || {
                let mut force = Force::new();
                for _ in 0..n {
                    force.add(None);
                }
                force.set_links(&links);
                for _ in 0..MAX_SETTLE {
                    if force.step() == 0.0 {
                        break;
                    }
                }
                force.positions().to_vec()
            })
            .await;
            let Some(view) = view.upgrade() else {
                return;
            };
            let imp = view.imp();
            if let Some(at) = at.filter(|_| imp.generation.get() == generation) {
                imp.at.replace(at);
                #[cfg(feature = "bench")]
                imp.settled.set(true);
                view.queue_draw();
            }
        });
    }

    /// Draw the file keyed `key` in the accent, or none.
    pub fn set_front(&self, key: Option<String>) {
        if *self.imp().front.borrow() != key {
            self.imp().front.replace(key);
            self.queue_draw();
        }
    }

    /// The names are laid out again in the new style, and everything drawn in its colours.
    pub fn restyle(&self) {
        self.imp().labels.borrow_mut().clear();
        self.queue_draw();
    }

    pub fn camera(&self) -> Camera {
        self.imp().camera.get()
    }

    fn set_camera(&self, camera: Camera) {
        self.imp().camera.set(camera);
        self.queue_draw();
        if let Some(zoomed) = self.imp().zoomed.borrow().clone() {
            zoomed();
        }
    }

    fn size(&self) -> [f64; 2] {
        [f64::from(self.width()), f64::from(self.height())]
    }

    /// Fitted again to what it shows, and kept so as it settles.
    pub fn fit(&self) {
        self.set_camera(Camera::default());
    }

    /// Zoomed a step in or out round the canvas's middle.
    pub fn zoom_step(&self, out: bool) {
        let size = self.size();
        self.zoom_to(
            crate::zoom::stepped_zoom(self.camera().zoom, out),
            [size[0] / 2.0, size[1] / 2.0],
        );
    }

    fn zoom_to(&self, zoom: f64, at: [f64; 2]) {
        let mut camera = self.camera();
        camera.zoom_at(crate::zoom::clamp_scale(zoom), at);
        self.set_camera(camera);
    }

    /// What the status bar's readout says: Fit while it is kept fitted, as an image's does.
    pub fn zoom_label(&self) -> String {
        let camera = self.camera();
        match camera.fitting {
            true => "Fit".to_string(),
            false => format!("{} %", (camera.zoom * 100.0).round() as i32),
        }
    }

    /// The node under canvas point `at`.
    fn node_at(&self, at: [f64; 2]) -> Option<usize> {
        let graph = self.imp().graph.borrow();
        let places = self.imp().at.borrow();
        (places.len() == graph.nodes.len())
            .then(|| {
                self.camera()
                    .hit(at, &places, |i| model::radius(graph.adjacency[i].len()))
            })
            .flatten()
    }

    /// A press on a node opens it, the second of a double click keeps it; a drag of the
    /// background, or of anything with the middle button, pans; the pointer over a node lights
    /// it and its neighbours.
    fn wire_pointer(&self) {
        let click = gtk::GestureClick::builder()
            .button(gdk::BUTTON_PRIMARY)
            .build();
        click.connect_pressed(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |click, _, x, y| {
                // The keyboard stays on the graph, as it stays in the Files tree.
                view.grab_focus();
                view.imp().press.set(Some(Press {
                    at: [x, y],
                    node: view.node_at([x, y]),
                    slop: crate::widgets::click_slop(click),
                }));
            }
        ));
        click.connect_released(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_, n, x, y| {
                let Some(press) = view.imp().press.take() else {
                    return;
                };
                let moved = (x - press.at[0]).hypot(y - press.at[1]);
                let open = view.imp().open.borrow().clone();
                if let (Some(i), Some(open), true) = (press.node, open, moved <= press.slop) {
                    open(i, n >= 2);
                }
            }
        ));
        self.add_controller(click);

        // How far the drag has panned, `None` while it is still within a click's slop.
        let drag = gtk::GestureDrag::builder().button(0).build();
        let panned = Rc::new(Cell::new(None::<[f64; 2]>));
        drag.connect_drag_begin(glib::clone!(
            #[strong]
            panned,
            move |drag, _, _| {
                if !matches!(drag.current_button(), 0..=2) {
                    drag.set_state(gtk::EventSequenceState::Denied);
                }
                panned.set(None);
            }
        ));
        drag.connect_drag_update(glib::clone!(
            #[weak(rename_to = view)]
            self,
            #[strong]
            panned,
            move |drag, dx, dy| {
                let was = match panned.get() {
                    Some(was) => was,
                    None if dx.hypot(dy) <= crate::widgets::click_slop(drag) => return,
                    None => [0.0; 2],
                };
                panned.set(Some([dx, dy]));
                let mut camera = view.camera();
                camera.pan([dx - was[0], dy - was[1]]);
                view.set_camera(camera);
            }
        ));
        self.add_controller(drag);

        let motion = gtk::EventControllerMotion::new();
        motion.connect_motion(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_, x, y| view.hover(view.node_at([x, y]))
        ));
        motion.connect_leave(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_| view.hover(None)
        ));
        self.add_controller(motion);

        let pointer = crate::zoom::track_pointer(self);
        let view = self.downgrade();
        crate::zoom::zoom_on_wheel(self, gtk::PropagationPhase::Bubble, move |out| {
            if let Some(view) = view.upgrade() {
                let size = view.size();
                let at = pointer.get().unwrap_or((size[0] / 2.0, size[1] / 2.0));
                let zoom = crate::zoom::stepped_zoom(view.camera().zoom, out);
                view.zoom_to(zoom, [at.0, at.1]);
            }
        });
        let (from, to) = (self.downgrade(), self.downgrade());
        crate::zoom::zoom_on_pinch(
            self,
            move || from.upgrade().map_or(1.0, |view| view.camera().zoom),
            move |zoom, (x, y)| {
                if let Some(view) = to.upgrade() {
                    view.zoom_to(zoom, [x, y]);
                }
            },
        );
    }

    fn hover(&self, node: Option<usize>) {
        if self.imp().hover.replace(node) != node {
            self.set_cursor_from_name(node.map(|_| "pointer"));
            self.queue_draw();
        }
    }

    /// The wheel and the touchpad pan, both ways: a touchpad's pixels as they come, a wheel's
    /// notch [`NOTCH`] pixels eased as a page's scroll is, at once with animations off. With
    /// Control it zooms instead (`zoom::zoom_on_wheel`).
    fn wire_wheel(&self) {
        let target = adw::CallbackAnimationTarget::new(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |t| {
                let (from, to) = view.imp().easing.get();
                let mut camera = view.camera();
                camera.offset = [
                    from[0] + (to[0] - from[0]) * t,
                    from[1] + (to[1] - from[1]) * t,
                ];
                camera.fitting = false;
                view.set_camera(camera);
            }
        ));
        let animation = adw::TimedAnimation::new(self, 0.0, 1.0, crate::widgets::WHEEL_MS, target);
        animation.set_easing(adw::Easing::EaseOutCubic);
        self.imp().wheel.replace(Some(animation));
        let wheel = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
        wheel.connect_scroll(glib::clone!(
            #[weak(rename_to = view)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |wheel, dx, dy| {
                let state = wheel.current_event_state();
                if state.contains(crate::platform::PRIMARY_MODIFIER) {
                    return glib::Propagation::Proceed;
                }
                // Shift turns a wheel's turn sideways.
                let (dx, dy) = match state.contains(gdk::ModifierType::SHIFT_MASK) && dx == 0.0 {
                    true => (dy, 0.0),
                    false => (dx, dy),
                };
                match wheel.unit() {
                    gdk::ScrollUnit::Wheel => view.ease_pan([-dx * NOTCH, -dy * NOTCH]),
                    _ => {
                        let mut camera = view.camera();
                        camera.pan([-dx, -dy]);
                        view.set_camera(camera);
                    }
                }
                glib::Propagation::Stop
            }
        ));
        self.add_controller(wheel);
    }

    /// Pan by `by` over a notch's easing, from where an easing under way has got to and on to
    /// where it was going plus `by`.
    fn ease_pan(&self, by: [f64; 2]) {
        let imp = self.imp();
        let Some(animation) = imp.wheel.borrow().clone() else {
            return;
        };
        let now = self.camera().offset;
        let going = match animation.state() {
            adw::AnimationState::Playing => imp.easing.get().1,
            _ => now,
        };
        imp.easing.set((now, [going[0] + by[0], going[1] + by[1]]));
        animation.play();
    }

    /// Draw the links, then the nodes a path per colour, then the names, all culled to the
    /// canvas.
    fn paint(&self, snapshot: &gtk::Snapshot) {
        let imp = self.imp();
        let graph = imp.graph.borrow();
        let at = imp.at.borrow();
        if graph.nodes.is_empty() || at.len() != graph.nodes.len() {
            return;
        }
        let size = self.size();
        let mut camera = imp.camera.get();
        if camera.fitting {
            camera = Camera::fit(&at, size);
            imp.camera.set(camera);
        }
        snapshot.push_clip(&graphene::Rect::new(
            0.0,
            0.0,
            size[0] as f32,
            size[1] as f32,
        ));
        let ink = self.color();
        let screen: Vec<[f64; 2]> = at.iter().map(|&p| camera.to_screen(p)).collect();
        let radius = |i: usize| {
            (f64::from(model::radius(graph.adjacency[i].len())) * camera.zoom)
                .max(camera::MIN_RADIUS)
        };
        // A node whose disc, or a link whose box, reaches the canvas.
        let pad = f64::from(model::radius(usize::MAX)) * camera.zoom + 4.0;
        let on_screen = |p: [f64; 2]| {
            (-pad..=size[0] + pad).contains(&p[0]) && (-pad..=size[1] + pad).contains(&p[1])
        };
        let lit = imp.hover.get();
        let near: HashSet<usize> = lit
            .map(|l| graph.adjacency[l].iter().copied().chain([l]).collect())
            .unwrap_or_default();
        let dimmed = |i: usize| lit.is_some() && !near.contains(&i);
        let front = (imp.front.borrow().as_deref()).and_then(|key| graph.index_of(key));

        let (links, lit_links) = (gsk::PathBuilder::new(), gsk::PathBuilder::new());
        for e in &graph.edges {
            let (a, b) = (screen[e.from as usize], screen[e.to as usize]);
            let misses = a[0].max(b[0]) < 0.0
                || a[0].min(b[0]) > size[0]
                || a[1].max(b[1]) < 0.0
                || a[1].min(b[1]) > size[1];
            if misses {
                continue;
            }
            let path = match lit.is_some_and(|l| e.from as usize == l || e.to as usize == l) {
                true => &lit_links,
                false => &links,
            };
            path.move_to(a[0] as f32, a[1] as f32);
            path.line_to(b[0] as f32, b[1] as f32);
        }
        let hairline = gsk::Stroke::new(1.0);
        snapshot.append_stroke(
            &links.to_path(),
            &hairline,
            &theme::at(ink, theme::GRAPH_EDGE_ALPHA),
        );
        snapshot.append_stroke(
            &lit_links.to_path(),
            &hairline,
            &theme::at(ink, theme::GRAPH_FAINT_ALPHA),
        );

        // One path a colour: a note's, the five hues', a faint file's, a tag's square, a ghost's
        // ring, and the dimmed discs and rings outside the lit node's neighbourhood.
        let paths: [gsk::PathBuilder; 11] = std::array::from_fn(|_| gsk::PathBuilder::new());
        let mut used = [false; 11];
        const DIM: usize = 9;
        let mut named = Vec::new();
        for (i, &p) in screen.iter().enumerate() {
            if !on_screen(p) || Some(i) == front {
                continue;
            }
            let (r, paint) = (radius(i), model::paint_of(graph.nodes[i].kind));
            let slot = match (paint, dimmed(i)) {
                (Paint::Ghost, true) => DIM + 1,
                (_, true) => DIM,
                (Paint::Ink, _) => 0,
                (Paint::Lane(n), _) => n,
                (Paint::Faint, _) => 6,
                (Paint::Tag, _) => 7,
                (Paint::Ghost, _) => 8,
            };
            used[slot] = true;
            let centre = graphene::Point::new(p[0] as f32, p[1] as f32);
            match paint {
                Paint::Tag => paths[slot].add_rect(&graphene::Rect::new(
                    (p[0] - r) as f32,
                    (p[1] - r) as f32,
                    (2.0 * r) as f32,
                    (2.0 * r) as f32,
                )),
                _ => paths[slot].add_circle(&centre, r as f32),
            }
            if camera.zoom >= LABEL_ZOOM || near.contains(&i) {
                named.push(i);
            }
        }
        for (slot, path) in paths.iter().enumerate().filter(|(slot, _)| used[*slot]) {
            let path = path.to_path();
            let faint = theme::at(ink, theme::GRAPH_FAINT_ALPHA);
            match slot {
                8 | 10 => snapshot.append_stroke(
                    &path,
                    &gsk::Stroke::new(1.5),
                    &theme::at(
                        ink,
                        match slot {
                            8 => theme::GRAPH_RING_ALPHA,
                            _ => theme::GRAPH_DIM_ALPHA,
                        },
                    ),
                ),
                _ => snapshot.append_fill(
                    &path,
                    gsk::FillRule::Winding,
                    &match slot {
                        0 => ink,
                        1..=5 => crate::highlight::lane_colour(slot),
                        6 | 7 => faint,
                        _ => theme::at(ink, theme::GRAPH_DIM_ALPHA),
                    },
                ),
            }
        }
        // The file in front in the accent, a ring round it.
        if let Some(i) = front.filter(|&i| on_screen(screen[i])) {
            let (p, r) = (screen[i], radius(i) as f32);
            let centre = graphene::Point::new(p[0] as f32, p[1] as f32);
            let disc = gsk::PathBuilder::new();
            disc.add_circle(&centre, r);
            snapshot.append_fill(&disc.to_path(), gsk::FillRule::Winding, &theme::accent());
            let ring = gsk::PathBuilder::new();
            ring.add_circle(&centre, r + 3.0);
            snapshot.append_stroke(&ring.to_path(), &gsk::Stroke::new(1.5), &theme::accent());
            named.push(i);
        }
        self.paint_labels(snapshot, &graph, &screen, &named, &radius, &dimmed, ink);
        snapshot.pop();
    }

    /// The names of the nodes `named`, right of each disc, [`LABELS_MAX`] at most.
    #[allow(clippy::too_many_arguments)]
    fn paint_labels(
        &self,
        snapshot: &gtk::Snapshot,
        graph: &Graph,
        screen: &[[f64; 2]],
        named: &[usize],
        radius: &dyn Fn(usize) -> f64,
        dimmed: &dyn Fn(usize) -> bool,
        ink: gdk::RGBA,
    ) {
        let mut labels = self.imp().labels.borrow_mut();
        // A whole vault panned across lays out every name in turn: kept to a few frames' worth.
        if labels.len() > 4 * LABELS_MAX {
            labels.clear();
        }
        for &i in named.iter().take(LABELS_MAX) {
            let node = &graph.nodes[i];
            let layout = labels.entry(node.key.clone()).or_insert_with(|| {
                let layout = self.create_pango_layout(Some(model::label(node)));
                layout.set_width(LABEL_WIDTH * pango::SCALE);
                layout.set_ellipsize(pango::EllipsizeMode::End);
                layout
            });
            let colour = match (dimmed(i), model::paint_of(node.kind)) {
                (true, _) => theme::at(ink, theme::GRAPH_DIM_ALPHA),
                (false, Paint::Ghost) => theme::at(ink, theme::GRAPH_FAINT_ALPHA),
                (false, _) => ink,
            };
            let height = layout.pixel_size().1 as f32;
            let p = screen[i];
            snapshot.save();
            snapshot.translate(&graphene::Point::new(
                (p[0] + radius(i) + 3.0) as f32,
                p[1] as f32 - height / 2.0,
            ));
            snapshot.append_layout(layout, &colour);
            snapshot.restore();
        }
    }
}

#[cfg(feature = "bench")]
impl GraphView {
    /// A press on node `i`, as a click lands one.
    pub fn press(&self, i: usize, keep: bool) {
        self.grab_focus();
        if let Some(open) = self.imp().open.borrow().clone() {
            open(i, keep);
        }
    }

    pub fn settled(&self) -> bool {
        self.imp().settled.get()
    }

    pub fn front(&self) -> Option<String> {
        self.imp().front.borrow().clone()
    }
}

impl Default for GraphView {
    fn default() -> Self {
        GraphView::new()
    }
}
