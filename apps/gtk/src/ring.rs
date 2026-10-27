//! A movable drawing-tool ring over the page, with options on an outer orbit.
//!
//! [`Tool`] supplies the tools; the owning tab supplies their options. Keeping the ring in
//! the tab's overlay lets readers move it away from the area they are working on.

use adw::prelude::*;
use gtk::{gdk, glib};
use std::cell::Cell;

/// The distance from the hub to a tool button, in pixels.
const ORBIT: f64 = 54.0;
/// The distance from the hub to an option button: just clear of the tools.
const OPTIONS: f64 = 88.0;
/// The whole ring's box, wide enough for the outer orbit plus a button either side of it.
const SIZE: i32 = 204;
/// How far the ring sits from the corner it starts in.
const INSET: f64 = 24.0;

/// A ring tool. Its action supplies the shared label for the button, status bar and palette.
pub trait Tool: Copy + PartialEq + 'static {
    /// Top first, then clockwise: the tool and its icon name.
    const TOOLS: &'static [(Self, &'static str)];
    /// Window action selecting this tool, or `None` for no tool.
    fn action(self) -> Option<&'static str>;
    /// Initial state with no tool selected.
    fn none() -> Self;
}

/// A ring of tool buttons, and the hub that moves it.
pub struct Ring<T: Tool> {
    root: RingBox,
    buttons: Vec<(T, gtk::ToggleButton)>,
    tool: Cell<T>,
    /// Where the ring's corner is in the pane, as it was last drawn or put.
    at: std::cell::Cell<(f64, f64)>,
    /// Whether the reader moved the ring. Until then it sits in its default corner, wherever the
    /// pane's size puts that.
    moved: std::cell::Cell<bool>,
}

impl<T: Tool> Ring<T> {
    /// Build the ring. It is hidden until the window says otherwise.
    pub fn new() -> std::rc::Rc<Ring<T>> {
        let root: RingBox = glib::Object::builder()
            .property("width-request", SIZE)
            .property("height-request", SIZE)
            .property("visible", false)
            .build();

        let centre = f64::from(SIZE) / 2.0;
        let hub = gtk::Button::builder()
            .icon_name("view-more-symbolic")
            .tooltip_text("Move the tools")
            .build();
        hub.set_cursor_from_name(Some("grab"));
        hub.add_css_class("circular");
        hub.add_css_class("opaque");
        hub.add_css_class("accent-floating");
        hub.add_css_class("accent-ring-hub");
        place(&root, &hub, centre, centre, BUTTON);

        let mut buttons = Vec::new();
        for (i, (tool, icon)) in T::TOOLS.iter().enumerate() {
            let Some(action) = tool.action() else {
                continue;
            };
            let (dx, dy) = orbit(i, T::TOOLS.len(), ORBIT);
            let button = gtk::ToggleButton::builder()
                .icon_name(*icon)
                .tooltip_text(crate::actions::label_of(action))
                .action_name(action)
                .build();
            button.add_css_class("circular");
            button.add_css_class("opaque");
            button.add_css_class("accent-floating");
            button.add_css_class("accent-ring-tool");
            place(&root, &button, centre + dx, centre + dy, BUTTON);
            buttons.push((*tool, button));
        }

        let ring = std::rc::Rc::new(Ring {
            root,
            buttons,
            tool: Cell::new(T::none()),
            at: std::cell::Cell::new((INSET, INSET)),
            moved: std::cell::Cell::new(false),
        });
        ring.wire_drag(&hub);
        ring
    }

    /// A button on the outer orbit, slot `i` of `n`, holding `child`. Hidden until shown.
    pub fn add_option(&self, i: usize, n: usize, child: &gtk::Widget) -> gtk::ToggleButton {
        let button = gtk::ToggleButton::new();
        button.set_child(Some(child));
        button.set_visible(false);
        button.add_css_class("circular");
        button.add_css_class("opaque");
        button.add_css_class("accent-floating");
        button.add_css_class("accent-ring-tool");
        let centre = f64::from(SIZE) / 2.0;
        let (dx, dy) = orbit(i, n, OPTIONS);
        place(&self.root, &button, centre + dx, centre + dy, OPTION);
        button
    }

    /// Dragging the hub moves the whole ring, an overlay child the overlay places where `at` says.
    fn wire_drag(self: &std::rc::Rc<Self>, hub: &gtk::Button) {
        let drag = gtk::GestureDrag::new();
        // Capture, ahead of the hub's own click gesture: a `GtkButton` claims the sequence on the
        // press, so a drag controller in the ordinary bubble phase never sees the motion.
        drag.set_propagation_phase(gtk::PropagationPhase::Capture);
        drag.connect_drag_update(glib::clone!(
            #[weak(rename_to = ring)]
            self,
            move |gesture, dx, dy| {
                // Claimed on the first motion, not on the press, so the hub can still be clicked
                // without the ring jumping.
                gesture.set_state(gtk::EventSequenceState::Claimed);
                ring.moved.set(true);
                // Added to where the ring *is*, not to where it was when the hand took hold. The
                // gesture measures its offset inside the hub, and the hub is what this moves, so
                // the moment the ring catches up the reported offset is zero again — treating it
                // as an offset from the press would leave the ring trailing the pointer by half.
                let (x, y) = ring.at.get();
                ring.move_to(x + dx, y + dy);
            }
        ));
        hub.add_controller(drag);
    }

    /// The ring's widget.
    pub fn widget(&self) -> &gtk::Widget {
        self.root.upcast_ref()
    }

    /// Float the ring over `overlay`'s child. The overlay places it at every allocation, so it
    /// stays inside a pane resized under it, as a split halves one, rather than where the larger
    /// pane had it, over the pane beside.
    pub fn attach(self: &std::rc::Rc<Self>, overlay: &gtk::Overlay) {
        overlay.add_overlay(&self.root);
        let ring = std::rc::Rc::downgrade(self);
        overlay.connect_get_child_position(move |overlay, child| {
            let ring = ring.upgrade().filter(|ring| child == ring.widget())?;
            let (x, y) = ring.inside(overlay.width(), overlay.height());
            Some(gdk::Rectangle::new(x as i32, y as i32, SIZE, SIZE))
        });
    }

    /// Where the ring's corner goes in a pane of this size: where the reader put it, or the top
    /// right while they have not, kept inside either way so it cannot be dragged out of reach.
    /// That corner avoids left-to-right reading and a drawing hand entering from the bottom right.
    fn inside(&self, width: i32, height: i32) -> (f64, f64) {
        let (x, y) = match self.moved.get() {
            true => self.at.get(),
            false => ((f64::from(width - SIZE) - INSET).max(INSET), INSET),
        };
        let room = |extent: i32| f64::from((extent - SIZE).max(0));
        let at = (x.clamp(0.0, room(width)), y.clamp(0.0, room(height)));
        // Kept, so a drag goes on from where the ring is drawn.
        self.at.set(at);
        at
    }

    /// Put the ring's corner here.
    pub fn move_to(&self, x: f64, y: f64) {
        self.at.set((x, y));
        if let Some(overlay) = self.root.parent() {
            self.inside(overlay.width(), overlay.height());
            overlay.queue_allocate();
        }
    }

    /// Where the reader dragged the ring, so the next one opens there. `None` while they have
    /// not moved it, which leaves the next one free to find its own corner.
    pub fn at(&self) -> Option<(f64, f64)> {
        self.moved.get().then(|| self.at.get())
    }

    /// Restore the ring's saved position once the reader has moved it; until then it takes its
    /// corner as it is allocated.
    fn place(&self, remembered: Option<(f64, f64)>) {
        if let Some((x, y)) = remembered.filter(|_| self.moved.get()) {
            self.move_to(x, y);
        }
    }

    /// Show or hide the ring, putting it where the window says when it comes out.
    pub fn set_visible(&self, visible: bool, at: Option<(f64, f64)>) {
        if visible {
            self.place(at);
        }
        self.root.set_visible(visible);
    }

    /// Reflect the active tool without firing actions again, avoiding feedback with button handlers.
    /// Dim other tools so outer-orbit options clearly belong to the active one.
    pub fn set_tool(&self, tool: T) {
        let holding = tool != T::none();
        for (held, button) in &self.buttons {
            let wanted = *held == tool;
            if button.is_active() != wanted {
                button.set_active(wanted);
            }
            crate::widgets::set_class(button, "accent-ring-dim", holding && !wanted);
        }
        self.tool.set(tool);
    }

    /// The tool in hand.
    pub fn tool(&self) -> T {
        self.tool.get()
    }
}

/// A filled circle of `diameter` pixels in whatever colour `colour` says when it is drawn.
pub(crate) fn dot(
    diameter: f64,
    colour: impl Fn(&gtk::DrawingArea) -> gdk::RGBA + 'static,
) -> gtk::DrawingArea {
    let area = gtk::DrawingArea::new();
    area.set_size_request(OPTION - 8, OPTION - 8);
    area.set_draw_func(move |area, cr, w, h| {
        let c = colour(area);
        cr.set_source_rgba(
            f64::from(c.red()),
            f64::from(c.green()),
            f64::from(c.blue()),
            f64::from(c.alpha()),
        );
        cr.arc(
            f64::from(w) / 2.0,
            f64::from(h) / 2.0,
            diameter / 2.0,
            0.0,
            std::f64::consts::TAU,
        );
        let _ = cr.fill();
    });
    area
}

mod imp {
    use gtk::glib;
    use gtk::subclass::prelude::*;

    #[derive(Default)]
    pub struct RingBox;

    #[glib::object_subclass]
    impl ObjectSubclass for RingBox {
        const NAME: &'static str = "AccentRingBox";
        type Type = super::RingBox;
        type ParentType = gtk::Fixed;
    }

    impl ObjectImpl for RingBox {}

    impl WidgetImpl for RingBox {
        /// Only the buttons are the ring; the box around them is the page. GTK picks the
        /// children before it asks the parent, so this only decides the space between them.
        fn contains(&self, _x: f64, _y: f64) -> bool {
            false
        }
    }

    impl FixedImpl for RingBox {}
}

glib::wrapper! {
    /// A `gtk::Fixed` that a press between its buttons falls through.
    pub struct RingBox(ObjectSubclass<imp::RingBox>)
        @extends gtk::Fixed, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

/// Where slot `i` of `n` sits on an orbit of this radius, relative to the hub: the first at the
/// top, the rest clockwise.
pub(crate) fn orbit(i: usize, n: usize, radius: f64) -> (f64, f64) {
    let angle = std::f64::consts::PI * (-0.5 + i as f64 * 2.0 / n as f64);
    (radius * angle.cos(), radius * angle.sin())
}

/// Put a round button on the ring by its centre rather than its corner, which is how the
/// positions above are worked out.
fn place(root: &RingBox, button: &impl IsA<gtk::Widget>, cx: f64, cy: f64, size: i32) {
    let button = button.as_ref();
    // The buttons are square and sized by their CSS, so the size request is what centres them.
    button.set_size_request(size, size);
    let half = f64::from(size) / 2.0;
    root.put(button, cx - half, cy - half);
}

/// A tool button's diameter. Big enough to hit while drawing, and above libadwaita's 34 px
/// minimum, small enough to leave the page visible around the ring.
const BUTTON: i32 = 36;
/// An option button's: a dot, not a glyph, so it can be smaller.
const OPTION: i32 = 24;

#[cfg(test)]
mod tests {
    use super::{BUTTON, OPTION, OPTIONS, ORBIT, orbit};

    /// The most tools the inner orbit holds a button apart; the PDF has seven.
    const TOOLS: usize = 9;

    #[test]
    fn tools_start_at_the_top_and_sit_a_button_apart() {
        let (dx, dy) = orbit(0, TOOLS, ORBIT);
        assert!(dx.abs() < 1e-9 && (dy + ORBIT).abs() < 1e-9);
        for i in 0..TOOLS {
            let a = orbit(i, TOOLS, ORBIT);
            let b = orbit((i + 1) % TOOLS, TOOLS, ORBIT);
            let gap = (a.0 - b.0).hypot(a.1 - b.1);
            assert!(
                gap >= f64::from(BUTTON),
                "slots {i} and next are {gap} px apart"
            );
        }
        for i in 0..9 {
            let a = orbit(i, 9, OPTIONS);
            let b = orbit((i + 1) % 9, 9, OPTIONS);
            let gap = (a.0 - b.0).hypot(a.1 - b.1);
            assert!(
                gap >= f64::from(OPTION),
                "options {i} and next are {gap} px apart"
            );
        }
    }
}
