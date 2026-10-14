//! Expose [`accent_core::fuzzy::rank`] through UniFFI so mobile and desktop
//! rank the same notes. Add only a result cap and UniFFI-compatible indices.

use std::collections::HashMap;
use std::sync::Arc;

const MAX_RESULTS: usize = 200;

/// Indices of `haystacks` that match `query`, best first, capped at 200.
///
/// `recent` is either empty or one entry per haystack, holding how recently it was used — 0 for
/// the most recent; see [`accent_core::fuzzy::rank`] for what that does to the order.
#[uniffi::export]
pub fn fuzzy_rank(query: String, haystacks: Vec<String>, recent: Vec<Option<u32>>) -> Vec<u32> {
    let recent: Vec<Option<usize>> = recent.iter().map(|r| r.map(|n| n as usize)).collect();
    ranked(&haystacks, &recent, &query)
}

/// The switcher's names, held on this side: ranking them sends the query and the few recent
/// paths, where [`fuzzy_rank`] sends every name again on every keystroke — tens of thousands of
/// strings, which on a phone cost more than a frame before the ranking had begun.
#[derive(uniffi::Object)]
pub struct Ranker {
    haystacks: Vec<String>,
}

#[uniffi::export]
impl Ranker {
    #[uniffi::constructor]
    pub fn new(haystacks: Vec<String>) -> Arc<Self> {
        Arc::new(Ranker { haystacks })
    }

    /// [`fuzzy_rank`] over the names held. `recent` is the most recently used first, a name
    /// taking the place of each one equal to it.
    pub fn rank(&self, query: String, recent: Vec<String>) -> Vec<u32> {
        let order: HashMap<&str, usize> = recent
            .iter()
            .enumerate()
            .map(|(i, r)| (r.as_str(), i))
            .collect();
        let recent: Vec<Option<usize>> = self
            .haystacks
            .iter()
            .map(|h| order.get(h.as_str()).copied())
            .collect();
        ranked(&self.haystacks, &recent, &query)
    }
}

fn ranked(haystacks: &[String], recent: &[Option<usize>], query: &str) -> Vec<u32> {
    accent_core::fuzzy::rank(haystacks, recent, query, accent_core::fuzzy::Corpus::Paths)
        .into_iter()
        .take(MAX_RESULTS)
        .map(|i| i as u32)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify the FFI result cap and recency conversion; core tests cover ranking.
    #[test]
    fn a_ranking_stops_at_the_cap_and_recency_crosses() {
        let paths: Vec<String> = (0..MAX_RESULTS + 5)
            .map(|n| format!("note{n}.md"))
            .collect();
        assert_eq!(
            fuzzy_rank("note".into(), paths.clone(), vec![]).len(),
            MAX_RESULTS
        );
        let last = paths.len() - 1;
        let mut recent = vec![None; paths.len()];
        recent[last] = Some(0);
        assert_eq!(fuzzy_rank("note".into(), paths, recent)[0], last as u32);
    }

    /// Recency named by path ranks as the per-name list [`fuzzy_rank`] takes.
    #[test]
    fn a_ranker_ranks_as_the_names_would_with_their_recency() {
        let paths: Vec<String> = (0..50).map(|n| format!("dir/note{n}.md")).collect();
        let ranker = Ranker::new(paths.clone());
        let mut recent = vec![None; paths.len()];
        recent[40] = Some(0);
        recent[7] = Some(1);
        let by_path = vec![paths[40].clone(), paths[7].clone()];
        for query in ["note", "note4", "dir"] {
            assert_eq!(
                ranker.rank(query.into(), by_path.clone()),
                fuzzy_rank(query.into(), paths.clone(), recent.clone()),
                "{query}"
            );
        }
    }
}
