//! Formula labels: the HTML a label with a formula is typeset from ([`label_html`]), MathML
//! from the core converter, and what typesets it, which only a build with WebKit has.
//!
//! Without the `webkit` feature nothing does: [`Typesetter`] has no values there, so a diagram's
//! typesetter is always `None` and a formula is painted as its source, as `render_diagram`
//! draws one.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
#[cfg(not(feature = "webkit"))]
use std::rc::Rc;

use accent_drawio::{Align, Color, Font, Marks, Run};
use gtk::{gdk, glib};

/// The page zoom the labels are rendered at: a formula stays sharp up to this canvas zoom and
/// is scaled down, not up, below it.
pub(super) const RENDER_ZOOM: f64 = 3.0;

/// A label to typeset: its HTML and the width it wraps at, if it wraps.
pub struct Label {
    pub key: u64,
    pub html: String,
}

/// A typeset label: its picture, and its size in page units.
#[derive(Clone)]
pub struct Rendered {
    pub texture: gdk::Texture,
    pub size: (f64, f64),
}

#[cfg(feature = "webkit")]
pub use super::typeset::{Typesetter, shared};

#[cfg(not(feature = "webkit"))]
pub enum Typesetter {}

#[cfg(not(feature = "webkit"))]
impl Typesetter {
    pub fn busy(&self) -> bool {
        match *self {}
    }

    pub fn connect_ready(&self, _f: impl Fn() -> bool + 'static) {
        match *self {}
    }

    pub fn get(&self, _key: u64) -> Option<Option<Rendered>> {
        match *self {}
    }

    pub fn ask(self: &Rc<Self>, _label: Label) {
        match **self {}
    }
}

#[cfg(not(feature = "webkit"))]
pub fn shared() -> Option<Rc<Typesetter>> {
    None
}

/// A label as the HTML WebKit lays out: the label's font, colour and alignment on its box, the
/// runs as spans, each formula as MathML (its source, if it does not parse), wrapped at `width`
/// page units or not at all.
pub fn label_html(runs: &[Run], font: &Font, align: Align, width: Option<f64>) -> String {
    // `Arial,Helvetica` is a list of two families, as draw.io's default is: each is quoted on its
    // own, or CSS reads the whole thing as one family name nothing answers to. Each is a CSS
    // string inside an HTML attribute, as whoever wrote the file wrote it: a quote, a backslash,
    // a line break or a tag could end one or the other, and so could the character an `&` names.
    let families: Vec<String> = font
        .family
        .split(',')
        .map(|f| {
            let f: String = f
                .trim()
                .chars()
                .filter(|c| !matches!(c, '\'' | '"' | '\\' | '&' | '<' | '>') && !c.is_control())
                .collect();
            format!("'{f}'")
        })
        .collect();
    let mut style = format!(
        "font-family: {}, sans-serif; font-size: {}px; color: {};",
        families.join(", "),
        font.size,
        colour(font.color)
    );
    if font.bold {
        style.push_str(" font-weight: bold;");
    }
    if font.italic {
        style.push_str(" font-style: italic;");
    }
    if font.underline {
        style.push_str(" text-decoration: underline;");
    }
    style.push_str(match align {
        Align::Left => " text-align: left;",
        Align::Center => " text-align: center;",
        Align::Right => " text-align: right;",
    });
    match width {
        Some(w) => style.push_str(&format!(" width: {w}px; white-space: normal;")),
        None => style.push_str(" white-space: nowrap;"),
    }
    let mut body = String::new();
    for run in runs {
        match run {
            Run::Text { text, marks } => {
                let span = marks_style(marks);
                match span.is_empty() {
                    true => body.push_str(&escape(text)),
                    false => {
                        body.push_str(&format!("<span style=\"{span}\">{}</span>", escape(text)))
                    }
                }
            }
            Run::Math { tex, display } => match accent_core::markdown::mathml(tex, *display) {
                Some(mathml) => body.push_str(&mathml),
                None => body.push_str(&escape(&format!("\\({tex}\\)"))),
            },
            Run::Break => body.push_str("<br>"),
            Run::Bullet => body.push_str("• "),
        }
    }
    format!("<div style=\"{style}\">{body}</div>")
}

/// Which `Label` key `html` is typeset under.
pub fn key_of(html: &str) -> u64 {
    let mut h = DefaultHasher::new();
    html.hash(&mut h);
    h.finish()
}

fn marks_style(marks: &Marks) -> String {
    let mut s = String::new();
    if marks.bold {
        s.push_str("font-weight: bold;");
    }
    if marks.italic {
        s.push_str("font-style: italic;");
    }
    if marks.underline {
        s.push_str("text-decoration: underline;");
    }
    if let Some(c) = marks.color {
        s.push_str(&format!("color: {};", colour(c)));
    }
    if let Some(size) = marks.size {
        s.push_str(&format!("font-size: {size}px;"));
    }
    s
}

fn colour(c: Color) -> String {
    format!(
        "rgba({}, {}, {}, {})",
        c.r,
        c.g,
        c.b,
        f64::from(c.a) / 255.0
    )
}

fn escape(text: &str) -> String {
    glib::markup_escape_text(text).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_label_becomes_html_with_its_formula_as_mathml() {
        let font = Font {
            size: 14.0,
            family: "Arial,Helvetica".into(),
            color: Color::rgb(0, 0, 0),
            bold: false,
            italic: false,
            underline: false,
        };
        let runs = [
            Run::Text {
                text: "a < b".into(),
                marks: Marks {
                    bold: true,
                    ..Marks::default()
                },
            },
            Run::Math {
                tex: "x^2".into(),
                display: false,
            },
        ];
        let html = label_html(&runs, &font, Align::Center, Some(100.0));
        assert!(html.contains("width: 100px"), "{html}");
        assert!(
            html.contains("font-family: 'Arial', 'Helvetica', sans-serif;"),
            "{html}"
        );
        assert!(
            html.contains("<span style=\"font-weight: bold;\">a &lt; b</span>"),
            "{html}"
        );
        assert!(html.contains("<math"), "{html}");
        assert_ne!(
            key_of(&html),
            key_of(&label_html(&runs, &font, Align::Left, None))
        );
    }

    /// Escape a file-provided font family as both a CSS string and an HTML attribute
    /// to prevent injected markup or network-fetching style declarations.
    #[test]
    fn a_font_family_stays_inside_its_quotes() {
        for family in [
            "x\"><img src=\"http://example.com/x.png\">",
            "x'; background: url(http://example.com/x.png); '",
            "x\\'; background: url(http://example.com/x.png); '",
            "x&#39;; background: url(http://example.com/x.png); &#39;",
            "x\n;background: url(http://example.com/x.png);",
        ] {
            let font = Font {
                size: 14.0,
                family: family.into(),
                color: Color::rgb(0, 0, 0),
                bold: false,
                italic: false,
                underline: false,
            };
            let html = label_html(&[], &font, Align::Left, None);
            let quoted = html
                .strip_prefix("<div style=\"font-family: '")
                .and_then(|s| s.split_once("', sans-serif;"))
                .map_or("", |(quoted, _)| quoted);
            assert!(
                quoted.starts_with('x') && !quoted.contains(['\'', '"', '\\', '&', '<', '\n']),
                "{html}"
            );
        }
    }
}
