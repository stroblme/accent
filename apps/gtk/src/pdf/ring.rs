//! PDF drawing-ring options for width, colour and eraser mode, stored in config, and the Select
//! tool's commands, whose Colour recolours what it holds.

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

/// Told which swatch was picked for what the Select tool holds, `None` being the accent.
type OnRecolour = Box<dyn Fn(Option<[u8; 3]>)>;

/// The eraser's two ways, on the first two of the slots the other tools give their colours to:
/// whole strokes, or only what it passes over.
const ERASERS: [(bool, &str, &str); 2] = [
    (false, "edit-delete-symbolic", "Erase Whole Strokes"),
    (true, "edit-cut-symbolic", "Erase Only What It Passes Over"),
];

/// The Select tool's commands and their icons, on the slots the other tools give their widths and
/// colours to, as the page's menu lists them too: Paste while there is something to paste, the
/// rest while it holds something. Adwaita has no duplicate, so that one is tabler's `copy-plus`.
pub(super) const COMMANDS: [(&str, &str); 5] = [
    ("win.pdf-cut", "edit-cut-symbolic"),
    ("win.pdf-copy", "edit-copy-symbolic"),
    ("win.pdf-paste", "edit-paste-symbolic"),
    ("win.pdf-duplicate", "tool-duplicate-symbolic"),
    ("win.pdf-delete", "edit-delete-symbolic"),
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
    /// The Select tool's [`COMMANDS`], and whether it holds anything and there is anything to
    /// paste, which say which of them show.
    commands: Vec<gtk::Button>,
    picked: Cell<bool>,
    pasteable: Cell<bool>,
    /// Select's Colour command, on the slot before its first, and the six swatches it puts on the
    /// commands' slots in their place, for what Select holds.
    colour: gtk::ToggleButton,
    recolours: Vec<gtk::ToggleButton>,
    /// The colours of what Select holds that a recolour reaches, one each: whether Colour shows,
    /// and which swatch is checked.
    colours: RefCell<Vec<[u8; 3]>>,
    /// Whether the swatches are out in place of the commands.
    choosing: Cell<bool>,
    config: RefCell<DrawingConfig>,
    /// The document's recolouring, which the swatches are painted through.
    theme: Rc<Cell<Theme>>,
    /// The tool in hand, as the window has it.
    tool: Cell<Mode>,
    /// Whether the pen's eraser end is at the page, which the ring shows in place of the tool.
    eraser: Cell<bool>,
    on_choice: RefCell<Option<OnChoice>>,
    on_recolour: RefCell<Option<OnRecolour>>,
}

impl PdfRing {
    /// Build the ring. It is hidden until the window says otherwise.
    pub fn new() -> Rc<PdfRing> {
        let ring = Ring::<Mode>::new();
        let theme = Rc::new(Cell::new(Theme::Plain));
        // Nine slots outside the tools: the widths at the top, the colours after them. Select's
        // commands start at the top too, its Colour on the last slot, beside the first, so the
        // swatches it puts out take the commands' slots, clockwise from it.
        let swatch_count = crate::theme::swatches().len();
        let slots = 3 + swatch_count;
        let option = |i: usize, child: &gtk::Widget| ring.add_option(i, slots, child);
        let widths: Vec<_> = [5.0, 8.0, 12.0]
            .into_iter()
            .enumerate()
            .map(|(i, diameter)| option(i, dot(diameter, |area| area.color()).upcast_ref()))
            .collect();
        // Swatch `i` on the slot `at`, painted as pages recoloured by the theme show it.
        let swatch = |at: usize, i: usize| {
            let theme = theme.clone();
            let colour =
                move |_: &gtk::DrawingArea| crate::theme::rgba(painted(i, theme.get()), 1.0);
            let button = option(at, dot(14.0, colour).upcast_ref());
            let label = match i {
                0 => "Accent Colour".to_string(),
                i if i + 1 == swatch_count => "Black".to_string(),
                _ => format!("Colour {}", i + 1),
            };
            button.set_tooltip_text(Some(&label));
            button
        };
        let swatches: Vec<_> = (0..swatch_count).map(|i| swatch(3 + i, i)).collect();
        let erasers: Vec<_> = ERASERS
            .iter()
            .enumerate()
            .map(|(i, (_, icon, label))| {
                let button = option(3 + i, gtk::Image::from_icon_name(icon).upcast_ref());
                button.set_tooltip_text(Some(label));
                button
            })
            .collect();
        let commands = (COMMANDS.iter().enumerate())
            .map(|(i, (action, icon))| ring.add_button(i, slots, icon, action))
            .collect();
        let palette = gtk::Image::from_icon_name("preferences-color-symbolic");
        let colour = option(slots - 1, palette.upcast_ref());
        colour.set_tooltip_text(Some("Colour"));
        let recolours = (0..swatch_count).map(|i| swatch(i, i)).collect();

        let ring = Rc::new(PdfRing {
            ring,
            widths,
            swatches,
            erasers,
            commands,
            picked: Cell::new(false),
            pasteable: Cell::new(false),
            colour,
            recolours,
            colours: RefCell::default(),
            choosing: Cell::new(false),
            config: RefCell::new(DrawingConfig::default()),
            theme,
            tool: Cell::new(Mode::Read),
            eraser: Cell::new(false),
            on_choice: RefCell::new(None),
            on_recolour: RefCell::new(None),
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
        // Colour puts the swatches out in place of the commands, or away again. A swatch picked
        // recolours what Select holds, never through a `Choice`: the pen's colour stays its own.
        self.colour.connect_clicked(glib::clone!(
            #[weak(rename_to = ring)]
            self,
            move |_| {
                ring.choosing.set(!ring.choosing.get());
                ring.sync_options();
            }
        ));
        for (i, button) in self.recolours.iter().enumerate() {
            button.connect_clicked(glib::clone!(
                #[weak(rename_to = ring)]
                self,
                move |_| {
                    ring.choosing.set(false);
                    if let Some(f) = ring.on_recolour.borrow().as_ref() {
                        f(crate::theme::swatches()[i]);
                    }
                    ring.sync_options();
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

    /// Called with the swatch picked for what the Select tool holds, `None` being the accent.
    pub fn connect_recolour(&self, f: impl Fn(Option<[u8; 3]>) + 'static) {
        *self.on_recolour.borrow_mut() = Some(Box::new(f));
    }

    /// Put the Colour swatches away and the commands back, changing nothing: `Escape`. Whether
    /// they were out.
    pub fn close_colours(&self) -> bool {
        let open = self.choosing.replace(false);
        if open {
            self.sync_options();
        }
        open
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
        let selecting = self.ring.tool() == Mode::Select;
        let colours = self.colours.borrow();
        let recolourable = selecting && self.picked.get() && !colours.is_empty();
        let choosing = recolourable && self.choosing.get();
        self.choosing.set(choosing);
        for (button, (action, _)) in self.commands.iter().zip(COMMANDS) {
            let offered = match action {
                "win.pdf-paste" => self.pasteable.get(),
                _ => self.picked.get(),
            };
            button.set_visible(selecting && offered && !choosing);
        }
        check(&self.colour, recolourable, choosing);
        let checked = swatch_of(&colours, &swatches.map(resolved));
        for (i, button) in self.recolours.iter().enumerate() {
            check(button, choosing, checked == Some(i));
        }
    }

    /// Whether the Select tool holds anything, the colours of what it holds that a recolour
    /// reaches, and whether there is anything to paste: which of its commands the ring shows.
    pub fn set_selection(&self, picked: bool, colours: Vec<[u8; 3]>, pasteable: bool) {
        self.picked.set(picked);
        self.colours.replace(colours);
        self.pasteable.set(pasteable);
        self.sync_options();
    }

    /// Paint the swatches as pages recoloured by `theme` show them.
    pub fn set_theme(&self, theme: Theme) {
        self.theme.set(theme);
        for swatch in self.swatches.iter().chain(&self.recolours) {
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
        let commands = self.commands.iter().filter(|b| b.is_visible()).count();
        let swatches = crate::theme::swatches().map(resolved);
        let checked = swatch_of(&self.colours.borrow(), &swatches);
        format!(
            "shown={:?} widths={} swatches={} erasers={} commands={commands} colour={} \
             recolours={} checked={checked:?} painted={}",
            self.ring.tool(),
            shown(&self.widths),
            shown(&self.swatches),
            shown(&self.erasers),
            self.colour.is_visible(),
            shown(&self.recolours),
            painted.join(",")
        )
    }
}

/// Swatch `i` as a page recoloured by `theme` shows its ink, which on a recoloured page is not the
/// colour the file is given: Solarized shows black as its blue-grey text.
fn painted(i: usize, theme: Theme) -> [u8; 3] {
    super::view::shown(resolved(crate::theme::swatches()[i]), theme)
}

/// A swatch's colour as the file is given it: the accent as it is now for `None`.
fn resolved(swatch: Option<[u8; 3]>) -> [u8; 3] {
    swatch.unwrap_or_else(crate::theme::accent_rgb)
}

/// Which of `swatches` every one of `colours` is: the swatch shown checked for what the Select
/// tool holds, none for nothing held or colours that differ.
fn swatch_of(colours: &[[u8; 3]], swatches: &[[u8; 3]]) -> Option<usize> {
    let (first, rest) = colours.split_first()?;
    match rest.iter().all(|c| c == first) {
        true => swatches.iter().position(|s| s == first),
        false => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{Choice, Mode, swatch_of};

    /// The swatch checked for what Select holds is the one all of it shares, and none when its
    /// colours differ, when it is no swatch's or when nothing is held.
    #[test]
    fn the_checked_swatch_is_the_one_every_colour_shares() {
        let swatches = [[0, 0, 200], [200, 0, 0], [0, 0, 0]];
        assert_eq!(swatch_of(&[[0, 0, 0], [0, 0, 0]], &swatches), Some(2));
        assert_eq!(swatch_of(&[[0, 0, 0], [200, 0, 0]], &swatches), None);
        assert_eq!(swatch_of(&[[1, 2, 3]], &swatches), None);
        assert_eq!(swatch_of(&[], &swatches), None);
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
