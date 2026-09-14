//! The one fuzzy matcher: what every list narrowed by a typed query asks.
//!
//! Three callers ask it. The desktop palette and the Android file switcher rank a whole corpus
//! with [`rank`], which is why they offer the same note first for the same query. The note
//! completion popup scores one candidate at a time with [`Query`], because it orders its rows by
//! rules of its own — what starts with the query, then the shortest path.
//!
//! `nucleo-matcher` is fzf's algorithm as a library: a subsequence match with bonuses for word
//! and path-segment starts, so `dpwk` finds `deep-work.md` and a contiguous match outscores a
//! scattered one.

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

use crate::path::basename;

/// What the haystacks are, which is what the matcher hands its bonuses out for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Corpus {
    /// `/`-separated paths: the start of a segment scores like the start of a word.
    Paths,
    /// Plain labels: a command's name, a tag.
    Words,
}

impl Corpus {
    fn config(self) -> Config {
        match self {
            Corpus::Paths => Config::DEFAULT.match_paths(),
            Corpus::Words => Config::DEFAULT,
        }
    }
}

/// One parsed query, asked about one haystack at a time.
///
/// Holding both the pattern and the matcher is what makes asking cheap: the matcher owns a
/// scratch matrix that is allocated once and reused for every haystack.
pub struct Query {
    matcher: Matcher,
    pattern: Pattern,
    buf: Vec<char>,
}

impl Query {
    pub fn new(query: &str, corpus: Corpus) -> Query {
        Query {
            matcher: Matcher::new(corpus.config()),
            pattern: Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart),
            buf: Vec::new(),
        }
    }

    /// How well `haystack` matches, or `None` when it does not. Higher is better; an empty query
    /// matches everything with 0.
    pub fn score(&mut self, haystack: &str) -> Option<u32> {
        let text = Utf32Str::new(haystack, &mut self.buf);
        self.pattern.score(text, &mut self.matcher)
    }
}

/// Indices of `haystacks` that match `query`, best first. Ties keep corpus order.
///
/// A haystack whose *last segment* matches leads, because a query is nearly always the name of
/// the thing and only rarely the folder it sits in: for "tes", `test.md` comes before `tes/t.md`.
/// The whole path is still matched — it is what lets a folder narrow a search — it just sorts
/// below. A haystack with no delimiter is its own last segment, so a list of labels is
/// unaffected.
///
/// `recent` is either empty or one entry per haystack, holding how recently it was used — 0 for
/// the most recent. Inside each of the two tiers anything used before leads, in use order, and
/// the rest follow by score: a command run twice is what the user means by that half-typed
/// query, however well something else scores. This is VS Code's quick-open behaviour. Where the
/// recency comes from is the caller's business; that it outranks the score is not.
pub fn rank(
    haystacks: &[String],
    recent: &[Option<usize>],
    query: &str,
    corpus: Corpus,
) -> Vec<usize> {
    let mut q = Query::new(query, corpus);
    // (index, score against the last segment, score against the whole path).
    let mut hits: Vec<(usize, Option<u32>, u32)> = haystacks
        .iter()
        .enumerate()
        .filter_map(|(i, h)| {
            let path = q.score(h)?;
            let name = basename(h);
            // Scoring the whole path again when it has no folder in it would give the same number.
            let base = match name.len() == h.len() {
                true => Some(path),
                false => q.score(name),
            };
            Some((i, base, path))
        })
        .collect();
    let used = |i: usize| recent.get(i).copied().flatten().unwrap_or(usize::MAX);
    hits.sort_by(|a, b| {
        a.1.is_none()
            .cmp(&b.1.is_none())
            .then(used(a.0).cmp(&used(b.0)))
            .then(b.1.cmp(&a.1))
            .then(b.2.cmp(&a.2))
            .then(a.0.cmp(&b.0))
    });
    hits.into_iter().map(|(i, _, _)| i).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn corpus(paths: &[&str]) -> Vec<String> {
        paths.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn rank_keeps_matches_and_drops_the_rest() {
        let c = corpus(&[
            "archive/2020/notes.md",
            "daily/2026-09-03.md",
            "projects/deep-work.md",
        ]);
        assert_eq!(rank(&c, &[], "deep", Corpus::Paths), vec![2]);
        assert_eq!(rank(&c, &[], "daily", Corpus::Paths), vec![1]);
        assert!(rank(&c, &[], "zzzz", Corpus::Paths).is_empty());
        // An empty pattern matches everything, in corpus order.
        assert_eq!(rank(&c, &[], "", Corpus::Paths), vec![0, 1, 2]);
    }

    #[test]
    fn rank_prefers_a_contiguous_match() {
        let c = corpus(&["d-e-e-p.md", "deep-work.md"]);
        assert_eq!(rank(&c, &[], "deep", Corpus::Paths), vec![1, 0]);
    }

    #[test]
    fn rank_puts_a_filename_match_above_a_path_match() {
        let c = corpus(&["tes/t.md", "test.md"]);
        assert_eq!(rank(&c, &[], "tes", Corpus::Paths), vec![1, 0]);
        // Recency orders the files whose name matches; it does not promote one whose folder does.
        assert_eq!(rank(&c, &[Some(0), None], "tes", Corpus::Paths), vec![1, 0]);
    }

    #[test]
    fn rank_leads_with_what_was_used_before() {
        let c = corpus(&["d-e-e-p.md", "deep-work.md"]);
        // The worse match was used last, so it leads; score decides everything below.
        assert_eq!(
            rank(&c, &[Some(0), None], "deep", Corpus::Paths),
            vec![0, 1]
        );
        // Two recent hits keep their use order, not their score order.
        assert_eq!(
            rank(&c, &[Some(1), Some(0)], "deep", Corpus::Paths),
            vec![1, 0]
        );
        // Recency never rescues a non-match.
        assert!(rank(&c, &[Some(0), Some(1)], "zzzz", Corpus::Paths).is_empty());
        // Between two matching names it decides.
        let both = corpus(&["a/test.md", "b/test.md"]);
        assert_eq!(
            rank(&both, &[None, Some(0)], "test", Corpus::Paths),
            vec![1, 0]
        );
    }

    #[test]
    fn a_query_matches_anywhere_in_the_word() {
        let mut q = Query::new("bc", Corpus::Words);
        assert!(q.score("a/bc").is_some(), "not only from the start");
        assert!(q.score("a-bc.md").is_some());
        assert!(q.score("b/x/c").is_some(), "scattered still matches");
        assert!(q.score("cb").is_none(), "but only in order");
        assert!(Query::new("", Corpus::Words).score("anything").is_some());
    }
}
