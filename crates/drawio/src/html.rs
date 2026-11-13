//! A tolerant reader of HTML's tags and text, which labels are read with (`crate::label`) and a
//! formatted paste too (`accent_core::markdown::from_html`).
//!
//! It knows no elements: a `<` that opens no tag stays text, and what a tag means is the
//! caller's to decide.

/// A start or end tag.
pub struct Tag<'a> {
    /// Lowercased.
    pub name: String,
    pub close: bool,
    /// Everything between the name and the `>`.
    pub attrs: &'a str,
}

impl<'a> Tag<'a> {
    /// The tag `s` starts with and the text after it; `None` when there is none, so that a `<`
    /// that opens no tag stays text.
    pub fn read(s: &'a str) -> Option<(Tag<'a>, &'a str)> {
        let s = s.strip_prefix('<')?;
        let close = s.starts_with('/');
        let s = if close { &s[1..] } else { s };
        if !s.starts_with(|c: char| c.is_ascii_alphabetic()) {
            return None;
        }
        let name = s.find(|c: char| !c.is_ascii_alphanumeric())?;
        // The `>` that ends the tag, not one inside a quoted attribute value.
        let mut quote = None;
        let (end, _) = s.char_indices().find(|&(_, c)| {
            if quote.is_none() && (c == '"' || c == '\'') {
                quote = Some(c);
            } else if quote == Some(c) {
                quote = None;
            }
            quote.is_none() && c == '>'
        })?;
        let tag = Tag {
            name: s[..name].to_ascii_lowercase(),
            close,
            attrs: &s[name..end],
        };
        Some((tag, &s[end + 1..]))
    }
}

/// A tag's `name=value` pairs, names lowercased, values unquoted and their references decoded.
pub fn attributes(mut s: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    loop {
        s = s.trim_start_matches(|c: char| c.is_ascii_whitespace() || c == '/');
        let end = s
            .find(|c: char| c.is_ascii_whitespace() || c == '=' || c == '/')
            .unwrap_or(s.len());
        if end == 0 {
            return out;
        }
        let name = s[..end].to_ascii_lowercase();
        s = s[end..].trim_start();
        let mut value = "";
        if let Some(v) = s.strip_prefix('=') {
            let v = v.trim_start();
            (value, s) = match v.chars().next() {
                Some(q @ ('"' | '\'')) => v[1..].split_once(q).unwrap_or((&v[1..], "")),
                _ => v.split_at(v.find(|c: char| c.is_ascii_whitespace()).unwrap_or(v.len())),
            };
        }
        out.push((name, decode(value)));
    }
}

/// The first character of HTML text and the bytes it takes, a character reference decoded.
pub fn html_char(s: &str) -> Option<(char, usize)> {
    let c = s.chars().next()?;
    Some(match c {
        '&' => entity(s).unwrap_or((c, 1)),
        _ => (c, c.len_utf8()),
    })
}

/// The character reference `s` starts with, as its character and length: one of HTML's names
/// (`&middot;`) or a number. Others stay text.
fn entity(s: &str) -> Option<(char, usize)> {
    // The longest name, `CounterClockwiseContourIntegral`, has 31 letters.
    let (end, _) = s.char_indices().take(33).find(|&(_, c)| c == ';')?;
    let name = &s[1..end];
    let c = match name.strip_prefix('#') {
        Some(number) => {
            let code = match number.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => number.parse().ok()?,
            };
            char::from_u32(code)?
        }
        None => crate::entities::named(name)?,
    };
    Some((c, end + 1))
}

/// HTML text with its character references decoded.
pub fn decode(mut s: &str) -> String {
    let mut out = String::new();
    while let Some((c, len)) = html_char(s) {
        out.push(c);
        s = &s[len..];
    }
    out
}
