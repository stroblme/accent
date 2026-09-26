//! Block ids: the `^id` a `[[Note#^id]]` links to, as Obsidian writes a block reference.
//!
//! An id is `^` and Latin letters, digits and dashes at the very end of a block's text, after a
//! space or on a line of its own. At the end of a paragraph, a list item or a heading it marks
//! that block. On a line of its own it marks what the line follows: after a blank line the block
//! before it, and straight under a list, a quote or a table — which the line continues, as far as
//! markdown is concerned — the whole of that list, quote or table.

use super::options;
use pulldown_cmark::{Event, Parser, TagEnd};
use std::ops::Range;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockId {
    /// Without its `^`.
    pub id: String,
    /// Where the block it marks starts, which is where a link to it lands.
    pub start: usize,
    /// The `^id` as written.
    pub marker: Range<usize>,
}

/// A block open around the text being read.
struct Open {
    tag: TagEnd,
    start: usize,
    /// Where the block just before it starts, if one closed right before it opened.
    before: Option<usize>,
}

/// Every block id in `text`, in order. A note with no `^` is not parsed at all.
pub fn block_ids(text: &str) -> Vec<BlockId> {
    let mut out = Vec::new();
    if !text.contains('^') {
        return out;
    }
    let mut open: Vec<Open> = Vec::new();
    let mut closed: Option<usize> = None;
    // The innermost block's text so far: where it starts, and where it ends if what ended it
    // was text rather than code, a formula or a link.
    let mut run: Option<(usize, Option<usize>)> = None;
    for (ev, r) in Parser::new_ext(text, options()).into_offset_iter() {
        let just_closed = closed.take();
        let tag = match &ev {
            Event::Start(tag) => Some(tag.to_end()),
            Event::End(tag) => Some(*tag),
            _ => None,
        };
        if let Some(tag) = tag.filter(is_block) {
            if let Some((from, Some(to))) = run.take() {
                out.extend(marked(text, from..to, &open));
            }
            match ev {
                Event::Start(_) => open.push(Open {
                    tag,
                    start: r.start,
                    before: just_closed,
                }),
                _ => {
                    open.pop();
                    closed = Some(r.start);
                }
            }
            continue;
        }
        let from = run.map_or(r.start, |(from, _)| from);
        let to = matches!(ev, Event::Text(_)).then_some(r.end);
        run = Some((from, to));
    }
    out
}

/// The id ending the text at `run` in the innermost of `open`, if it ends with one.
fn marked(text: &str, run: Range<usize>, open: &[Open]) -> Option<BlockId> {
    let inner = open.last()?;
    if matches!(
        inner.tag,
        TagEnd::CodeBlock | TagEnd::HtmlBlock | TagEnd::MetadataBlock(_)
    ) {
        return None;
    }
    let s = &text[run.clone()];
    let len = s
        .bytes()
        .rev()
        .take_while(|b| b.is_ascii_alphanumeric() || *b == b'-')
        .count();
    let caret = s.len().checked_sub(len + 1).filter(|_| len > 0)?;
    let spaced = s[..caret].ends_with(char::is_whitespace) || caret == 0;
    if s.as_bytes()[caret] != b'^' || !spaced {
        return None;
    }
    let marker = run.start + caret..run.end;
    let alone = caret == 0;
    let own_line = alone || s[..caret].trim_end_matches([' ', '\t']).ends_with('\n');
    let outer = open.iter().find(|o| {
        matches!(
            o.tag,
            TagEnd::List(_) | TagEnd::BlockQuote(_) | TagEnd::Table
        )
    });
    let start = match inner.before {
        Some(before) if alone => before,
        _ if own_line => outer.unwrap_or(inner).start,
        _ => inner.start,
    };
    Some(BlockId {
        id: s[caret + 1..].to_string(),
        start,
        marker,
    })
}

/// A block, as opposed to the emphasis, strikeout and links inside one.
fn is_block(tag: &TagEnd) -> bool {
    !matches!(
        tag,
        TagEnd::Emphasis
            | TagEnd::Strong
            | TagEnd::Strikethrough
            | TagEnd::Superscript
            | TagEnd::Subscript
            | TagEnd::Link
            | TagEnd::Image
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each id and the line its block starts on.
    fn landings(text: &str) -> Vec<(String, &str)> {
        block_ids(text)
            .into_iter()
            .map(|b| {
                let line = text[b.start..].lines().next().unwrap_or_default();
                assert_eq!(&text[b.marker.clone()], format!("^{}", b.id));
                (b.id, line)
            })
            .collect()
    }

    #[test]
    fn an_id_ending_a_block_marks_it() {
        let text = "# Title ^head\n\nFirst line\nlast line ^para\n\n- one ^item\n  - nested ^deep\n\
                    - two\n\n1. [x] done [[Link]] ^task\n";
        assert_eq!(
            landings(text),
            [
                ("head".into(), "# Title ^head"),
                ("para".into(), "First line"),
                ("item".into(), "- one ^item"),
                ("deep".into(), "- nested ^deep"),
                ("task".into(), "1. [x] done [[Link]] ^task"),
            ]
        );
    }

    /// Straight under a list, a quote or a table the line belongs to it and marks all of it; after
    /// a blank line it marks the block before it.
    #[test]
    fn an_id_on_its_own_line_marks_what_it_follows() {
        let text = "- a\n  - b\n^list\n\n> quote\n^quote\n\n| a |\n|---|\n| 1 |\n^table\n\n\
                   > spaced\n\n^after\n\nplain\n^plain\n";
        assert_eq!(
            landings(text),
            [
                ("list".into(), "- a"),
                ("quote".into(), "> quote"),
                ("table".into(), "| a |"),
                ("after".into(), "> spaced"),
                ("plain".into(), "plain"),
            ]
        );
    }

    #[test]
    fn only_a_spaced_id_at_the_very_end_counts() {
        for text in [
            "x^2 and y^2\n",
            "mid ^not here\n",
            "`code ^no`\n",
            "*emph ^no*\n",
            "```\nfenced ^no\n```\n",
            "escaped \\^no\n",
            "under_score ^no_\n",
            "---\ntitle: a ^no\n---\n",
        ] {
            assert_eq!(block_ids(text), [], "{text:?}");
        }
    }
}
