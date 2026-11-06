//! The face a label's font family is drawn in, chosen as draw.io's browser chooses it, so a label
//! takes the room and the place it takes in draw.io.
//!
//! Chromium takes a family only when the font fontconfig matches it with is that family, or one
//! Skia counts as having its metrics (`SkFontConfigInterfaceDirect::isAcceptableMatch`); else it
//! tries Blink's alternate for the family (`AlternateFamilyName`, `alternate_font_family.h`), and
//! then the list's next family. Helvetica, which fontconfig matches with Nimbus Sans, is so drawn
//! in Arial's match, Liberation Sans, as draw.io desktop draws it on Linux, while a real
//! Helvetica, as on macOS, stays.

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

/// The family Chromium draws `css`, a CSS family list, in, `matched` naming the family of the
/// font fontconfig matches a family with; `css` as it is when Chromium would take none of it,
/// left to the toolkit's own fallback.
pub fn chosen(css: &str, matched: impl Fn(&str) -> Option<String>) -> String {
    let same = |a: &str, b: &str| a.eq_ignore_ascii_case(b);
    let alike = |a: &str, b: &str| {
        METRICS
            .iter()
            .any(|class| class.iter().any(|f| same(f, a)) && class.iter().any(|f| same(f, b)))
    };
    let takes =
        |family: &str| matched(family).is_some_and(|m| same(&m, family) || alike(&m, family));
    for family in css.split(',').map(|f| f.trim().trim_matches(['"', '\''])) {
        if takes(family) {
            return family.to_string();
        }
        let alternate = ALTERNATES.iter().find(|(f, _)| same(f, family));
        if let Some((_, alternate)) = alternate.filter(|(_, a)| takes(a)) {
            return alternate.to_string();
        }
    }
    css.to_string()
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_family_fontconfig_only_aliases_is_drawn_in_its_alternate() {
        // Linux as draw.io desktop meets it: Helvetica and Times are aliases, Arial and Times New
        // Roman have their metric twins.
        let linux = |family: &str| {
            let face = match family {
                "Helvetica" => "Nimbus Sans",
                "Arial" => "Liberation Sans",
                "Times" => "Nimbus Roman",
                "Times New Roman" => "Liberation Serif",
                "Verdana" => "DejaVu Sans",
                other => other,
            };
            Some(face.to_string())
        };
        assert_eq!(super::chosen("Helvetica", linux), "Arial");
        assert_eq!(super::chosen("Times", linux), "Times New Roman");
        assert_eq!(super::chosen("Times New Roman", linux), "Times New Roman");
        assert_eq!(super::chosen("Verdana,'Helvetica'", linux), "Arial");
        assert_eq!(super::chosen("Verdana", linux), "Verdana");
        // A real Helvetica stays.
        let real = |family: &str| Some(family.to_string());
        assert_eq!(super::chosen("Helvetica", real), "Helvetica");
    }
}
