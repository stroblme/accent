//! Git's conflict markers: the blocks a merge that stopped leaves in a file, found so an editor
//! can tint each one and resolve it with a click, as VS Code does.

use std::ops::Range;

/// One conflict: `<<<<<<<`, the current side, in diff3 style `|||||||` and the common base,
/// `=======`, the incoming side, `>>>>>>>`. Byte ranges into the text it was found in, each part
/// whole lines with their line endings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    /// The `<<<<<<<` line to the end of the `>>>>>>>` line.
    pub range: Range<usize>,
    /// What HEAD has: the branch the merge is into.
    pub ours: Range<usize>,
    /// What both sides started from, where the merge wrote it (`merge.conflictStyle = diff3`).
    pub base: Option<Range<usize>>,
    /// What the branch being merged has.
    pub theirs: Range<usize>,
}

/// Which side a resolution keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Take {
    Current,
    Incoming,
    /// The current side, then the incoming one.
    Both,
}

impl Block {
    /// The marker lines, line endings included, top to bottom.
    pub fn markers(&self) -> Vec<Range<usize>> {
        let split = self.base.as_ref().map_or(self.ours.end, |base| base.end);
        [
            Some(self.range.start..self.ours.start),
            self.base.as_ref().map(|base| self.ours.end..base.start),
            Some(split..self.theirs.start),
            Some(self.theirs.end..self.range.end),
        ]
        .into_iter()
        .flatten()
        .collect()
    }

    /// What the block becomes once `take` has resolved it: the lines it keeps, and no marker.
    pub fn resolve(&self, text: &str, take: Take) -> String {
        let (ours, theirs) = (&text[self.ours.clone()], &text[self.theirs.clone()]);
        let mut kept = match take {
            Take::Current => ours.to_string(),
            Take::Incoming => theirs.to_string(),
            Take::Both => format!("{ours}{theirs}"),
        };
        // The block ends the file on a line with no line ending, so what replaces it has none.
        if !text[..self.range.end].ends_with('\n') && kept.ends_with('\n') {
            kept.pop();
            if kept.ends_with('\r') {
                kept.pop();
            }
        }
        kept
    }
}

/// A marker is its character seven times at the start of a line — git's default
/// `conflict-marker-size` — then the line's end or a space and a label.
const SIZE: usize = 7;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Marker {
    Start,
    Base,
    Split,
    End,
}

/// The marker `line` is, if it is one.
fn marker(line: &str) -> Option<Marker> {
    let line = line.trim_end_matches(['\n', '\r']).as_bytes();
    let kind = match line.first()? {
        b'<' => Marker::Start,
        b'|' => Marker::Base,
        b'=' => Marker::Split,
        b'>' => Marker::End,
        _ => return None,
    };
    let run = line.len() >= SIZE && line[..SIZE].iter().all(|&b| b == line[0]);
    (run && matches!(line.get(SIZE), None | Some(b' '))).then_some(kind)
}

/// A block whose `>>>>>>>` has not come yet: where it starts, where its current side starts,
/// and the `|||||||` and `=======` lines seen so far.
struct Open {
    start: usize,
    ours: usize,
    base: Option<Range<usize>>,
    split: Option<Range<usize>>,
}

impl Open {
    /// The block the `>>>>>>>` line `end` closes, if it has had its `=======`.
    fn close(self, end: Range<usize>) -> Option<Block> {
        let split = self.split?;
        Some(Block {
            range: self.start..end.end,
            ours: self.ours..self.base.as_ref().map_or(split.start, |b| b.start),
            base: self.base.map(|b| b.end..split.start),
            theirs: split.end..end.start,
        })
    }
}

/// Every conflict block in `text`, top to bottom.
///
/// A marker out of its place — a second `=======`, a `>>>>>>>` before one — means the block it
/// is in is no block, and a `<<<<<<<` inside a block starts the next one there. So a block an
/// edit left unfinished, or one cut short by another inside it, is dropped alone: the blocks
/// after it are still found.
pub fn blocks(text: &str) -> Vec<Block> {
    let mut found = Vec::new();
    if !text.contains("<<<<<<<") {
        return found;
    }
    let (mut open, mut at) = (None::<Open>, 0);
    for line in text.split_inclusive('\n') {
        let lines = at..at + line.len();
        at = lines.end;
        let Some(kind) = marker(line) else {
            continue;
        };
        open = match (open, kind) {
            (_, Marker::Start) => Some(Open {
                start: lines.start,
                ours: lines.end,
                base: None,
                split: None,
            }),
            (Some(o), Marker::Base) if o.base.is_none() && o.split.is_none() => Some(Open {
                base: Some(lines),
                ..o
            }),
            (Some(o), Marker::Split) if o.split.is_none() => Some(Open {
                split: Some(lines),
                ..o
            }),
            (Some(o), Marker::End) => {
                found.extend(o.close(lines));
                None
            }
            _ => None,
        };
    }
    found
}

/// `text` with the marker lines of every block blanked to spaces, so it parses as the two sides
/// with a blank line between them at the same byte offsets; `None` where there is no block.
pub fn blank_markers(text: &str) -> Option<String> {
    let found = blocks(text);
    if found.is_empty() {
        return None;
    }
    let mut blank = text.to_string();
    for line in found.iter().flat_map(Block::markers) {
        let len = text[line.clone()].trim_end_matches(['\n', '\r']).len();
        blank.replace_range(line.start..line.start + len, &" ".repeat(len));
    }
    Some(blank)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The parts of the only block in `text`, as text.
    fn parts(text: &str) -> (&str, &str, Option<&str>, &str) {
        let found = blocks(text);
        assert_eq!(found.len(), 1, "one block in {text:?}");
        let b = &found[0];
        (
            &text[b.range.clone()],
            &text[b.ours.clone()],
            b.base.clone().map(|r| &text[r]),
            &text[b.theirs.clone()],
        )
    }

    #[test]
    fn a_two_way_block_is_found_with_its_labels_in_its_markers() {
        let text = "a\n<<<<<<< HEAD\nours\n=======\ntheirs\ntwo\n>>>>>>> side\nz\n";
        assert_eq!(
            parts(text),
            (
                "<<<<<<< HEAD\nours\n=======\ntheirs\ntwo\n>>>>>>> side\n",
                "ours\n",
                None,
                "theirs\ntwo\n"
            )
        );
        let b = &blocks(text)[0];
        let markers: Vec<&str> = b.markers().into_iter().map(|r| &text[r]).collect();
        assert_eq!(markers, ["<<<<<<< HEAD\n", "=======\n", ">>>>>>> side\n"]);
    }

    #[test]
    fn a_diff3_block_has_a_base() {
        let text =
            "<<<<<<< HEAD\nours\n||||||| merged common ancestors\nbase\n=======\n>>>>>>> side\n";
        assert_eq!(parts(text).1, "ours\n");
        assert_eq!(parts(text).2, Some("base\n"));
        assert_eq!(parts(text).3, "", "the incoming side deleted the lines");
        let b = &blocks(text)[0];
        assert_eq!(b.markers().len(), 4);
    }

    #[test]
    fn crlf_lines_keep_their_endings_in_every_part() {
        let text = "<<<<<<< HEAD\r\nours\r\n=======\r\ntheirs\r\n>>>>>>> side\r\nz\r\n";
        let (_, ours, _, theirs) = parts(text);
        assert_eq!((ours, theirs), ("ours\r\n", "theirs\r\n"));
        let b = &blocks(text)[0];
        assert_eq!(b.resolve(text, Take::Both), "ours\r\ntheirs\r\n");
    }

    #[test]
    fn a_marker_is_seven_characters_at_the_start_of_a_line() {
        // Inside a block, none of these is a marker: eight `=`, an indented one, and a label with
        // no space before it. So each stays part of the side it is in.
        let text =
            "<<<<<<< HEAD\n========\n =======\n=======x\n=======\n>>>>>>>> not\nt\n>>>>>>>\n";
        let (_, ours, _, theirs) = parts(text);
        assert_eq!(ours, "========\n =======\n=======x\n");
        assert_eq!(theirs, ">>>>>>>> not\nt\n", "a bare `>>>>>>>` ends it");
        assert!(blocks("<<<<<<<HEAD\na\n=======\nb\n>>>>>>> x\n").is_empty());
    }

    #[test]
    fn a_block_with_a_marker_out_of_place_is_no_block() {
        for text in [
            "<<<<<<< HEAD\na\n>>>>>>> side\n", // no `=======`
            "<<<<<<< HEAD\na\n=======\nb\n",   // never closed
            "<<<<<<< HEAD\na\n=======\nb\n=======\nc\n>>>>>>> side\n", // two
            "<<<<<<< HEAD\na\n=======\nb\n||||||| base\n>>>>>>> side\n", // base after
            "=======\nb\n>>>>>>> side\n",      // no start
        ] {
            assert_eq!(blocks(text), vec![], "{text:?}");
        }
    }

    #[test]
    fn a_start_inside_a_block_drops_it_and_opens_the_next() {
        // An outer block cut short by a nested one is no block; the one inside it is, and what
        // follows it is stray.
        let text = "<<<<<<< a\nx\n<<<<<<< b\ny\n=======\nz\n>>>>>>> c\n=======\nw\n>>>>>>> d\n";
        let found = blocks(text);
        assert_eq!(found.len(), 1);
        assert_eq!(
            &text[found[0].range.clone()],
            "<<<<<<< b\ny\n=======\nz\n>>>>>>> c\n"
        );
        // A block left unfinished by an edit does not take the one after it with it.
        let text = "<<<<<<< a\nx\n<<<<<<< HEAD\ny\n=======\nz\n>>>>>>> side\n";
        assert_eq!(blocks(text).len(), 1);
    }

    #[test]
    fn resolving_keeps_the_side_asked_for() {
        let text = "<<<<<<< HEAD\nours\n||||||| base\nbase\n=======\ntheirs\n>>>>>>> side\n";
        let b = &blocks(text)[0];
        assert_eq!(b.resolve(text, Take::Current), "ours\n");
        assert_eq!(b.resolve(text, Take::Incoming), "theirs\n");
        assert_eq!(b.resolve(text, Take::Both), "ours\ntheirs\n");
        // The file's last line has no line ending, and what replaces it brings none either.
        let text = "<<<<<<< HEAD\nours\n=======\ntheirs\n>>>>>>> side";
        let b = &blocks(text)[0];
        assert_eq!(b.range.end, text.len());
        assert_eq!(b.resolve(text, Take::Both), "ours\ntheirs");
    }

    #[test]
    fn blanking_keeps_every_offset_and_touches_only_markers() {
        let text = "é\n<<<<<<< HEAD ü\nours\n=======\ntheirs\n>>>>>>> side\n=======\n";
        let blank = blank_markers(text).unwrap();
        assert_eq!(blank.len(), text.len());
        assert_eq!(
            blank, "é\n               \nours\n       \ntheirs\n            \n=======\n",
            "the stray `=======` after the block is left alone"
        );
        assert_eq!(blank_markers("no conflict\n=======\n"), None);
    }
}
