//! Text strings (ISO 32000-1 §7.9.2.2, ISO 32000-2 §7.9.2.2.1): what a title, an author or a
//! bookmark says, in UTF-16 or UTF-8 after a byte order mark, or one byte a character in
//! PDFDocEncoding.

/// `bytes` read as a text string. A language escape in UTF-16 (`1B` … `1B`) is left out, as
/// readers leave it out.
pub fn text_string(bytes: &[u8]) -> String {
    let units = |be: bool| {
        let units = bytes[2..].as_chunks::<2>().0.iter();
        let units = units.map(move |&[a, b]| match be {
            true => u16::from_be_bytes([a, b]),
            false => u16::from_le_bytes([a, b]),
        });
        let mut escaped = false;
        let units = units.filter(move |&u| {
            if u == 0x1b {
                escaped = !escaped;
                return false;
            }
            !escaped
        });
        char::decode_utf16(units)
            .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect()
    };
    match bytes {
        [0xfe, 0xff, ..] => units(true),
        [0xff, 0xfe, ..] => units(false),
        [0xef, 0xbb, 0xbf, rest @ ..] => String::from_utf8_lossy(rest).into_owned(),
        _ => bytes.iter().map(|&b| pdf_doc(b)).collect(),
    }
}

/// A byte in PDFDocEncoding (Annex D.2): Latin-1 but for the accents at 0x18–0x1F and the
/// punctuation, ligatures and letters at 0x80–0xA0.
fn pdf_doc(b: u8) -> char {
    const LOW: [char; 8] = ['˘', 'ˇ', 'ˆ', '˙', '˝', '˛', '˚', '˜'];
    const HIGH: [char; 33] = [
        '•', '†', '‡', '…', '—', '–', 'ƒ', '⁄', '‹', '›', '−', '‰', '„', '“', '”', '‘', '’', '‚',
        '™', 'ﬁ', 'ﬂ', 'Ł', 'Œ', 'Š', 'Ÿ', 'Ž', 'ı', 'ł', 'œ', 'š', 'ž', '\u{fffd}', '€',
    ];
    match b {
        0x18..=0x1f => LOW[usize::from(b - 0x18)],
        0x80..=0xa0 => HIGH[usize::from(b - 0x80)],
        _ => char::from(b),
    }
}
