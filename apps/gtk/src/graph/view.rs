//! The graph's canvas (DESIGN.md, Graph): the links and nodes as the layout's thread drew them,
//! the names on top, and the input that moves round it, the keyboard's included: the arrows move
//! it from node to node, Enter opens the node it is on, and typing starts the pane's find bar on
//! the nodes' names.
//!
//! The canvas owns the [`Graph`] and where each of its nodes is. Every change to what it shows
//! goes through [`GraphView::apply`], into the graph and the layout's [`Worker`] together, so the
//! two keep their nodes in the same order. With animations on the worker moves the layout on its
//! own clock and the canvas shows the newest places at each frame, so the nodes settle as they
//! are watched; with them off it settles in one go. How the canvas wants the graph drawn, its
//! camera, size and what is lit, goes to the worker as each frame is drawn, and until a picture
//! of that comes back the last one is moved and scaled to the camera (user decision 2026-11-21),
//! so panning, zooming and the pointer never wait on the worker. A picture that lights another
//! node crossfades in over the one shown (user decision 2026-11-22), as one of the still layout
//! at another zoom does, and a picture of a camera the reader moved reaches [`OVERSCAN`] past the
//! canvas's edges, so a pan or a zoom out finds it drawn there meanwhile. The names follow the
//! picture on screen, placed best first wherever they overlap none placed before.

use std::cell::{Cell, RefCell};
use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene, gsk, pango};

use super::camera::{self, Arrow, Camera};
use super::model::{self, Changes, Graph, Paint};
use super::raster::{self, Colours, Rgba};
use super::worker::{Cmd, Picture, Worker};
use crate::theme;

/// The most names drawn in a frame, a whole vault zoomed in included.
const LABELS_MAX: usize = 400;
/// The most names kept laid out: past it only those drawn in the last frame are kept.
const LABELS_KEPT: usize = 4 * LABELS_MAX;
/// The widest a name is drawn before it is ellipsized, in pixels.
const LABEL_WIDTH: i32 = 160;
/// How far a wheel notch pans, in pixels.
const NOTCH: f64 = 64.0;
/// How long a new picture crossfades in over the one shown, in ms: quicker than the app's other
/// fades, since the pointer brings a new one at every node (user decision 2026-11-22).
const FADE_MS: u32 = 150;
/// How long the lit node stays lit after the pointer leaves it, so moving from node to node goes
/// from one neighbourhood straight to the next (user decision 2026-11-22).
const LINGER: Duration = Duration::from_millis(150);
/// How far past each edge a picture of a camera the reader moved is drawn, as a share of the
/// canvas's width and height: a pan of an eighth of the canvas, or a zoom step out from 90 % up
/// round any point and from 50 % up round the middle, still finds the picture there until the
/// next one lands.
const OVERSCAN: f64 = 0.125;

/// A press and whether it is still a click: where it went down and the node under it.
#[derive(Clone, Copy)]
pub struct Press {
    at: [f64; 2],
    node: Option<usize>,
    slop: f64,
}

/// A drag under way: from where, the node it holds or none for a pan, and how far it has
/// panned, `None` while it is still within a click's slop.
struct Drag {
    from: [f64; 2],
    node: Option<String>,
    panned: Option<[f64; 2]>,
}

/// What a press on a node asks of the tab: open its file, and keep it for a double click.
type Open = dyn Fn(usize, bool);

/// What a key typed over the canvas asks of the tab: the find bar opened, the key forwarded to it.
type Typed = dyn Fn(&gtk::EventControllerKey);

/// A node's name laid out, and the frame it was last drawn in.
pub struct Label {
    layout: pango::Layout,
    drawn: u64,
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct GraphView {
        pub graph: RefCell<Graph>,
        /// Where each node is, in the layout's units, in the graph's order.
        pub at: RefCell<Vec<[f32; 2]>>,
        pub camera: Cell<Camera>,
        /// The node lit: the one under the pointer, kept [`LINGER`] once the pointer left it.
        pub hover: Cell<Option<usize>>,
        /// The node under the pointer, for the cursor, and the wait before the lit node goes.
        pub under: Cell<Option<usize>>,
        pub linger: RefCell<Option<glib::SourceId>>,
        /// The file in front of the window, drawn in the accent, by key: it is no node until the
        /// answer holding it lands.
        pub front: RefCell<Option<String>>,
        /// The node the keyboard is on, by key, ringed while the canvas has the keyboard.
        pub current: RefCell<Option<String>>,
        /// Counts the changes sent to the layout, so only places of what is shown land.
        pub generation: Cell<u64>,
        pub worker: RefCell<Option<Worker>>,
        pub ticking: RefCell<Option<gtk::TickCallbackId>>,
        /// Whether the places shown are the settled layout of what is shown.
        pub settled: Cell<bool>,
        /// The last picture of the graph the worker drew, and the last view asked of it with
        /// its serial, which counts the views asked.
        pub picture: RefCell<Option<Picture>>,
        pub asked: RefCell<Option<raster::View>>,
        pub serial: Cell<u64>,
        /// The picture shown before the last, crossfading out under it while `fade` runs, and
        /// how far the crossfade has got, eased.
        pub from: RefCell<Option<Picture>>,
        pub fade: RefCell<Option<adw::TimedAnimation>>,
        pub progress: Cell<f64>,
        /// Whether the layout has placed the nodes since it started over: until then they are
        /// all in one spot, and nothing is drawn.
        pub placed: Cell<bool>,
        /// The node a drag holds, by key.
        pub dragging: RefCell<Option<String>>,
        /// Each name laid out once, by the node's key, until the style changes, and a count of
        /// the frames whose names were drawn.
        pub labels: RefCell<HashMap<String, Label>>,
        pub frame: Cell<u64>,
        /// The nodes best-linked first, and the generation they are of.
        pub by_degree: RefCell<(u64, Vec<usize>)>,
        /// A character's width and a line's height in the names' font, in pixels, until the
        /// style changes.
        pub metrics: Cell<Option<[f64; 2]>>,
        pub press: Cell<Option<Press>>,
        /// The pan a wheel notch eases towards: from where to where.
        pub easing: Cell<([f64; 2], [f64; 2])>,
        pub wheel: RefCell<Option<adw::TimedAnimation>>,
        pub open: RefCell<Option<Rc<Open>>>,
        pub typed: RefCell<Option<Rc<Typed>>>,
        pub zoomed: RefCell<Option<Rc<dyn Fn()>>>,
        /// How many of the layout's frames have landed since the last change, for the drill.
        #[cfg(feature = "bench")]
        pub frames: Cell<usize>,
        /// How long each frame took to draw, for the drill.
        #[cfg(feature = "bench")]
        pub painted: RefCell<Vec<f64>>,
        /// How long the worker took to draw each picture shown, for the drill.
        #[cfg(feature = "bench")]
        pub draws: RefCell<Vec<f64>>,
        /// When a picture of what is shown was first on screen, for the drill.
        #[cfg(feature = "bench")]
        pub shown_at: Cell<Option<std::time::Instant>>,
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
            let _drew = self.obj().paint(snapshot);
            #[cfg(feature = "bench")]
            if _drew {
                self.painted
                    .borrow_mut()
                    .push(t.elapsed().as_secs_f64() * 1000.0);
            }
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
        view.wire_keys();
        view.wire_fade();
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

    /// What a key that types text does: the find bar takes it.
    pub fn connect_typed(&self, f: impl Fn(&gtk::EventControllerKey) + 'static) {
        self.imp().typed.replace(Some(Rc::new(f)));
    }

    /// Hear a pan or a zoom, for the zoom readout.
    pub fn connect_zoomed(&self, f: impl Fn() + 'static) {
        self.imp().zoomed.replace(Some(Rc::new(f)));
    }

    /// Show what `changes` change, the nodes staying in place and the new ones placed beside the
    /// node they slide out of, and set the layout moving.
    pub fn apply(&self, changes: &Changes) {
        let imp = self.imp();
        if changes.is_empty() && changes.edges == imp.graph.borrow().edges {
            return;
        }
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
        let generation = imp.generation.get() + 1;
        imp.generation.set(generation);
        imp.settled.set(false);
        #[cfg(feature = "bench")]
        {
            imp.frames.set(0);
            imp.shown_at.set(None);
        }
        let (links, paints) = {
            let graph = imp.graph.borrow();
            let paints = graph.nodes.iter().map(|n| model::paint_of(n.kind));
            (graph.links(), paints.collect())
        };
        let animate = self.animations();
        imp.worker
            .borrow_mut()
            .get_or_insert_with(Worker::start)
            .send(Cmd::Change {
                generation,
                removed: changes.removed.clone(),
                added: changes.added.iter().map(|(_, near)| *near).collect(),
                links,
                paints,
                animate,
            });
        // Along with the change, so its first frame is drawn already.
        self.ask();
        self.run();
        self.queue_draw();
    }

    /// Start over with nothing shown and a cold layout, fitted again, the roots kept: for what
    /// shows a different part of the vault, so it is laid out afresh.
    pub fn restart(&self) {
        let imp = self.imp();
        imp.worker.replace(None);
        let roots = std::mem::take(&mut imp.graph.borrow_mut().roots);
        imp.graph.replace(Graph::default());
        imp.graph.borrow_mut().roots = roots;
        imp.at.borrow_mut().clear();
        imp.hover.set(None);
        imp.dragging.take();
        // Through the setter, so the zoom readout says Fit again.
        self.set_camera(Camera::default());
        imp.placed.set(false);
        imp.picture.take();
        imp.from.take();
        imp.asked.take();
    }

    fn animations(&self) -> bool {
        self.settings().is_gtk_enable_animations()
    }

    /// Take the layout's frames from the next frame of the canvas's on, until it settles.
    fn run(&self) {
        if self.imp().ticking.borrow().is_some() {
            return;
        }
        let id = self.add_tick_callback(|view, _| view.tick());
        self.imp().ticking.replace(Some(id));
    }

    /// A frame of the canvas: the newest places the layout reached and the newest picture
    /// shown, where they are of what is shown, until it has settled and the picture is of the
    /// last view asked.
    fn tick(&self) -> glib::ControlFlow {
        let imp = self.imp();
        let frame = match imp.worker.borrow().as_ref() {
            Some(worker) => worker.take(),
            None => {
                imp.ticking.take();
                return glib::ControlFlow::Break;
            }
        };
        let Some(frame) = frame else {
            return glib::ControlFlow::Continue;
        };
        if frame.generation != imp.generation.get() || frame.at.len() != imp.at.borrow().len() {
            return glib::ControlFlow::Continue;
        }
        imp.at.replace(frame.at);
        imp.settled.set(frame.settled);
        imp.placed.set(true);
        #[cfg(feature = "bench")]
        if frame.moved {
            imp.frames.set(imp.frames.get() + 1);
        }
        if let Some(picture) = frame.picture {
            if imp.dragging.borrow().is_none() {
                imp.camera.set(imp.camera.get().landed(picture.camera));
            }
            #[cfg(feature = "bench")]
            imp.draws.borrow_mut().push(picture.ms);
            self.land(picture, frame.settled);
        }
        self.queue_draw();
        let current = (imp.picture.borrow().as_ref()).is_some_and(|p| p.serial == imp.serial.get());
        if frame.settled && current && imp.dragging.borrow().is_none() {
            imp.ticking.take();
            return glib::ControlFlow::Break;
        }
        glib::ControlFlow::Continue
    }

    /// Show `picture`, crossfading to it from the one shown where it lights another node or, the
    /// layout `still`, shows it at another zoom: a moving layout's pictures are not faded, which
    /// would trail its nodes. One landing while a crossfade is less than half way takes the place
    /// of its end, and one landing later starts it again from that end, so the canvas never jumps
    /// by more than half a crossfade.
    fn land(&self, picture: Picture, still: bool) {
        let imp = self.imp();
        let Some(shown) = imp.picture.replace(None) else {
            imp.picture.replace(Some(picture));
            return;
        };
        let differs =
            shown.lit != picture.lit || (still && shown.camera.zoom != picture.camera.zoom);
        let early = imp.from.borrow().is_some() && imp.progress.get() < 0.5;
        imp.picture.replace(Some(picture));
        if differs && !early {
            imp.from.replace(Some(shown));
            imp.progress.set(0.0);
            // With animations off it skips to its end at once, and nothing fades.
            let fade = imp.fade.borrow().clone();
            if let Some(fade) = fade {
                fade.play();
            }
        }
    }

    /// The crossfade between two pictures, eased out as libadwaita's fades are.
    fn wire_fade(&self) {
        let target = adw::CallbackAnimationTarget::new(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |t| {
                view.imp().progress.set(t);
                view.queue_draw();
            }
        ));
        let fade = adw::TimedAnimation::new(self, 0.0, 1.0, FADE_MS, target);
        fade.set_easing(adw::Easing::EaseOutCubic);
        fade.connect_done(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_| {
                view.imp().from.take();
                view.queue_draw();
            }
        ));
        self.imp().fade.replace(Some(fade));
    }

    /// Hold node `key` under canvas point `at`, as a drag does: its neighbours follow it as the
    /// layout steps, and with animations off it moves alone.
    fn hold(&self, key: &str, at: [f64; 2]) {
        let imp = self.imp();
        let Some(i) = imp.graph.borrow().index_of(key) else {
            return;
        };
        let world = self.camera().to_world(at);
        if let Some(place) = imp.at.borrow_mut().get_mut(i) {
            *place = world;
        }
        if imp.dragging.replace(Some(key.to_string())).is_none() {
            self.set_cursor_from_name(Some("grabbing"));
        }
        if let Some(worker) = imp.worker.borrow().as_ref() {
            worker.send(Cmd::Pin(i, world));
        }
        self.run();
        self.queue_draw();
    }

    /// The drag let its node go, which settles back with the rest.
    fn let_go(&self) {
        let imp = self.imp();
        let Some(key) = imp.dragging.take() else {
            return;
        };
        self.set_cursor_from_name(Some("pointer"));
        if let (Some(i), Some(worker)) = (
            imp.graph.borrow().index_of(&key),
            imp.worker.borrow().as_ref(),
        ) {
            worker.send(Cmd::Unpin(i));
            // With animations off the node moved alone, and the rest settle round it at once.
            if !self.animations() {
                worker.send(Cmd::Settle);
            }
        }
        imp.settled.set(false);
        self.run();
    }

    /// Draw the file keyed `key` in the accent, or none.
    pub fn set_front(&self, key: Option<String>) {
        if *self.imp().front.borrow() != key {
            self.imp().front.replace(key);
            self.queue_draw();
        }
    }

    /// A file or a folder moved: its nodes keep their places under the new keys.
    pub fn rename(&self, from: &str, to: &str) {
        let imp = self.imp();
        imp.graph.borrow_mut().rename(from, to);
        for key in [&imp.current, &imp.front] {
            let moved = key.borrow().as_deref().map(|k| model::moved(k, from, to));
            if let Some(Some(moved)) = moved {
                key.replace(Some(moved));
            }
        }
        imp.labels.borrow_mut().clear();
        self.queue_draw();
    }

    /// The node the keyboard is on, by key.
    pub fn current(&self) -> Option<String> {
        self.imp().current.borrow().clone()
    }

    /// Put the keyboard on node `key`, said to a screen reader as its name and links, and
    /// `centre` it, as a find bar's step does, or else keep it on the canvas.
    pub fn set_current(&self, key: &str, centre: bool) {
        let imp = self.imp();
        let (i, said) = {
            let graph = imp.graph.borrow();
            let Some(i) = graph.index_of(key) else {
                return;
            };
            let links = graph.adjacency[i].len();
            let label = model::label(&graph.nodes[i]).to_string();
            (
                i,
                match links {
                    1 => format!("{label}, 1 link"),
                    n => format!("{label}, {n} links"),
                },
            )
        };
        imp.current.replace(Some(key.to_string()));
        self.update_property(&[gtk::accessible::Property::Description(&said)]);
        let (size, Some(&p)) = (self.size(), imp.at.borrow().get(i)) else {
            return;
        };
        let mut camera = self.camera();
        let s = camera.to_screen(p);
        let inside = |v: f64, max: f64| (camera::MARGIN..=max - camera::MARGIN).contains(&v);
        if centre || !(inside(s[0], size[0]) && inside(s[1], size[1])) {
            camera.centre(p, size);
            camera.fitting = false;
            self.set_camera(camera);
        }
        self.queue_draw();
    }

    /// An arrow from the node the keyboard is on: to the nearest node it links to that way, else
    /// the nearest node that way at all. The first press puts the keyboard on the file in front.
    fn arrow(&self, arrow: Arrow) {
        let imp = self.imp();
        let next = {
            let graph = imp.graph.borrow();
            let at = imp.at.borrow();
            let current = imp
                .current
                .borrow()
                .as_deref()
                .and_then(|k| graph.index_of(k));
            let front = imp
                .front
                .borrow()
                .as_deref()
                .and_then(|k| graph.index_of(k));
            let next = match current {
                None => front.or((!graph.nodes.is_empty()).then_some(0)),
                Some(i) => {
                    let near = graph.adjacency[i].iter().map(|&j| (j, at[j]));
                    camera::step(at[i], arrow, near).or_else(|| {
                        let all = (0..at.len()).filter(|&j| j != i).map(|j| (j, at[j]));
                        camera::step(at[i], arrow, all)
                    })
                }
            };
            next.map(|i| graph.nodes[i].key.clone())
        };
        if let Some(key) = next {
            self.set_current(&key, false);
        }
    }

    /// Open the node the keyboard is on, as a single click does.
    fn open_current(&self) {
        let i = (self.imp().current.borrow().as_deref()).and_then(|k| self.graph().index_of(k));
        let open = self.imp().open.borrow().clone();
        if let (Some(i), Some(open)) = (i, open) {
            open(i, false);
        }
    }

    /// The arrows, Enter, and a key that types text, which opens the find bar on it; every other
    /// key goes on to the window, Escape to the find bar.
    fn wire_keys(&self) {
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(glib::clone!(
            #[weak(rename_to = view)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |keys, key, _, state| view.key(keys, key, state)
        ));
        self.add_controller(keys);
        // The ring round the node the keyboard is on shows only while the canvas has it.
        let focus = gtk::EventControllerFocus::new();
        focus.connect_enter(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_| view.queue_draw()
        ));
        focus.connect_leave(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_| view.queue_draw()
        ));
        self.add_controller(focus);
    }

    fn key(
        &self,
        keys: &gtk::EventControllerKey,
        key: gdk::Key,
        state: gdk::ModifierType,
    ) -> glib::Propagation {
        let held = gdk::ModifierType::CONTROL_MASK
            | gdk::ModifierType::ALT_MASK
            | gdk::ModifierType::SUPER_MASK
            | gdk::ModifierType::META_MASK;
        if state.intersects(held) {
            return glib::Propagation::Proceed;
        }
        let arrow = match key {
            gdk::Key::Left | gdk::Key::KP_Left => Some(Arrow::Left),
            gdk::Key::Right | gdk::Key::KP_Right => Some(Arrow::Right),
            gdk::Key::Up | gdk::Key::KP_Up => Some(Arrow::Up),
            gdk::Key::Down | gdk::Key::KP_Down => Some(Arrow::Down),
            _ => None,
        };
        if let Some(arrow) = arrow {
            self.arrow(arrow);
            return glib::Propagation::Stop;
        }
        if matches!(
            key,
            gdk::Key::Return | gdk::Key::KP_Enter | gdk::Key::ISO_Enter
        ) {
            self.open_current();
            return glib::Propagation::Stop;
        }
        // A dead key starts a composed letter, which the find bar's entry finishes.
        use glib::translate::IntoGlib;
        let dead = (0xfe50..=0xfe8f).contains(&key.into_glib());
        let text = key.to_unicode().is_some_and(|c| !c.is_control());
        let typed = self.imp().typed.borrow().clone();
        match (typed, text || dead) {
            (Some(typed), true) => {
                typed(keys);
                glib::Propagation::Stop
            }
            _ => glib::Propagation::Proceed,
        }
    }

    /// The names are laid out again in the new style, and everything drawn in its colours.
    pub fn restyle(&self) {
        self.imp().labels.borrow_mut().clear();
        self.imp().metrics.set(None);
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

    /// Fitted again to what it shows, at once, and kept so as it settles.
    pub fn fit(&self) {
        let camera = Camera::fit(&self.imp().at.borrow(), self.size());
        self.set_camera(camera);
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

        // The first button or a finger on a node drags the node, anywhere else pans; the middle
        // button always pans.
        let drag = gtk::GestureDrag::builder().button(0).build();
        let held = Rc::new(RefCell::new(None::<Drag>));
        drag.connect_drag_begin(glib::clone!(
            #[weak(rename_to = view)]
            self,
            #[strong]
            held,
            move |drag, x, y| {
                let button = drag.current_button();
                if !matches!(button, 0..=2) {
                    drag.set_state(gtk::EventSequenceState::Denied);
                    return;
                }
                let node = (button != 2)
                    .then(|| view.node_at([x, y]))
                    .flatten()
                    .map(|i| view.graph().nodes[i].key.clone());
                held.replace(Some(Drag {
                    from: [x, y],
                    node,
                    panned: None,
                }));
            }
        ));
        drag.connect_drag_update(glib::clone!(
            #[weak(rename_to = view)]
            self,
            #[strong]
            held,
            move |drag, dx, dy| {
                let mut held = held.borrow_mut();
                let Some(on) = held.as_mut() else {
                    return;
                };
                let was = match on.panned {
                    Some(was) => was,
                    None if dx.hypot(dy) <= crate::widgets::click_slop(drag) => return,
                    None => [0.0; 2],
                };
                on.panned = Some([dx, dy]);
                match &on.node {
                    Some(key) => view.hold(key, [on.from[0] + dx, on.from[1] + dy]),
                    None => {
                        let mut camera = view.camera();
                        camera.pan([dx - was[0], dy - was[1]]);
                        view.set_camera(camera);
                    }
                }
            }
        ));
        drag.connect_drag_end(glib::clone!(
            #[weak(rename_to = view)]
            self,
            #[strong]
            held,
            move |_, _, _| {
                held.take();
                view.let_go();
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

    /// The pointer over `node`, or over none: a node lights at once, and the lit one stays lit
    /// [`LINGER`] after the pointer left it, unless the pointer reaches another first.
    fn hover(&self, node: Option<usize>) {
        let imp = self.imp();
        if imp.under.replace(node) != node {
            self.set_cursor_from_name(node.map(|_| "pointer"));
        }
        if node.is_some() || imp.hover.get().is_none() {
            if let Some(wait) = imp.linger.take() {
                wait.remove();
            }
            self.light(node);
        } else if imp.linger.borrow().is_none() {
            let view = self.downgrade();
            let wait = glib::timeout_add_local_once(LINGER, move || {
                if let Some(view) = view.upgrade() {
                    view.imp().linger.take();
                    view.light(None);
                }
            });
            imp.linger.replace(Some(wait));
        }
    }

    /// Light `node` and its neighbours, or none.
    fn light(&self, node: Option<usize>) {
        if self.imp().hover.replace(node) != node {
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

    /// How the canvas wants the graph drawn now, sent to the worker when it changed: its camera,
    /// size and display scale, what is lit, and the theme's colours.
    fn ask(&self) {
        let imp = self.imp();
        let size = self.size();
        if size[0] < 1.0 || size[1] < 1.0 {
            return;
        }
        let view = {
            let graph = imp.graph.borrow();
            let index = |key: &RefCell<Option<String>>| {
                (key.borrow().as_deref()).and_then(|key| graph.index_of(key))
            };
            let camera = imp.camera.get();
            // The worker fits a fitting camera to what it draws, which then holds all of it, and
            // draws any other OVERSCAN past each edge.
            let (camera, size) = match camera.fitting {
                true => (Camera::default(), size),
                false => {
                    let m = size.map(|side| (side * OVERSCAN).round());
                    let offset = [camera.offset[0] + m[0], camera.offset[1] + m[1]];
                    let size = [size[0] + 2.0 * m[0], size[1] + 2.0 * m[1]];
                    (Camera { offset, ..camera }, size)
                }
            };
            let surface = self.native().and_then(|n| n.surface());
            raster::View {
                camera,
                size,
                scale: surface.map_or(f64::from(self.scale_factor()), |s| s.scale()),
                lit: imp.hover.get(),
                front: index(&imp.front),
                current: index(&imp.current).filter(|_| self.has_focus()),
                colours: Colours {
                    ink: rgba(self.color()),
                    lanes: std::array::from_fn(|k| rgba(crate::highlight::lane_colour(k + 1))),
                    accent: rgba(theme::accent()),
                },
            }
        };
        if imp.asked.borrow().as_ref() == Some(&view) {
            return;
        }
        let serial = imp.serial.get() + 1;
        match imp.worker.borrow().as_ref() {
            Some(worker) => worker.view(serial, view.clone()),
            None => return,
        }
        imp.serial.set(serial);
        imp.asked.replace(Some(view));
        self.run();
    }

    /// The worker's last picture of the graph, moved and scaled from the camera it was drawn at
    /// to the canvas's, crossfading in over the one before while the fade runs, then the names on
    /// top, at the places it was drawn with. Whether there was anything to draw.
    fn paint(&self, snapshot: &gtk::Snapshot) -> bool {
        self.ask();
        let imp = self.imp();
        let graph = imp.graph.borrow();
        let at = imp.at.borrow();
        let picture = imp.picture.borrow();
        let Some(picture) = picture.as_ref() else {
            return false;
        };
        if graph.nodes.is_empty() || at.len() != graph.nodes.len() || !imp.placed.get() {
            return false;
        }
        let size = self.size();
        let camera = imp.camera.get();
        snapshot.push_clip(&graphene::Rect::new(
            0.0,
            0.0,
            size[0] as f32,
            size[1] as f32,
        ));
        #[cfg(feature = "bench")]
        if picture.generation == imp.generation.get() && imp.shown_at.get().is_none() {
            imp.shown_at.set(Some(std::time::Instant::now()));
        }
        let append = |picture: &Picture| {
            let (drawn, k) = (picture.camera, camera.zoom / picture.camera.zoom);
            snapshot.append_scaled_texture(
                &picture.texture,
                gsk::ScalingFilter::Linear,
                &graphene::Rect::new(
                    (camera.offset[0] - drawn.offset[0] * k) as f32,
                    (camera.offset[1] - drawn.offset[1] * k) as f32,
                    (picture.size[0] * k) as f32,
                    (picture.size[1] * k) as f32,
                ),
            );
        };
        let from = imp.from.borrow();
        let fade = (from.as_ref()).map(|from| (from, imp.progress.get()));
        let fade = fade.filter(|&(_, t)| t < 1.0);
        match fade {
            Some((from, t)) => {
                snapshot.push_cross_fade(t);
                append(from);
                snapshot.pop();
                append(picture);
                snapshot.pop();
            }
            None => append(picture),
        }
        let screen: Vec<[f64; 2]> = at.iter().map(|&p| camera.to_screen(p)).collect();
        let radius = |i: usize| {
            (f64::from(model::radius(graph.adjacency[i].len())) * camera.zoom)
                .max(camera::MIN_RADIUS)
        };
        // The node each picture shown lights, a picture of a graph since changed lighting one it
        // may no longer hold, and its share of the canvas.
        let lit = |p: &Picture| p.lit.filter(|&l| l < graph.nodes.len());
        let ends = match fade {
            Some((from, t)) if lit(from) != lit(picture) => {
                vec![(lit(from), 1.0 - t), (lit(picture), t)]
            }
            _ => vec![(lit(picture), 1.0)],
        };
        self.paint_labels(snapshot, &graph, &screen, &radius, &ends, self.color());
        snapshot.pop();
        true
    }

    /// The names placed for each lit node of `ends`, drawn at its share, so they crossfade with
    /// the pictures: a name placed for both at both shares together. Each sits right of its disc,
    /// dimmed outside the lit node's neighbourhood.
    fn paint_labels(
        &self,
        snapshot: &gtk::Snapshot,
        graph: &Graph,
        screen: &[[f64; 2]],
        radius: &dyn Fn(usize) -> f64,
        ends: &[(Option<usize>, f64)],
        ink: gdk::RGBA,
    ) {
        let imp = self.imp();
        let frame = imp.frame.get() + 1;
        imp.frame.set(frame);
        // Each name drawn with its alpha, and where it is in that list.
        let mut names: Vec<(usize, pango::Layout, f32)> = Vec::new();
        let mut slot: HashMap<usize, usize> = HashMap::new();
        for &(lit, share) in ends {
            let near: HashSet<usize> = lit
                .map(|l| graph.adjacency[l].iter().copied().chain([l]).collect())
                .unwrap_or_default();
            for (i, layout) in self.place(graph, screen, radius, lit) {
                let alpha = match (lit.is_some() && !near.contains(&i), graph.nodes[i].kind) {
                    (true, _) => theme::GRAPH_DIM_ALPHA,
                    (false, kind) if model::paint_of(kind) == Paint::Ghost => {
                        theme::GRAPH_FAINT_ALPHA
                    }
                    (false, _) => ink.alpha(),
                } * share as f32;
                match slot.entry(i) {
                    Entry::Occupied(at) => names[*at.get()].2 += alpha,
                    Entry::Vacant(at) => {
                        at.insert(names.len());
                        names.push((i, layout, alpha));
                    }
                }
            }
        }
        for (i, layout, alpha) in names {
            let height = layout.pixel_size().1 as f32;
            let p = screen[i];
            snapshot.save();
            snapshot.translate(&graphene::Point::new(
                (p[0] + radius(i) + 3.0) as f32,
                p[1] as f32 - height / 2.0,
            ));
            snapshot.append_layout(&layout, &theme::at(ink, alpha));
            snapshot.restore();
        }
        // A whole vault panned across lays out every name in turn: past a few frames' worth only
        // the names drawn in this one are kept.
        let mut labels = imp.labels.borrow_mut();
        if labels.len() > LABELS_KEPT {
            labels.retain(|_, label| label.drawn == frame);
        }
    }

    /// The names that go on the canvas with `lit` lit, best first, each laid out: the file in
    /// front, the node the keyboard is on and the lit node always, then the lit node's
    /// neighbours and the rest, best-linked first, each where its box meets none placed before,
    /// [`LABELS_MAX`] in all. A name's box is guessed before it is laid out, so one that does not
    /// fit costs no layout.
    fn place(
        &self,
        graph: &Graph,
        screen: &[[f64; 2]],
        radius: &dyn Fn(usize) -> f64,
        lit: Option<usize>,
    ) -> Vec<(usize, pango::Layout)> {
        let imp = self.imp();
        let index = |key: &RefCell<Option<String>>| {
            (key.borrow().as_deref()).and_then(|k| graph.index_of(k))
        };
        let always = [index(&imp.front), index(&imp.current), lit];
        let mut near = lit.map_or_else(Vec::new, |l| graph.adjacency[l].clone());
        near.sort_by_key(|&j| std::cmp::Reverse(graph.adjacency[j].len()));
        let by_degree = self.by_degree(graph);
        let [char_width, line] = self.metrics();
        let frame = imp.frame.get();
        let mut labels = imp.labels.borrow_mut();
        let mut taken = camera::Occupied::new(self.size(), imp.camera.get().offset);
        let mut placed: Vec<(usize, pango::Layout)> = Vec::new();
        let candidates = always.into_iter().flatten().chain(near);
        for i in candidates.chain(by_degree.iter().copied()) {
            if placed.len() == LABELS_MAX {
                break;
            }
            let must = always.contains(&Some(i));
            if must && placed.iter().any(|&(j, _)| j == i) {
                continue;
            }
            let room = |[width, height]: [f64; 2]| {
                let (x, y) = (screen[i][0] + radius(i) + 3.0, screen[i][1] - height / 2.0);
                [x, y, x + width, y + height]
            };
            let fits = |b: [f64; 4]| match must {
                true => taken.shows(b),
                false => taken.fits(b),
            };
            let node = &graph.nodes[i];
            let name = model::label(node);
            let guess = name.chars().count() as f64 * char_width;
            if !fits(room([guess.min(f64::from(LABEL_WIDTH)), line])) {
                continue;
            }
            if !labels.contains_key(&node.key) {
                let layout = self.create_pango_layout(Some(name));
                layout.set_width(LABEL_WIDTH * pango::SCALE);
                layout.set_ellipsize(pango::EllipsizeMode::End);
                labels.insert(node.key.clone(), Label { layout, drawn: 0 });
            }
            let Some(label) = labels.get_mut(&node.key) else {
                continue;
            };
            let (width, height) = label.layout.pixel_size();
            let b = room([f64::from(width), f64::from(height)]);
            if !fits(b) {
                continue;
            }
            label.drawn = frame;
            taken.take(b);
            placed.push((i, label.layout.clone()));
        }
        placed
    }

    /// The nodes best-linked first, the lower index first where two have as many links, sorted
    /// once a generation.
    fn by_degree(&self, graph: &Graph) -> std::cell::Ref<'_, Vec<usize>> {
        let imp = self.imp();
        let generation = imp.generation.get();
        let stale = {
            let (of, order) = &*imp.by_degree.borrow();
            *of != generation || order.len() != graph.nodes.len()
        };
        if stale {
            let mut order: Vec<usize> = (0..graph.nodes.len()).collect();
            order.sort_by_key(|&i| std::cmp::Reverse(graph.adjacency[i].len()));
            imp.by_degree.replace((generation, order));
        }
        std::cell::Ref::map(imp.by_degree.borrow(), |(_, order)| order)
    }

    /// A character's width and a line's height in the names' font, in pixels, which guess a
    /// name's box before it is laid out.
    fn metrics(&self) -> [f64; 2] {
        if let Some(metrics) = self.imp().metrics.get() {
            return metrics;
        }
        let font = self.pango_context().metrics(None, None);
        let metrics = [
            font.approximate_char_width(),
            font.ascent() + font.descent(),
        ]
        .map(|v| f64::from(v) / f64::from(pango::SCALE));
        self.imp().metrics.set(Some(metrics));
        metrics
    }
}

/// `colour` as the drawer takes it.
fn rgba(colour: gdk::RGBA) -> Rgba {
    [colour.red(), colour.green(), colour.blue(), colour.alpha()]
}

#[cfg(feature = "bench")]
impl GraphView {
    /// A key pressed over the canvas, as the keyboard lands one: the arrows and Enter.
    pub fn press_key(&self, key: gdk::Key) {
        self.grab_focus();
        self.key(
            &gtk::EventControllerKey::new(),
            key,
            gdk::ModifierType::empty(),
        );
    }

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

    /// The layout's frames landed since the last change.
    pub fn frames(&self) -> usize {
        self.imp().frames.get()
    }

    /// When a picture of what is shown was first on screen.
    pub fn shown_at(&self) -> Option<std::time::Instant> {
        self.imp().shown_at.get()
    }

    /// How long each frame drawn since the last call took, in ms.
    pub fn take_paints(&self) -> Vec<f64> {
        self.imp().painted.take()
    }

    /// How long the worker took to draw each picture shown since the last call, in ms.
    pub fn take_draws(&self) -> Vec<f64> {
        self.imp().draws.take()
    }

    /// Input `k` of a run, as the pointer gives it: a pan, a zoom in round the middle, or the
    /// pointer over node `k`.
    pub fn bench_input(&self, k: usize) {
        let size = self.size();
        match k % 3 {
            0 => {
                let mut camera = self.camera();
                camera.pan([24.0, 12.0]);
                self.set_camera(camera);
            }
            1 => self.zoom_to(self.camera().zoom * 1.1, [size[0] / 2.0, size[1] / 2.0]),
            _ => {
                let at = self.imp().at.borrow().get(k).copied();
                if let Some(p) = at {
                    let node = self.node_at(self.camera().to_screen(p));
                    self.hover(node);
                }
            }
        }
    }

    /// The serial of the last view asked of the worker, and of the picture shown.
    pub fn serials(&self) -> (u64, u64) {
        let imp = self.imp();
        let shown = imp.picture.borrow().as_ref().map_or(0, |p| p.serial);
        (imp.serial.get(), shown)
    }

    /// What tells whether the layout's thread has ended once the canvas is gone.
    pub fn worker_life(&self) -> Option<std::sync::Weak<impl Sized + use<>>> {
        self.imp().worker.borrow().as_ref().map(Worker::life)
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
