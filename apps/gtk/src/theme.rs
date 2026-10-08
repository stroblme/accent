//! The window's colour scheme: System, Light, Dark and Solarized.
//!
//! The first three are libadwaita's own, set through `AdwStyleManager`. Solarized is ours, and it
//! is the only place in the app where a colour is written out as a literal: libadwaita 1.7
//! declares `--window-bg-color` and friends on `:root` at theme priority and resolves every one of
//! its own rules through `var()`, so a provider of ours at `STYLE_PROVIDER_PRIORITY_APPLICATION`
//! that redeclares those variables recolours the whole window without touching a single widget.
//! Application priority rather than user priority on purpose: the user's own
//! `~/.config/gtk-4.0/gtk.css` must still win over ours.
//!
//! Solarized deliberately changes only the surfaces and the text on them. Accents, shades and
//! borders keep coming from GNOME, so DESIGN.md's one-accent rule holds in every theme and the
//! seven system accents all still work.

use accent_core::config::Theme;
use gtk::gdk;
use std::cell::{Cell, RefCell};

/// Ethan Schoonover's Solarized: `base3` / `base2` / `base00` in light, `base03` / `base02` /
/// `base0` in dark. The first of each pair is the flat background every surface shares, the
/// second the one raised surfaces (popovers, dialogs, cards) sit on.
const LIGHT_BASE: &str = "#fdf6e3";
const LIGHT_RAISED: &str = "#eee8d5";
const LIGHT_TEXT: &str = "#657b83";
const DARK_BASE: &str = "#002b36";
const DARK_RAISED: &str = "#073642";
const DARK_TEXT: &str = "#839496";

/// libadwaita's own `--view-bg-color`, which the preview needs spelled out because WebKit cannot
/// resolve GTK's CSS variables.
const VIEW_LIGHT: &str = "#ffffff";
const VIEW_DARK: &str = "#1d1d20";

/// The text colour libadwaita's dark scheme puts on `VIEW_DARK`. Only the PDF renderer needs it
/// spelled out: every widget gets it from `--view-fg-color`, but a rendered page is pixels.
const VIEW_DARK_TEXT: &str = "#ebebeb";

/// The light half of the same pair, pre-composited. libadwaita's light `--view-fg-color` is
/// `RGB(0 0 6 / 80%)`, and VTE stores a foreground as opaque RGB and drops the alpha, so handing it
/// the translucent value would paint pure black — blacker than every other piece of text in the
/// window. Over `VIEW_LIGHT` that is `0×0.8 + 255×0.2 = 0x33` and `6×0.8 + 255×0.2 = 0x38`.
const VIEW_LIGHT_TEXT: &str = "#333338";

/// The sixteen ANSI colours a terminal is handed, in the usual order: black, red, green, yellow,
/// blue, magenta, cyan, white, then the bright eight.
///
/// Ayu, by Ivan Demchenko (MIT), transcribed from `mbadolato/iTerm2-Color-Schemes` (MIT). VTE's
/// built-in default is arithmetic rather than designed — its blue is `#0000c0`, which on
/// `VIEW_DARK` is a 1.41:1 contrast ratio and unreadable.
const AYU_DARK_ANSI: [&str; 16] = [
    "#11151c", "#ea6c73", "#7fd962", "#f9af4f", "#53bdfa", "#cda1fa", "#90e1c6", "#c7c7c7",
    "#686868", "#f07178", "#aad94c", "#ffb454", "#59c2ff", "#d2a6ff", "#95e6cb", "#ffffff",
];
/// Ayu Light, same source.
const AYU_LIGHT_ANSI: [&str; 16] = [
    "#000000", "#ea6c6d", "#6cbf43", "#eca944", "#3199e1", "#9e75c7", "#46ba94", "#bababa",
    "#686868", "#f07171", "#86b300", "#f2ae49", "#399ee6", "#a37acc", "#4cbf99", "#d1d1d1",
];
/// Solarized's own ANSI-16 mapping, on both of its bases: Schoonover defines one palette and lets
/// the background decide which end of it reads as foreground.
const SOLARIZED_ANSI: [&str; 16] = [
    "#073642", "#dc322f", "#859900", "#b58900", "#268bd2", "#d33682", "#2aa198", "#eee8d5",
    "#002b36", "#cb4b16", "#586e75", "#657b83", "#839496", "#6c71c4", "#93a1a1", "#fdf6e3",
];

/// Surfaces that take the flat background: the window and everything painted on it.
const FLAT: [&str; 5] = [
    "window",
    "view",
    "headerbar",
    "sidebar",
    "secondary-sidebar",
];
/// The bars that dim when the window loses focus; kept on the same colour, so nothing bands.
const BACKDROP: [&str; 3] = ["headerbar", "sidebar", "secondary-sidebar"];
/// Surfaces that float above the window and are a shade apart from it.
const RAISED: [&str; 4] = ["popover", "dialog", "card", "thumbnail"];

thread_local! {
    /// The Solarized provider and whether it holds the dark half, so the next `apply` can leave
    /// it be or take it off the display again.
    static PROVIDER: RefCell<Option<(gtk::CssProvider, bool)>> = const { RefCell::new(None) };
    /// What the user picked, for `refresh` and for the two lookups below.
    static CHOICE: Cell<Theme> = const { Cell::new(Theme::System) };
}

/// Put `theme` on screen: the colour scheme libadwaita understands, plus our own variables when
/// the answer is Solarized.
///
/// Every window asks, as it is built and on each dark or accent notify, so the provider is
/// swapped only when the half it would hold differs from the one it holds: a swap restyles every
/// widget in every window.
pub fn apply(theme: Theme) {
    CHOICE.set(theme);
    adw::StyleManager::default().set_color_scheme(match theme {
        Theme::Light => adw::ColorScheme::ForceLight,
        Theme::Dark => adw::ColorScheme::ForceDark,
        // Solarized has a light and a dark half and follows the system between them, exactly as
        // the plain system theme does.
        Theme::System | Theme::Solarized => adw::ColorScheme::Default,
    });
    let Some(display) = gdk::Display::default() else {
        return;
    };
    let wanted = (theme == Theme::Solarized).then(is_dark);
    PROVIDER.with_borrow_mut(|slot| {
        if slot.as_ref().map(|(_, dark)| *dark) == wanted {
            return;
        }
        if let Some((old, _)) = slot.take() {
            gtk::style_context_remove_provider_for_display(&display, &old);
        }
        let Some(dark) = wanted else {
            return;
        };
        let provider = gtk::CssProvider::new();
        provider.load_from_string(&css(dark));
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        *slot = Some((provider, dark));
    });
}

/// The system flipped between light and dark: Solarized has to swap which half of its palette is
/// installed, and the other themes have nothing to do.
pub fn refresh() {
    apply(CHOICE.get());
}

fn is_dark() -> bool {
    adw::StyleManager::default().is_dark()
}

/// Whether Solarized is on screen, for the one number that is not a colour and still differs by
/// theme: the contrast floor dim text is held above (`highlight::dim`).
pub fn solarized() -> bool {
    CHOICE.get() == Theme::Solarized
}

/// The colour the preview paints behind the note. WebKit cannot see GTK's CSS variables, so the
/// pane is handed the literal the rest of the window resolves to.
pub fn view_bg(dark: bool) -> &'static str {
    match (CHOICE.get(), dark) {
        (Theme::Solarized, true) => DARK_BASE,
        (Theme::Solarized, false) => LIGHT_BASE,
        (_, true) => VIEW_DARK,
        (_, false) => VIEW_LIGHT,
    }
}

/// The ink on that background, for the one widget that cannot read `--view-fg-color`: a
/// `VteTerminal` is coloured by libvte's own widget rule (`color: @theme_text_color`), and that
/// legacy named colour is what libadwaita's CSS variables are derived *from*, so redeclaring them
/// the way Solarized does cannot flow back into it. Written as the mirror of `view_bg` on purpose:
/// the pair can no longer disagree, which is what left the foreground behind on a theme change.
pub fn view_fg(dark: bool) -> &'static str {
    match (CHOICE.get(), dark) {
        (Theme::Solarized, true) => DARK_TEXT,
        (Theme::Solarized, false) => LIGHT_TEXT,
        (_, true) => VIEW_DARK_TEXT,
        (_, false) => VIEW_LIGHT_TEXT,
    }
}

/// The page a note is printed or exported onto, and its ink: Light's view whatever the theme on
/// screen, since paper is white and a printout or a shared file is read away from the window. The
/// ink is the pre-composited one, so text reaches the page solid rather than as translucent black.
pub fn paper() -> (&'static str, gdk::RGBA) {
    (VIEW_LIGHT, rgba(rgb(VIEW_LIGHT_TEXT), 1.0))
}

/// The sixteen ANSI colours for that theme.
pub fn terminal_palette(dark: bool) -> &'static [&'static str; 16] {
    match (CHOICE.get(), dark) {
        (Theme::Solarized, _) => &SOLARIZED_ANSI,
        (_, true) => &AYU_DARK_ANSI,
        (_, false) => &AYU_LIGHT_ANSI,
    }
}

/// A page's paper and ink, the two ends of the ramp `accent_core::recolour` puts it on.
pub type Page = ([u8; 3], [u8; 3]);

/// The paper and ink a PDF page or a document-like image is recoloured onto, or `None` to leave
/// it exactly as the file defines it.
///
/// A light theme leaves a page alone: white paper on a white view is what the author intended
/// and what a printout looks like. Every other theme is asking for the page to belong to the
/// window, so it is remapped — which is also how Solarized gets its cream paper rather than a
/// white rectangle in the middle of a cream window.
pub fn page_colours(dark: bool) -> Option<Page> {
    match (CHOICE.get(), dark) {
        (Theme::Solarized, true) => Some((rgb(DARK_BASE), rgb(DARK_TEXT))),
        (Theme::Solarized, false) => Some((rgb(LIGHT_BASE), rgb(LIGHT_TEXT))),
        (_, true) => Some((rgb(VIEW_DARK), rgb(VIEW_DARK_TEXT))),
        (_, false) => None,
    }
}

/// How strongly what the reader is doing to a PDF page is painted over it: a note's highlight,
/// the text selection, a search match and the one being stepped to, the ghost of a stroke the
/// Adjust tool is moving, and the hairline that tells white paper from a white window.
///
/// Here rather than in the widget that paints them, because this file is the one that says what
/// a colour is (DESIGN.md, Colour) and an alpha is half of one.
pub const HIGHLIGHT_ALPHA: f32 = 0.2;
pub const SELECTION_ALPHA: f32 = 0.35;
pub const MARK_ALPHA: f32 = 0.3;
pub const CURRENT_MARK_ALPHA: f32 = 0.6;
pub const GHOST_ALPHA: f32 = 0.6;
pub const PAGE_EDGE_ALPHA: f32 = 0.15;
/// A diagram's grid over its paper, in the ink that reads on it: draw.io's `#e6e6e6` on white.
pub const GRID_ALPHA: f32 = 0.1;

/// libadwaita's text selection as its stylesheet writes it: the accent at 30 % while the text has
/// the keyboard (`selection:focus-within`), the text colour at 10 % when it does not. GTK paints
/// the primary caret's selection from that rule, and a column's other carets are painted to match
/// (`multicaret::View::selection_colour`).
pub const TEXT_SELECTION_ALPHA: f32 = 0.3;
pub const UNFOCUSED_SELECTION_ALPHA: f32 = 0.1;

/// How much of the page a highlighter lets through. It also multiplies rather than covers, so
/// this is about how strong the colour is, not about whether the text survives.
pub const HIGHLIGHTER_ALPHA: f32 = 0.4;

/// The minimap's words, in the foreground at this alpha so a page of them reads as grey under
/// the text beside it, those the editor dims at the second, and those it writes in the accent in
/// the accent at the third; and the band over the lines on screen, at rest and under the pointer
/// or a drag.
pub const MAP_INK_ALPHA: f32 = 0.45;
pub const MAP_DIM_ALPHA: f32 = 0.2;
pub const MAP_ACCENT_ALPHA: f32 = 0.8;
pub const MAP_BAND_ALPHA: f32 = 0.08;
pub const MAP_BAND_HOVER_ALPHA: f32 = 0.16;

/// A conflict block's incoming side and its marker line over `page` where the theme has a blue of
/// its own for it, or `None` for the one derived from the foreground (`conflict::tints`).
///
/// Solarized's light page only: its cream cancels a light wash of the derived blue, mixed with an
/// ink that is a blue-grey itself, into grey. Solarized's own blue, the terminal's, reads there at
/// a little more strength, which its ink allows: base00 keeps 3.3:1 on the side and 3.0:1 on the
/// marker line, against its own 4.1:1 on the page.
pub fn incoming_on(page: gdk::RGBA) -> Option<(gdk::RGBA, gdk::RGBA)> {
    let blue = rgb(SOLARIZED_ANSI[4]);
    (rgb_of(page) == rgb(LIGHT_BASE)).then(|| (rgba(blue, 0.2), rgba(blue, 0.27)))
}

/// A colour written down as bytes, painted at `alpha`. The one place a byte becomes a channel.
pub fn rgba(rgb: [u8; 3], alpha: f32) -> gdk::RGBA {
    let channel = |v: u8| f32::from(v) / 255.0;
    gdk::RGBA::new(channel(rgb[0]), channel(rgb[1]), channel(rgb[2]), alpha)
}

/// The same colour at another alpha, which is what every overlay above is.
pub fn at(colour: gdk::RGBA, alpha: f32) -> gdk::RGBA {
    gdk::RGBA::new(colour.red(), colour.green(), colour.blue(), alpha)
}

/// `colour` laid over the opaque `page`, as the one opaque colour the eye gets there.
pub fn over(colour: gdk::RGBA, page: gdk::RGBA) -> gdk::RGBA {
    let a = colour.alpha();
    let mix = |c: f32, p: f32| a * c + (1.0 - a) * p;
    gdk::RGBA::new(
        mix(colour.red(), page.red()),
        mix(colour.green(), page.green()),
        mix(colour.blue(), page.blue()),
        1.0,
    )
}

/// The system accent, for painting with. [`accent_rgb`] is the same colour on its way into a
/// file.
pub fn accent() -> gdk::RGBA {
    adw::StyleManager::default().accent_color_rgba()
}

/// The system accent as bytes, for the two places a colour has to end up inside a file rather
/// than on screen: a highlight's `/C` and an ink stroke's colour.
///
/// The same exception a rendered page already is — pixels in a PDF cannot read a CSS variable —
/// so the value is resolved here and handed over, and this stays the only file that says what a
/// colour is.
pub fn accent_rgb() -> [u8; 3] {
    rgb_of(accent())
}

/// A colour on screen as bytes, on its way into a file: the way back from [`rgba`].
pub fn rgb_of(c: gdk::RGBA) -> [u8; 3] {
    [byte(c.red()), byte(c.green()), byte(c.blue())]
}

/// The six colours the ring offers for ink: the accent — `None`, resolved when a stroke is
/// written, so a tool left on it follows the system — four hues around the wheel from it on the
/// CSV columns' rule (DESIGN.md, Colour), and black, the one literal, which is why this lives
/// here.
pub fn swatches() -> [Option<[u8; 3]>; 6] {
    let c = accent();
    let hsv = gtk::rgb_to_hsv(c.red(), c.green(), c.blue());
    let hue = |column: usize| {
        let (h, s, v) = crate::highlight::rotate(hsv, column);
        let (r, g, b) = gtk::hsv_to_rgb(h, s, v);
        Some([byte(r), byte(g), byte(b)])
    };
    [None, hue(1), hue(2), hue(3), hue(4), Some(rgb("#000000"))]
}

fn byte(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// One of this module's own `#rrggbb` constants as bytes. Nothing else parses colours: this file
/// is the only one allowed to write them down (DESIGN.md, Colour).
fn rgb(hex: &str) -> [u8; 3] {
    let byte = |at: usize| u8::from_str_radix(&hex[at..at + 2], 16).unwrap_or(0);
    [byte(1), byte(3), byte(5)]
}

/// The GtkSourceView style scheme for the editor. GtkSourceView paints its background from the
/// scheme rather than from GTK CSS, so this is the one widget that has to be told about Solarized
/// by name; both schemes ship with gtksourceview 5.
pub fn scheme_id(dark: bool) -> &'static str {
    match (CHOICE.get(), dark) {
        (Theme::Solarized, true) => "solarized-dark",
        (Theme::Solarized, false) => "solarized-light",
        (_, true) => "Adwaita-dark",
        (_, false) => "Adwaita",
    }
}

/// The Solarized variable block. Only backgrounds and their foregrounds: accent, shade and border
/// variables are left to GNOME.
fn css(dark: bool) -> String {
    let (base, raised, text) = match dark {
        true => (DARK_BASE, DARK_RAISED, DARK_TEXT),
        false => (LIGHT_BASE, LIGHT_RAISED, LIGHT_TEXT),
    };
    let mut out = String::from(":root {");
    for name in FLAT {
        out.push_str(&format!(
            " --{name}-bg-color: {base}; --{name}-fg-color: {text};"
        ));
    }
    for name in BACKDROP {
        out.push_str(&format!(" --{name}-backdrop-color: {base};"));
    }
    for name in RAISED {
        out.push_str(&format!(
            " --{name}-bg-color: {raised}; --{name}-fg-color: {text};"
        ));
    }
    out.push_str(" }");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn solarized_css_paints_the_window_and_the_surfaces_above_it() {
        let light = css(false);
        assert!(light.contains("--window-bg-color: #fdf6e3;"), "{light}");
        assert!(light.contains("--view-bg-color: #fdf6e3;"), "{light}");
        assert!(light.contains("--popover-bg-color: #eee8d5;"), "{light}");
        assert!(light.contains("--window-fg-color: #657b83;"), "{light}");

        let dark = css(true);
        assert!(dark.contains("--window-bg-color: #002b36;"), "{dark}");
        assert!(dark.contains("--popover-bg-color: #073642;"), "{dark}");
    }

    /// The regression the terminal had: its background was a pure function of the theme and its
    /// foreground was read off resolved CSS, which is blind to Solarized, so a theme change moved
    /// one and not the other.
    #[test]
    fn terminal_foreground_follows_the_theme() {
        CHOICE.set(Theme::Solarized);
        assert_eq!(view_fg(true), DARK_TEXT);
        assert_eq!(view_fg(false), LIGHT_TEXT);
        CHOICE.set(Theme::System);
        assert_eq!(view_fg(true), VIEW_DARK_TEXT);
        assert_eq!(view_fg(false), VIEW_LIGHT_TEXT);
    }

    /// WCAG 2.1 relative luminance, for the one assertion that has to be a number.
    fn contrast(a: &str, b: &str) -> f64 {
        let luminance = |hex: &str| {
            let channel = |v: f64| match v <= 0.03928 {
                true => v / 12.92,
                false => ((v + 0.055) / 1.055).powf(2.4),
            };
            let [r, g, b] = rgb(hex).map(|c| channel(f64::from(c) / 255.0));
            0.2126 * r + 0.7152 * g + 0.0722 * b
        };
        let (x, y) = (luminance(a), luminance(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }

    /// The reported bug was dark-mode readability — VTE's own blue is `#0000c0`, which scores 1.41
    /// here — so that is what the floor guards, and only there. Ayu Light and Solarized are
    /// deliberately low-contrast designs that fail a 3.0 floor on their authors' own values (Ayu
    /// Light's yellow is 2.03 on white, Solarized's green 2.97 on `base3`); they ship verbatim.
    #[test]
    fn the_dark_palette_is_readable_on_its_background() {
        for (i, colour) in AYU_DARK_ANSI.iter().enumerate() {
            // 0 and 8 are the background-adjacent and dim slots by convention.
            if i == 0 || i == 8 {
                continue;
            }
            let ratio = contrast(colour, VIEW_DARK);
            assert!(
                ratio >= 3.0,
                "ANSI {i} {colour} is {ratio:.2}:1 on {VIEW_DARK}"
            );
        }
        assert!(contrast("#0000c0", VIEW_DARK) < 3.0);
    }

    /// Solarized's light page alone has a blue of its own for a conflict's incoming side, and its
    /// ink keeps 3:1 on both tints, laid over the page.
    #[test]
    fn only_solarized_light_has_its_own_incoming_blue() {
        let page = |hex| rgba(rgb(hex), 1.0);
        for other in [VIEW_LIGHT, VIEW_DARK, DARK_BASE] {
            assert!(incoming_on(page(other)).is_none(), "{other}");
        }
        let cream = page(LIGHT_BASE);
        let (side, line) = incoming_on(cream).expect("Solarized light");
        for tint in [side, line] {
            let laid = |t: f32, p: f32| t * tint.alpha() + p * (1.0 - tint.alpha());
            let [r, g, b] = rgb_of(gdk::RGBA::new(
                laid(tint.red(), cream.red()),
                laid(tint.green(), cream.green()),
                laid(tint.blue(), cream.blue()),
                1.0,
            ));
            let ratio = contrast(LIGHT_TEXT, &format!("#{r:02x}{g:02x}{b:02x}"));
            assert!(ratio >= 3.0, "{ratio:.2}:1 at {}", tint.alpha());
        }
    }

    #[test]
    fn every_palette_is_sixteen_well_formed_colours() {
        for palette in [AYU_DARK_ANSI, AYU_LIGHT_ANSI, SOLARIZED_ANSI] {
            for colour in palette {
                assert_eq!(colour.len(), 7, "{colour}");
                assert!(colour.starts_with('#'), "{colour}");
                assert!(
                    colour[1..].chars().all(|c| c.is_ascii_hexdigit()),
                    "{colour}"
                );
            }
        }
    }

    /// DESIGN.md's one-accent rule: nothing here may redefine the system accent.
    #[test]
    fn solarized_css_leaves_the_accent_alone() {
        for dark in [false, true] {
            assert!(!css(dark).contains("--accent"));
        }
    }
}
