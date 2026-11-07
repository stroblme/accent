//! The face a label's font family is drawn in, chosen as draw.io's browser chooses it, so a label
//! takes the room and the place it takes in draw.io.
//!
//! Chromium takes a family only when the font fontconfig matches it with is that family, or one
//! Skia counts as having its metrics (`SkFontConfigInterfaceDirect::isAcceptableMatch`); else it
//! tries Blink's alternate for the family (`AlternateFamilyName`, `alternate_font_family.h`), and
//! then the list's next family. Helvetica, which fontconfig matches with Nimbus Sans, is so drawn
//! in Arial's match, Liberation Sans, as draw.io desktop draws it on Linux, while a real
//! Helvetica, as on macOS, stays.
//!
//! A generic family is the one Chromium's settings name for it, and a list none of whose families
//! is taken falls to the standard one (`FontFallbackList::GetFontData`), so Verdana, where it is
//! missing, is drawn in Times New Roman's face as draw.io desktop draws it.

/// Blink's alternate families, Courier New to Courier everywhere but on Windows.
const ALTERNATES: [(&str, &str); 6] = [
    ("Courier", "Courier New"),
    ("Courier New", "Courier"),
    ("Times", "Times New Roman"),
    ("Times New Roman", "Times"),
    ("Arial", "Helvetica"),
    ("Helvetica", "Arial"),
];

/// Skia's families of one another's metrics (`GetFontEquivClass`).
// ponytail: its CJK families are left out, which only a CJK font named in a label's style meets;
// add them from Skia's table if such a label turns up drawn in another face than draw.io's.
const METRICS: [&[&str]; 6] = [
    &["Arial", "Arimo", "Liberation Sans"],
    &["Times New Roman", "Tinos", "Liberation Serif"],
    &["Courier New", "Cousine", "Liberation Mono"],
    &["Symbol", "Symbol Neu"],
    &["Cambria", "Caladea"],
    &["Calibri", "Carlito"],
];

/// The names Skia takes whatever fontconfig matches for (`IsFallbackFontAllowed`), and sans-serif.
// ponytail: Skia takes sans-serif's match only when it is the first family fontconfig's rules
// put for it (`isAcceptableMatch`), which it is wherever that font is installed.
const FALLBACKS: [&str; 4] = ["sans", "serif", "monospace", "sans-serif"];

/// The family Chromium's settings name for the generic family `generic`, for `None` the standard
/// one (`FontSelector::FamilyNameFromSettings`, defaults from `locale_settings_linux.grd` and
/// `locale_settings_mac.grd`).
fn setting(generic: Option<&str>) -> Option<&'static str> {
    let mac = cfg!(target_os = "macos");
    let on = |linux, macos| Some(if mac { macos } else { linux });
    match generic.map(str::to_ascii_lowercase).as_deref() {
        None | Some("serif") => on("Times New Roman", "Times"),
        Some("sans-serif") => on("Arial", "Helvetica"),
        Some("monospace") => on("Monospace", "Menlo"),
        Some("cursive") => on("Comic Sans MS", "Apple Chancery"),
        Some(_) => None,
    }
}

/// The family Chromium draws `css`, a CSS family list, in, `matched` naming the family of the
/// font fontconfig matches a family with; `css` as it is when Chromium would take none of it,
/// not even its standard family, left to the toolkit's own fallback.
pub fn chosen(css: &str, matched: impl Fn(&str) -> Option<String>) -> String {
    let same = |a: &str, b: &str| a.eq_ignore_ascii_case(b);
    let alike = |a: &str, b: &str| {
        METRICS
            .iter()
            .any(|class| class.iter().any(|f| same(f, a)) && class.iter().any(|f| same(f, b)))
    };
    let takes = |family: &str| {
        FALLBACKS.iter().any(|f| same(f, family))
            || matched(family).is_some_and(|m| same(&m, family) || alike(&m, family))
    };
    let alternate = |family: &str| {
        let (_, alternate) = ALTERNATES.iter().find(|(f, _)| same(f, family))?;
        Some(*alternate)
    };
    let pick = |family: &str| match takes(family) {
        true => Some(family.to_string()),
        false => alternate(family).filter(|a| takes(a)).map(str::to_string),
    };
    for family in css.split(',').map(|f| f.trim().trim_matches(['"', '\''])) {
        // A generic family is its setting's, or else what fontconfig matches its name with.
        let found = setting(Some(family)).and_then(pick);
        if let Some(found) = found.or_else(|| pick(family)) {
            return found;
        }
    }
    let standard = setting(None).and_then(pick);
    standard.unwrap_or_else(|| css.to_string())
}

#[cfg(test)]
mod tests {
    /// Linux as draw.io desktop meets it: Helvetica and Times are aliases, Arial and Times New
    /// Roman have their metric twins, and fontconfig falls back on DejaVu Sans.
    fn linux(family: &str) -> Option<String> {
        let face = match family {
            "Helvetica" => "Nimbus Sans",
            "Arial" => "Liberation Sans",
            "Times" => "Nimbus Roman",
            "Times New Roman" => "Liberation Serif",
            "Monospace" => "DejaVu Sans Mono",
            _ => "DejaVu Sans",
        };
        Some(face.to_string())
    }

    #[test]
    fn a_family_fontconfig_only_aliases_is_drawn_in_its_alternate() {
        assert_eq!(super::chosen("Helvetica", linux), "Arial");
        assert_eq!(super::chosen("Times", linux), "Times New Roman");
        assert_eq!(super::chosen("Times New Roman", linux), "Times New Roman");
        assert_eq!(super::chosen("Verdana,'Helvetica'", linux), "Arial");
        // A real Helvetica stays.
        let real = |family: &str| Some(family.to_string());
        assert_eq!(super::chosen("Helvetica", real), "Helvetica");
    }

    #[test]
    fn a_list_with_no_family_installed_takes_its_generic_family_or_the_standard_one() {
        // The generic families as Chromium's settings name them, a cursive one being as missing
        // as Verdana is.
        assert_eq!(super::chosen("Verdana,sans-serif", linux), "Arial");
        assert_eq!(super::chosen("serif", linux), "Times New Roman");
        assert_eq!(super::chosen("Verdana,monospace", linux), "Monospace");
        assert_eq!(super::chosen("cursive", linux), "Times New Roman");
        // No generic family: the standard one, a serif.
        assert_eq!(super::chosen("Verdana", linux), "Times New Roman");
        // Without Arial's face a generic family is fontconfig's own.
        let bare = |_: &str| Some("DejaVu Sans".to_string());
        assert_eq!(super::chosen("Verdana,sans-serif", bare), "sans-serif");
    }
}
