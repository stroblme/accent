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

    /// The order is the core's and tested there; what is tested here is that the switcher gets it
    /// through the ffi's own types.
    #[test]
    fn a_name_leads_a_folder_and_recency_orders_the_names() {
        let paths: Vec<String> = ["tes/t.md", "test.md", "Daily/2026-09-12.md"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(fuzzy_rank("tes".into(), paths.clone(), vec![]), vec![1, 0]);
        // Recency does not promote a folder match over a name match.
        let recent = vec![Some(0), None, None];
        assert_eq!(fuzzy_rank("tes".into(), paths, recent), vec![1, 0]);
    }
}
