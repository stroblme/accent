//! OSC 52, the copy a program asks of the terminal it runs in: Codex and Claude Code copy through
//! it, and over ssh it is the only way they can.
//!
//! VTE ignores it (0.84 lists it among the sequences it does not implement), so `attach`, which
//! every shell's output passes through, here and on a host alike, picks the copy out on its way
//! past. It keeps the copy beside the holder under the shell's id and raises the valueless
//! `vte.ext.accent.clipboard` termprop, VTE's way of telling its application something; the tab
//! then takes the copy with `accent-cli clip <id>`, over the master on a host. A termprop cannot
//! carry the copy itself: VTE caps a value at 2 KiB and keeps only the last value of a burst. The
//! sequence goes on to the terminal untouched, and a query (`?`) is never answered.

use std::io::{self, Write};
use std::path::PathBuf;

/// What `attach` writes after a copy: `OSC 666 ; <termprop>! ST`, VTE's signal for a valueless
/// termprop. ST, because VTE ignores a termprop sequence ended by BEL.
pub const SIGNAL: &[u8] = b"\x1b]666;vte.ext.accent.clipboard!\x1b\\";

/// Keep `data`, the base64 of the last copy shell `id` made, for [`take`]. Written aside and
/// renamed, so a copy taken while the next is being kept is one or the other, whole.
pub fn keep(id: &str, data: &[u8]) -> io::Result<()> {
    let path = path(id)?;
    let part = path.with_extension("part");
    std::fs::write(&part, data)?;
    std::fs::rename(&part, &path)
}

/// `accent-cli clip <id>`: print the last copy shell `id` made, as base64, and forget it.
pub fn take(id: &str) -> io::Result<()> {
    let path = path(id)?;
    let data = std::fs::read(&path)?;
    std::fs::remove_file(&path)?;
    io::stdout().write_all(&data)
}

/// `clip-<id>` in the holder's private directory. The id names a file, so it may not name a way
/// out of that directory.
fn path(id: &str) -> io::Result<PathBuf> {
    if id.is_empty() || id.contains(['/', '.']) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a shell id",
        ));
    }
    Ok(super::dir()?.join(format!("clip-{id}")))
}

/// The most a copy may hold, base64 and all: past it the copy is dropped rather than kept in
/// memory.
const MAX: usize = 16 << 20;

const ESC: u8 = 0x1b;
const BEL: u8 = 0x07;
/// What follows `ESC ]` in a copy: the OSC number and its separator.
const INTRO: &[u8] = b"52;";

/// Where in the output an OSC 52 has got to. A read can end anywhere in one, so this lasts from
/// one chunk of output to the next.
#[derive(Default)]
pub struct Scanner {
    state: State,
    /// `<selection>;<base64>` of the copy being read.
    body: Vec<u8>,
    /// Whether the copy being read has outgrown [`MAX`].
    full: bool,
}

#[derive(Default, Clone, Copy)]
enum State {
    #[default]
    Ground,
    Esc,
    /// This many bytes of [`INTRO`] have matched.
    Intro(usize),
    Body,
    /// An ESC inside the body: the start of ST, or of a sequence that cuts the copy short.
    BodyEsc,
}

impl Scanner {
    /// Read the next chunk of output, answering the base64 of the last copy that ended in it.
    pub fn feed(&mut self, bytes: &[u8]) -> Option<Vec<u8>> {
        let mut copy = None;
        for &byte in bytes {
            self.state = match (self.state, byte) {
                (State::Body, BEL) | (State::BodyEsc, b'\\') => {
                    copy = self.finish().or(copy);
                    State::Ground
                }
                (State::Body, ESC) => State::BodyEsc,
                (State::Body, _) => {
                    self.full |= self.body.len() >= MAX;
                    if !self.full {
                        self.body.push(byte);
                    }
                    State::Body
                }
                (State::Esc | State::BodyEsc, b']') => State::Intro(0),
                (State::Intro(n), _) if byte == INTRO[n] => match n + 1 == INTRO.len() {
                    true => {
                        self.body.clear();
                        self.full = false;
                        State::Body
                    }
                    false => State::Intro(n + 1),
                },
                // Any other OSC, or a copy cut short by the next sequence.
                (_, ESC) => State::Esc,
                _ => State::Ground,
            };
        }
        copy
    }

    /// The base64 of the copy just read, unless it was a query, empty or too long.
    fn finish(&mut self) -> Option<Vec<u8>> {
        let body = std::mem::take(&mut self.body);
        let (_, data) = body.split_at(body.iter().position(|&b| b == b';')? + 1);
        (!self.full && !data.is_empty() && data != b"?").then(|| data.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn copy(sel: &str, data: &str, st: &[u8]) -> Vec<u8> {
        [b"\x1b]52;", sel.as_bytes(), b";", data.as_bytes(), st].concat()
    }

    #[test]
    fn a_copy_is_read_whole_across_any_split_and_either_terminator() {
        for st in [&b"\x07"[..], b"\x1b\\"] {
            let out = [b"before ".as_slice(), &copy("c", "aGVsbG8=", st), b" after"].concat();
            for cut in 0..out.len() {
                let mut scanner = Scanner::default();
                let first = scanner.feed(&out[..cut]);
                let second = scanner.feed(&out[cut..]);
                assert_eq!(first.or(second), Some(b"aGVsbG8=".to_vec()), "cut at {cut}");
            }
        }
    }

    #[test]
    fn the_last_copy_wins_and_a_query_other_oscs_or_a_cut_copy_are_no_copy() {
        let mut scanner = Scanner::default();
        let out = [
            copy("c", "Zmlyc3Q=", b"\x07"),
            copy("", "c2Vjb25k", b"\x07"),
        ]
        .concat();
        assert_eq!(scanner.feed(&out), Some(b"c2Vjb25k".to_vec()));
        assert_eq!(scanner.feed(&copy("c", "?", b"\x07")), None);
        assert_eq!(
            scanner.feed(b"\x1b]0;title\x07\x1b]7;file:///x\x1b\\"),
            None
        );
        // A sequence starting inside the copy ends it without a copy; the next one still counts.
        assert_eq!(scanner.feed(b"\x1b]52;c;aGVs\x1b[0m"), None);
        assert_eq!(
            scanner.feed(&copy("p", "aGk=", b"\x07")),
            Some(b"aGk=".to_vec())
        );
    }

    #[test]
    fn a_copy_past_the_cap_is_dropped_and_the_next_one_is_not() {
        let mut scanner = Scanner::default();
        let big = "A".repeat(MAX + 1);
        assert_eq!(scanner.feed(&copy("c", &big, b"\x07")), None);
        assert_eq!(
            scanner.feed(&copy("c", "aGk=", b"\x07")),
            Some(b"aGk=".to_vec())
        );
    }
}
