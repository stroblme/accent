//! Helvetica as an appearance stream names it without embedding it, being one of the fonts every
//! reader has (ISO 32000-1 §9.6.2.2): the WinAnsi encoding text is set in (Annex D.2), which is
//! Windows-1252 — Latin-1, the euro and typographic quotes and dashes — and its widths.

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
