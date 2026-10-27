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

    /// The blocks of code `text` names a language for ([`markdown::code_blocks`]).
    pub fn code_blocks(self, text: &str) -> Vec<markdown::CodeBlock> {
        match self {
            Syntax::Markdown => markdown::code_blocks(text),
            Syntax::Org => org::code_blocks(text),
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

/// Where `anchor` lands in `text`, the text of `rel`, by the file it lands in whichever syntax the
/// link was written in, as a heading's anchor is: a `/regexp/` its first match in any file, as an
/// org link's search names one ([`org::Search`]), else an org file's target, heading or custom ID
/// ([`org::anchor_range`]) and any other file's heading or block ([`markdown::anchor_range`]).
pub fn anchor_range(rel: &str, text: &str, anchor: &str) -> Option<Range<usize>> {
    match org::Search::of(anchor) {
        search @ org::Search::Regex(_) => search.in_text(text),
        _ if path::is_org(rel) => org::anchor_range(text, anchor),
        _ => markdown::anchor_range(text, anchor),
    }
}

/// What `anchor` looks for in `rel`, read as [`anchor_range`] reads it, for saying it is not there:
/// "heading Intro", "block ^id", "match for /re/".
pub fn looked_for(rel: &str, anchor: &str) -> String {
    match org::Search::of(anchor) {
        search @ org::Search::Regex(_) => search.to_string(),
        search if path::is_org(rel) => search.to_string(),
        _ if anchor.trim_start().starts_with('^') => format!("block {anchor}"),
        _ => format!("heading {anchor}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A regexp matches in any file, and the rest is read as the file reads an anchor: an org
    /// file's custom ID, target or heading, a markdown note's heading or block.
    #[test]
    fn an_anchor_is_read_as_the_file_it_lands_in_reads_one() {
        let at = |rel: &str, text: &'static str, anchor: &str| {
            anchor_range(rel, text, anchor).map(|r| &text[r])
        };
        assert_eq!(at("main.c", "int a;\nint b;\n", "/T B/"), Some("t b"));
        assert_eq!(at("x.md", "# 2\nb\n", "2"), Some("# 2"));
        assert_eq!(looked_for("x.org", "#id"), "heading with CUSTOM_ID id");
        assert_eq!(looked_for("x.md", "^id"), "block ^id");
        assert_eq!(looked_for("x.md", "2"), "heading 2");
    }
}
