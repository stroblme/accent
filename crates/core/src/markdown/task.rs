//! Task boxes: Toggle Task's cycle `[ ]` → `[/]` → `[x]` → `[ ]`, and the in-progress box `[/]`,
//! which GFM, and so pulldown-cmark, reads as text.

use std::ops::Range;

/// `line` with its task box one step on: `[ ]` → `[/]` → `[x]` (or `[X]`) → `[ ]`. A list item
/// without a box gets `[ ]` behind its marker, and any other line, a blank one too, becomes
/// `- [ ]` and itself. The indentation and any quote markers stay in front.
pub fn toggle_task(line: &str) -> String {
    let rest = line.trim_start_matches([' ', '\t', '>']);
    let head = &line[..line.len() - rest.len()];
    let Some(width) = marker(rest) else {
        return format!("{head}- [ ] {rest}");
    };
    let (marker, body) = rest.split_at(width);
    let next = match tick(body) {
        Some(b' ') => '/',
        Some(b'/') => 'x',
        Some(_) => ' ',
        // `-` alone at the end of a line is an empty item, with no space after it yet.
        None if marker.trim_end() == marker => return format!("{head}{marker} [ ] {body}"),
        None => return format!("{head}{marker}[ ] {body}"),
    };
    format!("{head}{marker}[{next}]{}", &body[3..])
}

/// The `[/]` of an in-progress task item whose list marker ends at `after` in `text`.
pub(super) fn in_progress(text: &str, after: usize) -> Option<Range<usize>> {
    let body = &text[after..];
    let start = after + body.len() - body.trim_start_matches([' ', '\t']).len();
    (tick(&text[start..]) == Some(b'/')).then_some(start..start + 3)
}

/// The width of the list marker `rest` starts with, the whitespace after it included: `- `,
/// `* `, `+ `, `1. ` or `1) `, at the end of the line with none. `-word` has none.
fn marker(rest: &str) -> Option<usize> {
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    let head = match rest.as_bytes().get(digits) {
        Some(b'-' | b'*' | b'+') if digits == 0 => 1,
        Some(b'.' | b')') if (1..=9).contains(&digits) => digits + 1,
        _ => return None,
    };
    let after = &rest[head..];
    let space = after.len() - after.trim_start_matches([' ', '\t']).len();
    (space > 0 || after.is_empty()).then_some(head + space)
}

/// The tick of the box `body` starts with — ` `, `/`, `x` or `X` — where whitespace or the end
/// of the line follows it, as GFM asks of a task's box.
fn tick(body: &str) -> Option<u8> {
    match body.as_bytes() {
        [b'[', tick @ (b' ' | b'/' | b'x' | b'X'), b']', rest @ ..]
            if rest.first().is_none_or(u8::is_ascii_whitespace) =>
        {
            Some(*tick)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Style, analyze, testing::spans_of};
    use super::*;

    #[test]
    fn a_box_cycles_and_anything_else_becomes_a_task() {
        for (line, toggled) in [
            ("- [ ] open", "- [/] open"),
            ("- [/] doing", "- [x] doing"),
            ("- [x] done", "- [ ] done"),
            ("- [X] done", "- [ ] done"),
            ("  * [ ]", "  * [/]"),
            ("\t+ item", "\t+ [ ] item"),
            ("12. item", "12. [ ] item"),
            ("1) [x] item", "1) [ ] item"),
            ("> - [/] quoted", "> - [x] quoted"),
            ("-", "- [ ] "),
            ("plain", "- [ ] plain"),
            ("  -word", "  - [ ] -word"),
            ("- [ ]word", "- [ ] [ ]word"),
            ("", "- [ ] "),
        ] {
            assert_eq!(toggle_task(line), toggled, "{line:?}");
        }
    }

    #[test]
    fn an_in_progress_box_is_a_task_marker() {
        let t = "- [/] doing\n1. [/]\n- [/]x\n\n`- [/]` text [/]\n";
        let open = spans_of(&analyze(t), Style::TaskMarker { checked: false });
        assert_eq!(open, [2..5, 15..18]);
    }
}
