// Derived from formulary 0.3.0 `src/mathvariant.rs` (MIT OR Apache-2.0, Copyright (c) 2026
// Jeffrey T. Peckham (Ophymx)), modified for accent; see crates/math/NOTICE.
//! The italic MathML Core gives a single-character `<mi>`. pulldown-latex maps every other
//! style to Mathematical Alphanumeric Symbols itself and writes no `mathvariant` but `normal`.

/// `c` in Mathematical Italic, or `c` itself where the block has no counterpart. Italic `h` is
/// the hole Letterlike Symbols' ℎ fills; dotless i and j, ∂ and ∇ sit outside the letter runs.
pub(crate) fn to_math_italic(c: char) -> char {
    let mapped = match c {
        'h' => 0x210E,
        '\u{0131}' => 0x1D6A4,
        '\u{0237}' => 0x1D6A5,
        '\u{2202}' => 0x1D715,
        '\u{2207}' => 0x1D6FB,
        'A'..='Z' => 0x1D434 + (c as u32 - 'A' as u32),
        'a'..='z' => 0x1D44E + (c as u32 - 'a' as u32),
        // Greek capitals; the reserved U+03A2 lines up with the block's THETA SYMBOL, so a
        // straight offset is right.
        '\u{0391}'..='\u{03A9}' => 0x1D6E2 + (c as u32 - 0x0391),
        '\u{03B1}'..='\u{03C9}' => 0x1D6FC + (c as u32 - 0x03B1),
        _ => return c,
    };
    char::from_u32(mapped).unwrap_or(c)
}
