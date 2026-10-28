//! PDF drawing-ring options for width, colour and eraser mode, stored in config.

use adw::prelude::*;
use gtk::glib;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use accent_core::config::DrawingConfig;
use accent_core::pdf::Theme;

use super::Mode;
use super::tools::{Hand, drag_of, nearest, widths};
use crate::ring::{Ring, dot};

/// Told which tool was in hand and what was picked for it.
type OnChoice = Box<dyn Fn(Mode, Choice)>;

/// The eraser's two ways, on the first two of the slots the other tools give their colours to:
/// whole strokes, or only what it passes over.
const ERASERS: [(bool, &str, &str); 2] = [
    (false, "edit-delete-symbolic", "Erase Whole Strokes"),
    (true, "edit-cut-symbolic", "Erase Only What It Passes Over"),
];

/// What the three width buttons are called, and for the text, which they size.
const WIDTHS: [&str; 3] = ["Narrow Width", "Medium Width", "Wide Width"];
const SIZES: [&str; 3] = ["Small Text", "Medium Text", "Large Text"];

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
    /// Write the choice for `tool` into the config. The shapes share the pen's style, the
    /// eraser's width is its reach and the text's its size.
    pub fn apply(self, tool: Mode, config: &mut DrawingConfig) {
        match (tool.style_owner(), self) {
            (Mode::Text, Choice::Width(w)) => config.text_size = w,
            (Mode::Text, Choice::Colour(c)) => config.text_color = c,
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
    /// The document's recolouring, which the swatches are painted through.
    theme: Rc<Cell<Theme>>,
    /// The tool in hand, as the window has it.
    tool: Cell<Mode>,
    /// Whether the pen's eraser end is at the page, which the ring shows in place of the tool.
    eraser: Cell<bool>,
    on_choice: RefCell<Option<OnChoice>>,
}

impl PdfRing {
    /// Build the ring. It is hidden until the window says otherwise.
    pub fn new() -> Rc<PdfRing> {
        let ring = Ring::<Mode>::new();
        let theme = Rc::new(Cell::new(Theme::Plain));
        // Nine slots outside the tools: the widths at the top, the colours after them.
        let swatch_count = crate::theme::swatches().len();
        let slots = 3 + swatch_count;
        let option = |i: usize, child: &gtk::Widget| ring.add_option(i, slots, child);
        let widths: Vec<_> = [5.0, 8.0, 12.0]
            .into_iter()
            .enumerate()
            .map(|(i, diameter)| option(i, dot(diameter, |area| area.color()).upcast_ref()))
            .collect();
        let swatches: Vec<_> = (0..swatch_count)
            .map(|i| {
                let theme = theme.clone();
                let colour =
                    move |_: &gtk::DrawingArea| crate::theme::rgba(painted(i, theme.get()), 1.0);
                let button = option(3 + i, dot(14.0, colour).upcast_ref());
                let label = match i {
                    0 => "Accent Colour".to_string(),
                    i if i + 1 == swatch_count => "Black".to_string(),
                    _ => format!("Colour {}", i + 1),
                };
                button.set_tooltip_text(Some(&label));
                button
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
            theme,
            tool: Cell::new(Mode::Select),
            eraser: Cell::new(false),
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
                    if let Some(widths) = widths(tool) {
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
        let widths = widths(self.ring.tool());
        let (width, colour) = match tool {
            Mode::Eraser => (config.eraser_radius, None),
            Mode::Highlighter => (config.highlighter_width, Some(config.highlighter_color)),
            Mode::Text => (config.text_size, Some(config.text_color)),
            _ => (config.pen_width, Some(config.pen_color)),
        };
        let names = match tool {
            Mode::Text => SIZES,
            _ => WIDTHS,
        };
        let check = |button: &gtk::ToggleButton, shown: bool, wanted: bool| {
            button.set_visible(shown);
            if button.is_active() != wanted {
                button.set_active(wanted);
            }
        };
        // The nearest of the three, which is the one the tool draws at.
        let nearest = widths.map(|w| nearest(w, width));
        for (i, button) in self.widths.iter().enumerate() {
            check(button, widths.is_some(), nearest == Some(i));
            button.set_tooltip_text(Some(names[i]));
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

    /// Paint the swatches as pages recoloured by `theme` show them.
    pub fn set_theme(&self, theme: Theme) {
        self.theme.set(theme);
        for swatch in &self.swatches {
            if let Some(dot) = swatch.child() {
                dot.queue_draw();
            }
        }
    }

    /// The ring's widget, for the drills.
    #[cfg(feature = "bench")]
    pub fn widget(&self) -> &gtk::Widget {
        self.ring.widget()
    }

    /// Float the ring over the page; see [`Ring::attach`].
    pub fn attach(&self, overlay: &gtk::Overlay) {
        self.ring.attach(overlay);
    }

    /// Show or hide the ring, putting it where the window says when it comes out.
    pub fn set_visible(&self, visible: bool, at: Option<(f64, f64)>) {
        self.ring.set_visible(visible, at);
    }

    /// See [`Ring::connect_moved`].
    pub fn connect_moved(&self, f: impl Fn((f64, f64)) + 'static) {
        self.ring.connect_moved(f);
    }

    /// Show which tool is in hand, and its options.
    pub fn set_tool(&self, mode: Mode) {
        self.tool.set(mode);
        self.show_tool();
    }

    /// Show the eraser while the pen's eraser end is at the page, and the tool in hand again once
    /// it is not. Nothing is picked: the window's tool and the config stay as they were.
    pub fn set_eraser(&self, eraser: bool) {
        self.eraser.set(eraser);
        self.show_tool();
    }

    /// The tool the pen would use, by the end at the page (`tools::drag_of`): the eraser under any
    /// tool in hand, the tool itself under its tip or with none in hand.
    fn show_tool(&self) {
        let tool = self.tool.get();
        let pen = Hand::Pen {
            eraser: self.eraser.get(),
        };
        self.ring.set_tool(drag_of(tool, pen, true).unwrap_or(tool));
        self.sync_options();
    }

    /// The tool shown, which options show, and the swatches' colours as painted, for the drills.
    #[cfg(feature = "bench")]
    pub fn state(&self) -> String {
        let shown =
            |buttons: &[gtk::ToggleButton]| buttons.iter().filter(|b| b.is_visible()).count();
        let painted: Vec<String> = (0..self.swatches.len())
            .map(|i| {
                let [r, g, b] = painted(i, self.theme.get());
                format!("#{r:02x}{g:02x}{b:02x}")
            })
            .collect();
        format!(
            "shown={:?} widths={} swatches={} erasers={} painted={}",
            self.ring.tool(),
            shown(&self.widths),
            shown(&self.swatches),
            shown(&self.erasers),
            painted.join(",")
        )
    }
}

/// Swatch `i` as a page recoloured by `theme` shows its ink, which on a recoloured page is not the
/// colour the file is given: Solarized shows black as its blue-grey text.
fn painted(i: usize, theme: Theme) -> [u8; 3] {
    let rgb = crate::theme::swatches()[i].unwrap_or_else(crate::theme::accent_rgb);
    super::view::shown(rgb, theme)
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
        Choice::Width(20.0).apply(Mode::Text, &mut config);
        Choice::Colour(Some([4, 5, 6])).apply(Mode::Text, &mut config);
        assert_eq!(
            (config.text_size, config.text_color),
            (20.0, Some([4, 5, 6]))
        );
        assert_eq!(config.pen_width, 4.0, "the text sizes nothing else");
    }
}
