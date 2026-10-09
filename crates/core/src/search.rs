//! The three match modes of a VS Code search box, compiled into one regular expression.
//!
//! Match Case, Match Whole Word and Regular Expression are independent toggles, but every
//! combination of them is still just a pattern over the note text. Collapsing them here means the
//! index, the replace pass and the UI all work with a single `Regex` instead of branching on three
//! booleans three times over.

use regex::RegexBuilder;
pub use regex::{NoExpand, Regex};
use serde::{Deserialize, Serialize};
use std::ops::Range;

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
pub fn pattern(query: &str, options: Options) -> crate::Result<Regex> {
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
        .map_err(|e| crate::Error::Invalid(e.to_string()))
}

/// Where `re` matches in `text`, first to last, as character offsets rather than bytes: what a
/// text widget counts in. A match of no width (`^`, `a*`) is listed like any other, as the Search
/// pane lists it.
pub fn char_ranges(re: &Regex, text: &str) -> Vec<Range<usize>> {
    let mut chars = Chars::new(text);
    re.find_iter(text)
        .map(|m| chars.at(m.start())..chars.at(m.end()))
        .collect()
}

/// Every match of `re` in `text` with what Replace writes in its place, as character offsets:
/// `replacement` as written, or, under Regular Expression, with `$1` and `${name}` naming the
/// match's groups. The rule Replace All in the Search pane follows, so a find bar that rewrites
/// one match at a time writes what `re.replace_all` would.
pub fn replacements(
    re: &Regex,
    text: &str,
    replacement: &str,
    options: Options,
) -> Vec<(Range<usize>, String)> {
    let mut chars = Chars::new(text);
    re.captures_iter(text)
        .map(|caps| {
            let m = caps.get(0).expect("group 0 is the whole match");
            let mut with = String::new();
            match options.regex {
                true => caps.expand(replacement, &mut with),
                false => with.push_str(replacement),
            }
            (chars.at(m.start())..chars.at(m.end()), with)
        })
        .collect()
}

/// Byte offsets into one text as character offsets, for offsets that never go backwards, which a
/// walk over the matches never does: one pass over the text however many matches it holds.
struct Chars<'t> {
    text: &'t str,
    byte: usize,
    char: usize,
}

impl<'t> Chars<'t> {
    fn new(text: &'t str) -> Self {
        Chars {
            text,
            byte: 0,
            char: 0,
        }
    }

    fn at(&mut self, byte: usize) -> usize {
        self.char += self.text[self.byte..byte].chars().count();
        self.byte = byte;
        self.char
    }
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

    /// Offsets a text widget can use: characters, so what comes after a multi-byte letter is
    /// where the widget has it.
    #[test]
    fn matches_are_counted_in_characters() {
        let text = "Ärger foo, äöü Foo";
        let re = pattern("foo", opts(false, false, false)).unwrap();
        assert_eq!(char_ranges(&re, text), [6..9, 15..18]);
        let word = pattern("foo", opts(false, true, false)).unwrap();
        // `_` is a word character, as the Search pane has it: `foo_bar` holds no whole word foo.
        assert_eq!(char_ranges(&word, "foo_bar food foo foo"), [13..16, 17..20]);
    }

    /// One match at a time, from the end, gives what `replace_all` gives at once: the groups are
    /// expanded under Regular Expression and the replacement is taken as written otherwise.
    #[test]
    fn replacements_write_what_replace_all_writes() {
        let text = "é foo(1) bar(22) $1";
        for (query, replacement, regex) in
            [(r"(\w+)\((\d+)\)", "$2-$1", true), ("$1", "[$0]", false)]
        {
            let options = opts(false, false, regex);
            let re = pattern(query, options).unwrap();
            let mut chars: Vec<char> = text.chars().collect();
            for (at, with) in replacements(&re, text, replacement, options)
                .into_iter()
                .rev()
            {
                chars.splice(at, with.chars());
            }
            let expected = match regex {
                true => re.replace_all(text, replacement),
                false => re.replace_all(text, NoExpand(replacement)),
            };
            assert_eq!(chars.into_iter().collect::<String>(), expected);
        }
    }
}
