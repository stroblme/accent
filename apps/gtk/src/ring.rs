//! The drawing tools, as a ring that floats over the page.
//!
//! Three round buttons orbiting a hub, dragged around the pane by that hub. It is an overlay
//! child of the PDF tab rather than a bar in the chrome, because the reader puts it wherever the
//! part of the page they are working on is not.

use adw::prelude::*;
use gtk::glib;

use crate::pdfview::Mode;

/// The distance from the hub to a tool button, in pixels.
const ORBIT: f64 = 52.0;
/// The whole ring's box, wide enough for the orbit plus a button either side of it.
const SIZE: i32 = 144;
/// Where the ring sits when a window has not moved it yet: clear of the page's left edge and
/// below the tab bar, which is where a right-handed reader's hand is not.
pub const HOME: (f64, f64) = (24.0, 96.0);

/// The three tools, in the order they sit on the ring: pen at the top, then clockwise.
const TOOLS: [(Mode, &str, &str); 3] = [
    (Mode::Pen, "win.pdf-pen", "tool-pen-symbolic"),
    (Mode::Eraser, "win.pdf-eraser", "tool-eraser-symbolic"),
    (
        Mode::Highlighter,
        "win.pdf-highlighter",
        "tool-highlighter-symbolic",
    ),
];

/// A ring of tool buttons, and the hub that moves it.
pub struct Ring {
    root: gtk::Fixed,
    buttons: Vec<(Mode, gtk::ToggleButton)>,
    /// Where the ring's top-left corner sits in the pane, which is what the overlay's margins are
    /// set from.
    at: std::cell::Cell<(f64, f64)>,
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
            // Pen at the top, the other two at the foot of the ring, so the three sit on the
            // points of a triangle rather than crowding one side.
            let angle = std::f64::consts::PI * (-0.5 + i as f64 * 2.0 / 3.0);
            let button = gtk::ToggleButton::builder()
                .icon_name(*icon)
                .tooltip_text(crate::label_of(action))
                .action_name(*action)
                .build();
            button.add_css_class("circular");
            button.add_css_class("osd");
            button.add_css_class("accent-ring-tool");
            place(
                &root,
                &button,
                centre + ORBIT * angle.cos(),
                centre + ORBIT * angle.sin(),
            );
            buttons.push((*mode, button));
        }

        let ring = std::rc::Rc::new(Ring {
            root,
            buttons,
            at: std::cell::Cell::new(HOME),
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

    /// Where the ring is, so the window can put the next one in the same place.
    pub fn at(&self) -> (f64, f64) {
        self.at.get()
    }

    pub fn set_visible(&self, visible: bool) {
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
