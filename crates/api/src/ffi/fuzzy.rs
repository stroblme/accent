//! The file switcher's ranking.
//!
//! The same matcher and the same order as the desktop palette, which is the point: one query
//! typed on the phone and on the laptop should offer the same note first.
//!
//! ponytail: this is `apps/gtk/src/palette.rs::rank` copied, not shared — moving it into core
//! would take `nucleo-matcher` with it, and nothing else in core wants a fuzzy matcher yet. The
//! duplication is in NOTEPAD; fold the two when the palette is next opened.

use accent_core::path::basename;
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

const MAX_RESULTS: usize = 200;

/// Indices of `haystacks` that match `query`, best first, capped at 200.
///
/// A haystack whose *last segment* matches leads, because a query is nearly always the name of
/// the thing and only rarely the folder it sits in: for "tes", `test.md` comes before `tes/t.md`.
///
/// `recent` is either empty or one entry per haystack, holding how recently it was used — 0 for
/// the most recent. Inside each of the two tiers anything used before leads, in use order, and
/// the rest follow by score. This is VS Code's quick-open behaviour.
#[uniffi::export]
pub fn fuzzy_rank(query: String, haystacks: Vec<String>, recent: Vec<Option<u32>>) -> Vec<u32> {
    let mut matcher = Matcher::new(Config::DEFAULT.match_paths());
    let pattern = Pattern::parse(&query, CaseMatching::Ignore, Normalization::Smart);
    let mut buf = Vec::new();
    // (index, score against the last segment, score against the whole path).
    let mut hits: Vec<(usize, Option<u32>, u32)> = haystacks
        .iter()
        .enumerate()
        .filter_map(|(i, h)| {
            let path = pattern.score(Utf32Str::new(h, &mut buf), &mut matcher)?;
            let name = basename(h);
            // Scoring the whole path again when it has no folder in it would give the same number.
            let base = match name.len() == h.len() {
                true => Some(path),
                false => pattern.score(Utf32Str::new(name, &mut buf), &mut matcher),
            };
            Some((i, base, path))
        })
        .collect();
    let used = |i: usize| recent.get(i).copied().flatten().unwrap_or(u32::MAX);
    hits.sort_by(|a, b| {
        a.1.is_none()
            .cmp(&b.1.is_none())
            .then(used(a.0).cmp(&used(b.0)))
            .then(b.1.cmp(&a.1))
            .then(b.2.cmp(&a.2))
            .then(a.0.cmp(&b.0))
    });
    hits.into_iter()
        .take(MAX_RESULTS)
        .map(|(i, _, _)| i as u32)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths() -> Vec<String> {
        ["tes/t.md", "test.md", "Daily/2026-09-12.md"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    /// The two rules the desktop palette settled on, in the order it settled them: a matching
    /// name beats a matching folder, and recency orders what is left.
    #[test]
    fn a_name_leads_a_folder_and_recency_orders_the_names() {
        assert_eq!(fuzzy_rank("tes".into(), paths(), vec![]), vec![1, 0]);
        // Recency does not promote a folder match over a name match.
        let recent = vec![Some(0), None, None];
        assert_eq!(fuzzy_rank("tes".into(), paths(), recent), vec![1, 0]);
        // Between two matching names it decides.
        let both = vec!["a/test.md".to_string(), "b/test.md".to_string()];
        assert_eq!(
            fuzzy_rank("test".into(), both, vec![None, Some(0)]),
            vec![1, 0]
        );
        assert!(fuzzy_rank("zzz".into(), paths(), vec![]).is_empty());
        // An empty query keeps everything, in corpus order.
        assert_eq!(fuzzy_rank(String::new(), paths(), vec![]), vec![0, 1, 2]);
    }
}
