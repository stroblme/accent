//! What the window takes from the platform: its document and monospace fonts, which the editor,
//! the code tags, the terminal and libadwaita's own style classes all share, and on macOS the
//! interface font and the window controls' side, which GTK sets otherwise than macOS does.
//!
//! GNOME's fonts are its settings, through libadwaita. macOS has none: libadwaita falls back to
//! GSettings' defaults there, Adwaita Sans and Adwaita Mono, which macOS does not have, and
//! CoreText's stand-in for them is proportional.

use gtk::{gdk, pango};

/// The modifier GTK spells `<Primary>` in accelerator strings.
pub const PRIMARY_MODIFIER: gdk::ModifierType = if cfg!(target_os = "macos") {
    gdk::ModifierType::META_MASK
} else {
    gdk::ModifierType::CONTROL_MASK
};

/// AppKit's default font for each role (`NSFont`, macOS 12) as Pango's CoreText backend names
/// them: the user font for documents, the fixed-pitch user font, and the system font, menus' and
/// title bars' 13 pt, for the interface, which GDK sets at the user font's 12 pt. SF Mono is not
/// among them: it ships inside Terminal and Xcode, out of other apps' reach.
const MAC_DOCUMENT: &str = "Helvetica 12";
const MAC_MONOSPACE: &str = "Menlo 11";
const MAC_INTERFACE: &str = "system-ui 13";

/// The document font as a Pango description; a note takes its size (DESIGN.md, Typography).
pub fn document_font() -> String {
    match cfg!(target_os = "macos") {
        true => MAC_DOCUMENT.to_string(),
        false => adw::StyleManager::default()
            .document_font_name()
            .to_string(),
    }
}

/// The monospace font as a Pango description, for code and terminals.
pub fn monospace_font() -> String {
    match cfg!(target_os = "macos") {
        true => MAC_MONOSPACE.to_string(),
        false => adw::StyleManager::default()
            .monospace_font_name()
            .to_string(),
    }
}

/// [`monospace_font`]'s family, for notes, code tabs, inline code and formulas.
///
/// A named family matches the default document face; generic CSS `monospace` can choose
/// another face whose metrics rasterise inline formulas poorly at the prose font's size.
pub fn monospace_family() -> String {
    pango::FontDescription::from_string(&monospace_font())
        .family()
        .map(|family| family.to_string())
        .unwrap_or_else(|| "monospace".to_string())
}

/// On macOS, set the interface in the system font, put a dialog's close button on the left with
/// the window's own, and point libadwaita's `.document` and `.monospace` classes at
/// [`document_font`] and [`monospace_font`]. GNOME's settings already say all of it.
pub fn install() {
    if !cfg!(target_os = "macos") {
        return;
    }
    let Some(display) = gdk::Display::default() else {
        return;
    };
    let settings = gtk::Settings::for_display(&display);
    settings.set_gtk_font_name(Some(MAC_INTERFACE));
    settings.set_gtk_decoration_layout(Some("close,minimize,maximize:"));
    let var = |name: &str, font: &str| {
        let desc = pango::FontDescription::from_string(font);
        format!(
            "--{name}-font-family: \"{}\"; --{name}-font-size: {}pt;",
            desc.family().unwrap_or_default(),
            desc.size() / pango::SCALE
        )
    };
    let provider = gtk::CssProvider::new();
    provider.load_from_string(&format!(
        ":root {{ {} {} }}",
        var("document", &document_font()),
        var("monospace", &monospace_font())
    ));
    gtk::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}
