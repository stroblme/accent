//! A cell's style string (`rounded=1;fillColor=#dae8fc;html=1;`): parsed so that it writes back
//! exactly as it came, and resolved against draw.io's default stylesheet for drawing.

use std::collections::HashMap;
use std::fmt;

/// The style as written in the file: `key=value` pairs and bare named styles (`text`, `ellipse`,
/// `group`) in their original order, unknown keys included.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Style {
    items: Vec<(String, Option<String>)>,
    /// A style that starts with `;` does not inherit the default vertex or edge style.
    leading_semicolon: bool,
}

impl Style {
    pub fn parse(s: &str) -> Style {
        let items = s
            .split(';')
            .filter(|part| !part.is_empty())
            .map(|part| match part.split_once('=') {
                Some((k, v)) => (k.to_string(), Some(v.to_string())),
                None => (part.to_string(), None),
            })
            .collect();
        Style {
            items,
            leading_semicolon: s.starts_with(';'),
        }
    }

    /// The value written for `key`, never a named style's.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.items
            .iter()
            .rev()
            .find(|(k, v)| k == key && v.is_some())
            .and_then(|(_, v)| v.as_deref())
    }

    /// Set `key` in place, append it if it is new, or remove it for `None`.
    pub fn set(&mut self, key: &str, value: Option<&str>) {
        match value {
            None => self.items.retain(|(k, v)| !(k == key && v.is_some())),
            Some(value) => match self.items.iter_mut().find(|(k, v)| k == key && v.is_some()) {
                Some(item) => item.1 = Some(value.to_string()),
                None => self.items.push((key.to_string(), Some(value.to_string()))),
            },
        }
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty() && !self.leading_semicolon
    }

    /// The style as drawn: draw.io's defaults for a vertex or an edge, then each named style and
    /// pair in order (`mxStylesheet.getCellStyle`). A pair set to `none` removes the key, which
    /// is how `fillColor=none` comes to mean "no fill".
    pub fn resolve(&self, edge: bool) -> Resolved {
        let mut map: HashMap<String, String> = HashMap::new();
        let put = |pairs: &[(&str, &str)], map: &mut HashMap<String, String>| {
            for (k, v) in pairs {
                map.insert(k.to_string(), v.to_string());
            }
        };
        if !self.leading_semicolon {
            put(if edge { DEFAULT_EDGE } else { DEFAULT_VERTEX }, &mut map);
        }
        for (key, value) in &self.items {
            match value {
                None => {
                    if let Some(named) = named_style(key) {
                        put(named, &mut map);
                    }
                }
                Some(v) if v == "none" => {
                    map.remove(key);
                }
                Some(v) => {
                    map.insert(key.clone(), v.clone());
                }
            }
        }
        Resolved { map }
    }
}

impl fmt::Display for Style {
    /// `k=v;` for every pair and `name;` for every named style, as draw.io writes them.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.leading_semicolon {
            f.write_str(";")?;
        }
        for (k, v) in &self.items {
            match v {
                Some(v) => write!(f, "{k}={v};")?,
                None => write!(f, "{k};")?,
            }
        }
        Ok(())
    }
}

/// `defaultVertex` in draw.io's `styles/default.xml`.
const DEFAULT_VERTEX: &[(&str, &str)] = &[
    ("shape", "label"),
    ("perimeter", "rectanglePerimeter"),
    ("fontSize", "12"),
    ("fontFamily", "Helvetica"),
    ("align", "center"),
    ("verticalAlign", "middle"),
    ("fillColor", "default"),
    ("strokeColor", "default"),
    ("fontColor", "default"),
];

/// `defaultEdge` in the same file.
const DEFAULT_EDGE: &[(&str, &str)] = &[
    ("shape", "connector"),
    ("labelBackgroundColor", "default"),
    ("endArrow", "classic"),
    ("fontSize", "11"),
    ("fontFamily", "Helvetica"),
    ("align", "center"),
    ("verticalAlign", "middle"),
    ("rounded", "1"),
    ("strokeColor", "default"),
    ("fontColor", "default"),
];

/// The named styles of `styles/default.xml` a cell can carry as a bare token. The ones that only
/// name a shape are here so that the shape is known to be unsupported rather than taken for a
/// rectangle.
fn named_style(name: &str) -> Option<&'static [(&'static str, &'static str)]> {
    const TEXT: &[(&str, &str)] = &[
        ("fillColor", "none"),
        ("gradientColor", "none"),
        ("strokeColor", "none"),
        ("align", "left"),
        ("verticalAlign", "top"),
    ];
    Some(match name {
        "text" => TEXT,
        "edgeLabel" => &[
            ("fillColor", "none"),
            ("gradientColor", "none"),
            ("strokeColor", "none"),
            ("align", "left"),
            ("verticalAlign", "top"),
            ("labelBackgroundColor", "default"),
            ("fontSize", "11"),
        ],
        "label" => &[
            ("fontStyle", "1"),
            ("align", "left"),
            ("verticalAlign", "middle"),
            ("spacing", "2"),
            ("spacingLeft", "52"),
            ("imageWidth", "42"),
            ("imageHeight", "42"),
            ("rounded", "1"),
        ],
        "group" => &[
            ("verticalAlign", "top"),
            ("fillColor", "none"),
            ("strokeColor", "none"),
            ("gradientColor", "none"),
            ("pointerEvents", "0"),
        ],
        "ellipse" => &[("shape", "ellipse"), ("perimeter", "ellipsePerimeter")],
        "image" => &[
            ("shape", "image"),
            ("labelBackgroundColor", "default"),
            ("verticalAlign", "top"),
            ("verticalLabelPosition", "bottom"),
        ],
        "rhombus" => &[("shape", "rhombus"), ("perimeter", "rhombusPerimeter")],
        "triangle" => &[("shape", "triangle"), ("perimeter", "trianglePerimeter")],
        "swimlane" => &[
            ("shape", "swimlane"),
            ("fontStyle", "1"),
            ("startSize", "23"),
        ],
        "line" => &[
            ("shape", "line"),
            ("strokeWidth", "4"),
            ("verticalAlign", "top"),
        ],
        "arrow" => &[("shape", "arrow"), ("edgeStyle", "none")],
        _ => return None,
    })
}

/// A style with its defaults filled in: what drawing reads.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Resolved {
    map: HashMap<String, String>,
}

impl Resolved {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.map.get(key).map(String::as_str)
    }

    /// A number, or `default` when the key is missing or not a number.
    pub fn num(&self, key: &str, default: f64) -> f64 {
        self.get(key).and_then(parse_num).unwrap_or(default)
    }

    /// A switch: any number but 0 is on, and so is `true`.
    pub fn flag(&self, key: &str, default: bool) -> bool {
        match self.get(key) {
            Some("true") => true,
            Some("false") => false,
            Some(v) => v.trim().parse::<f64>().map_or(default, |n| n != 0.0),
            None => default,
        }
    }

    /// A colour, or `None` for `none` and anything unreadable. `default` is draw.io's light-mode
    /// answer: black ink for strokes and text, white for fills and label backgrounds.
    pub fn color(&self, key: &str) -> Option<Color> {
        match self.get(key)? {
            "default" => Some(match key {
                "fillColor" | "labelBackgroundColor" | "gradientColor" | "swimlaneFillColor" => {
                    Color::WHITE
                }
                _ => Color::BLACK,
            }),
            v => Color::parse(v),
        }
    }

    /// The shape name, `label` (a rectangle) when none is given.
    pub fn shape(&self) -> &str {
        self.get("shape").unwrap_or("label")
    }
}

/// A number as a file writes it, spaces around it allowed. `None` for anything else, infinities
/// and NaN included, which would otherwise carry into every coordinate worked out from them.
pub(crate) fn parse_num(v: &str) -> Option<f64> {
    v.trim().parse::<f64>().ok().filter(|n| n.is_finite())
}

/// An sRGB colour with straight alpha.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const BLACK: Color = Color::rgb(0, 0, 0);
    pub const WHITE: Color = Color::rgb(255, 255, 255);

    pub const fn rgb(r: u8, g: u8, b: u8) -> Color {
        Color { r, g, b, a: 255 }
    }

    /// `#rgb`, `#rrggbb`, `#rrggbbaa`, `rgb(…)`, `rgba(…)`, `light-dark(a, b)` (its light half),
    /// `transparent`, and any CSS colour name, whatever its case. `none`, `default` and anything
    /// else give `None`.
    pub fn parse(s: &str) -> Option<Color> {
        let s = s.trim();
        if let Some(hex) = s.strip_prefix('#') {
            let digit = |i: usize| u8::from_str_radix(hex.get(i..i + 1)?, 16).ok();
            let byte = |i: usize| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok();
            return match hex.len() {
                3 => Some(Color::rgb(digit(0)? * 17, digit(1)? * 17, digit(2)? * 17)),
                6 => Some(Color::rgb(byte(0)?, byte(2)?, byte(4)?)),
                8 => Some(Color {
                    a: byte(6)?,
                    ..Color::rgb(byte(0)?, byte(2)?, byte(4)?)
                }),
                _ => None,
            };
        }
        if let Some(inner) = s
            .strip_prefix("light-dark(")
            .and_then(|r| r.strip_suffix(')'))
        {
            return Color::parse(split_args(inner).first()?);
        }
        let function = s
            .strip_prefix("rgba(")
            .or_else(|| s.strip_prefix("rgb("))
            .and_then(|r| r.strip_suffix(')'));
        if let Some(inner) = function {
            let args = split_args(inner);
            let channel = |i: usize| -> Option<u8> {
                Some(
                    args.get(i)?
                        .trim()
                        .parse::<f64>()
                        .ok()?
                        .clamp(0.0, 255.0)
                        .round() as u8,
                )
            };
            let alpha = match args.get(3) {
                Some(a) => (a.trim().parse::<f64>().ok()?.clamp(0.0, 1.0) * 255.0).round() as u8,
                None => 255,
            };
            return Some(Color {
                a: alpha,
                ..Color::rgb(channel(0)?, channel(1)?, channel(2)?)
            });
        }
        if s.eq_ignore_ascii_case("transparent") {
            return Some(Color {
                a: 0,
                ..Color::BLACK
            });
        }
        let name = s.to_ascii_lowercase();
        let i = NAMED
            .binary_search_by_key(&name.as_str(), |(n, _)| *n)
            .ok()?;
        let rgb = NAMED[i].1;
        Some(Color::rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8))
    }

    /// `#rrggbb`, alpha dropped: the form a style value is written in.
    pub fn hex(&self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }

    /// The same colour with its alpha multiplied by `factor` (0–1).
    pub fn fade(self, factor: f64) -> Color {
        Color {
            a: (f64::from(self.a) * factor.clamp(0.0, 1.0)).round() as u8,
            ..self
        }
    }
}

/// The named colours of CSS Color Module Level 4 (`red`, `rebeccapurple`), sorted so that a
/// lookup is a binary search. `transparent` is not here: it is the only keyword that is not an
/// RGB triple.
const NAMED: &[(&str, u32)] = &[
    ("aliceblue", 0xf0f8ff),
    ("antiquewhite", 0xfaebd7),
    ("aqua", 0x00ffff),
    ("aquamarine", 0x7fffd4),
    ("azure", 0xf0ffff),
    ("beige", 0xf5f5dc),
    ("bisque", 0xffe4c4),
    ("black", 0x000000),
    ("blanchedalmond", 0xffebcd),
    ("blue", 0x0000ff),
    ("blueviolet", 0x8a2be2),
    ("brown", 0xa52a2a),
    ("burlywood", 0xdeb887),
    ("cadetblue", 0x5f9ea0),
    ("chartreuse", 0x7fff00),
    ("chocolate", 0xd2691e),
    ("coral", 0xff7f50),
    ("cornflowerblue", 0x6495ed),
    ("cornsilk", 0xfff8dc),
    ("crimson", 0xdc143c),
    ("cyan", 0x00ffff),
    ("darkblue", 0x00008b),
    ("darkcyan", 0x008b8b),
    ("darkgoldenrod", 0xb8860b),
    ("darkgray", 0xa9a9a9),
    ("darkgreen", 0x006400),
    ("darkgrey", 0xa9a9a9),
    ("darkkhaki", 0xbdb76b),
    ("darkmagenta", 0x8b008b),
    ("darkolivegreen", 0x556b2f),
    ("darkorange", 0xff8c00),
    ("darkorchid", 0x9932cc),
    ("darkred", 0x8b0000),
    ("darksalmon", 0xe9967a),
    ("darkseagreen", 0x8fbc8f),
    ("darkslateblue", 0x483d8b),
    ("darkslategray", 0x2f4f4f),
    ("darkslategrey", 0x2f4f4f),
    ("darkturquoise", 0x00ced1),
    ("darkviolet", 0x9400d3),
    ("deeppink", 0xff1493),
    ("deepskyblue", 0x00bfff),
    ("dimgray", 0x696969),
    ("dimgrey", 0x696969),
    ("dodgerblue", 0x1e90ff),
    ("firebrick", 0xb22222),
    ("floralwhite", 0xfffaf0),
    ("forestgreen", 0x228b22),
    ("fuchsia", 0xff00ff),
    ("gainsboro", 0xdcdcdc),
    ("ghostwhite", 0xf8f8ff),
    ("gold", 0xffd700),
    ("goldenrod", 0xdaa520),
    ("gray", 0x808080),
    ("green", 0x008000),
    ("greenyellow", 0xadff2f),
    ("grey", 0x808080),
    ("honeydew", 0xf0fff0),
    ("hotpink", 0xff69b4),
    ("indianred", 0xcd5c5c),
    ("indigo", 0x4b0082),
    ("ivory", 0xfffff0),
    ("khaki", 0xf0e68c),
    ("lavender", 0xe6e6fa),
    ("lavenderblush", 0xfff0f5),
    ("lawngreen", 0x7cfc00),
    ("lemonchiffon", 0xfffacd),
    ("lightblue", 0xadd8e6),
    ("lightcoral", 0xf08080),
    ("lightcyan", 0xe0ffff),
    ("lightgoldenrodyellow", 0xfafad2),
    ("lightgray", 0xd3d3d3),
    ("lightgreen", 0x90ee90),
    ("lightgrey", 0xd3d3d3),
    ("lightpink", 0xffb6c1),
    ("lightsalmon", 0xffa07a),
    ("lightseagreen", 0x20b2aa),
    ("lightskyblue", 0x87cefa),
    ("lightslategray", 0x778899),
    ("lightslategrey", 0x778899),
    ("lightsteelblue", 0xb0c4de),
    ("lightyellow", 0xffffe0),
    ("lime", 0x00ff00),
    ("limegreen", 0x32cd32),
    ("linen", 0xfaf0e6),
    ("magenta", 0xff00ff),
    ("maroon", 0x800000),
    ("mediumaquamarine", 0x66cdaa),
    ("mediumblue", 0x0000cd),
    ("mediumorchid", 0xba55d3),
    ("mediumpurple", 0x9370db),
    ("mediumseagreen", 0x3cb371),
    ("mediumslateblue", 0x7b68ee),
    ("mediumspringgreen", 0x00fa9a),
    ("mediumturquoise", 0x48d1cc),
    ("mediumvioletred", 0xc71585),
    ("midnightblue", 0x191970),
    ("mintcream", 0xf5fffa),
    ("mistyrose", 0xffe4e1),
    ("moccasin", 0xffe4b5),
    ("navajowhite", 0xffdead),
    ("navy", 0x000080),
    ("oldlace", 0xfdf5e6),
    ("olive", 0x808000),
    ("olivedrab", 0x6b8e23),
    ("orange", 0xffa500),
    ("orangered", 0xff4500),
    ("orchid", 0xda70d6),
    ("palegoldenrod", 0xeee8aa),
    ("palegreen", 0x98fb98),
    ("paleturquoise", 0xafeeee),
    ("palevioletred", 0xdb7093),
    ("papayawhip", 0xffefd5),
    ("peachpuff", 0xffdab9),
    ("peru", 0xcd853f),
    ("pink", 0xffc0cb),
    ("plum", 0xdda0dd),
    ("powderblue", 0xb0e0e6),
    ("purple", 0x800080),
    ("rebeccapurple", 0x663399),
    ("red", 0xff0000),
    ("rosybrown", 0xbc8f8f),
    ("royalblue", 0x4169e1),
    ("saddlebrown", 0x8b4513),
    ("salmon", 0xfa8072),
    ("sandybrown", 0xf4a460),
    ("seagreen", 0x2e8b57),
    ("seashell", 0xfff5ee),
    ("sienna", 0xa0522d),
    ("silver", 0xc0c0c0),
    ("skyblue", 0x87ceeb),
    ("slateblue", 0x6a5acd),
    ("slategray", 0x708090),
    ("slategrey", 0x708090),
    ("snow", 0xfffafa),
    ("springgreen", 0x00ff7f),
    ("steelblue", 0x4682b4),
    ("tan", 0xd2b48c),
    ("teal", 0x008080),
    ("thistle", 0xd8bfd8),
    ("tomato", 0xff6347),
    ("turquoise", 0x40e0d0),
    ("violet", 0xee82ee),
    ("wheat", 0xf5deb3),
    ("white", 0xffffff),
    ("whitesmoke", 0xf5f5f5),
    ("yellow", 0xffff00),
    ("yellowgreen", 0x9acd32),
];

/// The arguments of a CSS function, split on the commas that are not inside a nested call.
fn split_args(s: &str) -> Vec<&str> {
    let (mut depth, mut start, mut out) = (0usize, 0usize, Vec::new());
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                out.push(s[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(s[start..].trim());
    out
}

/// The styles draw.io itself gives a new cell from its sidebar, so what accent inserts looks and
/// behaves the same when the file is opened there.
pub mod presets {
    use super::Style;
    use crate::route::Constraint;

    pub const RECT: &str = "rounded=0;whiteSpace=wrap;html=1;";
    pub const ROUNDED: &str = "rounded=1;whiteSpace=wrap;html=1;";
    pub const ELLIPSE: &str = "ellipse;whiteSpace=wrap;html=1;";
    /// `Editor.defaultTextStyle`.
    pub const TEXT: &str = "text;html=1;whiteSpace=wrap;strokeColor=none;fillColor=none;align=center;verticalAlign=middle;rounded=0;";

    /// How a new connector is routed.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub enum EdgeKind {
        Straight,
        /// draw.io's own default (`Graph.defaultEdgeStyle`).
        #[default]
        Orthogonal,
        Curved,
    }

    /// A connector's style; `arrow` false ends it plainly.
    pub fn edge(kind: EdgeKind, arrow: bool) -> String {
        let route = match kind {
            EdgeKind::Straight => "",
            EdgeKind::Orthogonal => {
                "edgeStyle=orthogonalEdgeStyle;orthogonalLoop=1;jettySize=auto;"
            }
            EdgeKind::Curved => "curved=1;",
        };
        let end = if arrow { "" } else { "endArrow=none;" };
        format!("{route}rounded=0;{end}html=1;")
    }

    /// `style` with its ends pinned to connection points, as `mxGraph.setConnectionConstraint`
    /// writes them: `exitX`, `exitY`, `exitDx`, `exitDy` and, for a point off the outline,
    /// `exitPerimeter=0`; the `entry` keys likewise for the target. An end given `None` keeps
    /// what the style says.
    // mxGraph.setConnectionConstraint, mxGraph.js 7163-7213
    pub fn constrained(
        style: &str,
        exit: Option<&Constraint>,
        entry: Option<&Constraint>,
    ) -> String {
        let mut style = Style::parse(style);
        for (end, c) in [("exit", exit), ("entry", entry)] {
            let Some(c) = c else { continue };
            for (key, n) in [
                ("X", c.point.x),
                ("Y", c.point.y),
                ("Dx", c.dx),
                ("Dy", c.dy),
            ] {
                style.set(&format!("{end}{key}"), Some(&n.to_string()));
            }
            style.set(&format!("{end}Perimeter"), (!c.perimeter).then_some("0"));
        }
        style.to_string()
    }

    /// An embedded picture. draw.io leaves `;base64` out of the data URI, the `;` being the
    /// style's own separator.
    pub fn image(mime: &str, bytes: &[u8]) -> String {
        format!(
            "shape=image;verticalLabelPosition=bottom;verticalAlign=top;imageAspect=0;aspect=fixed;image=data:{mime},{};",
            crate::base64::encode(bytes)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::Point;
    use crate::route::Constraint;

    #[test]
    fn a_style_writes_back_as_it_came() {
        for s in [
            "text;html=1;strokeColor=none;fillColor=none;align=left;",
            ";strokeColor=#ff0000;",
            "shape=image;image=data:image/png,iVBORw0KGgo=;",
            "",
        ] {
            assert_eq!(Style::parse(s).to_string(), s);
        }
        assert_eq!(Style::parse("a=1;b").to_string(), "a=1;b;");
    }

    #[test]
    fn set_keeps_the_order_and_none_removes() {
        let mut s = Style::parse("rounded=1;fillColor=#fff;foo=bar;");
        s.set("fillColor", Some("#000000"));
        s.set("dashed", Some("1"));
        s.set("foo", None);
        assert_eq!(s.to_string(), "rounded=1;fillColor=#000000;dashed=1;");
    }

    #[test]
    fn named_styles_and_none_resolve_as_draw_io_does() {
        let text = Style::parse("text;html=1;fontSize=20;").resolve(false);
        assert_eq!(text.color("fillColor"), None, "text has no fill");
        assert_eq!(text.get("align"), Some("left"));
        assert_eq!(text.num("fontSize", 0.0), 20.0);
        let edge = Style::parse("endArrow=none;").resolve(true);
        assert_eq!(edge.get("endArrow"), None);
        assert_eq!(edge.get("shape"), Some("connector"));
        let bare = Style::parse(";rounded=1").resolve(false);
        assert_eq!(
            bare.get("fillColor"),
            None,
            "a leading ; skips the defaults"
        );
        let plain = Style::parse("rounded=1;").resolve(false);
        assert_eq!(plain.color("fillColor"), Some(Color::WHITE));
        assert_eq!(plain.color("strokeColor"), Some(Color::BLACK));
        assert_eq!(plain.shape(), "label");
    }

    #[test]
    fn a_pinned_end_is_written_as_draw_io_writes_it() {
        let on = Constraint {
            point: Point::new(0.25, 0.0),
            dx: 0.0,
            dy: 0.0,
            perimeter: true,
        };
        let off = Constraint {
            point: Point::new(1.0, 0.5),
            dx: 2.5,
            dy: 0.0,
            perimeter: false,
        };
        assert_eq!(
            presets::constrained("html=1;exitPerimeter=0;", Some(&on), Some(&off)),
            "html=1;exitX=0.25;exitY=0;exitDx=0;exitDy=0;entryX=1;entryY=0.5;entryDx=2.5;entryDy=0;entryPerimeter=0;"
        );
    }

    #[test]
    fn colours_parse_in_every_form_draw_io_writes() {
        assert_eq!(Color::parse("#DAE8FC"), Some(Color::rgb(0xda, 0xe8, 0xfc)));
        assert_eq!(Color::parse("#fff"), Some(Color::WHITE));
        assert_eq!(
            Color::parse("rgb(0, 150, 130)"),
            Some(Color::rgb(0, 150, 130))
        );
        assert_eq!(Color::parse("rgba(0,0,0,0.5)").map(|c| c.a), Some(128));
        assert_eq!(
            Color::parse("light-dark(#ffffff, var(--x, #121212))"),
            Some(Color::WHITE)
        );
        assert_eq!(Color::parse("white"), Some(Color::WHITE));
        assert_eq!(Color::parse("transparent").map(|c| c.a), Some(0));
        assert_eq!(Color::parse("red"), Some(Color::rgb(255, 0, 0)));
        assert_eq!(
            Color::parse("rebeccapurple"),
            Some(Color::rgb(0x66, 0x33, 0x99))
        );
        assert_eq!(Color::parse("darkgrey"), Color::parse("darkgray"));
        assert_eq!(Color::parse(" Red "), Color::parse("RED"));
        assert_eq!(Color::parse("none"), None);
        assert_eq!(Color::parse("default"), None);
        assert_eq!(Color::rgb(0, 150, 130).hex(), "#009682");
    }

    #[test]
    fn the_named_colour_table_is_sorted_and_whole() {
        assert_eq!(NAMED.len(), 148, "CSS Color 4, both grey spellings");
        assert!(
            NAMED.windows(2).all(|w| w[0].0 < w[1].0),
            "sorted for the binary search"
        );
        assert!(
            NAMED.iter().all(|(n, _)| *n != "none" && *n != "default"),
            "draw.io's two words for no colour are not colours"
        );
    }
}
