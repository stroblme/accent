//! Task boxes: Toggle Task's cycle `[ ]` → `[/]` → `[x]` → `[ ]`, a click on a rendered box, and
//! the in-progress box `[/]`, which GFM, and so pulldown-cmark, reads as text.

use std::ops::Range;

/// `line` with its task box one step on: `[ ]` → `[/]` → `[x]` (or `[X]`) → `[ ]`. A list item
/// without a box gets `[ ]` behind its marker, and any other line, a blank one too, becomes
/// `- [ ]` and itself. The indentation and any quote markers stay in front.
pub fn toggle_task(line: &str) -> String {
    toggle_box(line, &[' ', '\t', '>'], b'/', b'x')
}

/// [`toggle_task`] for a syntax that writes a task in progress as `[{doing}]` and a done one as
/// `[{done}]`, keeping `lead` in front: org's `[-]` and `[X]`, which has no quote markers.
pub(crate) fn toggle_box(line: &str, lead: &[char], doing: u8, done: u8) -> String {
    let rest = line.trim_start_matches(lead);
    let head = &line[..line.len() - rest.len()];
    let Some(width) = marker(rest) else {
        return format!("{head}- [ ] {rest}");
    };
    let (marker, body) = rest.split_at(width);
    let next = match tick(body, doing) {
        Some(b' ') => doing,
        Some(t) if t == doing => done,
        Some(_) => b' ',
        // `-` alone at the end of a line is an empty item, with no space after it yet.
        None if marker.trim_end() == marker => return format!("{head}{marker} [ ] {body}"),
        None => return format!("{head}{marker}[ ] {body}"),
    };
    format!("{head}{marker}[{}]{}", next as char, &body[3..])
}

/// What a click on the box of the task item on line `line` (1-based) of `text` writes, as
/// Obsidian's reading view writes it: an open box is ticked `x`, and any other (`[x]`, `[X]`,
/// `[/]`) is opened. The byte of its tick and the tick it becomes, the rest of the line as it was;
/// `None` where no box follows the line's indentation, quote markers and list markers.
pub fn click_box(text: &str, line: u32) -> Option<(usize, char)> {
    clicked_box(text, line, &[' ', '\t', '>'], b'/', 'x')
}

/// [`click_box`] for a syntax whose lines start with `lead`, whose task in progress is
/// `[{doing}]` and whose done box is ticked `done`: org's.
pub(crate) fn clicked_box(
    text: &str,
    line: u32,
    lead: &[char],
    doing: u8,
    done: char,
) -> Option<(usize, char)> {
    let start = match line {
        0 => return None,
        1 => 0,
        n => text.match_indices('\n').nth(n as usize - 2)?.0 + 1,
    };
    let row = text[start..].split('\n').next().unwrap_or_default();
    // A list in a quote in an item (`- > - [ ]`) has a marker of each before its box.
    let mut at = 0;
    loop {
        let rest = &row[at..];
        at += rest.len() - rest.trim_start_matches(lead).len();
        at += marker(&row[at..])?;
        if let Some(tick) = tick(&row[at..], doing) {
            let next = if tick == b' ' { done } else { ' ' };
            return Some((start + at + 1, next));
        }
    }
}

/// The `[/]` of an in-progress task item whose list marker ends at `after` in `text`.
pub(super) fn in_progress(text: &str, after: usize) -> Option<Range<usize>> {
    let body = &text[after..];
    let start = after + body.len() - body.trim_start_matches([' ', '\t']).len();
    (tick(&text[start..], b'/') == Some(b'/')).then_some(start..start + 3)
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

/// The tick of the box `body` starts with — ` `, `doing`, `x` or `X` — where whitespace or the
/// end of the line follows it, as GFM asks of a task's box.
fn tick(body: &str, doing: u8) -> Option<u8> {
    match body.as_bytes() {
        [b'[', tick, b']', rest @ ..] if matches!(tick, b' ' | b'x' | b'X') || *tick == doing => {
            rest.first()
                .is_none_or(u8::is_ascii_whitespace)
                .then_some(*tick)
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

    /// An open box is ticked and any other opened, the rest of the note byte for byte as it was;
    /// a line without a box after its markers is left alone.
    #[test]
    fn a_click_ticks_an_open_box_and_opens_any_other() {
        let text = "# T\n- [ ] open\n- [x] done\n- [X] Done\n  * [/] doing\r\n> 1. [ ] quoted\n\
                    - - [ ] nested\n- > - [x] deep\n[ ] bare\n- item\n- [ ]word\n";
        let clicked = |line| {
            let (at, tick) = click_box(text, line)?;
            let mut out = text.to_string();
            out.replace_range(at..at + 1, &tick.to_string());
            Some(out.lines().nth(line as usize - 1)?.to_string())
        };
        let lines: Vec<_> = (0..=13).map(clicked).collect();
        assert_eq!(
            lines,
            [
                None,
                None,
                Some("- [x] open".into()),
                Some("- [ ] done".into()),
                Some("- [ ] Done".into()),
                Some("  * [ ] doing".into()),
                Some("> 1. [x] quoted".into()),
                Some("- - [x] nested".into()),
                Some("- > - [ ] deep".into()),
                None,
                None,
                None,
                None,
                None,
            ]
        );
        let (at, _) = click_box(text, 2).unwrap();
        assert_eq!(&text[at - 1..at + 2], "[ ]");
    }

    #[test]
    fn an_in_progress_box_is_a_task_marker() {
        let t = "- [/] doing\n1. [/]\n- [/]x\n\n`- [/]` text [/]\n";
        let open = spans_of(&analyze(t), Style::TaskMarker { checked: false });
        assert_eq!(open, [2..5, 15..18]);
    }
}
