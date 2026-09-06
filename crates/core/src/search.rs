//! The three match modes of a VS Code search box, compiled into one regular expression.
//!
//! Match Case, Match Whole Word and Regular Expression are independent toggles, but every
//! combination of them is still just a pattern over the note text. Collapsing them here means the
//! index, the replace pass and the UI all work with a single `Regex` instead of branching on three
//! booleans three times over.

use regex::RegexBuilder;
pub use regex::{Error, NoExpand, Regex};
use serde::{Deserialize, Serialize};

/// What the search box's three toggles say. All off is a case-insensitive literal, which is what
/// a plain query means everywhere else in the app.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Options {
    pub case: bool,
    pub word: bool,
    pub regex: bool,
}

impl Options {
    /// Whether anything is switched on, i.e. whether the plain ranked full-text path still
    /// answers the question the user is asking.
    pub fn any(self) -> bool {
        self.case || self.word || self.regex
    }
}

/// Compile `query` under `options`, or say why it does not compile.
///
/// The word wrapper is a non-capturing group, so `\b` binds to the whole alternation and the
/// group numbers a replacement refers to stay the ones the user typed.
pub fn pattern(query: &str, options: Options) -> Result<Regex, Error> {
    let body = match options.regex {
        true => query.to_string(),
        false => regex::escape(query),
    };
    let body = match options.word {
        true => format!(r"\b(?:{body})\b"),
        false => body,
    };
    RegexBuilder::new(&body)
        .case_insensitive(!options.case)
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(case: bool, word: bool, regex: bool) -> Options {
        Options { case, word, regex }
    }

    #[test]
    fn a_plain_query_is_a_case_insensitive_literal() {
        let re = pattern("a.b", opts(false, false, false)).unwrap();
        assert!(re.is_match("A.B"));
        assert!(!re.is_match("axb"), "the dot must not be a metacharacter");
    }

    #[test]
    fn case_and_word_narrow_the_same_query() {
        assert!(
            !pattern("foo", opts(true, false, false))
                .unwrap()
                .is_match("Foo")
        );
        let word = pattern("foo", opts(false, true, false)).unwrap();
        assert!(word.is_match("a foo b"));
        assert!(!word.is_match("foobar"));
    }

    /// Word mode has to wrap an alternation as a whole, not just its first branch.
    #[test]
    fn word_mode_wraps_a_regex_alternation() {
        let re = pattern("foo|bar", opts(false, true, true)).unwrap();
        assert!(re.is_match("a bar b"));
        assert!(!re.is_match("barn"));
    }

    #[test]
    fn an_invalid_regex_is_reported_not_escaped() {
        assert!(pattern("foo(", opts(false, false, true)).is_err());
        assert!(pattern("foo(", opts(false, false, false)).is_ok());
    }
}
