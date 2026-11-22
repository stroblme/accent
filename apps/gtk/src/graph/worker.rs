//! The graph's two threads (user decision 2026-11-21). The layout's thread holds the force
//! layout ([`Force`]) and takes the canvas's changes and drags in the order they were made,
//! between its steps. While the layout moves it steps it on a clock of its own, [`STEP`] apart,
//! and hands each state to the drawing thread, the newest replacing one not drawn yet. The
//! drawing thread draws the links and nodes ([`raster`]) for the newest places and the newest
//! view the canvas asked for, once the canvas has taken the last frame, and leaves the places
//! and that picture in a slot. A change shows at once, its new nodes where they were placed,
//! before the layout has taken a step, and a layout started from nothing shows its nodes before
//! its links (user decision 2026-11-21). So a long draw neither slows the layout down nor holds
//! the main loop, and a slow frame drops places rather than falling behind. With animations off
//! the layout settles in one go. Both threads end when the canvas drops its [`Worker`].

#[cfg(any(test, feature = "bench"))]
use std::sync::Weak;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use accent_core::graph::layout::{Layout as Force, Spread};
use gtk::{gdk, glib};

use super::camera::Camera;
use super::model::Paint;
use super::raster::{self, Scene, View};

/// The most steps a settle takes, which a cold layout of any size settles well within (300).
const MAX_SETTLE: usize = 1000;

/// How long a step of the moving layout takes at least: a frame at 60 Hz, so it moves at the
/// speed it was tuned at whatever the canvas paints.
const STEP: Duration = Duration::from_micros(16_667);

pub enum Cmd {
    /// A change of what the canvas shows, generation `generation`: the nodes `removed` taken out
    /// in this order as `Vec::swap_remove` does, then those `added` appended, each beside the
    /// node it names, then every link, by the indices after the change, and each node's paint.
    /// With `animate` the layout moves on its clock, else it settles at once.
    Change {
        generation: u64,
        removed: Vec<usize>,
        added: Vec<Option<usize>>,
        links: Vec<(usize, usize)>,
        paints: Vec<Paint>,
        animate: bool,
    },
    /// Node `i` held at a place, as a drag holds it.
    Pin(usize, [f32; 2]),
    Unpin(usize),
    /// Settle at once and hand the places over once: a drag let go with animations off.
    Settle,
    /// The nodes spread anew, moving there as the layout steps, or at once without animations.
    Spread(Spread),
}

/// The places the layout reached, for the generation of the canvas's graph they are of, and
/// the graph drawn there.
pub struct Frame {
    pub generation: u64,
    pub at: Vec<[f32; 2]>,
    pub settled: bool,
    /// None until the canvas has said how it wants the graph drawn.
    pub picture: Option<Picture>,
    /// Whether the places moved since the last frame, for the drill.
    #[cfg(any(test, feature = "bench"))]
    pub moved: bool,
}

/// The graph drawn for a view: at the camera it was drawn at, which is the fitted one while the
/// view fits, on a canvas of `size`.
pub struct Picture {
    pub texture: gdk::MemoryTexture,
    pub camera: Camera,
    pub size: [f64; 2],
    /// The view's serial, and the node it lit.
    pub serial: u64,
    pub lit: Option<usize>,
    /// How long it took to draw, in ms, and the generation drawn, for the drill.
    #[cfg(feature = "bench")]
    pub ms: f64,
    #[cfg(feature = "bench")]
    pub generation: u64,
}

pub struct Worker {
    tx: Sender<Cmd>,
    shared: Arc<Shared>,
}

/// What the two threads and the canvas share.
#[derive(Default)]
struct Shared {
    /// The newest frame drawn, for the canvas to take.
    slot: Mutex<Option<Frame>>,
    draw: Mutex<Draw>,
    /// Wakes the drawing thread when there is something to draw or the canvas took a frame.
    wake: Condvar,
}

/// What the drawing thread draws next.
struct Draw {
    places: Option<Places>,
    view: Option<(u64, View)>,
    /// Whether the places or the view changed since the last frame was drawn.
    dirty: bool,
    /// Whether the canvas took the last frame, so the next may be drawn.
    taken: bool,
    /// Whether the layout's thread has ended.
    done: bool,
}

impl Default for Draw {
    fn default() -> Self {
        Draw {
            places: None,
            view: None,
            dirty: false,
            taken: true,
            done: false,
        }
    }
}

/// A state of the layout, and what it is drawn with.
struct Places {
    generation: u64,
    at: Arc<Vec<[f32; 2]>>,
    settled: bool,
    /// Whether a drag holds a node, which a fitting view is not fitted round.
    held: bool,
    shape: Arc<Shape>,
    /// A layout started from nothing whose nodes are not drawn yet: they are, before its links.
    cold: bool,
    #[cfg(any(test, feature = "bench"))]
    moved: bool,
}

/// What a generation's places are drawn with: its links, its nodes' paints, their neighbours.
struct Shape {
    links: Vec<(usize, usize)>,
    paints: Vec<Paint>,
    near: Vec<Vec<usize>>,
}

impl Worker {
    /// The threads, the layout spread as `spread` says.
    pub fn start(spread: Spread) -> Worker {
        let (tx, rx) = mpsc::channel();
        let shared = Arc::new(Shared::default());
        let (layout, drawing) = (shared.clone(), shared.clone());
        std::thread::Builder::new()
            .name("accent-graph".into())
            .spawn(move || {
                lay_out(&rx, &layout, spread);
                lock(&layout.draw).done = true;
                layout.wake.notify_all();
            })
            .expect("spawn the graph's layout thread");
        std::thread::Builder::new()
            .name("accent-graph-draw".into())
            .spawn(move || draw(&drawing))
            .expect("spawn the graph's drawing thread");
        Worker { tx, shared }
    }

    pub fn send(&self, cmd: Cmd) {
        // The layout's thread only ends once this side drops its sender.
        let _ = self.tx.send(cmd);
    }

    /// Draw the graph as `view` asks from now on, `serial` naming it.
    pub fn view(&self, serial: u64, view: View) {
        let mut draw = lock(&self.shared.draw);
        draw.view = Some((serial, view));
        draw.dirty = true;
        self.shared.wake.notify_all();
    }

    /// The latest frame drawn, once; the next is drawn once this one is taken.
    pub fn take(&self) -> Option<Frame> {
        let frame = lock(&self.shared.slot).take();
        if frame.is_some() {
            lock(&self.shared.draw).taken = true;
            self.shared.wake.notify_all();
        }
        frame
    }

    /// What tells whether the threads have ended once this side is gone: what they share.
    #[cfg(any(test, feature = "bench"))]
    pub fn life(&self) -> Weak<impl Sized + use<>> {
        Arc::downgrade(&self.shared)
    }
}

/// `mutex` locked, whether or not a thread panicked holding it: what it holds is replaced
/// whole, never left half-written.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The layout's thread, until the canvas drops its sender.
fn lay_out(rx: &Receiver<Cmd>, shared: &Shared, spread: Spread) {
    let mut force = Force::new();
    force.set_spread(spread);
    let mut shape = Arc::new(Shape {
        links: Vec::new(),
        paints: Vec::new(),
        near: Vec::new(),
    });
    let (mut generation, mut animate) = (0, true);
    let mut held = None::<(usize, [f32; 2])>;
    // The places, the node a drag holds at its pin, handed to the drawing thread.
    let hand =
        |force: &Force, generation, shape: &Arc<Shape>, held: Option<(usize, [f32; 2])>, cold| {
            let mut at = force.positions().to_vec();
            if let Some((i, p)) = held.filter(|&(i, _)| i < at.len()) {
                at[i] = p;
            }
            let mut draw = lock(&shared.draw);
            let undrawn = draw
                .places
                .take()
                .filter(|p| p.cold && p.generation == generation);
            draw.places = Some(Places {
                generation,
                at: Arc::new(at),
                settled: force.settled(),
                held: held.is_some(),
                shape: shape.clone(),
                cold: cold || undrawn.is_some(),
                #[cfg(any(test, feature = "bench"))]
                moved: true,
            });
            draw.dirty = true;
            shared.wake.notify_all();
        };
    loop {
        // An empty layout, before the first change, has nothing to move.
        let moving = animate && !force.settled() && !force.positions().is_empty();
        // Moving, it takes what came meanwhile and steps on; still, it waits for the next.
        let mut cmds = Vec::new();
        if !moving {
            match rx.recv() {
                Ok(cmd) => cmds.push(cmd),
                Err(_) => return,
            }
        }
        loop {
            match rx.try_recv() {
                Ok(cmd) => cmds.push(cmd),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }
        let (mut settle, mut changed, mut cold) = (false, false, false);
        for cmd in cmds {
            let len = force.positions().len();
            match cmd {
                Cmd::Change {
                    generation: g,
                    removed,
                    added,
                    links,
                    paints,
                    animate: a,
                } => {
                    cold |= len == 0;
                    for i in removed {
                        force.remove(i);
                        // The node a drag holds moves as the layout's own does.
                        let last = force.positions().len();
                        held = (held.filter(|&(h, _)| h != i))
                            .map(|(h, at)| (if h == last { i } else { h }, at));
                    }
                    for near in added {
                        force.add(near);
                    }
                    force.set_links(&links);
                    let mut near = vec![Vec::new(); force.positions().len()];
                    for &(a, b) in &links {
                        near[a].push(b);
                        near[b].push(a);
                    }
                    shape = Arc::new(Shape {
                        links,
                        paints,
                        near,
                    });
                    (generation, animate) = (g, a);
                    settle |= !a;
                    changed = true;
                }
                Cmd::Pin(i, at) if i < len => {
                    force.pin(i, at);
                    held = Some((i, at));
                    changed = true;
                }
                // Handed over too, so a canvas waiting on a layout that settled under the
                // drag gets a frame and stops.
                Cmd::Unpin(i) if i < len => {
                    force.unpin(i);
                    held = None;
                    changed = true;
                }
                Cmd::Pin(..) | Cmd::Unpin(_) => {}
                Cmd::Settle => settle = true,
                Cmd::Spread(spread) => {
                    force.set_spread(spread);
                    settle |= !animate;
                }
            }
        }
        // A change shows where its nodes were placed, before a step.
        if changed {
            hand(&force, generation, &shape, held, cold);
        }
        let t = Instant::now();
        let moving = animate && !force.settled() && !force.positions().is_empty();
        match (settle, moving) {
            (true, _) => {
                for _ in 0..MAX_SETTLE {
                    if force.step() == 0.0 {
                        break;
                    }
                }
                hand(&force, generation, &shape, held, false);
            }
            (false, true) => {
                force.step();
                hand(&force, generation, &shape, held, false);
                std::thread::sleep(STEP.saturating_sub(t.elapsed()));
            }
            (false, false) => {}
        }
    }
}

/// The drawing thread, until the layout's thread has ended.
fn draw(shared: &Shared) {
    // The camera last drawn at, which a fitting view keeps while a drag holds a node.
    let mut fitted = Camera::default();
    loop {
        let (places, view, bare) = {
            let mut draw = lock(&shared.draw);
            while !draw.done && !(draw.dirty && draw.taken && draw.places.is_some()) {
                draw = shared.wake.wait(draw).unwrap_or_else(|p| p.into_inner());
            }
            let draw = &mut *draw;
            let Some(p) = draw.places.as_mut().filter(|_| !draw.done) else {
                return;
            };
            (draw.dirty, draw.taken) = (false, false);
            // A cold layout's nodes first, and its links in the next picture.
            let bare = p.cold && draw.view.is_some();
            if bare {
                p.cold = false;
                draw.dirty = true;
            }
            let places = Places {
                generation: p.generation,
                at: p.at.clone(),
                settled: p.settled,
                held: p.held,
                shape: p.shape.clone(),
                cold: false,
                #[cfg(any(test, feature = "bench"))]
                moved: std::mem::take(&mut p.moved),
            };
            (places, draw.view.clone(), bare)
        };
        #[cfg(feature = "bench")]
        let t = Instant::now();
        let picture = view.and_then(|(serial, view)| {
            let camera = match (view.camera.fitting, places.held) {
                (true, false) => Camera::fit(&places.at, view.size),
                (true, true) => fitted,
                (false, _) => view.camera,
            };
            fitted = camera;
            let scene = Scene {
                at: &places.at,
                paints: &places.shape.paints,
                near: &places.shape.near,
                links: if bare { &[] } else { &places.shape.links },
            };
            picture(&scene, &view, camera, serial)
        });
        #[cfg(feature = "bench")]
        let picture = picture.map(|p| Picture {
            ms: t.elapsed().as_secs_f64() * 1000.0,
            generation: places.generation,
            ..p
        });
        *lock(&shared.slot) = Some(Frame {
            generation: places.generation,
            at: places.at.to_vec(),
            // Not while its links are still to come, so the canvas waits for them.
            settled: places.settled && !bare,
            picture,
            #[cfg(any(test, feature = "bench"))]
            moved: places.moved,
        });
    }
}

/// `scene` drawn as `view` asks at `camera`; none on a canvas of no pixels.
fn picture(scene: &Scene, view: &View, camera: Camera, serial: u64) -> Option<Picture> {
    let px = raster::draw(scene, view, camera);
    (px.width > 0 && px.height > 0).then(|| Picture {
        texture: gdk::MemoryTexture::new(
            px.width as i32,
            px.height as i32,
            gdk::MemoryFormat::B8g8r8a8Premultiplied,
            &glib::Bytes::from_owned(px.data),
            px.width * 4,
        ),
        camera,
        size: view.size,
        serial,
        lit: view.lit,
        #[cfg(feature = "bench")]
        ms: 0.0,
        #[cfg(feature = "bench")]
        generation: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gtk::prelude::*;
    use std::time::{Duration, Instant};

    fn frame(worker: &Worker) -> Frame {
        let t = Instant::now();
        loop {
            if let Some(frame) = worker.take() {
                return frame;
            }
            assert!(t.elapsed() < Duration::from_secs(10), "no frame");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// `added` new nodes, `nodes` in all, the first two linked.
    fn change(generation: u64, added: usize, nodes: usize, animate: bool) -> Cmd {
        Cmd::Change {
            generation,
            removed: Vec::new(),
            added: vec![None; added],
            links: vec![(0, 1)],
            paints: vec![Paint::Ink; nodes],
            animate,
        }
    }

    fn view() -> View {
        View {
            camera: Camera::default(),
            size: [40.0, 30.0],
            scale: 2.0,
            lit: None,
            front: Some(0),
            current: None,
            colours: raster::Colours {
                ink: [0.0, 0.0, 0.0, 1.0],
                lanes: [[1.0; 4]; 5],
                accent: [0.0, 0.0, 1.0, 1.0],
            },
        }
    }

    /// A change shows at once where its nodes were placed, a moving layout hands over places
    /// on its own, drawn once there is a view, one without animations ends on one settled
    /// frame, and the threads end with the sender.
    #[test]
    fn the_thread_steps_draws_settles_and_ends() {
        let worker = Worker::start(Spread::default());
        worker.send(change(1, 3, 3, true));
        let first = frame(&worker);
        assert_eq!((first.generation, first.at.len()), (1, 3));
        assert!(!first.settled && first.moved && first.picture.is_none());
        worker.view(7, view());
        let drawn = loop {
            if let Some(picture) = frame(&worker).picture {
                break picture;
            }
        };
        assert_eq!(drawn.serial, 7);
        assert_eq!((drawn.texture.width(), drawn.texture.height()), (80, 60));
        // A change without animations, while the layout still moves.
        worker.send(change(2, 1, 4, false));
        let settled = loop {
            let f = frame(&worker);
            if f.generation == 2 && f.settled {
                break f;
            }
        };
        assert!(settled.at.len() == 4 && settled.moved);
        let drawn = settled.picture.expect("a picture of the settled layout");
        assert_eq!(drawn.serial, 7);
        std::thread::sleep(Duration::from_millis(50));
        assert!(worker.take().is_none(), "one frame a settle");
        // A new spread, animations still off, settles at once with the nodes further apart.
        worker.send(Cmd::Spread(Spread(2.0)));
        let spread = frame(&worker);
        let apart = |at: &[[f32; 2]]| (at[0][0] - at[1][0]).hypot(at[0][1] - at[1][1]);
        assert!(spread.settled && apart(&spread.at) > 1.5 * apart(&settled.at));
        let life = worker.life();
        drop(worker);
        let t = Instant::now();
        while life.upgrade().is_some() {
            assert!(t.elapsed() < Duration::from_secs(10), "the thread runs on");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// A cold layout settled at once shows its nodes, then a settled frame with its links.
    #[test]
    fn a_cold_layout_settled_at_once_gets_its_links() {
        let worker = Worker::start(Spread::default());
        let view = View {
            size: [400.0, 300.0],
            ..view()
        };
        worker.view(1, view.clone());
        worker.send(change(1, 2, 2, false));
        let settled = loop {
            let f = frame(&worker);
            if f.settled {
                break f;
            }
        };
        let picture = settled.picture.expect("a picture");
        let (w, h) = (picture.texture.width(), picture.texture.height());
        let mut pixels = vec![0; (w * h * 4) as usize];
        picture.texture.download(&mut pixels, (w * 4) as usize);
        let [a, b] = [0, 1].map(|i| picture.camera.to_screen(settled.at[i]));
        let mid = [
            (a[0] + b[0]) * view.scale / 2.0,
            (a[1] + b[1]) * view.scale / 2.0,
        ];
        let alpha = pixels[(mid[1] as usize * w as usize + mid[0] as usize) * 4 + 3];
        assert!(alpha > 0, "the link between the two nodes is drawn");
    }
}
