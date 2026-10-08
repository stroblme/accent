//! What the minimap draws, as numbers: how many rows each line of the buffer takes, where each
//! one starts, and the conversions between the view's lines, the map's rows and the pointer.
//! Pure, so the arithmetic is tested without a widget; [`super::Minimap`] fills it from the
//! buffer.
//!
//! The rows are an estimate of the page's shape, never asked of GTK's layout: a line takes as
//! many rows as the page would wrap it into at the column's width, a heading's scale included.
//! The band and every jump go through a line and a fraction of it, so where the estimate is off
//! the map is only drawn a little taller or shorter, and the view still lands on the line asked.

use std::ops::Range;

/// One line of the buffer as the map sees it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Line {
    /// Characters on the line, its terminator left out.
    pub chars: u32,
    /// A heading's text scale, which wraps it sooner; 1.0 for everything else.
    pub scale: f32,
    /// Hidden by a fold or a comparison's collapsed run: no rows at all.
    pub hidden: bool,
    /// The fewest rows the line takes: a heading's label needs more than one.
    pub min_rows: u32,
    /// The level of the heading whose label the line carries, 0 for none.
    pub heading: u8,
    /// Read from the buffer since it last changed there.
    pub measured: bool,
}

impl Default for Line {
    fn default() -> Self {
        Line {
            chars: 0,
            scale: 1.0,
            hidden: false,
            min_rows: 1,
            heading: 0,
            measured: false,
        }
    }
}

impl Line {
    /// The rows the line takes when `cols` characters fit on one, or one row each in an unwrapped
    /// view (`None`).
    pub fn rows(&self, cols: Option<u32>) -> u32 {
        if self.hidden {
            return 0;
        }
        let wrapped = cols.map_or(1, |cols| {
            (self.chars as f32 * self.scale / cols.max(1) as f32).ceil() as u32
        });
        wrapped.max(self.min_rows).max(1)
    }

    /// How many characters fit on one of its rows.
    pub fn per_row(&self, cols: Option<u32>) -> u32 {
        cols.map_or(u32::MAX, |cols| {
            ((cols as f32 / self.scale).floor() as u32).max(1)
        })
    }
}

#[derive(Default)]
pub struct Model {
    lines: Vec<Line>,
    /// `starts[i]` is the first row of line `i`, and the last entry the rows in all.
    starts: Vec<u32>,
    cols: Option<u32>,
    /// `starts` agrees with `lines`.
    summed: bool,
    /// Some line is not measured.
    stale: bool,
}

impl Model {
    /// `count` lines, none of them measured yet.
    pub fn reset(&mut self, count: usize) {
        self.lines = vec![Line::default(); count];
        self.summed = false;
        self.stale = count > 0;
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn cols(&self) -> Option<u32> {
        self.cols
    }

    /// Wrap at `cols` characters, `None` for an unwrapped view: `true` when that is a change,
    /// which moves every row.
    pub fn set_cols(&mut self, cols: Option<u32>) -> bool {
        if self.cols == cols {
            return false;
        }
        self.cols = cols;
        self.summed = false;
        true
    }

    pub fn line(&self, at: usize) -> Line {
        self.lines[at]
    }

    /// Line `at` as measured.
    pub fn set(&mut self, at: usize, line: Line) {
        self.lines[at] = Line {
            measured: true,
            ..line
        };
        self.summed = false;
    }

    /// Read the lines in `range` again before the next draw.
    pub fn unmeasure(&mut self, range: Range<usize>) {
        let end = range.end.min(self.lines.len());
        for line in &mut self.lines[range.start.min(end)..end] {
            line.measured = false;
        }
        self.stale |= range.start < end;
    }

    /// The lines waiting to be read, and from then on none: the caller measures them all.
    pub fn take_unmeasured(&mut self) -> Vec<usize> {
        if !std::mem::take(&mut self.stale) {
            return Vec::new();
        }
        (0..self.lines.len())
            .filter(|&at| !self.lines[at].measured)
            .collect()
    }

    /// The buffer's edit at line `at`, which left it `delta` lines longer (or shorter): as many
    /// lines put in (or taken out) after it, and it read again.
    pub fn splice(&mut self, at: usize, delta: isize) {
        let at = at.min(self.lines.len().saturating_sub(1));
        let after = (at + 1).min(self.lines.len());
        match delta {
            0 => {}
            d if d > 0 => {
                let new = std::iter::repeat_n(Line::default(), d as usize);
                self.lines.splice(after..after, new);
            }
            d => {
                let end = (after + d.unsigned_abs()).min(self.lines.len());
                self.lines.drain(after..end);
            }
        }
        self.unmeasure(at..after + delta.max(0) as usize);
        self.summed = false;
    }

    /// Sum the rows again after a change; every reader below wants it done.
    pub fn sum(&mut self) {
        if self.summed {
            return;
        }
        self.starts.clear();
        self.starts.reserve(self.lines.len() + 1);
        let mut row = 0;
        for line in &self.lines {
            self.starts.push(row);
            row += line.rows(self.cols);
        }
        self.starts.push(row);
        self.summed = true;
    }

    /// The rows in all.
    pub fn total(&self) -> u32 {
        debug_assert!(self.summed);
        self.starts.last().copied().unwrap_or(0)
    }

    /// Line `at`'s first row.
    pub fn start(&self, at: usize) -> u32 {
        debug_assert!(self.summed);
        self.starts[at.min(self.starts.len() - 1)]
    }

    pub fn rows(&self, at: usize) -> u32 {
        self.lines[at].rows(self.cols)
    }

    /// The row `frac` of the way down line `at`.
    pub fn row_of(&self, at: usize, frac: f64) -> f64 {
        if at >= self.lines.len() {
            return f64::from(self.total());
        }
        f64::from(self.start(at)) + frac.clamp(0.0, 1.0) * f64::from(self.rows(at))
    }

    /// The line at `row` and how far down it `row` is, never a hidden one: [`Model::row_of`]
    /// backwards. Clamped into the rows there are.
    pub fn line_at_row(&self, row: f64) -> (usize, f64) {
        debug_assert!(self.summed);
        let total = self.total();
        if total == 0 {
            return (0, 0.0);
        }
        let row = row.clamp(0.0, f64::from(total) - 1e-6);
        // The last line starting at or above the row: a hidden line shares its start with the
        // line after it, so this is that line and never the hidden one.
        let at = self.starts[..self.lines.len()].partition_point(|&s| f64::from(s) <= row) - 1;
        let rows = f64::from(self.rows(at));
        (at, (row - f64::from(self.starts[at])) / rows)
    }
}

/// How the map scrolls itself: over `total` rows on a map `height` rows tall, `usual` being the
/// rows a screen of the document holds on average.
#[derive(Clone, Copy, Debug)]
pub struct Pace {
    pub total: f64,
    pub height: f64,
    pub usual: f64,
}

impl Pace {
    /// The rows of the document per row of the map the band moves by: more than one once the map
    /// scrolls itself. The band drawn `screen` rows down the map has its top on row
    /// `screen × ratio`, which is how a drag keeps it under the pointer.
    pub fn ratio(&self) -> f64 {
        if self.total <= self.height || self.height <= self.usual {
            return 1.0;
        }
        (self.total - self.usual) / (self.height - self.usual)
    }

    /// Where the map's own scroll is, in rows, with the band's top at row `top` and `band` rows
    /// tall.
    ///
    /// The map scrolls itself only when the document outgrows it, and then in step with the band,
    /// as VS Code's minimap does: the band's top runs down the map as the view goes down the
    /// document. At the pace of the usual band rather than this one, which grows and shrinks with
    /// the lines on screen (a heading's label takes several rows), so a heading coming into view
    /// does not jolt the map; at either end the band is kept on the map.
    pub fn offset(&self, top: f64, band: f64) -> f64 {
        if self.total <= self.height {
            return 0.0;
        }
        let paced = top - top / self.ratio();
        paced
            .max(top + band - self.height)
            .min(top)
            .clamp(0.0, self.total - self.height)
    }
}

/// A run of non-blank characters of one tone, as the map draws it: the row of its line it falls
/// on, the column it starts at there and how many columns it takes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bar<T> {
    pub row: u32,
    pub col: u32,
    pub len: u32,
    pub tone: T,
}

/// The bars of a line, given its characters each with its tone: wrapped every `per_row` columns,
/// a tab taking the columns to the next multiple of `tab`, and nothing past row `rows`. A word
/// is cut at the wrap rather than carried to the next row, which a bar a pixel per character
/// wide does not show.
pub fn bars<T: Copy + PartialEq>(
    chars: impl Iterator<Item = (char, T)>,
    per_row: u32,
    rows: u32,
    tab: u32,
) -> Vec<Bar<T>> {
    let mut out = Vec::new();
    let mut col = 0;
    let mut run: Option<(u32, T)> = None;
    for (c, tone) in chars {
        if c.is_whitespace() {
            if let Some((start, tone)) = run.take() {
                split(&mut out, start..col, tone, per_row, rows);
            }
            col += if c == '\t' { tab - col % tab } else { 1 };
            continue;
        }
        match run {
            Some((_, current)) if current == tone => {}
            Some((start, current)) => {
                split(&mut out, start..col, current, per_row, rows);
                run = Some((col, tone));
            }
            None => run = Some((col, tone)),
        }
        col += 1;
    }
    if let Some((start, tone)) = run {
        split(&mut out, start..col, tone, per_row, rows);
    }
    out
}

/// The columns `cols` of a line as a bar on each row they cross.
fn split<T: Copy>(out: &mut Vec<Bar<T>>, cols: Range<u32>, tone: T, per_row: u32, rows: u32) {
    let mut from = cols.start;
    while from < cols.end {
        let row = from / per_row;
        if row >= rows {
            return;
        }
        let to = cols.end.min((row + 1).saturating_mul(per_row));
        out.push(Bar {
            row,
            col: from - row * per_row,
            len: to - from,
            tone,
        });
        from = to;
    }
}

/// What a heading's label on the map says: its text without the `#` markers, an ATX closing
/// sequence's included, or `None` where nothing is left to read (a setext heading's underline).
pub fn label(line: &str) -> Option<&str> {
    let text = line.trim().trim_start_matches('#');
    let closed = text.trim_end_matches('#');
    let text = match closed.ends_with(char::is_whitespace) {
        true => closed,
        false => text,
    }
    .trim();
    text.chars().any(char::is_alphanumeric).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_label_is_the_heading_without_its_markers() {
        assert_eq!(label("## Section 2"), Some("Section 2"));
        assert_eq!(label("# Closed #"), Some("Closed"));
        assert_eq!(label("### C#"), Some("C#"), "a # in the text stays");
        assert_eq!(label("Setext title"), Some("Setext title"));
        assert_eq!(label("========"), None, "an underline has nothing to read");
        assert_eq!(label("##"), None);
    }

    #[test]
    fn a_bar_is_a_word_wrapped_at_the_column() {
        let plain = |s: &'static str| s.chars().map(|c| (c, ()));
        let spans = |bars: Vec<Bar<()>>| -> Vec<(u32, u32, u32)> {
            bars.iter().map(|b| (b.row, b.col, b.len)).collect()
        };
        assert_eq!(
            spans(bars(plain("ab  cde\tf"), 100, 1, 4)),
            [(0, 0, 2), (0, 4, 3), (0, 8, 1)],
            "a tab goes to the next stop"
        );
        assert_eq!(
            spans(bars(plain("abcdefg hi"), 4, 9, 4)),
            [(0, 0, 4), (1, 0, 3), (2, 0, 2)]
        );
        assert_eq!(
            spans(bars(plain("abcdefg hi"), 4, 2, 4)),
            [(0, 0, 4), (1, 0, 3)],
            "no further than the line's rows"
        );
        assert_eq!(spans(bars(plain("abc"), u32::MAX, 1, 4)), [(0, 0, 3)]);
        let toned = [('a', 1), ('b', 1), ('c', 2)].into_iter();
        let toned = bars(toned, 100, 1, 4);
        assert_eq!(
            toned
                .iter()
                .map(|b| (b.col, b.len, b.tone))
                .collect::<Vec<_>>(),
            [(0, 2, 1), (2, 1, 2)],
            "a new tone starts a new bar"
        );
    }

    fn line(chars: u32) -> Line {
        Line {
            chars,
            measured: true,
            ..Line::default()
        }
    }

    /// Lines of `chars`, wrapped at 10, with the ones in `hidden` hidden.
    fn model(chars: &[u32], hidden: &[usize]) -> Model {
        let mut m = Model::default();
        m.reset(chars.len());
        m.set_cols(Some(10));
        for (at, &c) in chars.iter().enumerate() {
            let mut l = line(c);
            l.hidden = hidden.contains(&at);
            m.set(at, l);
        }
        m.sum();
        m
    }

    #[test]
    fn a_line_takes_the_rows_the_page_wraps_it_into() {
        assert_eq!(line(0).rows(Some(10)), 1, "an empty line still takes a row");
        assert_eq!(line(10).rows(Some(10)), 1);
        assert_eq!(line(11).rows(Some(10)), 2);
        assert_eq!(line(500).rows(None), 1, "an unwrapped view never wraps");
        let heading = Line {
            scale: 1.6,
            ..line(10)
        };
        assert_eq!(heading.rows(Some(10)), 2, "a scaled heading wraps sooner");
        assert_eq!(heading.per_row(Some(10)), 6);
        let labelled = Line {
            min_rows: 4,
            ..line(3)
        };
        assert_eq!(labelled.rows(Some(10)), 4, "a label's room");
        let hidden = Line {
            hidden: true,
            ..labelled
        };
        assert_eq!(hidden.rows(Some(10)), 0);
    }

    #[test]
    fn rows_and_lines_convert_both_ways_past_hidden_lines() {
        let m = model(&[5, 25, 0, 7, 3], &[2, 3]);
        assert_eq!(m.total(), 1 + 3 + 1);
        for (at, frac) in [(0, 0.0), (1, 0.0), (1, 0.5), (4, 0.0)] {
            let row = m.row_of(at, frac);
            let (back, f) = m.line_at_row(row);
            assert_eq!(back, at, "row {row}");
            assert!((f - frac).abs() < 1e-9, "line {at}: {f} for {frac}");
        }
        assert_eq!(
            m.line_at_row(4.0).0,
            4,
            "the row after line 1 is line 4's, not a hidden line's"
        );
        assert_eq!(m.line_at_row(-3.0), (0, 0.0), "clamped above");
        assert_eq!(m.line_at_row(99.0).0, 4, "and below");
    }

    #[test]
    fn nothing_to_draw_converts_to_the_first_line() {
        let mut m = Model::default();
        m.sum();
        assert_eq!(m.total(), 0);
        assert_eq!(m.line_at_row(3.0), (0, 0.0));
        let all_hidden = model(&[4, 4], &[0, 1]);
        assert_eq!(all_hidden.line_at_row(0.0), (0, 0.0));
    }

    #[test]
    fn a_splice_follows_the_buffer_edit() {
        let mut m = model(&[1, 2, 3], &[]);
        m.splice(1, 2);
        assert_eq!(m.len(), 5);
        assert_eq!(
            m.take_unmeasured(),
            vec![1, 2, 3],
            "the edited line and the new ones"
        );
        assert_eq!(m.line(4).chars, 3, "the lines after it move down");
        m.splice(0, -3);
        assert_eq!(m.len(), 2);
        assert_eq!(m.line(1).chars, 3);
        assert_eq!(m.take_unmeasured(), vec![0]);
        assert!(m.take_unmeasured().is_empty(), "taken once");
    }

    /// The map's scroll is a number in every case a document can be in: no lines, fewer rows than
    /// the map, a band as tall as the document or taller than the map.
    #[test]
    fn the_offset_is_always_finite() {
        for (top, band, usual, total, height) in [
            (0.0, 0.0, 0.0, 0.0, 0.0),
            (0.0, 10.0, 10.0, 5.0, 400.0),
            (0.0, 50.0, 50.0, 50.0, 10.0),
            (3.0, 0.0, 0.0, 1000.0, 0.0),
            (2000.0, 40.0, 40.0, 1000.0, 400.0),
        ] {
            let pace = Pace {
                total,
                height,
                usual,
            };
            let at = pace.offset(top, band);
            assert!(at.is_finite(), "{top} {band} {total} {height}");
            assert!((0.0..=total.max(0.0)).contains(&at));
            assert!(pace.ratio().is_finite());
        }
        let pace = Pace {
            total: 1000.0,
            height: 400.0,
            usual: 40.0,
        };
        assert_eq!(pace.offset(960.0, 40.0), 600.0, "the end at the end");
        assert_eq!(pace.offset(950.0, 50.0), 600.0, "with a taller band too");
        let fits = Pace {
            total: 300.0,
            ..pace
        };
        assert_eq!(fits.offset(10.0, 40.0), 0.0, "fits: no scroll");
    }

    /// A band drawn some rows down the map, its top put where [`Pace::ratio`] says, is drawn
    /// exactly there again, scrolled map or not: a drag keeps it under the pointer. And however
    /// tall the band is, away from the ends: a heading coming onto the screen does not move it.
    #[test]
    fn a_drag_keeps_the_band_under_the_pointer() {
        for total in [1000.0, 300.0] {
            let pace = Pace {
                total,
                height: 400.0,
                usual: 40.0,
            };
            for screen in [0.0, 50.0, 200.0, 300.0] {
                let top = (screen * pace.ratio()).min(total - 40.0);
                for band in [30.0, 40.0, 46.0] {
                    let drawn = top - pace.offset(top, band);
                    assert!((drawn - screen).abs() < 1e-9 || top == total - 40.0);
                }
            }
        }
    }
}
