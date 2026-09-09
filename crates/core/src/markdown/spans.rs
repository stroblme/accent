//! Where a construct's delimiters really are, read back from the source rather than assumed:
//! pulldown-cmark's event ranges do not always start at their own markers.

use super::{Span, Style, sp};
use std::ops::Range;

/// Style the content, dim the delimiters (`**bold**` -> Strong on `bold`, Marker on each `**`).
pub(super) fn delimited(
    out: &mut Vec<Span>,
    text: &str,
    r: Range<usize>,
    style: Style,
    chars: &[u8],
    n: usize,
) {
    let b = text.as_bytes();
    let fits = r.len() > n * 2
        && b[r.start..r.start + n].iter().all(|c| chars.contains(c))
        && b[r.end - n..r.end].iter().all(|c| chars.contains(c));
    if fits {
        out.push(sp(r.start + n..r.end - n, style));
        out.push(sp(r.start..r.start + n, Style::Marker));
        out.push(sp(r.end - n..r.end, Style::Marker));
    } else {
        out.push(sp(r, style));
    }
}

/// Style the whole range (delimiters included) and dim the delimiters on top.
///
/// The delimiters are read back rather than assumed, like [`delimited`] does: an event range
/// need not start where its opening delimiter does — pulldown-cmark reports the math in
/// `[[$|é$]]` as starting inside the alias — and counting `n` bytes in from an end that is not
/// a delimiter can cut a character in half.
pub(super) fn wrapped(
    out: &mut Vec<Span>,
    text: &str,
    r: Range<usize>,
    style: Style,
    chars: &[u8],
    n: usize,
) {
    let b = text.as_bytes();
    out.push(sp(r.clone(), style));
    if n > 0
        && r.len() >= n * 2
        && b[r.start..r.start + n].iter().all(|c| chars.contains(c))
        && b[r.end - n..r.end].iter().all(|c| chars.contains(c))
    {
        out.push(sp(r.start..r.start + n, Style::Marker));
        out.push(sp(r.end - n..r.end, Style::Marker));
    }
}

/// `[[` (or `![[`) and the `]]` that closes it, read back from the source: an event range does
/// not always start where its own brackets do, and taking `open` bytes on faith can cut a
/// character in half.
pub(super) fn wiki_markers(out: &mut Vec<Span>, text: &str, r: &Range<usize>, open: &[u8]) {
    let seg = &text.as_bytes()[r.clone()];
    if seg.len() > open.len() + 2 && seg.starts_with(open) && seg.ends_with(b"]]") {
        out.push(sp(r.start..r.start + open.len(), Style::Marker));
        out.push(sp(r.end - 2..r.end, Style::Marker));
    }
}

pub(super) fn run(text: &str, r: &Range<usize>, chars: &[u8]) -> usize {
    text.as_bytes()[r.clone()]
        .iter()
        .take_while(|c| chars.contains(c))
        .count()
}

pub(super) fn trim_eol(text: &str, r: &Range<usize>) -> Range<usize> {
    let b = text.as_bytes();
    let mut end = r.end;
    while end > r.start && matches!(b[end - 1], b'\n' | b'\r') {
        end -= 1;
    }
    r.start..end
}

/// `#`, `##`… plus the spaces after it (ATX only; setext headings have no prefix).
pub(super) fn heading_marker(text: &str, r: &Range<usize>) -> Option<Range<usize>> {
    let b = text.as_bytes();
    if b.get(r.start) != Some(&b'#') {
        return None;
    }
    let mut i = r.start;
    while i < r.end && b[i] == b'#' {
        i += 1;
    }
    while i < r.end && matches!(b[i], b' ' | b'\t') {
        i += 1;
    }
    Some(r.start..i)
}

/// The closing `---` (or `...`) line of a frontmatter block, when the block's range reaches it.
///
/// The range's last three bytes are not it: the parser can end a block short of its delimiter
/// (an indented `---` inside a list item), and taking them anyway can cut a character in half.
/// Reading the delimiter back is also the only thing that keeps the marker off the trailing
/// newline the range carries.
pub(super) fn meta_close(text: &str, r: &Range<usize>) -> Option<Range<usize>> {
    let end = trim_eol(text, r).end;
    let start = text[r.start..end].rfind('\n').map(|i| r.start + i + 1)?;
    let line = &text[start..end];
    let closes =
        !line.is_empty() && (line.bytes().all(|c| c == b'-') || line.bytes().all(|c| c == b'.'));
    closes.then_some(start..end)
}

/// Opening and closing fence lengths of a fenced code block.
pub(super) fn fence(text: &str, r: &Range<usize>) -> (usize, usize) {
    let b = text.as_bytes();
    let c = b[r.start];
    if c != b'`' && c != b'~' {
        return (0, 0);
    }
    let mut i = r.start;
    while i < r.end && b[i] == c {
        i += 1;
    }
    let mut j = r.end;
    while j > i && b[j - 1] == c {
        j -= 1;
    }
    (i - r.start, r.end - j)
}

pub(super) fn list_marker(text: &str, r: &Range<usize>) -> Option<Range<usize>> {
    let b = text.as_bytes();
    let mut i = r.start;
    while i < r.end && matches!(b[i], b' ' | b'\t') {
        i += 1;
    }
    let start = i;
    if i < r.end && matches!(b[i], b'-' | b'*' | b'+') {
        return Some(start..i + 1);
    }
    while i < r.end && b[i].is_ascii_digit() {
        i += 1;
    }
    if i > start && i < r.end && matches!(b[i], b'.' | b')') {
        return Some(start..i + 1);
    }
    None
}

pub(super) fn quote_markers(text: &str, r: &Range<usize>, out: &mut Vec<Span>) {
    let b = text.as_bytes();
    let mut i = r.start;
    while i < r.end {
        let mut j = i;
        while j < r.end && matches!(b[j], b' ' | b'\t') {
            j += 1;
        }
        if j < r.end && b[j] == b'>' {
            out.push(sp(j..j + 1, Style::Marker));
        }
        match text[i..r.end].find('\n') {
            Some(k) => i += k + 1,
            None => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markdown::analyze;
    use crate::markdown::testing::{at, spans_of};

    #[test]
    fn spans_and_markers_at_exact_offsets() {
        let t = "# Head\n\nSome **bold** and *em* and `code` and ~~out~~.\n";
        let a = analyze(t);

        let heading = a
            .spans
            .iter()
            .find(|s| s.style == Style::Heading(1))
            .unwrap();
        assert_eq!(at(t, heading), "# Head");
        let strong = a.spans.iter().find(|s| s.style == Style::Strong).unwrap();
        assert_eq!(at(t, strong), "bold");
        let em = a.spans.iter().find(|s| s.style == Style::Emphasis).unwrap();
        assert_eq!(at(t, em), "em");
        let code = a
            .spans
            .iter()
            .find(|s| s.style == Style::CodeInline)
            .unwrap();
        assert_eq!(at(t, code), "`code`");
        let strike = a
            .spans
            .iter()
            .find(|s| s.style == Style::Strikethrough)
            .unwrap();
        assert_eq!(at(t, strike), "out");

        let markers: Vec<&str> = a
            .spans
            .iter()
            .filter(|s| s.style == Style::Marker)
            .map(|s| at(t, s))
            .collect();
        assert_eq!(markers, ["# ", "**", "**", "*", "*", "`", "`", "~~", "~~"]);
    }

    #[test]
    fn underscore_emphasis_marker_lengths() {
        let t = "__strong__ and _em_";
        let a = analyze(t);
        let markers: Vec<&str> = a
            .spans
            .iter()
            .filter(|s| s.style == Style::Marker)
            .map(|s| at(t, s))
            .collect();
        assert_eq!(markers, ["__", "__", "_", "_"]);
        assert_eq!(
            at(
                t,
                a.spans.iter().find(|s| s.style == Style::Strong).unwrap()
            ),
            "strong"
        );
    }

    #[test]
    fn block_spans() {
        let t =
            "> quoted\n\n- one\n- [x] done\n\n```rust\nfn x() {}\n```\n\n$$e$$\n\n<div>h</div>\n";
        let a = analyze(t);
        assert_eq!(&t[spans_of(&a, Style::Quote)[0].clone()], "> quoted");
        let list: Vec<&str> = spans_of(&a, Style::ListMarker)
            .iter()
            .map(|r| &t[r.clone()])
            .collect();
        assert_eq!(list, ["-", "-"]);
        let task = spans_of(&a, Style::TaskMarker { checked: true });
        assert_eq!(&t[task[0].clone()], "[x]");
        let code = spans_of(&a, Style::CodeBlock);
        assert_eq!(&t[code[0].clone()], "```rust\nfn x() {}\n```");
        let math = spans_of(&a, Style::Math);
        assert_eq!(&t[math[0].clone()], "$$e$$");
        let html = spans_of(&a, Style::Html);
        assert_eq!(&t[html[0].clone()], "<div>h</div>");
        // the `>` and both fences are dimmed
        let markers: Vec<&str> = spans_of(&a, Style::Marker)
            .iter()
            .map(|r| &t[r.clone()])
            .collect();
        assert!(markers.contains(&">"), "{markers:?}");
        assert!(markers.contains(&"```"), "{markers:?}");
    }

    #[test]
    fn frontmatter_and_wikilink_markers() {
        let t = "---\ntags: [a]\n---\n\n[[Note]] ![[i.png]]\n";
        let a = analyze(t);
        let fm = spans_of(&a, Style::Frontmatter);
        assert_eq!(&t[fm[0].clone()], "---\ntags: [a]\n---");
        let markers: Vec<&str> = spans_of(&a, Style::Marker)
            .iter()
            .map(|r| &t[r.clone()])
            .collect();
        assert!(markers.contains(&"---"), "{markers:?}");
        assert!(markers.contains(&"[["), "{markers:?}");
        assert!(markers.contains(&"]]"), "{markers:?}");
        assert!(markers.contains(&"![["), "{markers:?}");
        assert_eq!(&t[spans_of(&a, Style::WikiLink)[0].clone()], "[[Note]]");
        assert_eq!(&t[spans_of(&a, Style::Image)[0].clone()], "![[i.png]]");
    }
}
