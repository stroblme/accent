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
    /// The Solarized provider, so the next `apply` can take it off the display again.
    static PROVIDER: RefCell<Option<gtk::CssProvider>> = const { RefCell::new(None) };
    /// What the user picked, for `refresh` and for the two lookups below.
    static CHOICE: Cell<Theme> = const { Cell::new(Theme::System) };
}

/// Put `theme` on screen: the colour scheme libadwaita understands, plus our own variables when
/// the answer is Solarized.
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
    PROVIDER.with_borrow_mut(|slot| {
        if let Some(old) = slot.take() {
            gtk::style_context_remove_provider_for_display(&display, &old);
        }
        if theme != Theme::Solarized {
            return;
        }
        let provider = gtk::CssProvider::new();
        provider.load_from_string(&css(is_dark()));
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        *slot = Some(provider);
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

    /// DESIGN.md's one-accent rule: nothing here may redefine the system accent.
    #[test]
    fn solarized_css_leaves_the_accent_alone() {
        for dark in [false, true] {
            assert!(!css(dark).contains("--accent"));
        }
    }
}
