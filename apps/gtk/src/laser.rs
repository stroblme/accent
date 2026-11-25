//! Presentation's laser pointer (DESIGN.md, Views), Xournal++'s laser highlighter: while it is
//! up, a press and drag over the presented document draws a translucent red stroke, and the
//! strokes fade together once the last one has rested a moment. They lie over the presented pane
//! in its own coordinates, so no document view knows of them and a scroll leaves them where they
//! were drawn. The pointer meanwhile is the dot a press leaves, which the laser keeps on whatever
//! lies under it, so no view knows of that either, and which goes while the pointer rests.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene, gsk};

use crate::widgets::Debounce;

/// Xournal++'s laser highlighter: red at its highlighters' alpha of 120 of 255.
const COLOUR: gdk::RGBA = gdk::RGBA::new(1.0, 0.0, 0.0, 120.0 / 255.0);
/// How wide the strokes are on the screen (user decision 2026-11-18), where Xournal++ draws its
/// medium 8.5 pt at the page's zoom.
const WIDTH: f32 = 12.0;
/// How long the strokes stay once the last one ends, Xournal++'s `laserPointerFadeOutTime`.
const HOLD: Duration = Duration::from_millis(500);
/// How long they then take to fade out: Xournal++'s 25 of 255 every 50 ms, evenly.
const FADE_MS: u32 = 510;
/// How long the pointer rests before its dot goes, as PowerPoint's Automatic pointer hides after
/// three seconds without a move. A key is no move, so paging leaves it gone.
pub(crate) const IDLE: Duration = Duration::from_secs(3);

/// The strokes and what draws them: the presses on the document area they lie over, and the
/// moves there that show its dot.
pub struct Laser {
    painter: Painter,
    drag: gtk::GestureDrag,
    motion: gtk::EventControllerMotion,
    /// The widgets showing the dot, so the laser gives each its own cursor back as it goes.
    covered: RefCell<Vec<Covered>>,
    /// Whether the pointer has rested past [`IDLE`], its dot gone.
    resting: Rc<Cell<bool>>,
    idle: Debounce,
    hold: Debounce,
    fade: adw::TimedAnimation,
}

/// A widget the laser shows its dot on: the handler that puts the dot back whenever the widget
/// sets a cursor of its own, and the cursor it last asked for, which it gets back.
struct Covered {
    widget: glib::WeakRef<gtk::Widget>,
    handler: glib::SignalHandlerId,
    wanted: Rc<RefCell<Option<gdk::Cursor>>>,
}

impl Laser {
    pub fn new() -> Rc<Laser> {
        let painter: Painter = glib::Object::builder()
            .property("can-target", false)
            .build();
        let target = adw::PropertyAnimationTarget::new(&painter, "opacity");
        let fade = adw::TimedAnimation::new(&painter, 1.0, 0.0, FADE_MS, target);
        fade.set_easing(adw::Easing::Linear);
        // Played out, or skipped to its end with animations off: the strokes are gone, and the
        // next comes in at full strength.
        fade.connect_done(glib::clone!(
            #[weak]
            painter,
            move |_| {
                painter.imp().strokes.take();
                painter.set_opacity(1.0);
                painter.queue_draw();
            }
        ));
        // A press wherever it lands on the document, the mouse's, a finger's or a pen's, taken
        // ahead of the document, so that it neither selects, follows a link nor moves its page.
        // The wheel and the keys are left to it.
        let drag = gtk::GestureDrag::builder()
            .propagation_phase(gtk::PropagationPhase::Capture)
            .build();
        // A move ahead of the document too, which may set its cursor as it hears it.
        let motion = gtk::EventControllerMotion::builder()
            .propagation_phase(gtk::PropagationPhase::Capture)
            .build();
        let laser = Rc::new(Laser {
            painter,
            drag,
            motion,
            covered: RefCell::default(),
            resting: Rc::default(),
            idle: Debounce::new(IDLE),
            hold: Debounce::new(HOLD),
            fade,
        });
        let weak = Rc::downgrade(&laser);
        laser.motion.connect_motion(move |motion, x, y| {
            if let (Some(laser), Some(area)) = (weak.upgrade(), motion.widget()) {
                laser.stir();
                laser.cover(&area, x, y);
            }
        });
        let weak = Rc::downgrade(&laser);
        laser.drag.connect_drag_begin(move |gesture, x, y| {
            gesture.set_state(gtk::EventSequenceState::Claimed);
            if let Some(laser) = weak.upgrade() {
                laser.stir();
                laser.begin(x, y);
            }
        });
        let painter = laser.painter.downgrade();
        laser.drag.connect_drag_update(move |gesture, dx, dy| {
            if let (Some(painter), Some((x, y))) = (painter.upgrade(), gesture.start_point()) {
                painter.extend(x + dx, y + dy);
            }
        });
        let weak = Rc::downgrade(&laser);
        laser.drag.connect_drag_end(move |_, _, _| {
            if let Some(laser) = weak.upgrade() {
                let fade = laser.fade.clone();
                laser.hold.call(move || fade.play());
            }
        });
        laser
    }

    /// Whether the laser is up over a document area.
    pub fn is_on(&self) -> bool {
        self.painter.parent().is_some()
    }

    /// Draw over `area`, the presented pane's document area, with the presses made there; one up
    /// over another pane's leaves it, its strokes with it.
    pub fn lay_over(self: &Rc<Self>, area: &gtk::Overlay) {
        if self.painter.parent().as_ref() == Some(area.upcast_ref()) {
            return;
        }
        self.take_off();
        area.add_overlay(&self.painter);
        area.add_controller(self.drag.clone());
        area.add_controller(self.motion.clone());
        // `L` is a key, so the laser may come up under a pointer that has not moved, its dot
        // showing where until the pointer has rested.
        let area = area.upcast_ref();
        if let Some((x, y)) = pointer_over(area) {
            self.cover(area, x, y);
        }
        self.stir();
    }

    /// Put the laser away, its strokes with it.
    pub fn take_off(&self) {
        let Some(area) = self.painter.parent() else {
            return;
        };
        area.remove_controller(&self.drag);
        area.remove_controller(&self.motion);
        for covered in self.covered.take() {
            if let Some(widget) = covered.widget.upgrade() {
                widget.disconnect(covered.handler);
                widget.set_cursor(covered.wanted.borrow().as_ref());
            }
        }
        self.idle.cancel();
        self.resting.set(false);
        self.painter.unparent();
        self.hold.cancel();
        self.fade.reset();
        self.painter.imp().strokes.take();
    }

    /// Show the dot on each widget from the one at `(x, y)` up to `area`, the area too. GTK shows
    /// the cursor of the deepest widget under the pointer that sets one, and a document sets its
    /// own as it pleases (a PDF its I-beam on every move, a text view or a shell theirs), so each
    /// gets the dot back whenever it does, keeping what it asked for until the laser goes.
    fn cover(&self, area: &gtk::Widget, x: f64, y: f64) {
        let mut at = area.pick(x, y, gtk::PickFlags::DEFAULT);
        while let Some(widget) = at {
            let laser = shown(self.resting.get());
            if widget.cursor().as_ref() != Some(&laser) {
                let wanted = Rc::new(RefCell::new(widget.cursor()));
                widget.set_cursor(Some(&laser));
                let handler = widget.connect_cursor_notify({
                    let (wanted, resting) = (wanted.clone(), self.resting.clone());
                    move |widget| {
                        let (cursor, laser) = (widget.cursor(), shown(resting.get()));
                        if cursor.as_ref() != Some(&laser) {
                            wanted.replace(cursor);
                            widget.set_cursor(Some(&laser));
                        }
                    }
                });
                let widget = widget.downgrade();
                let covered = Covered {
                    widget,
                    handler,
                    wanted,
                };
                self.covered.borrow_mut().push(covered);
            }
            if &widget == area {
                break;
            }
            at = widget.parent();
        }
    }

    /// The pointer moved or pressed: its dot back if it had gone, and gone again once it rests.
    fn stir(self: &Rc<Self>) {
        if self.resting.replace(false) {
            self.show(&cursor());
        }
        let weak = Rc::downgrade(self);
        self.idle.call(move || {
            if let Some(laser) = weak.upgrade() {
                laser.resting.set(true);
                laser.show(&shown(true));
            }
        });
    }

    /// Show `cursor` on each widget the laser covers.
    fn show(&self, cursor: &gdk::Cursor) {
        for covered in self.covered.borrow().iter() {
            if let Some(widget) = covered.widget.upgrade() {
                widget.set_cursor(Some(cursor));
            }
        }
    }

    /// A stroke from `(x, y)`, with every stroke still up back at full strength, as a press
    /// before or during Xournal++'s fade brings them back.
    fn begin(&self, x: f64, y: f64) {
        self.hold.cancel();
        self.fade.reset();
        let point = graphene::Point::new(x as f32, y as f32);
        self.painter.imp().strokes.borrow_mut().push(vec![point]);
        self.painter.queue_draw();
    }

    /// The strokes up, each the points it went through, for a drill.
    #[cfg(feature = "bench")]
    pub fn strokes(&self) -> Vec<usize> {
        let strokes = self.painter.imp().strokes.borrow();
        strokes.iter().map(Vec::len).collect()
    }

    /// Whether a press is drawing a stroke now, for a drill.
    #[cfg(feature = "bench")]
    pub fn pressed(&self) -> bool {
        self.drag.is_active()
    }

    /// How strong the strokes are drawn, for a drill: 1 until they fade.
    #[cfg(feature = "bench")]
    pub fn strength(&self) -> f64 {
        self.painter.opacity()
    }

    /// Hold the fade where it is, for a drill that presses into it at its own pace.
    #[cfg(feature = "bench")]
    pub fn hold_fade(&self) {
        self.fade.pause();
    }
}

/// The pointer while the laser is up: the dot a press leaves without moving, as wide and as red,
/// its hotspot in the middle. Built once per thread, so a widget showing it is told by identity.
pub fn cursor() -> gdk::Cursor {
    const SIZE: i32 = WIDTH as i32;
    thread_local! {
        static DOT: gdk::Cursor = {
            let middle = WIDTH / 2.0;
            let byte = |channel: f32| (channel * 255.0).round() as u8;
            let pixels: Vec<u8> = (0..SIZE * SIZE)
                .flat_map(|at| {
                    let x = (at % SIZE) as f32 + 0.5 - middle;
                    let y = (at / SIZE) as f32 + 0.5 - middle;
                    // How much of the pixel the dot covers: a soft pixel at its edge.
                    let cover = (middle + 0.5 - x.hypot(y)).clamp(0.0, 1.0);
                    let (r, g, b) = (COLOUR.red(), COLOUR.green(), COLOUR.blue());
                    [byte(r), byte(g), byte(b), byte(COLOUR.alpha() * cover)]
                })
                .collect();
            let texture = gdk::MemoryTexture::new(
                SIZE,
                SIZE,
                gdk::MemoryFormat::R8g8b8a8,
                &glib::Bytes::from_owned(pixels),
                SIZE as usize * 4,
            );
            gdk::Cursor::from_texture(&texture, SIZE / 2, SIZE / 2, None)
        };
    }
    DOT.with(Clone::clone)
}

/// What the laser shows for a pointer, its dot or, once the pointer has rested, none at all.
fn shown(resting: bool) -> gdk::Cursor {
    thread_local! {
        static NONE: gdk::Cursor =
            gdk::Cursor::from_name("none", None).expect("GDK makes a cursor of any name");
    }
    match resting {
        true => NONE.with(Clone::clone),
        false => cursor(),
    }
}

/// Where the pointer rests over `area`, in its coordinates.
fn pointer_over(area: &gtk::Widget) -> Option<(f64, f64)> {
    let native = area.native()?;
    let pointer = area.display().default_seat()?.pointer()?;
    let (x, y, _) = native.surface()?.device_position(&pointer)?;
    let (dx, dy) = native.surface_transform();
    let at = graphene::Point::new((x - dx) as f32, (y - dy) as f32);
    let at = native.compute_point(area, &at)?;
    Some((at.x().into(), at.y().into()))
}

/// Whether the keyboard at `focus` types an `l` itself, where `L` cannot be the laser's: an
/// entry (the find bar's, a dialog's), a shell, or a text view still editable, as a presented
/// source file is.
pub fn types(focus: &gtk::Widget) -> bool {
    focus.is::<gtk::Editable>()
        || focus.is::<vte4::Terminal>()
        || focus
            .downcast_ref::<gtk::TextView>()
            .is_some_and(gtk::TextView::is_editable)
}

/// Whether `key` with `state` is the laser's: a bare `L`, Shift or not.
pub fn is_key(key: gdk::Key, state: gdk::ModifierType) -> bool {
    key.to_lower() == gdk::Key::l
        && !state.intersects(
            gdk::ModifierType::CONTROL_MASK
                | gdk::ModifierType::ALT_MASK
                | gdk::ModifierType::SUPER_MASK
                | gdk::ModifierType::META_MASK,
        )
}

impl Painter {
    /// The stroke being drawn on to `(x, y)`.
    fn extend(&self, x: f64, y: f64) {
        if let Some(stroke) = self.imp().strokes.borrow_mut().last_mut() {
            stroke.push(graphene::Point::new(x as f32, y as f32));
        }
        self.queue_draw();
    }
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Painter {
        /// Each stroke, the points it went through in the widget's coordinates.
        pub strokes: RefCell<Vec<Vec<graphene::Point>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Painter {
        const NAME: &'static str = "AccentLaser";
        type Type = super::Painter;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for Painter {}

    impl WidgetImpl for Painter {
        /// Each stroke a path of its own, so one that crosses itself is no stronger there, and
        /// one press that did not move a dot, as Xournal++ draws them.
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let stroke = gsk::Stroke::new(WIDTH);
            stroke.set_line_cap(gsk::LineCap::Round);
            stroke.set_line_join(gsk::LineJoin::Round);
            for points in self.strokes.borrow().iter() {
                let Some((first, rest)) = points.split_first() else {
                    continue;
                };
                let path = gsk::PathBuilder::new();
                if rest.is_empty() {
                    path.add_circle(first, WIDTH / 2.0);
                    snapshot.append_fill(&path.to_path(), gsk::FillRule::Winding, &COLOUR);
                    continue;
                }
                path.move_to(first.x(), first.y());
                for point in rest {
                    path.line_to(point.x(), point.y());
                }
                snapshot.append_stroke(&path.to_path(), &stroke, &COLOUR);
            }
        }
    }
}

glib::wrapper! {
    /// The layer the strokes are drawn on, over the whole document area and never under a press.
    pub struct Painter(ObjectSubclass<imp::Painter>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}
