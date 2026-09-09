//! The drawing tools, as a ring that floats over the page.
//!
//! Round buttons orbiting a hub, dragged around the pane by that hub, with the tool in hand's
//! widths and colours on a second orbit outside them. It is an overlay child of the PDF tab
//! rather than a bar in the chrome, because the reader puts it wherever the part of the page they
//! are working on is not.

use adw::prelude::*;
use gtk::{gdk, glib};
use std::cell::{Cell, RefCell};

use accent_core::config::DrawingConfig;

use crate::pdfview::Mode;

/// The distance from the hub to a tool button, in pixels.
const ORBIT: f64 = 60.0;
/// The distance from the hub to an option button.
const OPTIONS: f64 = 96.0;
/// The whole ring's box, wide enough for the outer orbit plus a button either side of it.
const SIZE: i32 = 220;
/// How far the ring sits from the corner it starts in.
const INSET: f64 = 24.0;

/// Three widths per tool, in page points — fine, the default, bold — and the eraser's reach.
const WIDTHS: [(Mode, [f32; 3]); 3] = [
    (Mode::Pen, [1.0, 2.0, 4.0]),
    (Mode::Highlighter, [8.0, 14.0, 24.0]),
    (Mode::Eraser, [4.0, 8.0, 16.0]),
];

/// Told which tool was in hand and what was picked for it.
type OnChoice = Box<dyn Fn(Mode, Choice)>;

/// What an option button picks for the tool in hand.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Choice {
    Width(f32),
    /// `None` is the accent.
    Colour(Option<[u8; 3]>),
}

impl Choice {
    /// Write the choice for `tool` into the config. The shapes share the pen's style, and the
    /// eraser's width is its reach.
    pub fn apply(self, tool: Mode, config: &mut DrawingConfig) {
        match (tool.style_owner(), self) {
            (Mode::Eraser, Choice::Width(w)) => config.eraser_radius = w,
            (Mode::Eraser, Choice::Colour(_)) => {}
            (Mode::Highlighter, Choice::Width(w)) => config.highlighter_width = w,
            (Mode::Highlighter, Choice::Colour(c)) => config.highlighter_color = c,
            (_, Choice::Width(w)) => config.pen_width = w,
            (_, Choice::Colour(c)) => config.pen_color = c,
        }
    }
}

/// The tools, in the order they sit on the ring: pen at the top, then clockwise. Each one's
/// action and label are [`Mode::action`]'s, so a button says what the palette says.
const TOOLS: [(Mode, &str); 7] = [
    (Mode::Pen, "tool-pen-symbolic"),
    (Mode::Highlighter, "tool-highlighter-symbolic"),
    (Mode::Eraser, "tool-eraser-symbolic"),
    (Mode::Line, "tool-line-symbolic"),
    (Mode::Rect, "tool-rect-symbolic"),
    (Mode::Circle, "tool-circle-symbolic"),
    (Mode::Adjust, "tool-adjust-symbolic"),
];

/// A ring of tool buttons, and the hub that moves it.
pub struct Ring {
    root: RingBox,
    buttons: Vec<(Mode, gtk::ToggleButton)>,
    /// The outer orbit: three widths, then six colours, shown for the tool in hand.
    widths: Vec<gtk::ToggleButton>,
    swatches: Vec<gtk::ToggleButton>,
    tool: Cell<Mode>,
    config: RefCell<DrawingConfig>,
    on_choice: RefCell<Option<OnChoice>>,
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
        let root: RingBox = glib::Object::builder()
            .property("width-request", SIZE)
            .property("height-request", SIZE)
            .property("halign", gtk::Align::Start)
            .property("valign", gtk::Align::Start)
            .property("visible", false)
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
        place(&root, &hub, centre, centre, BUTTON);

        let mut buttons = Vec::new();
        for (i, (mode, icon)) in TOOLS.iter().enumerate() {
            let Some(action) = mode.action() else {
                continue;
            };
            let (dx, dy) = orbit(i, TOOLS.len(), ORBIT);
            let button = gtk::ToggleButton::builder()
                .icon_name(*icon)
                .tooltip_text(crate::actions::label_of(action))
                .action_name(action)
                .build();
            button.add_css_class("circular");
            button.add_css_class("osd");
            button.add_css_class("accent-ring-tool");
            place(&root, &button, centre + dx, centre + dy, BUTTON);
            buttons.push((*mode, button));
        }

        // Nine slots outside the tools: the widths at the top, the colours after them.
        let slots = 3 + crate::theme::swatches().len();
        let option = |i: usize, dot: gtk::DrawingArea| {
            let button = gtk::ToggleButton::new();
            button.set_child(Some(&dot));
            button.add_css_class("circular");
            button.add_css_class("osd");
            button.add_css_class("accent-ring-tool");
            let (dx, dy) = orbit(i, slots, OPTIONS);
            place(&root, &button, centre + dx, centre + dy, OPTION);
            button
        };
        let widths: Vec<_> = [5.0, 8.0, 12.0]
            .into_iter()
            .enumerate()
            .map(|(i, diameter)| option(i, dot(diameter, |area| area.color())))
            .collect();
        let swatches: Vec<_> = (0..crate::theme::swatches().len())
            .map(|i| {
                option(
                    3 + i,
                    dot(14.0, move |_| match crate::theme::swatches()[i] {
                        Some(rgb) => crate::theme::rgba(rgb, 1.0),
                        None => crate::theme::accent(),
                    }),
                )
            })
            .collect();

        let ring = std::rc::Rc::new(Ring {
            root,
            buttons,
            widths,
            swatches,
            tool: Cell::new(Mode::Select),
            config: RefCell::new(DrawingConfig::default()),
            on_choice: RefCell::new(None),
            at: std::cell::Cell::new((INSET, INSET)),
            moved: std::cell::Cell::new(false),
        });
        ring.wire_drag(&hub);
        ring.wire_options();
        ring
    }

    /// An option button picks for whichever tool is in hand when it is pressed. The buttons are
    /// toggles only to wear the checked look; which one is checked is the config's say, through
    /// [`Ring::set_config`], not the click's.
    fn wire_options(self: &std::rc::Rc<Self>) {
        for (i, button) in self.widths.iter().enumerate() {
            button.connect_clicked(glib::clone!(
                #[weak(rename_to = ring)]
                self,
                move |_| {
                    let tool = ring.tool.get();
                    let owner = tool.style_owner();
                    if let Some((_, widths)) = WIDTHS.iter().find(|(m, _)| *m == owner) {
                        ring.choose(tool, Choice::Width(widths[i]));
                    }
                }
            ));
        }
        for (i, button) in self.swatches.iter().enumerate() {
            button.connect_clicked(glib::clone!(
                #[weak(rename_to = ring)]
                self,
                move |_| {
                    let tool = ring.tool.get();
                    ring.choose(tool, Choice::Colour(crate::theme::swatches()[i]));
                }
            ));
        }
    }

    fn choose(&self, tool: Mode, choice: Choice) {
        if let Some(f) = self.on_choice.borrow().as_ref() {
            f(tool, choice);
        }
        // Whatever the config comes back as, the button the hand is on stops looking toggled
        // by the click alone.
        self.sync_options();
    }

    /// Called with the tool in hand and what was picked for it.
    pub fn connect_choice(&self, f: impl Fn(Mode, Choice) + 'static) {
        *self.on_choice.borrow_mut() = Some(Box::new(f));
    }

    /// What the preferences say about the tools, which is what the options show as checked.
    pub fn set_config(&self, config: &DrawingConfig) {
        *self.config.borrow_mut() = config.clone();
        self.sync_options();
    }

    /// Show the tool in hand's options, checked as the config has them, and nothing for a tool
    /// with none.
    fn sync_options(&self) {
        let tool = self.tool.get().style_owner();
        let config = self.config.borrow();
        let widths = match self.tool.get() {
            Mode::Select | Mode::Adjust => None,
            _ => WIDTHS.iter().find(|(m, _)| *m == tool).map(|(_, w)| *w),
        };
        let (width, colour) = match tool {
            Mode::Eraser => (config.eraser_radius, None),
            Mode::Highlighter => (config.highlighter_width, Some(config.highlighter_color)),
            _ => (config.pen_width, Some(config.pen_color)),
        };
        let check = |button: &gtk::ToggleButton, shown: bool, wanted: bool| {
            button.set_visible(shown);
            if button.is_active() != wanted {
                button.set_active(wanted);
            }
        };
        // The nearest of the three rather than an exact match: a hand-edited config, or one
        // written before these three widths were, would otherwise show no width selected at all.
        let nearest = widths.map(|w| {
            let distance = |i: &usize| (w[*i] - width).abs();
            (0..w.len())
                .min_by(|a, b| distance(a).total_cmp(&distance(b)))
                .unwrap_or(0)
        });
        for (i, button) in self.widths.iter().enumerate() {
            check(button, widths.is_some(), nearest == Some(i));
        }
        let swatches = crate::theme::swatches();
        let shown = widths.is_some() && colour.is_some();
        for (i, button) in self.swatches.iter().enumerate() {
            check(button, shown, shown && colour == Some(swatches[i]));
        }
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
        self.tool.set(mode);
        self.sync_options();
    }
}

/// A filled circle of `diameter` pixels in whatever colour `colour` says when it is drawn.
fn dot(
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

/// A tool button's diameter. Big enough to hit while drawing, small enough to leave the page
/// visible around the ring.
const BUTTON: i32 = 38;
/// An option button's: a dot, not a glyph, so it can be smaller.
const OPTION: i32 = 24;

#[cfg(test)]
mod tests {
    use super::{BUTTON, Choice, OPTION, OPTIONS, ORBIT, TOOLS, orbit};
    use crate::pdfview::Mode;

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

    #[test]
    fn a_choice_lands_in_the_right_tool() {
        let mut config = accent_core::config::DrawingConfig::default();
        Choice::Width(4.0).apply(Mode::Line, &mut config);
        assert_eq!(config.pen_width, 4.0, "a shape draws in the pen's width");
        Choice::Width(8.0).apply(Mode::Eraser, &mut config);
        assert_eq!(config.eraser_radius, 8.0);
        Choice::Colour(Some([0, 0, 0])).apply(Mode::Highlighter, &mut config);
        assert_eq!(config.highlighter_color, Some([0, 0, 0]));
        assert_eq!(config.pen_color, None);
        Choice::Colour(Some([1, 2, 3])).apply(Mode::Eraser, &mut config);
        assert_eq!(config.pen_color, None, "the eraser has no colour");
    }
}
