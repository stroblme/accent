//! Tags a note might take, from the notes most like it: its most distinctive words, as tf-idf
//! against the index, find those notes by `bm25`, and their tags are ranked by how alike they are.

use std::collections::HashMap;

use rusqlite::{OptionalExtension, params};

use super::Index;
use super::search::fold;
use crate::Result;
use crate::walk::FileKind;

/// How many of the note's words are weighed, its most frequent: what bounds a long note's cost.
const WEIGHED: usize = 200;
/// How many of those, the most distinctive, look for the notes like it.
const WORDS: usize = 10;
/// How many notes like it lend their tags.
const NOTES: usize = 20;
/// How many tags are suggested.
const TAGS: usize = 10;

impl Index {
    /// The tags of the notes most like `text`, the note at `rel`, best first. Suggestions only:
    /// the caller leaves out the tags the note has already.
    ///
    /// The words that tell the note apart are the ones it uses often and few other notes use
    /// (tf-idf, document counts from the search index's own vocabulary); the notes holding most
    /// of them, by `bm25`, are the notes like it, and a tag counts for as much as the notes
    /// carrying it are alike. A word only this note has finds nothing, so it does not count.
    pub fn similar_tags(&self, rel: &str, text: &str) -> Result<Vec<String>> {
        let mut uses: HashMap<String, usize> = HashMap::new();
        for word in text.split(|c: char| !c.is_alphanumeric()) {
            if word.chars().nth(2).is_some() && !word.chars().all(|c| c.is_ascii_digit()) {
                *uses.entry(fold(word)).or_default() += 1;
            }
        }
        let mut frequent: Vec<(String, usize)> = uses.into_iter().collect();
        frequent.sort_unstable_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        frequent.truncate(WEIGHED);

        self.conn.execute(
            "CREATE VIRTUAL TABLE IF NOT EXISTS temp.notes_vocab
             USING fts5vocab(main, 'notes_fts', 'row')",
            [],
        )?;
        // A row a body, and a small one: counting `notes` itself reads every body's first page.
        let total: i64 =
            self.conn
                .query_row("SELECT count(*) FROM notes_fts_docsize", [], |r| r.get(0))?;
        // The note's own row counts its words once already.
        let indexed = self
            .conn
            .query_row("SELECT 1 FROM files WHERE rel_path = ?1", [rel], |_| Ok(()))
            .optional()?
            .is_some();
        let floor = 1 + i64::from(indexed);
        let mut docs = self
            .conn
            .prepare_cached("SELECT doc FROM temp.notes_vocab WHERE term = ?1")?;
        let mut weighed = Vec::new();
        for (word, n) in frequent {
            let df: i64 = docs
                .query_row([&word], |r| r.get(0))
                .optional()?
                .unwrap_or(0);
            if df >= floor && df <= total / 2 {
                weighed.push((n as f64 * (total as f64 / df as f64).ln(), word));
            }
        }
        weighed.sort_unstable_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        let query: Vec<String> = weighed
            .iter()
            .take(WORDS)
            .map(|(_, word)| format!("\"{}\"", word.replace('"', "\"\"")))
            .collect();
        if query.is_empty() {
            return Ok(Vec::new());
        }

        let mut like = self.conn.prepare_cached(
            "SELECT f.id, -bm25(notes_fts, 1.0, 10.0) FROM notes_fts
             JOIN files f ON f.id = notes_fts.rowid
             WHERE notes_fts MATCH ?1 AND f.kind = ?2 AND f.git_ignored = 0 AND f.rel_path <> ?3
             ORDER BY bm25(notes_fts, 1.0, 10.0) LIMIT ?4",
        )?;
        let alike: Vec<(i64, f64)> = like
            .query_map(
                params![
                    query.join(" OR "),
                    FileKind::Markdown.as_i64(),
                    rel,
                    NOTES as i64
                ],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?
            .collect::<rusqlite::Result<_>>()?;
        let mut tagged = self
            .conn
            .prepare_cached("SELECT DISTINCT name FROM tags WHERE file_id = ?1")?;
        let mut weights: HashMap<String, f64> = HashMap::new();
        for (id, alike) in alike {
            for name in tagged.query_map([id], |r| r.get::<_, String>(0))? {
                *weights.entry(name?).or_default() += alike;
            }
        }
        let mut tags: Vec<(String, f64)> = weights.into_iter().collect();
        tags.sort_unstable_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        Ok(tags.into_iter().take(TAGS).map(|(name, _)| name).collect())
    }
}

#[cfg(test)]
mod tests {
    use crate::index::testing::open;
    use std::fs;

    #[test]
    fn a_note_is_offered_the_tags_of_the_notes_most_like_it() {
        let vault = tempfile::tempdir().unwrap();
        let notes = [
            (
                "q1.md",
                "Transmon qubit readout drifted after calibration. #hardware",
            ),
            (
                "q2.md",
                "Flux noise limits transmon qubit coherence. #hardware #lab",
            ),
            (
                "b1.md",
                "Sourdough bread wants a lively starter and strong flour. #cooking",
            ),
            (
                "b2.md",
                "Bread hydration: flour, water, starter, patience. #cooking",
            ),
            (
                "m1.md",
                "Meeting notes about the budget and the schedule. #admin",
            ),
            ("m2.md", "The schedule moved; the budget did not. #admin"),
        ];
        for (rel, text) in notes {
            fs::write(vault.path().join(rel), text).unwrap();
        }
        let db = tempfile::tempdir().unwrap();
        let mut index = open(&db);
        index.reconcile(vault.path(), |_| {}).unwrap();

        let draft = "The transmon qubit lost coherence again; readout looks noisy.";
        let tags = index.similar_tags("draft.md", draft).unwrap();
        assert_eq!(
            tags.first().map(String::as_str),
            Some("hardware"),
            "{tags:?}"
        );
        assert!(tags.contains(&"lab".to_string()), "{tags:?}");
        assert!(!tags.contains(&"cooking".to_string()), "{tags:?}");
        // A note sharing no word with any other has nothing to go on.
        assert!(
            index
                .similar_tags("x.md", "zebra quokka")
                .unwrap()
                .is_empty()
        );
    }
}
