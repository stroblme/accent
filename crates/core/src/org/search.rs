//! The search option of an org link, `[[file:main.c::255]]` and the like, as org's manual has
//! them ("Search Options in File Links"): `255` a line of the file a file link names
//! (`org-link-open-as-file`), and for any link (`org-link-search`) `/regexp/` its first match,
//! `#my-custom-id` the heading whose `CUSTOM_ID` property it is, and anything else a `<<target>>`
//! or a heading by its title ([`super::anchor_range`]). A link's anchor is stored without the `*`
//! of `::*Title` ([`super::links`]), so a heading searched for alone may be found as a target.

use regex::RegexBuilder;
use std::fmt;
use std::ops::Range;

/// A search option, told apart as `org-link-search` does.
#[derive(Debug, PartialEq, Eq)]
pub enum Search<'a> {
    /// A regular expression, matched ignoring case.
    Regex(&'a str),
    /// The value of a heading's `CUSTOM_ID` property.
    CustomId(&'a str),
    /// A `<<target>>` or a heading.
    Name(&'a str),
}

impl<'a> Search<'a> {
    pub fn of(anchor: &'a str) -> Search<'a> {
        let s = anchor.trim();
        if let Some(id) = s.strip_prefix('#') {
            return Search::CustomId(id);
        }
        match s.strip_prefix('/').and_then(|s| s.strip_suffix('/')) {
            Some(regex) => Search::Regex(regex),
            None => Search::Name(s),
        }
    }

    /// Where a match lands in any text; `None` for a search by name or by custom ID, which only
    /// an org file's headings answer.
    pub fn in_text(&self, text: &str) -> Option<Range<usize>> {
        match *self {
            // `^` and `$` at every line's ends, as in Emacs. Accent's own syntax, as its search
            // reads a regular expression: Emacs's `\(…\)` is a group to org, literal here.
            Search::Regex(regex) => RegexBuilder::new(regex)
                .case_insensitive(true)
                .multi_line(true)
                .build()
                .ok()?
                .find(text)
                .map(|m| m.range()),
            Search::CustomId(_) | Search::Name(_) => None,
        }
    }
}

/// The line a file link's search names in `text` when it is digits alone, counted from 1: past
/// the last line the last, as `org-goto-line` goes to the end, so a line is never missing.
pub fn file_line(text: &str, search: &str) -> Option<Range<usize>> {
    let s = search.trim();
    if s.is_empty() || !s.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let lines = super::lines(text);
    let n = s.parse::<usize>().unwrap_or(usize::MAX).max(1);
    Some(lines.get(n - 1).or(lines.last()).cloned().unwrap_or(0..0))
}

/// What the search looks for, for saying it is not there: "No line 255 in main.c".
impl fmt::Display for Search<'_> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Search::Regex(regex) => write!(f, "match for /{regex}/"),
            Search::CustomId(id) => write!(f, "heading with CUSTOM_ID {id}"),
            Search::Name(name) => write!(f, "heading {name}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::anchor_range;
    use super::*;

    #[test]
    fn a_search_is_told_apart_as_org_tells_it() {
        assert_eq!(Search::of("/a.c/"), Search::Regex("a.c"));
        assert_eq!(Search::of("#my-id"), Search::CustomId("my-id"));
        assert_eq!(Search::of("My Target"), Search::Name("My Target"));
        assert_eq!(Search::of("/"), Search::Name("/"));
        // Digits are a line to a file link only: to `[[2024]]` they are a name.
        assert_eq!(Search::of("2024"), Search::Name("2024"));
        assert_eq!(Search::of("/re/").to_string(), "match for /re/");
    }

    #[test]
    fn a_match_and_a_custom_id_land_in_an_org_file() {
        let text = "* One\n:PROPERTIES:\n:CUSTOM_ID: first\n:END:\nsome Words here\n* 2024\n";
        let at = |anchor| anchor_range(text, anchor).map(|r| &text[r]);
        assert_eq!(at("/words? h/"), Some("Words h"));
        assert_eq!(at("#FIRST"), Some("* One"));
        assert_eq!(at("2024"), Some("* 2024"));
        for missing in ["/nowhere/", "#second", "/(/"] {
            assert_eq!(at(missing), None, "{missing}");
        }
    }

    /// A file link's line, past the last line the last, as org's own goes to the end.
    #[test]
    fn a_file_links_digits_are_a_line() {
        let text = "a\nb\nc\n";
        let at = |search| file_line(text, search).map(|r| &text[r]);
        assert_eq!(at("2"), Some("b"));
        assert_eq!(at("99"), Some("c"));
        assert_eq!(at("x2"), None);
        assert_eq!(file_line("", "3"), Some(0..0));
    }
}
