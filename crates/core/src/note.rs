//! The syntax a file's links, tags and headings are written in, for what reads them alike in
//! either: the index, the notes provider, a move's or a Save As's link updates and a jump to an
//! anchor. A `match` per call, so another syntax is one more arm in each.

use crate::{markdown, org, path};
use std::collections::HashMap;
use std::ops::Range;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Syntax {
    Markdown,
    Org,
}

impl Syntax {
    /// Org for an `.org` file, markdown for any other: a note, or whatever else is read as one.
    pub fn of(rel: &str) -> Syntax {
        match path::is_org(rel) {
            true => Syntax::Org,
            false => Syntax::Markdown,
        }
    }

    pub fn analyze(self, text: &str) -> markdown::Analysis {
        match self {
            Syntax::Markdown => markdown::analyze(text),
            Syntax::Org => org::analyze(text),
        }
    }

    /// Where `anchor` lands in `text` ([`markdown::anchor_range`], [`org::anchor_range`]).
    pub fn anchor_range(self, text: &str, anchor: &str) -> Option<Range<usize>> {
        match self {
            Syntax::Markdown => markdown::anchor_range(text, anchor),
            Syntax::Org => org::anchor_range(text, anchor),
        }
    }

    /// The text of a file a move took from `src_old` to `src_new` with its links pointing again
    /// at what they named, or `None` when none has to change ([`markdown::rewrite_moved`]).
    pub fn rewrite_moved(
        self,
        text: &str,
        src_old: &str,
        src_new: &str,
        targets: &HashMap<String, String>,
        moves: &HashMap<String, String>,
    ) -> Option<String> {
        match self {
            Syntax::Markdown => markdown::rewrite_moved(text, src_old, src_new, targets, moves),
            Syntax::Org => org::rewrite_moved(text, src_old, src_new, targets, moves),
        }
    }

    /// The key of every path `text` spells from `src` ([`markdown::path_link_keys`]).
    pub fn path_link_keys(self, text: &str, src: &str) -> Vec<String> {
        match self {
            Syntax::Markdown => markdown::path_link_keys(text, src),
            Syntax::Org => org::path_link_keys(text, src),
        }
    }
}
