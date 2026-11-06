//! What a notebook keeps of a kernel's output, so that a flood neither fills the memory of what
//! shows it nor makes a file too large to read again: a stream's carriage returns played out, a
//! line past [`LINE`] characters cut, and a stream past [`HEAD`] and [`TAIL`] bytes its first
//! part and its newest with a line between them saying what was left out. Every limit says so
//! where it cuts, in a line of the output's own text, which is what a save writes.

use std::borrow::Cow;
use std::ops::Range;

use serde_json::{Value, json};

use super::output::carriage;

/// The bytes a stream keeps from its start once it is cut.
pub const HEAD: usize = 256 << 10;
/// The newest bytes a stream keeps once it is cut, moving on as it grows.
pub const TAIL: usize = 768 << 10;
/// The characters a line holds at most; text laid out in one line much longer holds a window
/// for minutes (a 100 KB line, 2026-11-06).
pub const LINE: usize = 10_000;
/// The characters a line past [`LINE`] keeps, room left for its marker.
const KEEP: usize = LINE - 64;
/// The outputs a run keeps in its cell, VS Code's limit; its result and its error are kept past
/// it.
pub const OUTPUTS: usize = 500;
/// The most a display or a result may hold, all its types together.
pub const DATA: usize = 8 << 20;

/// What a stream says once it is cut: its first [`HEAD`] bytes, what was left out, and its
/// newest [`TAIL`] bytes; until then all of it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Stream {
    head: String,
    /// The bytes and lines left out between the head and the tail; `None` until it is cut.
    gap: Option<(u64, u64)>,
    tail: String,
    /// The characters the last line of `tail` holds.
    column: usize,
    /// The characters left out of the last line, past [`LINE`].
    long: u64,
}

impl Stream {
    /// Take `more` of the stream, as a terminal shows it: a carriage return writes its line
    /// again from its start.
    pub fn push(&mut self, more: &str) {
        if more.contains('\r') || self.tail.ends_with('\r') {
            let start = self.tail.rfind('\n').map_or(0, |at| at + 1);
            let line = self.tail.split_off(start);
            let mut played = carriage(&(line + more));
            // Its line is still to be written over.
            if more.ends_with('\r') {
                played.push('\r');
            }
            (self.column, self.long) = (0, 0);
            self.append(&played);
        } else {
            self.append(more);
        }
        self.cut();
    }

    /// The text the stream shows: what [`Notebook::stream`](super::Notebook::stream) sets.
    pub fn text(&self) -> String {
        let mut text = self.head.clone();
        if let Some((bytes, lines)) = self.gap {
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            let lines = match lines {
                1 => "1 line".to_string(),
                n => format!("{n} lines"),
            };
            text.push_str(&format!("… {} ({lines}) left out …\n", size(bytes)));
        }
        text.push_str(&self.tail);
        if self.long > 0 {
            text.push_str(&cut_marker(self.long));
        }
        text
    }

    fn append(&mut self, text: &str) {
        for piece in text.split_inclusive('\n') {
            let (body, newline) = match piece.strip_suffix('\n') {
                Some(body) => (body, true),
                None => (piece, false),
            };
            match self.long {
                0 => {
                    // One past the most a line holds, which says it is cut.
                    let room = LINE + 1 - self.column;
                    let end = body
                        .char_indices()
                        .nth(room)
                        .map_or(body.len(), |(at, _)| at);
                    self.tail.push_str(&body[..end]);
                    self.column += body[..end].chars().count();
                    if self.column > LINE {
                        let start = self.tail.rfind('\n').map_or(0, |n| n + 1);
                        let line = &self.tail[start..];
                        let cut = start + line.char_indices().nth(KEEP).map_or(0, |(at, _)| at);
                        self.long = self.tail[cut..].chars().count() as u64;
                        self.tail.truncate(cut);
                        self.column = KEEP;
                    }
                    self.long += body[end..].chars().count() as u64;
                }
                _ => self.long += body.chars().count() as u64,
            }
            if newline {
                if self.long > 0 {
                    self.tail.push_str(&cut_marker(self.long));
                }
                self.tail.push('\n');
                (self.column, self.long) = (0, 0);
            }
        }
    }

    /// Past [`HEAD`] and [`TAIL`] together the stream is cut, each part ending at a line's end,
    /// and from then on the tail keeps its newest [`TAIL`] bytes.
    fn cut(&mut self) {
        if self.gap.is_none() {
            if self.tail.len() <= HEAD + TAIL {
                return;
            }
            let at = self.tail[..HEAD]
                .rfind('\n')
                .map_or(floor(&self.tail, HEAD), |n| n + 1);
            self.head = self.tail[..at].to_string();
            self.tail.drain(..at);
            self.gap = Some((0, 0));
        }
        let Some(over) = self.tail.len().checked_sub(TAIL).filter(|&over| over > 0) else {
            return;
        };
        let at = match self.tail[over..].find('\n') {
            Some(n) => over + n + 1,
            None => floor(&self.tail, over),
        };
        let gone = &self.tail[..at];
        let lines = gone.bytes().filter(|&b| b == b'\n').count() as u64;
        if let Some((bytes, n)) = &mut self.gap {
            (*bytes, *n) = (*bytes + at as u64, *n + lines);
        }
        self.tail.drain(..at);
    }
}

/// The changes that make a stream's `old` text its `new` one, each a range of `old` and what
/// takes its place, the later first so that each applies as it comes: small for a stream grown,
/// written over at its end, or cut and moved on, where its newest lines are found again past
/// the line that changed.
pub fn diff<'a>(old: &str, new: &'a str) -> Vec<(Range<usize>, &'a str)> {
    let p = common_prefix(old, new);
    let whole = vec![(p..old.len(), &new[p..])];
    let (Some(o), Some(n)) = (old[p..].find('\n'), new[p..].find('\n')) else {
        return whole;
    };
    let (o, n) = (p + o + 1, p + n + 1);
    let Some(probe) = new[n..].split_inclusive('\n').next() else {
        return whole;
    };
    let Some(f) = old[o..].find(probe).map(|f| o + f) else {
        return whole;
    };
    let q = common_prefix(&old[f..], &new[n..]);
    vec![(f + q..old.len(), &new[n + q..]), (p..f, &new[p..n])]
}

/// The bytes `a` and `b` begin with alike, up to a character of both.
pub fn common_prefix(a: &str, b: &str) -> usize {
    let same = a.bytes().zip(b.bytes()).take_while(|(x, y)| x == y).count();
    floor(b, same)
}

/// `text` with each line past [`LINE`] characters cut as a stream's is, for a window to draw.
pub fn cut_lines(text: &str) -> Cow<'_, str> {
    let long = |line: &str| line.len() > LINE && line.chars().nth(LINE).is_some();
    if !text.split('\n').any(long) {
        return Cow::Borrowed(text);
    }
    let lines: Vec<Cow<str>> = text
        .split('\n')
        .map(|line| match long(line) {
            true => {
                let at = line.char_indices().nth(KEEP).map_or(0, |(at, _)| at);
                let rest = line[at..].chars().count() as u64;
                Cow::Owned(format!("{}{}", &line[..at], cut_marker(rest)))
            }
            false => Cow::Borrowed(line),
        })
        .collect();
    Cow::Owned(lines.join("\n"))
}

/// What ends a line cut at [`LINE`]: how many of its characters were left out.
fn cut_marker(chars: u64) -> String {
    match chars {
        1 => " … 1 character left out …".to_string(),
        n => format!(" … {n} characters left out …"),
    }
}

/// An output in the place of one a notebook does not keep, saying why: a stderr stream, as the
/// note Jupyter's server writes where it stops output.
pub fn note(why: &str) -> Value {
    json!({"output_type": "stream", "name": "stderr", "text": line(why)})
}

/// The line saying `why` an output is not kept, as a note holds it.
pub fn line(why: &str) -> String {
    format!("… {why} …\n")
}

/// Why a display or result of `bytes`, chiefly of `mime`, is not kept: past [`DATA`].
pub fn too_much_data(bytes: usize, mime: &str) -> String {
    let (bytes, limit) = (size(bytes as u64), size(DATA as u64));
    format!("an output of {bytes} ({mime}) left out, past the {limit} an output keeps")
}

/// Why a message of `bytes` was not read: past `limit`, what the kernel client reads at once.
pub fn too_large(bytes: u64, limit: usize) -> String {
    let (bytes, limit) = (size(bytes), size(limit as u64));
    format!("an output of {bytes} left out, past the {limit} Accent reads at once")
}

/// Why `n` outputs past [`OUTPUTS`] are not in a cell.
pub fn too_many(n: usize) -> String {
    let n = match n {
        1 => "1 more output".to_string(),
        n => format!("{n} more outputs"),
    };
    format!("{n} left out, past the {OUTPUTS} a cell keeps")
}

/// Why an output of `bytes` was not saved: a notebook past `limit` would not open again.
pub fn too_large_to_save(bytes: usize, limit: u64) -> String {
    let (bytes, limit) = (size(bytes as u64), size(limit));
    format!("an output of {bytes} left out, past the {limit} a notebook keeps")
}

/// The size of an output's data: every string it holds, base64 pictures included.
pub fn data_size(value: &Value) -> usize {
    match value {
        Value::String(s) => s.len(),
        Value::Array(items) => items.iter().map(data_size).sum(),
        Value::Object(map) => map.values().map(data_size).sum(),
        _ => 8,
    }
}

/// `bytes` as GNOME writes a size, in binary units as the 16 MiB a text holds is written.
fn size(bytes: u64) -> String {
    let mib = bytes as f64 / (1 << 20) as f64;
    match bytes {
        b if b >= 10 << 20 => format!("{mib:.0} MiB"),
        b if b >= 1 << 20 => format!("{mib:.1} MiB"),
        b if b >= 1 << 10 => format!("{} KiB", b >> 10),
        b => format!("{b} bytes"),
    }
}

/// The char boundary at or before `at` in `text`.
fn floor(text: &str, at: usize) -> usize {
    (0..=at.min(text.len()))
        .rev()
        .find(|&n| text.is_char_boundary(n))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stream_past_its_bounds_keeps_its_head_a_gap_line_and_its_newest_text() {
        let mut stream = Stream::default();
        let line = format!("{}\n", "x".repeat(999));
        for n in 0..2000 {
            stream.push(&line.replace('x', &(n % 10).to_string()));
        }
        let text = stream.text();
        assert!(text.len() <= HEAD + TAIL + 100, "{}", text.len());
        assert!(text.starts_with(&line.replace('x', "0")));
        assert!(text.ends_with(&line.replace('x', "9")));
        let gap = text.lines().find(|l| l.starts_with('…')).unwrap();
        // 2000 lines of 1000 bytes: 262 in the head, 786 in the tail.
        assert_eq!(gap, "… 929 KiB (952 lines) left out …");
        let head = text.find('…').unwrap();
        assert!(head <= HEAD && text[..head].ends_with('\n'));
    }

    #[test]
    fn a_diff_is_what_changed_and_makes_the_new_text() {
        let apply = |old: &str, new: &str| {
            let mut text = old.to_string();
            let changes = diff(old, new);
            for (range, piece) in &changes {
                text.replace_range(range.clone(), piece);
            }
            assert_eq!(text, new);
            changes
                .iter()
                .map(|(r, p)| r.len() + p.len())
                .sum::<usize>()
        };
        assert_eq!(apply("one\n", "one\ntwo\n"), 4, "grown");
        assert_eq!(apply("one\n 1%", "one\n 2%"), 4, "written over");
        assert_eq!(apply("é", "è"), 4);
        // Cut and moved on: the gap line changes, two lines go and one comes.
        let lines = |r: Range<u32>| r.map(|n| format!("line {n}\n")).collect::<String>();
        let old = format!("{}… 9 lines left out …\n{}", lines(0..3), lines(12..20));
        let new = format!("{}… 11 lines left out …\n{}", lines(0..3), lines(14..21));
        assert_eq!(apply(&old, &new), 67, "{:?}", diff(&old, &new));
        let flood = "x\n".repeat(50);
        apply(&format!("a\n{flood}"), &format!("b\n{flood}y"));
    }

    #[test]
    fn carriage_returns_write_the_line_again_and_long_lines_are_cut() {
        let mut stream = Stream::default();
        stream.push("start\n");
        for n in 0..1000 {
            stream.push(&format!("\r{n:>4}%"));
        }
        assert_eq!(stream.text(), "start\n 999%");
        stream.push(" done\r");
        stream.push("again\n");
        assert_eq!(stream.text(), "start\nagain done\n");

        let mut stream = Stream::default();
        let whole = format!("{}\n", "z".repeat(LINE));
        stream.push(&whole);
        assert_eq!(
            stream.text(),
            whole,
            "a line of the most a line holds is whole"
        );
        stream.push(&"y".repeat(LINE + 5));
        assert!(stream.text().ends_with("y … 69 characters left out …"));
        stream.push(&"y".repeat(5));
        stream.push("\nnext");
        let text = stream.text();
        let cut = text.lines().nth(1).unwrap();
        assert_eq!(
            cut,
            format!("{} … 74 characters left out …", "y".repeat(KEEP))
        );
        assert!(cut.chars().count() <= LINE);
        assert!(text.ends_with("\nnext"));
        // A window draws a stream's lines as they are, and cuts only the longer ones.
        assert!(matches!(cut_lines(&text), Cow::Borrowed(_)));

        let long = format!("a\n{}\nb", "é".repeat(LINE + 1));
        let cut = cut_lines(&long);
        assert_eq!(
            cut,
            format!("a\n{} … 65 characters left out …\nb", "é".repeat(KEEP))
        );
        assert!(matches!(cut_lines("short\nlines"), Cow::Borrowed(_)));
    }
}
