//! The PDF's side of the drawing ring: the widths, colours and eraser ways on its outer orbit,
//! shown for the tool in hand, and what picking one writes into the config.

use adw::prelude::*;
use gtk::glib;
use std::cell::RefCell;
use std::rc::Rc;

use accent_core::config::DrawingConfig;

use super::Mode;
use crate::ring::{Ring, dot};

/// Three widths per tool, in page points — fine, the default, bold — and the eraser's reach.
const WIDTHS: [(Mode, [f32; 3]); 3] = [
    (Mode::Pen, [1.0, 2.0, 4.0]),
    (Mode::Highlighter, [8.0, 14.0, 24.0]),
    (Mode::Eraser, [4.0, 8.0, 16.0]),
];

/// Told which tool was in hand and what was picked for it.
type OnChoice = Box<dyn Fn(Mode, Choice)>;

/// The eraser's two ways, on the first two of the slots the other tools give their colours to:
/// whole strokes, or only what it passes over.
const ERASERS: [(bool, &str, &str); 2] = [
    (false, "edit-delete-symbolic", "Erase Whole Strokes"),
    (true, "edit-cut-symbolic", "Erase Only What It Passes Over"),
];

/// What an option button picks for the tool in hand.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Choice {
    Width(f32),
    /// `None` is the accent.
    Colour(Option<[u8; 3]>),
    /// The eraser's: partial, or whole strokes.
    Partial(bool),
}

impl Choice {
    /// Write the choice for `tool` into the config. The shapes share the pen's style, and the
    /// eraser's width is its reach.
    pub fn apply(self, tool: Mode, config: &mut DrawingConfig) {
        match (tool.style_owner(), self) {
            (Mode::Eraser, Choice::Width(w)) => config.eraser_radius = w,
            (Mode::Eraser, Choice::Partial(p)) => config.eraser_partial = p,
            (Mode::Eraser, Choice::Colour(_)) | (_, Choice::Partial(_)) => {}
            (Mode::Highlighter, Choice::Width(w)) => config.highlighter_width = w,
            (Mode::Highlighter, Choice::Colour(c)) => config.highlighter_color = c,
            (_, Choice::Width(w)) => config.pen_width = w,
            (_, Choice::Colour(c)) => config.pen_color = c,
        }
    }
}

/// The ring of drawing tools over a PDF, with the tool in hand's options around it.
pub struct PdfRing {
    ring: Rc<Ring<Mode>>,
    /// The outer orbit: three widths, then six colours, shown for the tool in hand — or for the
    /// eraser, its two ways where the colours would be.
    widths: Vec<gtk::ToggleButton>,
    swatches: Vec<gtk::ToggleButton>,
    erasers: Vec<gtk::ToggleButton>,
    config: RefCell<DrawingConfig>,
    on_choice: RefCell<Option<OnChoice>>,
}

impl PdfRing {
    /// Build the ring. It is hidden until the window says otherwise.
    pub fn new() -> Rc<PdfRing> {
        let ring = Ring::<Mode>::new();
        // Nine slots outside the tools: the widths at the top, the colours after them.
        let slots = 3 + crate::theme::swatches().len();
        let option = |i: usize, child: &gtk::Widget| ring.add_option(i, slots, child);
        let widths: Vec<_> = [5.0, 8.0, 12.0]
            .into_iter()
            .enumerate()
            .map(|(i, diameter)| option(i, dot(diameter, |area| area.color()).upcast_ref()))
            .collect();
        let swatches: Vec<_> = (0..crate::theme::swatches().len())
            .map(|i| {
                let colour = move |_: &gtk::DrawingArea| match crate::theme::swatches()[i] {
                    Some(rgb) => crate::theme::rgba(rgb, 1.0),
                    None => crate::theme::accent(),
                };
                option(3 + i, dot(14.0, colour).upcast_ref())
            })
            .collect();
        let erasers: Vec<_> = ERASERS
            .iter()
            .enumerate()
            .map(|(i, (_, icon, label))| {
                let button = option(3 + i, gtk::Image::from_icon_name(icon).upcast_ref());
                button.set_tooltip_text(Some(label));
                button
            })
            .collect();

        let ring = Rc::new(PdfRing {
            ring,
            widths,
            swatches,
            erasers,
            config: RefCell::new(DrawingConfig::default()),
            on_choice: RefCell::new(None),
        });
        ring.wire_options();
        ring
    }

    /// An option button picks for whichever tool is in hand when it is pressed. The buttons are
    /// toggles only to wear the checked look; which one is checked is the config's say, through
    /// [`PdfRing::set_config`], not the click's.
    fn wire_options(self: &Rc<Self>) {
        for (i, button) in self.widths.iter().enumerate() {
            button.connect_clicked(glib::clone!(
                #[weak(rename_to = ring)]
                self,
                move |_| {
                    let tool = ring.ring.tool();
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
                    let tool = ring.ring.tool();
                    ring.choose(tool, Choice::Colour(crate::theme::swatches()[i]));
                }
            ));
        }
        for (button, (partial, _, _)) in self.erasers.iter().zip(ERASERS) {
            button.connect_clicked(glib::clone!(
                #[weak(rename_to = ring)]
                self,
                move |_| ring.choose(ring.ring.tool(), Choice::Partial(partial))
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
        let tool = self.ring.tool().style_owner();
        let config = self.config.borrow();
        let widths = match self.ring.tool() {
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
        let erasing = self.ring.tool() == Mode::Eraser;
        for (button, (partial, _, _)) in self.erasers.iter().zip(ERASERS) {
            check(button, erasing, erasing && config.eraser_partial == partial);
        }
    }

    /// The widget to hand to the overlay.
    pub fn widget(&self) -> &gtk::Widget {
        self.ring.widget()
    }

    /// Show or hide the ring, putting it where the window says when it comes out.
    pub fn set_visible(&self, visible: bool, at: Option<(f64, f64)>) {
        self.ring.set_visible(visible, at);
    }

    /// Where the reader dragged the ring; see [`Ring::at`].
    pub fn at(&self) -> Option<(f64, f64)> {
        self.ring.at()
    }

    /// Show which tool is in hand, and its options.
    pub fn set_tool(&self, mode: Mode) {
        self.ring.set_tool(mode);
        self.sync_options();
    }
}

#[cfg(test)]
mod tests {
    use super::{Choice, Mode};

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
        Choice::Partial(true).apply(Mode::Pen, &mut config);
        assert!(!config.eraser_partial, "only the eraser has two ways");
        Choice::Partial(true).apply(Mode::Eraser, &mut config);
        assert!(config.eraser_partial);
    }
}
