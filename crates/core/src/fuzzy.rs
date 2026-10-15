//! Shared fuzzy matching for consistent desktop and Android rankings.
//!
//! [`rank`] orders whole corpora; completions use [`Query::as_typed`] and apply their own
//! ordering. `nucleo-matcher` rewards word and path starts and contiguous matches, so `dpwk`
//! finds `deep-work.md`.

use nucleo_matcher::pattern::{Atom, AtomKind, CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

use crate::path::basename;

/// Haystack kind used to choose match bonuses.
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

/// A parsed query matched against one haystack at a time.
///
/// The matcher reuses one scratch matrix across haystacks.
pub struct Query {
    matcher: Matcher,
    pattern: Pattern,
    buf: Vec<char>,
}

impl Query {
    /// The query as a list asks it: each word matched on its own, in any order, with fzf's `^`,
    /// `$`, `!` and `'`, and `cafe` finding `café`.
    pub fn new(query: &str, corpus: Corpus) -> Query {
        Query {
            matcher: Matcher::new(corpus.config()),
            pattern: Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart),
            buf: Vec::new(),
        }
    }

    /// Match the whole query in typed order, including spaces and punctuation, without fzf
    /// operators. Smart normalization still lets `cafe` match `café`.
    pub fn as_typed(query: &str, corpus: Corpus) -> Query {
        // No atom at all for an empty query, which then matches everything as `new`'s does.
        let mut pattern = Pattern::default();
        if !query.is_empty() {
            let (case, normalize) = (CaseMatching::Ignore, Normalization::Smart);
            let atom = Atom::new(query, case, normalize, AtomKind::Fuzzy, false);
            pattern.atoms.push(atom);
        }
        Query {
            matcher: Matcher::new(corpus.config()),
            pattern,
            buf: Vec::new(),
        }
    }

    /// How well `haystack` matches, or `None` when it does not. Higher is better; an empty query
    /// matches everything with 0.
    pub fn score(&mut self, haystack: &str) -> Option<u32> {
        let text = Utf32Str::new(haystack, &mut self.buf);
        self.pattern.score(text, &mut self.matcher)
    }

    /// [`Query::score`], and which characters of `haystack` matched, by character index in
    /// ascending order: what a row emboldens.
    pub fn indices(&mut self, haystack: &str) -> Option<(u32, Vec<u32>)> {
        let text = Utf32Str::new(haystack, &mut self.buf);
        let mut at = Vec::new();
        let score = self.pattern.indices(text, &mut self.matcher, &mut at)?;
        at.sort_unstable();
        at.dedup();
        Some((score, at))
    }
}

/// Indices matching `query`, best first; score ties retain corpus order.
///
/// Last-segment matches precede whole-path-only matches because queries usually name the file,
/// e.g. `test.md` precedes `tes/t.md` for "tes". Labels without a delimiter form one segment.
///
/// `recent` is empty or has one entry per haystack, with 0 most recent. Within each tier,
/// previously used entries lead in recency order; unused entries follow by score.
pub fn rank(
    haystacks: &[impl AsRef<str>],
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
            let h = h.as_ref();
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
    fn a_query_as_typed_keeps_its_order_and_folds_its_accents() {
        // A list takes the words in any order and folds `é`.
        assert!(
            Query::new("work deep", Corpus::Paths)
                .score("deep-work.md")
                .is_some()
        );
        assert!(Query::new("cafe", Corpus::Paths).score("café.md").is_some());
        // Completion keeps the order, and folds `é` the same.
        let mut typed = Query::as_typed("work deep", Corpus::Paths);
        assert!(typed.score("deep-work.md").is_none());
        assert!(typed.score("Work on the deep end.md").is_some());
        assert!(
            Query::as_typed("cafe", Corpus::Paths)
                .score("café.md")
                .is_some()
        );
        assert!(
            Query::as_typed("CAF", Corpus::Paths)
                .score("café.md")
                .is_some()
        );
        assert!(
            Query::as_typed("", Corpus::Paths)
                .score("anything")
                .is_some()
        );
    }

    #[test]
    fn indices_name_the_matched_characters() {
        let mut q = Query::as_typed("gf", Corpus::Words);
        assert_eq!(q.indices("get_func").map(|(_, at)| at), Some(vec![0, 4]));
        // Characters, not bytes: the `é` is one.
        let mut q = Query::as_typed("fn", Corpus::Words);
        assert_eq!(q.indices("café_fn").map(|(_, at)| at), Some(vec![5, 6]));
        assert!(q.indices("nothing").is_none());
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
