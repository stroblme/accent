//! The file switcher's ranking, as uniffi can carry it.
//!
//! [`accent_core::fuzzy::rank`] does the work: the same matcher and the same order as the desktop
//! palette, which is the point — one query typed on the phone and on the laptop should offer the
//! same note first. All this adds is the cap and the index type, since uniffi carries no `usize`.

const MAX_RESULTS: usize = 200;

/// Indices of `haystacks` that match `query`, best first, capped at 200.
///
/// `recent` is either empty or one entry per haystack, holding how recently it was used — 0 for
/// the most recent; see [`accent_core::fuzzy::rank`] for what that does to the order.
#[uniffi::export]
pub fn fuzzy_rank(query: String, haystacks: Vec<String>, recent: Vec<Option<u32>>) -> Vec<u32> {
    let recent: Vec<Option<usize>> = recent.iter().map(|r| r.map(|n| n as usize)).collect();
    accent_core::fuzzy::rank(
        &haystacks,
        &recent,
        &query,
        accent_core::fuzzy::Corpus::Paths,
    )
    .into_iter()
    .take(MAX_RESULTS)
    .map(|i| i as u32)
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The order is the core's and tested there; what is tested here is what this adds: the cap,
    /// and recency crossing in the ffi's own types.
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
}
