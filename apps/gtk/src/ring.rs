//! The drawing tools, as a ring that floats over the page.
//!
//! Round buttons orbiting a hub, dragged around the pane by that hub. It is an overlay
//! child of the PDF tab rather than a bar in the chrome, because the reader puts it wherever the
//! part of the page they are working on is not.

use adw::prelude::*;
use gtk::glib;

use crate::pdfview::Mode;

/// The distance from the hub to a tool button, in pixels.
const ORBIT: f64 = 60.0;
/// The whole ring's box, wide enough for the orbit plus a button either side of it.
const SIZE: i32 = 160;
/// How far the ring sits from the corner it starts in.
const INSET: f64 = 24.0;

/// The tools, in the order they sit on the ring: pen at the top, then clockwise.
const TOOLS: [(Mode, &str, &str); 7] = [
    (Mode::Pen, "win.pdf-pen", "tool-pen-symbolic"),
    (
        Mode::Highlighter,
        "win.pdf-highlighter",
        "tool-highlighter-symbolic",
    ),
    (Mode::Eraser, "win.pdf-eraser", "tool-eraser-symbolic"),
    (Mode::Line, "win.pdf-line", "tool-line-symbolic"),
    (Mode::Rect, "win.pdf-rect", "tool-rect-symbolic"),
    (Mode::Circle, "win.pdf-circle", "tool-circle-symbolic"),
    (Mode::Adjust, "win.pdf-adjust", "tool-adjust-symbolic"),
];

/// A ring of tool buttons, and the hub that moves it.
pub struct Ring {
    root: gtk::Fixed,
    buttons: Vec<(Mode, gtk::ToggleButton)>,
    /// Where the ring's top-left corner sits in the pane, which is what the overlay's margins are
    /// set from.
    at: std::cell::Cell<(f64, f64)>,
    /// Whether the reader has dragged it. Until they have, the ring goes back to its corner
    /// every time it is shown, which is how it finds that corner at all: the pane has no width to
    /// measure against until it has been allocated.
    moved: std::cell::Cell<bool>,
}

impl Ring {
    /// Build the ring. It is hidden until the window says otherwise.
    pub fn new() -> std::rc::Rc<Ring> {
        let root = gtk::Fixed::builder()
            .width_request(SIZE)
            .height_request(SIZE)
            .halign(gtk::Align::Start)
            .valign(gtk::Align::Start)
            .visible(false)
            .build();

        let centre = f64::from(SIZE) / 2.0;
        let hub = gtk::Button::builder()
            .icon_name("view-more-symbolic")
            .tooltip_text("Move the tools")
            .build();
        hub.set_cursor_from_name(Some("grab"));
        hub.add_css_class("circular");
        hub.add_css_class("osd");
        hub.add_css_class("accent-ring-hub");
        place(&root, &hub, centre, centre);

        let mut buttons = Vec::new();
        for (i, (mode, action, icon)) in TOOLS.iter().enumerate() {
            let (dx, dy) = orbit(i, TOOLS.len(), ORBIT);
            let button = gtk::ToggleButton::builder()
                .icon_name(*icon)
                .tooltip_text(crate::label_of(action))
                .action_name(*action)
                .build();
            button.add_css_class("circular");
            button.add_css_class("osd");
            button.add_css_class("accent-ring-tool");
            place(&root, &button, centre + dx, centre + dy);
            buttons.push((*mode, button));
        }

        let ring = std::rc::Rc::new(Ring {
            root,
            buttons,
            at: std::cell::Cell::new((INSET, INSET)),
            moved: std::cell::Cell::new(false),
        });
        ring.wire_drag(&hub);
        ring
    }

    /// Dragging the hub moves the whole ring, which is an overlay child positioned by its margins.
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

    /// The widget to hand to the overlay.
    pub fn widget(&self) -> &gtk::Widget {
        self.root.upcast_ref()
    }

    /// Put the ring's corner here, kept inside the pane so it cannot be dragged out of reach.
    pub fn move_to(&self, x: f64, y: f64) {
        let room = |extent: i32| f64::from((extent - SIZE).max(0));
        let parent = self.root.parent();
        let (w, h) = match &parent {
            Some(p) => (room(p.width()), room(p.height())),
            None => (f64::MAX, f64::MAX),
        };
        let at = (x.clamp(0.0, w), y.clamp(0.0, h));
        self.at.set(at);
        self.root.set_margin_start(at.0 as i32);
        self.root.set_margin_top(at.1 as i32);
    }

    /// Where the reader dragged the ring, so the next one opens there. `None` while they have
    /// not moved it, which leaves the next one free to find its own corner.
    pub fn at(&self) -> Option<(f64, f64)> {
        self.moved.get().then(|| self.at.get())
    }

    /// Put the ring where the window last had it, or in its own corner if nobody has moved it.
    ///
    /// The top right: the page is read from the left, and a right-handed reader's hand comes in
    /// from the bottom right, so the top right is the corner in the way of neither.
    fn place(&self, remembered: Option<(f64, f64)>) {
        let (x, y) = match remembered.filter(|_| self.moved.get()) {
            Some(at) => at,
            None => {
                let width = self.root.parent().map_or(0.0, |p| f64::from(p.width()));
                ((width - f64::from(SIZE) - INSET).max(INSET), INSET)
            }
        };
        self.move_to(x, y);
    }

    /// Show or hide the ring, putting it where the window says when it comes out.
    pub fn set_visible(&self, visible: bool, at: Option<(f64, f64)>) {
        if visible {
            self.place(at);
        }
        self.root.set_visible(visible);
    }

    /// Show which tool is in hand. The buttons fire actions, so this only reflects the state; it
    /// must not toggle them back or pressing one would fight its own handler.
    pub fn set_tool(&self, mode: Mode) {
        for (tool, button) in &self.buttons {
            let wanted = *tool == mode;
            if button.is_active() != wanted {
                button.set_active(wanted);
            }
        }
    }
}

/// Where slot `i` of `n` sits on an orbit of this radius, relative to the hub: the first at the
/// top, the rest clockwise.
pub(crate) fn orbit(i: usize, n: usize, radius: f64) -> (f64, f64) {
    let angle = std::f64::consts::PI * (-0.5 + i as f64 * 2.0 / n as f64);
    (radius * angle.cos(), radius * angle.sin())
}

/// Put a round button on the ring by its centre rather than its corner, which is how the
/// positions above are worked out.
fn place(root: &gtk::Fixed, button: &impl IsA<gtk::Widget>, cx: f64, cy: f64) {
    let button = button.as_ref();
    // The buttons are square and sized by their CSS, so the size request is what centres them.
    let size = f64::from(BUTTON);
    button.set_size_request(BUTTON, BUTTON);
    root.put(button, cx - size / 2.0, cy - size / 2.0);
}

/// A tool button's diameter. Big enough to hit while drawing, small enough to leave the page
/// visible around the ring.
const BUTTON: i32 = 38;

#[cfg(test)]
mod tests {
    use super::{BUTTON, ORBIT, TOOLS, orbit};

    #[test]
    fn tools_start_at_the_top_and_sit_a_button_apart() {
        let (dx, dy) = orbit(0, TOOLS.len(), ORBIT);
        assert!(dx.abs() < 1e-9 && (dy + ORBIT).abs() < 1e-9);
        for i in 0..TOOLS.len() {
            let a = orbit(i, TOOLS.len(), ORBIT);
            let b = orbit((i + 1) % TOOLS.len(), TOOLS.len(), ORBIT);
            let gap = (a.0 - b.0).hypot(a.1 - b.1);
            assert!(
                gap >= f64::from(BUTTON),
                "slots {i} and next are {gap} px apart"
            );
        }
    }
}
