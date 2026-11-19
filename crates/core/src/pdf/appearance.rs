//! A text field's appearance stream as pdf.js writes one on saving a fill (its
//! `TextWidgetAnnotation._getAppearance`): the widget's background and border, then the value in
//! its `/DA`'s font, size and colour inside `/Tx BMC … EMC`, padded 2 pt across and 1 pt down, a
//! size of 0 fitted to the box, one line or several wrapped at spaces, or a comb's cells, and
//! aligned as `/Q` says. A list box's as its `ChoiceWidgetAnnotation._getAppearance`: the options
//! that fit from the first that keeps the picks in view, the picks' rows filled. A form's readers
//! draw them as accent does.

use accent_pdf::{Content, SimpleFont, Widget};

/// The room round the text, in points: across, and down.
const PAD_ACROSS: f32 = 2.0;
const PAD_DOWN: f32 = 1.0;
/// How far apart lines are, and how far below the baseline a line reaches, in font sizes, where
/// the font says nothing of its own (pdf.js's `LINE_FACTOR`, `LINE_DESCENT_FACTOR`).
const LINE: f32 = 1.35;
const LINE_DESCENT: f32 = 0.35;

/// The text a field shows, set in a font: the font the resources name `name`, its size from
/// `/DA` (0 to fit), and its colour.
pub(super) struct Setting<'f> {
    pub name: &'f [u8],
    pub font: &'f SimpleFont,
    pub size: f32,
    pub rgb: [u8; 3],
}

/// The widget's box as the appearance draws in it: its width and height, swapped when `/MK /R`
/// turns it a quarter, and its fill and outline.
fn frame(w: &Widget) -> (f32, f32) {
    let [x0, y0, x1, y1] = w.rect;
    let (width, height) = (x1 - x0, y1 - y0);
    match w.rotation {
        90 | 270 => (height, width),
        _ => (width, height),
    }
}

/// The matrix `/MK /R` turns the appearance by, the widget's own box `width` by `height`: `None`
/// for none (pdf.js's `getRotationMatrix`).
pub(super) fn turned(w: &Widget) -> Option<[f32; 6]> {
    let [x0, y0, x1, y1] = w.rect;
    let (width, height) = (x1 - x0, y1 - y0);
    match w.rotation {
        90 => Some([0.0, 1.0, -1.0, 0.0, width, 0.0]),
        180 => Some([-1.0, 0.0, 0.0, -1.0, width, height]),
        270 => Some([0.0, -1.0, 1.0, 0.0, 0.0, height]),
        _ => None,
    }
}

/// The appearance's bounding box, `[0 0 width height]` of the box as it draws.
pub(super) fn bbox(w: &Widget) -> [f32; 4] {
    let (width, height) = frame(w);
    [0.0, 0.0, width, height]
}

/// The content of `w`'s appearance showing `value`, multi-line, a comb of `comb` cells or one line
/// as `multiline` and `comb` say; `None` where the font has no code for a character of it.
pub(super) fn text(
    w: &Widget,
    value: &str,
    set: &Setting,
    multiline: bool,
    comb: Option<usize>,
) -> Option<Vec<u8>> {
    let (width, height) = frame(w);
    let mut c = Content::default();
    c.marked(b"Tx").save();
    paint_box(&mut c, w, width, height);
    let value = value.trim_end();
    let lines: Vec<&str> = match multiline {
        true => value
            .split("\r\n")
            .flat_map(|l| l.split(['\r', '\n']))
            .collect(),
        false => vec![value],
    };
    let too_small = set.size == 0.0 && (width <= 2.0 * PAD_ACROSS || height <= 2.0 * PAD_DOWN);
    if value.is_empty() || too_small {
        c.restore().end_marked();
        return Some(c.finish());
    }
    let encoded: Vec<Vec<u8>> = (lines.iter())
        .map(|l| set.font.encode(&l.replace(['\r', '\n'], "")))
        .collect::<Option<_>>()?;
    let inner = (width - 2.0 * PAD_ACROSS, height - 2.0 * PAD_DOWN);
    let (size, line_height) = fit(set, &encoded, inner, multiline);
    let (ascent, descent) = match (set.font.ascent, set.font.descent) {
        (Some(a), Some(d)) => (a, d),
        _ => (LINE - LINE_DESCENT, LINE_DESCENT),
    };
    let v_shift = (height - (ascent + descent) * size) / 2.0 + descent * size;
    c.begin_text();
    c.font(set.name, size)
        .fill_rgb(set.rgb.map(|v| f32::from(v) / 255.0));
    let measure = |codes: &[u8]| set.font.width(codes) * size / 1000.0;
    match (multiline, comb) {
        (true, _) => {
            let below = set.font.descent.map_or(0.0, |d| d * size);
            let below = below.max(LINE_DESCENT / LINE * line_height);
            let pad_down = ((height - size) / 2.0).floor().min(PAD_DOWN);
            c.text_matrix([1.0, 0.0, 0.0, 1.0, 0.0, height]);
            let mut shifted = 0.0;
            let mut first = true;
            for line in &encoded {
                for chunk in split(set.font, line, size, width - 2.0 * PAD_ACROSS) {
                    let down = match first {
                        true => -pad_down - (line_height - below),
                        false => -line_height,
                    };
                    first = false;
                    let shift = aligned(w.align, width, measure(chunk));
                    c.next_line((shift - shifted, down)).show(chunk);
                    shifted = shift;
                }
            }
        }
        (false, Some(cells)) => {
            let cell = width / cells as f32;
            let glyphs: Vec<(u8, f32)> = (encoded[0].iter()).map(|&g| (g, measure(&[g]))).collect();
            let used = cell * glyphs.len() as f32;
            let across = match w.align {
                1 => ((width - used) / (2.0 * cell)).floor() * cell,
                2 => width - used,
                _ => 0.0,
            };
            c.text_matrix([1.0, 0.0, 0.0, 1.0, across, v_shift]);
            let mut before = None;
            for (glyph, wide) in glyphs {
                let shift = match before {
                    None => (cell - wide) / 2.0,
                    Some(before) => cell + (before - wide) / 2.0,
                };
                c.next_line((shift, 0.0)).show(&[glyph]);
                before = Some(wide);
            }
        }
        (false, None) => {
            let shift = aligned(w.align, width, measure(&encoded[0]));
            c.text_matrix([1.0, 0.0, 0.0, 1.0, shift, v_shift]);
            c.show(&encoded[0]);
        }
    }
    c.end_text().restore().end_marked();
    Some(c.finish())
}

/// The widget's fill and its outline, drawn first (pdf.js's `getBorderAndBackgroundAppearances`).
fn paint_box(c: &mut Content, w: &Widget, width: f32, height: f32) {
    if let Some(fill) = w.background {
        c.fill_rgb(fill).rect((0.0, 0.0), width, height).fill();
    }
    if let Some(line) = w.border.filter(|_| w.border_width > 0.0) {
        c.width(w.border_width).stroke_rgb(line);
        c.rect((0.0, 0.0), width, height).stroke();
    }
}

/// Where a line `wide` sets from across a box `width` wide under `/Q` (pdf.js's `_renderText`).
fn aligned(align: u8, width: f32, wide: f32) -> f32 {
    match align {
        1 => (width - wide) / 2.0,
        2 => width - wide - PAD_ACROSS,
        _ => PAD_ACROSS,
    }
}

/// The content of list box `w`'s appearance with the options `picked` picked, by export value;
/// `None` where the font has no code for a character of an option shown.
pub(super) fn list(w: &Widget, picked: &[String], set: &Setting) -> Option<Vec<u8>> {
    let (width, height) = frame(w);
    let count = w.options.len().max(1);
    let picks: Vec<usize> = (w.options.iter().enumerate())
        .filter(|(_, (export, _))| picked.contains(export))
        .map(|(i, _)| i)
        .collect();
    let shown: Vec<Option<Vec<u8>>> = (w.options.iter())
        .map(|(_, display)| set.font.encode(display))
        .collect();
    let size = match set.size > 0.0 {
        true => set.size,
        false => {
            let row = (height - PAD_DOWN) / count as f32;
            let widest = (shown.iter().flatten())
                .max_by(|a, b| set.font.width(a).total_cmp(&set.font.width(b)))
                .cloned()
                .unwrap_or_default();
            fit(set, &[widest], (width - 2.0 * PAD_ACROSS, row), false).0
        }
    };
    let row = size * LINE;
    let v_pad = (row - size) / 2.0;
    let visible = (height / row).floor() as usize;
    let first = match (picks.iter().min(), picks.iter().max()) {
        (Some(&low), Some(&high)) => (high + 1).saturating_sub(visible).min(low),
        _ => 0,
    };
    let end = (first + visible + 1).min(w.options.len());
    let mut c = Content::default();
    c.marked(b"Tx").save();
    c.rect((1.0, 1.0), width, height).clip();
    if !picks.is_empty() {
        c.fill_rgb([0.600006, 0.756866, 0.854904]);
        for &i in picks.iter().filter(|i| (first..end).contains(i)) {
            let y = height - (i - first + 1) as f32 * row;
            c.rect((1.0, y), width, row).fill();
        }
    }
    c.begin_text();
    c.font(set.name, size)
        .fill_rgb(set.rgb.map(|v| f32::from(v) / 255.0));
    c.text_matrix([1.0, 0.0, 0.0, 1.0, 0.0, height]);
    for (i, codes) in shown.iter().enumerate().take(end).skip(first) {
        let down = -row + if i == first { v_pad } else { 0.0 };
        let across = if i == first { PAD_ACROSS } else { 0.0 };
        c.next_line((across, down)).show(codes.as_ref()?);
    }
    c.end_text().restore().end_marked();
    Some(c.finish())
}

/// The size the text sets at, and how far apart its lines are: `/DA`'s size, or for 0 the
/// largest that fits the box, padding taken off already, a line across, or the lines wrapped
/// down (pdf.js's `_computeFontSize`).
fn fit(
    set: &Setting,
    lines: &[Vec<u8>],
    (width, height): (f32, f32),
    multiline: bool,
) -> (f32, f32) {
    let two_places = |v: f32| (v * 100.0).floor() / 100.0;
    let size = set.size;
    let mut count =
        ((height / ((if size > 0.0 { size } else { 12.0 }) * LINE)).round() as usize).max(1);
    if size > 0.0 {
        return (size, height / count as f32);
    }
    if !multiline {
        let wide = set.font.width(&lines[0]) / 1000.0;
        let across = if wide > 0.0 { width / wide } else { f32::MAX };
        return (two_places((height / LINE).min(across)), height);
    }
    let too_big = |size: f32| {
        let mut down = 0.0;
        for line in lines {
            down += split(set.font, line, size, width).len() as f32 * size;
            if down > height {
                return true;
            }
        }
        false
    };
    count = count.max(lines.len());
    let size_of = |n: usize| two_places(height / n as f32 / LINE);
    if height > 0.0 && too_big(size_of(count)) {
        let (mut low, mut high) = (count, 2 * count);
        while too_big(size_of(high)) {
            low = high;
            high *= 2;
        }
        while high - low > 1 {
            let mid = (low + high) / 2;
            match too_big(size_of(mid)) {
                true => low = mid,
                false => high = mid,
            }
        }
        count = high;
    }
    (size_of(count), height / count as f32)
}

/// `line` wrapped to `width` at `size`, at its last space before the edge or, with none, at the
/// edge (pdf.js's `_splitLine`).
fn split<'l>(font: &SimpleFont, line: &'l [u8], size: f32, width: f32) -> Vec<&'l [u8]> {
    if line.len() <= 1 {
        return vec![line];
    }
    let space = font.encode(" ").and_then(|s| s.first().copied());
    let wide = |g: u8| font.width(&[g]) * size / 1000.0;
    let (mut chunks, mut start, mut across) = (Vec::new(), 0, 0.0);
    let mut last_space: Option<usize> = None;
    let mut i = 0;
    while i < line.len() {
        let g = line[i];
        let w = wide(g);
        if Some(g) == space {
            if across + w > width {
                chunks.push(&line[start..i]);
                start = i;
                across = w;
                last_space = None;
            } else {
                across += w;
                last_space = Some(i);
            }
        } else if across + w > width {
            match last_space.take() {
                Some(at) => {
                    chunks.push(&line[start..=at]);
                    start = at + 1;
                    i = at + 1;
                    across = 0.0;
                    continue;
                }
                None => {
                    chunks.push(&line[start..i]);
                    start = i;
                    across = w;
                }
            }
        } else {
            across += w;
        }
        i += 1;
    }
    if start < line.len() {
        chunks.push(&line[start..]);
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_wraps_at_its_last_space_before_the_edge() {
        let font = SimpleFont::helvetica();
        let line = font.encode("aaa bbb ccc").unwrap();
        // "aaa " is 3 × 556 + 278 = 1946 thousandths: at 10 pt, 19.46 pt.
        let chunks = split(&font, &line, 10.0, 30.0);
        assert_eq!(chunks, [&b"aaa "[..], b"bbb ", b"ccc"]);
        let word = font.encode("aaaaaaaa").unwrap();
        assert_eq!(
            split(&font, &word, 10.0, 20.0),
            [&b"aaa"[..], b"aaa", b"aa"]
        );
    }
}
