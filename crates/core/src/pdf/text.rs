//! Glyphs into lines, and lines into Obsidian-style selection links and back.

use std::ops::Range;

use super::{Glyph, Rect, Selection, SelectionLink};

/// Split glyphs into runs that sit on one visual line.
///
// ponytail: pdfium does expose `PdfPageText::segments()`, but mapping a segment back to character
// indices goes through a fuzzy nearest-point lookup that silently drops the first/last character
// of a run. Grouping by glyph geometry is fewer lines, exact, and reused for both the quads and
// the link's item index. It assumes one column of horizontal text; rotated or multi-column pages
// get extra line breaks, which costs extra quads, never wrong ones.
pub(super) fn line_groups(glyphs: &[Glyph]) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut start = 0;
    for i in 1..glyphs.len() {
        let (prev, cur) = (&glyphs[i - 1].rect, &glyphs[i].rect);
        let tol = prev.height().abs().max(1.0) * 0.5;
        if (cur.top - prev.top).abs() > tol || cur.left + tol < prev.left {
            out.push(start..i);
            start = i;
        }
    }
    if !glyphs.is_empty() {
        out.push(start..glyphs.len());
    }
    out
}

/// Map a glyph index to `(line index, offset within line)`.
pub(super) fn item_offset(lines: &[Range<usize>], idx: usize) -> (usize, usize) {
    for (i, line) in lines.iter().enumerate() {
        if idx < line.end {
            return (i, idx.saturating_sub(line.start));
        }
    }
    match lines.last() {
        Some(line) => (lines.len() - 1, line.end - line.start),
        None => (0, 0),
    }
}

// ---------------------------------------------------------------------------------------------
// Selections
// ---------------------------------------------------------------------------------------------

/// One rectangle per visual line the glyph range `start..end` touches.
fn quads_between(glyphs: &[Glyph], lines: &[Range<usize>], start: usize, end: usize) -> Vec<Rect> {
    lines
        .iter()
        .filter_map(|line| {
            let lo = line.start.max(start);
            let hi = line.end.min(end);
            (lo < hi).then(|| {
                glyphs[lo..hi]
                    .iter()
                    .map(|g| g.rect)
                    .reduce(Rect::union)
                    .unwrap_or(Rect::ZERO)
            })
        })
        .collect()
}

/// Build an Obsidian-compatible link for a glyph range, plus the quads and text we keep for
/// re-anchoring.
///
/// Takes the page's glyphs rather than reading them: the caller that has a selection on screen
/// already holds them, and [`selection_quads`] has to walk the same lines to paint it again.
///
// ponytail: Obsidian's `selection=a,b,c,d` are PDF.js text-*item* indices with a character
// offset inside each item, and PDF.js segments a page differently from pdfium — there is no
// way to reproduce its numbering from here, so cross-engine fidelity is best-effort: a link we
// emit may land a few characters off when opened in Obsidian, and vice versa. That is why
// `SelectionLink` also carries `quads` and `text`: our own viewer re-anchors from those
// (quads first, text search as the fallback) and treats the numbers as a hint only.
pub fn selection_link(glyphs: &[Glyph], rel_pdf_path: &str, sel: &Selection) -> SelectionLink {
    let start = sel.start.min(glyphs.len());
    let end = sel.end.clamp(start, glyphs.len());
    let lines = line_groups(glyphs);

    let (a, b) = item_offset(&lines, start);
    let (c, d) = item_offset(&lines, end.saturating_sub(1));

    SelectionLink {
        // `d + 1`: PDF.js's end offset is exclusive.
        link: format!(
            "[[{rel_pdf_path}#page={}&selection={a},{b},{c},{}]]",
            sel.page + 1,
            d + 1
        ),
        quads: quads_between(glyphs, &lines, start, end),
        text: glyphs[start..end].iter().map(|g| g.ch).collect(),
    }
}

/// The reverse of [`selection_link`]: what `a,b,c,d` covers on this page today.
///
/// `None` when the numbers do not fit the page's lines, which is what a link written against
/// another engine's numbering, or against an edition of the document with different line breaks,
/// looks like. The caller falls back to searching the text the link quotes.
pub fn selection_quads(glyphs: &[Glyph], sel: [usize; 4]) -> Option<(Range<usize>, Vec<Rect>)> {
    let [a, b, c, d] = sel;
    let lines = line_groups(glyphs);
    let (first, last) = (lines.get(a)?, lines.get(c)?);
    let start = first.start + b;
    let end = last.start + d;
    if start > first.end || end > last.end || end <= start {
        return None;
    }
    Some((start..end, quads_between(glyphs, &lines, start, end)))
}

/// The top of the `line`-th visual line of a page, in page points.
///
/// What a selection link's first number means. A link whose numbers no longer fit the document
/// is re-anchored by searching the text it quotes, and that number is all there is to say which
/// of several identical phrases on the page was meant.
pub fn line_top(glyphs: &[Glyph], line: usize) -> Option<f32> {
    let lines = line_groups(glyphs);
    let range = lines.get(line)?.clone();
    glyphs[range].iter().map(|g| g.rect.top).reduce(f32::min)
}

/// Whether two quad lists cover the same place, to half a point.
///
/// What tells an exported highlight from the note link it came from, so a second export writes
/// nothing and the painted overlay steps aside once the annotation is in the file.
pub fn same_quads(a: &[Rect], b: &[Rect]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(x, y)| {
            let close = |p: f32, q: f32| (p - q).abs() < 0.5;
            close(x.left, y.left)
                && close(x.top, y.top)
                && close(x.right, y.right)
                && close(x.bottom, y.bottom)
        })
}
