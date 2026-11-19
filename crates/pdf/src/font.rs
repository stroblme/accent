//! The fonts an appearance stream sets text in. Helvetica, as one names it without embedding it,
//! being one of the fonts every reader has (ISO 32000-1 §9.6.2.2), in the WinAnsi encoding
//! (Annex D.2), which is Windows-1252 — Latin-1, the euro and typographic quotes and dashes —
//! and its widths. And a form field's own font ([`SimpleFont`]), a simple font (§9.6) whose
//! encoding and widths say which code each character has and how far it advances.

use crate::{Dict, Object};

/// Helvetica's advance widths in thousandths of the font size, for codes 32 to 255 of
/// WinAnsiEncoding; 0 where the encoding has no character. From Adobe's Helvetica AFM (the core
/// 14 metrics, 1997: "Copyright (c) 1985, 1987, 1989, 1990, 1997 Adobe Systems Incorporated.
/// Helvetica is a trademark of Linotype-Hell AG and/or its subsidiaries").
const WIDTHS: [u16; 224] = [
    278, 278, 355, 556, 556, 889, 667, 191, 333, 333, 389, 584, 278, 333, 278, 278, //
    556, 556, 556, 556, 556, 556, 556, 556, 556, 556, 278, 278, 584, 584, 584, 556, //
    1015, 667, 667, 722, 722, 667, 611, 778, 722, 278, 500, 667, 556, 833, 722, 778, //
    667, 778, 722, 667, 611, 722, 667, 944, 667, 667, 611, 278, 278, 278, 469, 556, //
    333, 556, 556, 500, 556, 556, 278, 556, 556, 222, 222, 500, 222, 833, 556, 556, //
    556, 556, 333, 500, 278, 556, 500, 722, 500, 500, 500, 334, 260, 334, 584, 0, //
    556, 0, 222, 556, 333, 1000, 556, 556, 333, 1000, 667, 333, 1000, 0, 611, 0, //
    0, 222, 222, 333, 333, 350, 556, 1000, 333, 1000, 500, 333, 944, 0, 500, 667, //
    278, 333, 556, 556, 556, 556, 260, 556, 333, 737, 370, 556, 584, 333, 737, 333, //
    400, 584, 333, 333, 333, 556, 537, 278, 333, 333, 365, 556, 834, 834, 834, 611, //
    667, 667, 667, 667, 667, 667, 1000, 722, 667, 667, 667, 667, 278, 278, 278, 278, //
    722, 722, 778, 778, 778, 778, 778, 584, 778, 722, 722, 722, 722, 667, 667, 611, //
    556, 556, 556, 556, 556, 556, 889, 500, 556, 556, 556, 556, 278, 278, 278, 278, //
    556, 556, 556, 556, 556, 556, 556, 584, 611, 556, 556, 556, 556, 500, 556, 500, //
];

/// Where Windows-1252 differs from Latin-1: what it puts in 0x80 to 0x9F.
const HIGH: [(char, u8); 27] = [
    ('\u{20AC}', 0x80),
    ('\u{201A}', 0x82),
    ('\u{0192}', 0x83),
    ('\u{201E}', 0x84),
    ('\u{2026}', 0x85),
    ('\u{2020}', 0x86),
    ('\u{2021}', 0x87),
    ('\u{02C6}', 0x88),
    ('\u{2030}', 0x89),
    ('\u{0160}', 0x8A),
    ('\u{2039}', 0x8B),
    ('\u{0152}', 0x8C),
    ('\u{017D}', 0x8E),
    ('\u{2018}', 0x91),
    ('\u{2019}', 0x92),
    ('\u{201C}', 0x93),
    ('\u{201D}', 0x94),
    ('\u{2022}', 0x95),
    ('\u{2013}', 0x96),
    ('\u{2014}', 0x97),
    ('\u{02DC}', 0x98),
    ('\u{2122}', 0x99),
    ('\u{0161}', 0x9A),
    ('\u{203A}', 0x9B),
    ('\u{0153}', 0x9C),
    ('\u{017E}', 0x9E),
    ('\u{0178}', 0x9F),
];

/// The standard 14's widths beside Helvetica's, as [`WIDTHS`] has them, from Adobe's AFMs of
/// the same release (Helvetica-Bold, Times-Roman, Times-Bold): Courier's are 600 throughout. Their
/// obliques and italics but Times' set as their uprights do.
const HELVETICA_BOLD: [u16; 224] = [
    278, 333, 474, 556, 556, 889, 722, 238, 333, 333, 389, 584, 278, 333, 278, 278, //
    556, 556, 556, 556, 556, 556, 556, 556, 556, 556, 333, 333, 584, 584, 584, 611, //
    975, 722, 722, 722, 722, 667, 611, 778, 722, 278, 556, 722, 611, 833, 722, 778, //
    667, 778, 722, 667, 611, 722, 667, 944, 667, 667, 611, 333, 278, 333, 584, 556, //
    333, 556, 611, 556, 611, 556, 333, 611, 611, 278, 278, 556, 278, 889, 611, 611, //
    611, 611, 389, 556, 333, 611, 556, 778, 556, 556, 500, 389, 280, 389, 584, 0, //
    556, 0, 278, 556, 500, 1000, 556, 556, 333, 1000, 667, 333, 1000, 0, 611, 0, //
    0, 278, 278, 500, 500, 350, 556, 1000, 333, 1000, 556, 333, 944, 0, 500, 667, //
    278, 333, 556, 556, 556, 556, 280, 556, 333, 737, 370, 556, 584, 333, 737, 333, //
    400, 584, 333, 333, 333, 611, 556, 278, 333, 333, 365, 556, 834, 834, 834, 611, //
    722, 722, 722, 722, 722, 722, 1000, 722, 667, 667, 667, 667, 278, 278, 278, 278, //
    722, 722, 778, 778, 778, 778, 778, 584, 778, 722, 722, 722, 722, 667, 667, 611, //
    556, 556, 556, 556, 556, 556, 889, 556, 556, 556, 556, 556, 278, 278, 278, 278, //
    611, 611, 611, 611, 611, 611, 611, 584, 611, 611, 611, 611, 611, 556, 611, 556, //
];
const TIMES: [u16; 224] = [
    250, 333, 408, 500, 500, 833, 778, 180, 333, 333, 500, 564, 250, 333, 250, 278, //
    500, 500, 500, 500, 500, 500, 500, 500, 500, 500, 278, 278, 564, 564, 564, 444, //
    921, 722, 667, 667, 722, 611, 556, 722, 722, 333, 389, 722, 611, 889, 722, 722, //
    556, 722, 667, 556, 611, 722, 722, 944, 722, 722, 611, 333, 278, 333, 469, 500, //
    333, 444, 500, 444, 500, 444, 333, 500, 500, 278, 278, 500, 278, 778, 500, 500, //
    500, 500, 333, 389, 278, 500, 500, 722, 500, 500, 444, 480, 200, 480, 541, 0, //
    500, 0, 333, 500, 444, 1000, 500, 500, 333, 1000, 556, 333, 889, 0, 611, 0, //
    0, 333, 333, 444, 444, 350, 500, 1000, 333, 980, 389, 333, 722, 0, 444, 722, //
    250, 333, 500, 500, 500, 500, 200, 500, 333, 760, 276, 500, 564, 333, 760, 333, //
    400, 564, 300, 300, 333, 500, 453, 250, 333, 300, 310, 500, 750, 750, 750, 444, //
    722, 722, 722, 722, 722, 722, 889, 667, 611, 611, 611, 611, 333, 333, 333, 333, //
    722, 722, 722, 722, 722, 722, 722, 564, 722, 722, 722, 722, 722, 722, 556, 500, //
    444, 444, 444, 444, 444, 444, 667, 444, 444, 444, 444, 444, 278, 278, 278, 278, //
    500, 500, 500, 500, 500, 500, 500, 564, 500, 500, 500, 500, 500, 500, 500, 500, //
];
const TIMES_BOLD: [u16; 224] = [
    250, 333, 555, 500, 500, 1000, 833, 278, 333, 333, 500, 570, 250, 333, 250, 278, //
    500, 500, 500, 500, 500, 500, 500, 500, 500, 500, 333, 333, 570, 570, 570, 500, //
    930, 722, 667, 722, 722, 667, 611, 778, 778, 389, 500, 778, 667, 944, 722, 778, //
    611, 778, 722, 556, 667, 722, 722, 1000, 722, 722, 667, 333, 278, 333, 581, 500, //
    333, 500, 556, 444, 556, 444, 333, 500, 556, 278, 333, 556, 278, 833, 556, 500, //
    556, 556, 444, 389, 333, 556, 500, 722, 500, 500, 444, 394, 220, 394, 520, 0, //
    500, 0, 333, 500, 500, 1000, 500, 500, 333, 1000, 556, 333, 1000, 0, 667, 0, //
    0, 333, 333, 500, 500, 350, 500, 1000, 333, 1000, 389, 333, 722, 0, 444, 722, //
    250, 333, 500, 500, 500, 500, 220, 500, 333, 747, 300, 500, 570, 333, 747, 333, //
    400, 570, 300, 300, 333, 556, 540, 250, 333, 300, 330, 500, 750, 750, 750, 500, //
    722, 722, 722, 722, 722, 722, 1000, 722, 667, 667, 667, 667, 389, 389, 389, 389, //
    722, 722, 778, 778, 778, 778, 778, 570, 778, 722, 722, 722, 722, 722, 611, 556, //
    500, 500, 500, 500, 500, 500, 722, 444, 444, 444, 444, 444, 278, 278, 278, 278, //
    500, 556, 500, 500, 500, 500, 500, 570, 500, 556, 556, 556, 556, 500, 556, 500, //
];

/// What StandardEncoding and MacRomanEncoding put at codes 32 to 255 (Annex D.2), `\0` where
/// they put nothing.
const STANDARD: &str = concat!(
    " !\"#$%&’()*+,-./0123456789:;<=>?",
    "@ABCDEFGHIJKLMNOPQRSTUVWXYZ[\\]^_",
    "‘abcdefghijklmnopqrstuvwxyz{|}~\0",
    "\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0",
    "\0¡¢£⁄¥ƒ§¤'“«‹›ﬁﬂ\0–†‡·\0¶•‚„”»…‰\0¿",
    "\0`´ˆ˜¯˘˙¨\0˚¸\0˝˛ˇ—\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0",
    "\0Æ\0ª\0\0\0\0ŁØŒº\0\0\0\0\0æ\0\0\0ı\0\0łøœß\0\0\0\0",
);
const MAC_ROMAN: &str = concat!(
    " !\"#$%&'()*+,-./0123456789:;<=>?",
    "@ABCDEFGHIJKLMNOPQRSTUVWXYZ[\\]^_",
    "`abcdefghijklmnopqrstuvwxyz{|}~\0",
    "ÄÅÇÉÑÖÜáàâäãåçéèêëíìîïñóòôöõúùûü",
    "†°¢£§•¶ß®©™´¨\0ÆØ\0±\0\0¥µ\0\0\0\0\0ªº\0æø",
    "¿¡¬\0ƒ\0\0«»… ÀÃÕŒœ–—“”‘’÷\0ÿŸ⁄¤‹›ﬁﬂ",
    "‡·‚„‰ÂÊÁËÈÍÎÏÌÓÔ\0ÒÚÛÙıˆ˜¯˘˙˚¸˝˛ˇ",
);
/// The glyph names the three encodings use, from the Adobe Glyph List, by name: what a font's
/// `/Differences` names its codes by.
#[rustfmt::skip]
const GLYPHS: [(&str, char); 228] = [
    ("A", 'A'), ("AE", '\u{c6}'), ("Aacute", '\u{c1}'), ("Acircumflex", '\u{c2}'),
    ("Adieresis", '\u{c4}'), ("Agrave", '\u{c0}'), ("Aring", '\u{c5}'), ("Atilde", '\u{c3}'),
    ("B", 'B'), ("C", 'C'), ("Ccedilla", '\u{c7}'), ("D", 'D'), ("E", 'E'), ("Eacute", '\u{c9}'),
    ("Ecircumflex", '\u{ca}'), ("Edieresis", '\u{cb}'), ("Egrave", '\u{c8}'), ("Eth", '\u{d0}'),
    ("Euro", '\u{20ac}'), ("F", 'F'), ("G", 'G'), ("H", 'H'), ("I", 'I'), ("Iacute", '\u{cd}'),
    ("Icircumflex", '\u{ce}'), ("Idieresis", '\u{cf}'), ("Igrave", '\u{cc}'), ("J", 'J'),
    ("K", 'K'), ("L", 'L'), ("Lslash", '\u{141}'), ("M", 'M'), ("N", 'N'), ("Ntilde", '\u{d1}'),
    ("O", 'O'), ("OE", '\u{152}'), ("Oacute", '\u{d3}'), ("Ocircumflex", '\u{d4}'),
    ("Odieresis", '\u{d6}'), ("Ograve", '\u{d2}'), ("Oslash", '\u{d8}'), ("Otilde", '\u{d5}'),
    ("P", 'P'), ("Q", 'Q'), ("R", 'R'), ("S", 'S'), ("Scaron", '\u{160}'), ("T", 'T'),
    ("Thorn", '\u{de}'), ("U", 'U'), ("Uacute", '\u{da}'), ("Ucircumflex", '\u{db}'),
    ("Udieresis", '\u{dc}'), ("Ugrave", '\u{d9}'), ("V", 'V'), ("W", 'W'), ("X", 'X'), ("Y", 'Y'),
    ("Yacute", '\u{dd}'), ("Ydieresis", '\u{178}'), ("Z", 'Z'), ("Zcaron", '\u{17d}'), ("a", 'a'),
    ("aacute", '\u{e1}'), ("acircumflex", '\u{e2}'), ("acute", '\u{b4}'), ("adieresis", '\u{e4}'),
    ("ae", '\u{e6}'), ("agrave", '\u{e0}'), ("ampersand", '&'), ("aring", '\u{e5}'),
    ("asciicircum", '^'), ("asciitilde", '~'), ("asterisk", '*'), ("at", '@'),
    ("atilde", '\u{e3}'), ("b", 'b'), ("backslash", '\u{5c}'), ("bar", '|'), ("braceleft", '{'),
    ("braceright", '}'), ("bracketleft", '['), ("bracketright", ']'), ("breve", '\u{2d8}'),
    ("brokenbar", '\u{a6}'), ("bullet", '\u{2022}'), ("c", 'c'), ("caron", '\u{2c7}'),
    ("ccedilla", '\u{e7}'), ("cedilla", '\u{b8}'), ("cent", '\u{a2}'), ("circumflex", '\u{2c6}'),
    ("colon", ':'), ("comma", ','), ("copyright", '\u{a9}'), ("currency", '\u{a4}'), ("d", 'd'),
    ("dagger", '\u{2020}'), ("daggerdbl", '\u{2021}'), ("degree", '\u{b0}'),
    ("dieresis", '\u{a8}'), ("divide", '\u{f7}'), ("dollar", '$'), ("dotaccent", '\u{2d9}'),
    ("dotlessi", '\u{131}'), ("e", 'e'), ("eacute", '\u{e9}'), ("ecircumflex", '\u{ea}'),
    ("edieresis", '\u{eb}'), ("egrave", '\u{e8}'), ("eight", '8'), ("ellipsis", '\u{2026}'),
    ("emdash", '\u{2014}'), ("endash", '\u{2013}'), ("equal", '='), ("eth", '\u{f0}'),
    ("exclam", '!'), ("exclamdown", '\u{a1}'), ("f", 'f'), ("fi", '\u{fb01}'), ("five", '5'),
    ("fl", '\u{fb02}'), ("florin", '\u{192}'), ("four", '4'), ("fraction", '\u{2044}'), ("g", 'g'),
    ("germandbls", '\u{df}'), ("grave", '`'), ("greater", '>'), ("guillemotleft", '\u{ab}'),
    ("guillemotright", '\u{bb}'), ("guilsinglleft", '\u{2039}'), ("guilsinglright", '\u{203a}'),
    ("h", 'h'), ("hungarumlaut", '\u{2dd}'), ("hyphen", '-'), ("i", 'i'), ("iacute", '\u{ed}'),
    ("icircumflex", '\u{ee}'), ("idieresis", '\u{ef}'), ("igrave", '\u{ec}'), ("j", 'j'),
    ("k", 'k'), ("l", 'l'), ("less", '<'), ("logicalnot", '\u{ac}'), ("lslash", '\u{142}'),
    ("m", 'm'), ("macron", '\u{af}'), ("mu", '\u{b5}'), ("multiply", '\u{d7}'), ("n", 'n'),
    ("nine", '9'), ("ntilde", '\u{f1}'), ("numbersign", '#'), ("o", 'o'), ("oacute", '\u{f3}'),
    ("ocircumflex", '\u{f4}'), ("odieresis", '\u{f6}'), ("oe", '\u{153}'), ("ogonek", '\u{2db}'),
    ("ograve", '\u{f2}'), ("one", '1'), ("onehalf", '\u{bd}'), ("onequarter", '\u{bc}'),
    ("onesuperior", '\u{b9}'), ("ordfeminine", '\u{aa}'), ("ordmasculine", '\u{ba}'),
    ("oslash", '\u{f8}'), ("otilde", '\u{f5}'), ("p", 'p'), ("paragraph", '\u{b6}'),
    ("parenleft", '('), ("parenright", ')'), ("percent", '%'), ("period", '.'),
    ("periodcentered", '\u{b7}'), ("perthousand", '\u{2030}'), ("plus", '+'),
    ("plusminus", '\u{b1}'), ("q", 'q'), ("question", '?'), ("questiondown", '\u{bf}'),
    ("quotedbl", '"'), ("quotedblbase", '\u{201e}'), ("quotedblleft", '\u{201c}'),
    ("quotedblright", '\u{201d}'), ("quoteleft", '\u{2018}'), ("quoteright", '\u{2019}'),
    ("quotesinglbase", '\u{201a}'), ("quotesingle", '\u{27}'), ("r", 'r'),
    ("registered", '\u{ae}'), ("ring", '\u{2da}'), ("s", 's'), ("scaron", '\u{161}'),
    ("section", '\u{a7}'), ("semicolon", ';'), ("seven", '7'), ("six", '6'), ("slash", '/'),
    ("space", ' '), ("sterling", '\u{a3}'), ("t", 't'), ("thorn", '\u{fe}'), ("three", '3'),
    ("threequarters", '\u{be}'), ("threesuperior", '\u{b3}'), ("tilde", '\u{2dc}'),
    ("trademark", '\u{2122}'), ("two", '2'), ("twosuperior", '\u{b2}'), ("u", 'u'),
    ("uacute", '\u{fa}'), ("ucircumflex", '\u{fb}'), ("udieresis", '\u{fc}'), ("ugrave", '\u{f9}'),
    ("underscore", '_'), ("v", 'v'), ("w", 'w'), ("x", 'x'), ("y", 'y'), ("yacute", '\u{fd}'),
    ("ydieresis", '\u{ff}'), ("yen", '\u{a5}'), ("z", 'z'), ("zcaron", '\u{17e}'), ("zero", '0'),
];

/// Which of the standard 14's widths a font has, by its name.
#[derive(Clone, Copy)]
enum Standard {
    Helvetica,
    HelveticaBold,
    Times,
    TimesBold,
    Courier,
}

impl Standard {
    /// The standard font `base` names, Arial and Times New Roman being their metric twins, as
    /// readers take them.
    fn of(base: &str) -> Option<Standard> {
        let bold = base.contains("Bold");
        Some(match base {
            b if b.starts_with("Helvetica") || b.starts_with("Arial") => match bold {
                true => Standard::HelveticaBold,
                false => Standard::Helvetica,
            },
            b if b.starts_with("Times") => match bold {
                true => Standard::TimesBold,
                false => Standard::Times,
            },
            b if b.starts_with("Courier") => Standard::Courier,
            _ => return None,
        })
    }

    /// How far `c` advances, in thousandths of the size; `None` for one WinAnsi lacks.
    fn width(self, c: char) -> Option<f32> {
        let at = usize::from(winansi(c)?.checked_sub(32)?);
        let table = match self {
            Standard::Helvetica => &WIDTHS,
            Standard::HelveticaBold => &HELVETICA_BOLD,
            Standard::Times => &TIMES,
            Standard::TimesBold => &TIMES_BOLD,
            Standard::Courier => return Some(600.0),
        };
        Some(f32::from(table[at]))
    }
}

/// A simple font as an appearance stream sets text in it: the character each code stands for and
/// how far it advances, in thousandths of the size, and how far above and below the baseline the
/// font's glyphs reach, where it says.
#[derive(Debug, Clone, PartialEq)]
pub struct SimpleFont {
    codes: Vec<(u8, char, f32)>,
    pub ascent: Option<f32>,
    pub descent: Option<f32>,
}

impl SimpleFont {
    /// Helvetica in WinAnsiEncoding: what accent's own text is set in, which every reader has.
    pub fn helvetica() -> SimpleFont {
        let chars = (' '..='\u{FF}').chain(HIGH.map(|(c, _)| c));
        let codes = (chars.filter_map(|c| Some((winansi(c)?, c))))
            .map(|(code, c)| (code, c, f32::from(helvetica_width(code))))
            .collect();
        SimpleFont {
            codes,
            ascent: None,
            descent: None,
        }
    }

    /// The font `font` says, `resolve` reading what it refers to. `None` for a composite or a
    /// Type 3 font, a symbolic one, a subset embedded with what it was first used for, which may
    /// lack a glyph for anything else, and one whose widths it does not give and which is none of
    /// the standard 14.
    pub fn of(font: &Dict, resolve: impl Fn(Option<&Object>) -> Object) -> Option<SimpleFont> {
        let name = |key: &[u8]| match resolve(font.get(key)) {
            Object::Name(n) => Some(String::from_utf8_lossy(&n).into_owned()),
            _ => None,
        };
        let subtype = name(b"Subtype")?;
        if !matches!(subtype.as_str(), "Type1" | "MMType1" | "TrueType") {
            return None;
        }
        let base = name(b"BaseFont").unwrap_or_default();
        let subset = base.find('+').is_some_and(|at| at == 6);
        let base = if subset { &base[7..] } else { &base[..] };
        if base.starts_with("Symbol") || base.starts_with("ZapfDingbats") {
            return None;
        }
        let descriptor = match resolve(font.get(b"FontDescriptor")) {
            Object::Dict(d) => d,
            _ => Dict::default(),
        };
        let embedded = [&b"FontFile"[..], b"FontFile2", b"FontFile3"]
            .iter()
            .any(|key| descriptor.get(key).is_some());
        if subset && embedded {
            return None;
        }
        let chars = encoding(&resolve(font.get(b"Encoding")), &subtype, &resolve)?;
        let number = |o: Object| match o {
            Object::Int(i) => Some(i as f32),
            Object::Real(r) => Some(r as f32),
            _ => None,
        };
        let widths = match resolve(font.get(b"Widths")) {
            Object::Array(w) => Some(w),
            _ => None,
        };
        let first = resolve(font.get(b"FirstChar")).as_int().unwrap_or(0);
        let missing = number(resolve(descriptor.get(b"MissingWidth"))).unwrap_or(0.0);
        let standard = Standard::of(base);
        let mut codes = Vec::new();
        for (code, c) in (0..=255u8).zip(chars) {
            if c == '\0' {
                continue;
            }
            let width = match &widths {
                Some(w) => Some(
                    usize::try_from(i64::from(code) - first)
                        .ok()
                        .and_then(|at| number(resolve(w.get(at))))
                        .unwrap_or(missing),
                ),
                None => standard?.width(c),
            };
            if let Some(width) = width {
                codes.push((code, c, width));
            }
        }
        let metric = |key: &[u8]| number(resolve(descriptor.get(key))).filter(|v| *v != 0.0);
        Some(SimpleFont {
            codes,
            ascent: metric(b"Ascent").map(|a| a / 1000.0),
            descent: metric(b"Descent").map(|d| d.abs() / 1000.0),
        })
    }

    /// `text` in the font's codes, or `None` where it has none for a character of it.
    pub fn encode(&self, text: &str) -> Option<Vec<u8>> {
        (text.chars())
            .map(|c| (self.codes.iter()).find_map(|&(code, has, _)| (has == c).then_some(code)))
            .collect()
    }

    /// How far `codes` advance, in thousandths of the size.
    pub fn width(&self, codes: &[u8]) -> f32 {
        (codes.iter())
            .map(|&c| (self.codes.iter()).find_map(|&(code, _, w)| (code == c).then_some(w)))
            .map(|w| w.unwrap_or(0.0))
            .sum()
    }
}

/// The character each code 0 to 255 stands for under `encoding`, a font's `/Encoding`: a base
/// encoding by name, or a dictionary of one and the `/Differences` from it; `\0` for none.
/// Without one, a TrueType font's is WinAnsi and a Type 1 font's its own, Standard for the Latin
/// fonts this reads. `None` for an encoding this does not know.
fn encoding(
    encoding: &Object,
    subtype: &str,
    resolve: &impl Fn(Option<&Object>) -> Object,
) -> Option<Vec<char>> {
    let named = |name: Option<&[u8]>| -> Option<Vec<char>> {
        let mut chars = vec!['\0'; 256];
        let table = match name {
            Some(b"WinAnsiEncoding") => None,
            Some(b"StandardEncoding") => Some(STANDARD),
            Some(b"MacRomanEncoding") => Some(MAC_ROMAN),
            None if subtype == "TrueType" => None,
            None => Some(STANDARD),
            Some(_) => return None,
        };
        match table {
            Some(table) => (32usize..)
                .zip(table.chars())
                .for_each(|(code, c)| chars[code] = c),
            None => {
                let all = (' '..='\u{FF}').chain(HIGH.map(|(c, _)| c));
                all.filter_map(|c| Some((winansi(c)?, c)))
                    .for_each(|(code, c)| chars[usize::from(code)] = c);
            }
        }
        Some(chars)
    };
    match encoding {
        Object::Name(n) => named(Some(n)),
        Object::Dict(d) => {
            let base = resolve(d.get(b"BaseEncoding"));
            let mut chars = named(base.as_name())?;
            let Object::Array(differences) = resolve(d.get(b"Differences")) else {
                return Some(chars);
            };
            let mut code = 0usize;
            for entry in &differences {
                match resolve(Some(entry)) {
                    Object::Int(at) => code = usize::try_from(at).unwrap_or(256),
                    Object::Name(glyph) => {
                        if let Some(slot) = chars.get_mut(code) {
                            *slot = glyph_char(&glyph).unwrap_or('\0');
                        }
                        code += 1;
                    }
                    _ => {}
                }
            }
            Some(chars)
        }
        _ => named(None),
    }
}

/// The character the glyph `name` is: one of the Latin encodings' names, or `uniXXXX`.
fn glyph_char(name: &[u8]) -> Option<char> {
    let name = std::str::from_utf8(name).ok()?;
    if let Some(hex) = name.strip_prefix("uni").filter(|h| h.len() == 4) {
        return char::from_u32(u32::from_str_radix(hex, 16).ok()?);
    }
    let at = GLYPHS.binary_search_by(|(n, _)| (*n).cmp(name)).ok()?;
    Some(GLYPHS[at].1)
}

/// `c`'s code in WinAnsiEncoding, or `None` for a character it has no glyph for, control
/// characters included.
pub fn winansi(c: char) -> Option<u8> {
    match c {
        ' '..='~' | '\u{A0}'..='\u{FF}' => Some(c as u8),
        _ => HIGH.iter().find(|(h, _)| *h == c).map(|&(_, code)| code),
    }
}

/// How far Helvetica advances over the WinAnsi code `code`, in thousandths of the font size.
pub fn helvetica_width(code: u8) -> u16 {
    code.checked_sub(32).map_or(0, |at| WIDTHS[usize::from(at)])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lex::Lexer;

    /// A font dictionary written out, with nothing to refer to.
    fn font(text: &str) -> Option<SimpleFont> {
        let Ok(Object::Dict(d)) = Lexer::new(text.as_bytes(), 0, true).object() else {
            panic!("{text}")
        };
        SimpleFont::of(&d, |o| o.cloned().unwrap_or(Object::Null))
    }

    /// A field's font reads as readers read it: the standard 14 by name with their own widths,
    /// a Latin encoding and its `/Differences` by glyph name, a TrueType font's `/Widths`; and a
    /// composite font, a symbolic one and an embedded subset are none this sets text in.
    #[test]
    fn a_field_s_font_encodes_and_measures_its_text() {
        let times = font(
            "<</Type/Font/Subtype/Type1/BaseFont/Times-Roman\
             /Encoding<</Type/Encoding/Differences[39/quotesingle 228/adieresis/uni03B1]>>>>",
        )
        .unwrap();
        assert_eq!(times.encode("A'ä"), Some(vec![b'A', 39, 228]));
        assert_eq!(times.encode("α"), None, "no width for it in Times");
        assert_eq!(
            times.encode("’"),
            None,
            "Standard's 39, which the differences replace"
        );
        assert_eq!(times.width(b"Ab"), 722.0 + 500.0);
        let courier = font("<</Type/Font/Subtype/Type1/BaseFont/Courier>>").unwrap();
        assert_eq!(courier.width(b"il"), 1200.0);
        let dejavu = font(
            "<</Type/Font/Subtype/TrueType/BaseFont/DejaVuSans/Encoding/WinAnsiEncoding\
             /FirstChar 65/LastChar 66/Widths[684 686]\
             /FontDescriptor<</MissingWidth 500/Ascent 928/Descent -236>>>>",
        )
        .unwrap();
        assert_eq!(
            dejavu.width(&dejavu.encode("ABz€").unwrap()),
            684.0 + 686.0 + 1000.0
        );
        assert_eq!((dejavu.ascent, dejavu.descent), (Some(0.928), Some(0.236)));
        assert_eq!(font("<</Type/Font/Subtype/Type1/BaseFont/Unheard>>"), None);
        assert_eq!(
            font("<</Type/Font/Subtype/Type0/BaseFont/Helvetica>>"),
            None
        );
        assert_eq!(
            font("<</Type/Font/Subtype/Type1/BaseFont/ZapfDingbats>>"),
            None
        );
        let subset = "<</Type/Font/Subtype/TrueType/BaseFont/BAAAAA+LiberationSerif\
                      /FirstChar 0/Widths[500]/FontDescriptor<</FontFile2 9 0 R>>>>";
        assert_eq!(font(subset), None);
        let helvetica = SimpleFont::helvetica();
        assert_eq!(helvetica.encode("€"), Some(vec![0x80]));
        assert_eq!(helvetica.width(b"A"), 667.0);
    }

    #[test]
    fn latin_1_and_the_euro_are_set_in_helvetica_and_nothing_else() {
        assert_eq!(winansi('A'), Some(b'A'));
        assert_eq!(winansi('ä'), Some(0xE4));
        assert_eq!(winansi('€'), Some(0x80));
        assert_eq!(winansi('„'), Some(0x84));
        assert_eq!((winansi('\n'), winansi('→')), (None, None));
        // Every character the encoding holds has a width, the euro's included.
        let mut codes = (' '..='\u{FF}')
            .chain(HIGH.map(|(c, _)| c))
            .filter_map(winansi);
        assert!(codes.all(|code| helvetica_width(code) > 0));
        assert_eq!(helvetica_width(b'A'), 667);
        assert_eq!(helvetica_width(0x80), 556);
    }
}
