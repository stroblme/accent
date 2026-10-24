//! A PDF's text in the index: read after the walk by a pass of its own (`accent-api`), and held as
//! the body of the PDF's row, as any other file's text is, its pages joined by form feeds.
//!
//! The walk writes a PDF's row with no body and `text_pages` NULL; a reader claims it (-1), reads
//! it, and writes the body and the page count back to the row if the row is still at the stat it
//! was claimed at. A claim no reader gave back — the process died reading it — stays -1, so a PDF
//! that crashes its reader is not read again until it changes.

use super::Index;
use super::reconcile::write_bodies;
use crate::Result;
use crate::pdf_text::Text;
use rusqlite::{OptionalExtension, params};
use std::path::PathBuf;

/// A PDF whose text a reader has claimed ([`Index::claim_pdfs`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnreadPdf {
    pub rel_path: String,
    /// The file to read, as the walk found it.
    pub path: PathBuf,
    id: i64,
    /// The row's `(mtime_ns, size, ino)` when it was claimed: its text is written only to a row
    /// still at it.
    stamp: (i64, i64, i64),
}

impl Index {
    /// How many PDFs wait for their text to be read.
    pub fn unread_pdfs(&self) -> Result<usize> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM files INDEXED BY idx_files_unread
              WHERE kind = 2 AND text_pages IS NULL",
            [],
            |r| r.get(0),
        )?;
        Ok(n as usize)
    }

    /// Take up to `n` PDFs whose text is unread, newest first, marking each as being read, so a
    /// second process sharing the index reads others.
    pub fn claim_pdfs(&mut self, n: usize) -> Result<Vec<UnreadPdf>> {
        let tx = self.write_tx()?;
        let claimed: Vec<UnreadPdf> = {
            let mut st = tx.prepare_cached(
                "SELECT id, rel_path, canonical, mtime_ns, size, ino
                   FROM files INDEXED BY idx_files_unread
                  WHERE kind = 2 AND text_pages IS NULL ORDER BY mtime_ns DESC LIMIT ?1",
            )?;
            let rows = st.query_map([n as i64], |r| {
                Ok(UnreadPdf {
                    id: r.get(0)?,
                    rel_path: r.get(1)?,
                    path: PathBuf::from(r.get::<_, String>(2)?),
                    stamp: (r.get(3)?, r.get(4)?, r.get(5)?),
                })
            })?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        for pdf in &claimed {
            tx.prepare_cached("UPDATE files SET text_pages = -1 WHERE id = ?1")?
                .execute([pdf.id])?;
        }
        tx.commit()?;
        Ok(claimed)
    }

    /// Write what was read of claimed PDFs: each one's page count, and its text where it has any,
    /// to a row still as it was claimed — a file changed since is the walk's again, to be claimed
    /// anew. The `unread` ones, which a stop came before, are given back.
    pub fn put_pdf_texts(
        &mut self,
        read: &[(UnreadPdf, Text)],
        unread: &[UnreadPdf],
    ) -> Result<()> {
        let tx = self.write_tx()?;
        for (pdf, text) in read {
            let (mtime_ns, size, ino) = pdf.stamp;
            let kept = tx
                .prepare_cached(
                    "UPDATE files SET text_pages = ?2
                      WHERE id = ?1 AND text_pages = -1 AND mtime_ns = ?3 AND size = ?4 AND ino = ?5",
                )?
                .execute(params![pdf.id, text.pages as i64, mtime_ns, size, ino])?;
            if kept == 1 && !text.body.trim().is_empty() {
                tx.prepare_cached(
                    "INSERT OR REPLACE INTO temp.bodies(file_id, body, title)
                     SELECT id, ?2, ifnull(title, '') FROM files WHERE id = ?1",
                )?
                .execute(params![pdf.id, text.body])?;
            }
        }
        for pdf in unread {
            tx.prepare_cached(
                "UPDATE files SET text_pages = NULL WHERE id = ?1 AND text_pages = -1",
            )?
            .execute([pdf.id])?;
        }
        write_bodies(&tx)?;
        tx.commit()?;
        Ok(())
    }

    /// The text the index holds of `rel`, a PDF's pages joined by form feeds: what the Search
    /// pane lists the rest of a PDF's matches from, the file itself being no text to read.
    pub fn indexed_text(&self, rel: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .prepare_cached(
                "SELECT n.body FROM notes n JOIN files f ON f.id = n.file_id WHERE f.rel_path = ?1",
            )?
            .query_row([rel], |r| r.get(0))
            .optional()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::walk::FileKind;

    fn text(body: &str, pages: usize) -> Text {
        Text {
            body: body.to_string(),
            pages,
        }
    }

    /// A vault of one note and the PDFs named, its index walked.
    fn vault(pdfs: &[&str]) -> (tempfile::TempDir, tempfile::TempDir, Index) {
        let vault = tempfile::tempdir().unwrap();
        std::fs::write(vault.path().join("note.md"), "a note about zebras\n").unwrap();
        for pdf in pdfs {
            std::fs::write(vault.path().join(pdf), b"%PDF-1.4\n").unwrap();
        }
        let cache = tempfile::tempdir().unwrap();
        let mut ix = Index::open(&cache.path().join("index.db")).unwrap();
        ix.reconcile(vault.path(), |_| {}).unwrap();
        (vault, cache, ix)
    }

    fn pages(ix: &Index, rel: &str) -> Option<i64> {
        ix.conn
            .query_row(
                "SELECT text_pages FROM files WHERE rel_path = ?1",
                [rel],
                |r| r.get(0),
            )
            .unwrap()
    }

    #[test]
    fn a_page_hit_names_its_page_in_every_kind_of_search() {
        let (_vault, _cache, mut ix) = vault(&["paper.pdf"]);
        assert_eq!(ix.unread_pdfs().unwrap(), 1);
        let claimed = ix.claim_pdfs(16).unwrap();
        assert_eq!(claimed.len(), 1);
        assert_eq!(ix.unread_pdfs().unwrap(), 0, "a claimed PDF is not unread");
        let body = "Intro page\nnothing here\x0czebras graze\x0c\x0czebras again\nand zebras";
        ix.put_pdf_texts(&[(claimed[0].clone(), text(body, 4))], &[])
            .unwrap();
        assert_eq!(pages(&ix, "paper.pdf"), Some(4));

        // Ranked: one row a page, the note's row as before.
        let hits = ix.search("zebras", 10, false).unwrap();
        let pdf: Vec<_> = hits.iter().filter(|h| h.rel_path == "paper.pdf").collect();
        assert_eq!(
            pdf.iter().map(|h| h.page).collect::<Vec<_>>(),
            [Some(2), Some(4)]
        );
        assert!(pdf.iter().all(|h| h.line.is_none()));
        assert_eq!(pdf[0].snippet, "«zebras» graze");
        assert_eq!(pdf[1].snippet, "«zebras» again");
        let note = hits.iter().find(|h| h.rel_path == "note.md").unwrap();
        assert_eq!((note.line, note.page), (Some(1), None));

        // Mid-word, from the trigram index.
        let hits = ix.search("ebras gr", 10, false).unwrap();
        assert_eq!(hits[0].page, Some(2));

        // Exact: a row a page, the line counted on its page, and nothing for Replace All.
        let re = crate::search::pattern("zebras", Default::default()).unwrap();
        let (rows, total) = ix.grep(&re, 10, false).unwrap();
        let pdf: Vec<_> = rows.iter().filter(|m| m.rel_path == "paper.pdf").collect();
        assert_eq!(
            pdf.iter().map(|m| (m.page, m.line)).collect::<Vec<_>>(),
            [(Some(2), 1), (Some(4), 1)]
        );
        assert_eq!(total, 1, "the note's match alone is replaceable");
        assert_eq!(ix.grep_paths(&re, false).unwrap(), ["note.md"]);

        // The rest of a file's rows go on from the page after the last one listed.
        let body = ix.indexed_text("paper.pdf").unwrap().unwrap();
        let first = Index::phrase_hits_from("paper.pdf", &body, "zebras", 0, 1);
        assert_eq!((first.len(), first[0].more), (1, 1));
        let rest = Index::phrase_hits_from(
            "paper.pdf",
            &body,
            "zebras",
            first[0].at.clone().unwrap().end,
            10,
        );
        assert_eq!(rest.iter().map(|h| h.page).collect::<Vec<_>>(), [Some(4)]);
        let (rest, found) =
            Index::matches_from("paper.pdf", None, &body, &re, pdf[0].offset + 1, 10);
        assert_eq!((rest.len(), found, rest[0].page), (1, 1, Some(4)));
    }

    #[test]
    fn a_changed_pdf_loses_its_text_until_it_is_read_again() {
        let (vault, _cache, mut ix) = vault(&["paper.pdf"]);
        let claimed = ix.claim_pdfs(16).unwrap();
        ix.put_pdf_texts(&[(claimed[0].clone(), text("old words", 1))], &[])
            .unwrap();
        assert_eq!(ix.search("old", 10, false).unwrap().len(), 1);

        std::fs::write(vault.path().join("paper.pdf"), b"%PDF-1.4\n% longer now\n").unwrap();
        ix.update_file(vault.path(), "paper.pdf").unwrap();
        assert!(ix.search("old", 10, false).unwrap().is_empty());
        assert_eq!(pages(&ix, "paper.pdf"), None);
        let claimed = ix.claim_pdfs(16).unwrap();
        ix.put_pdf_texts(&[(claimed[0].clone(), text("new words", 1))], &[])
            .unwrap();
        assert_eq!(ix.search("new", 10, false).unwrap()[0].page, Some(1));
    }

    #[test]
    fn text_read_of_a_file_changed_since_its_claim_is_not_written() {
        let (vault, _cache, mut ix) = vault(&["paper.pdf"]);
        let claimed = ix.claim_pdfs(16).unwrap();
        std::fs::write(vault.path().join("paper.pdf"), b"%PDF-1.4\n% changed\n").unwrap();
        ix.update_file(vault.path(), "paper.pdf").unwrap();
        ix.put_pdf_texts(&[(claimed[0].clone(), text("stale words", 1))], &[])
            .unwrap();
        assert!(ix.search("stale", 10, false).unwrap().is_empty());
        assert_eq!(ix.unread_pdfs().unwrap(), 1, "the walk's to read again");
    }

    #[test]
    fn a_pdf_without_text_is_read_once_and_holds_nothing() {
        let (_vault, _cache, mut ix) = vault(&["scan.pdf", "other.pdf"]);
        let claimed = ix.claim_pdfs(16).unwrap();
        assert_eq!(claimed.len(), 2);
        let scan = claimed.iter().find(|p| p.rel_path == "scan.pdf").unwrap();
        let other = claimed.iter().find(|p| p.rel_path == "other.pdf").unwrap();
        // A stop came before `other.pdf` was read: it is given back.
        ix.put_pdf_texts(
            &[(scan.clone(), text("\x0c\n\x0c", 3))],
            std::slice::from_ref(other),
        )
        .unwrap();
        assert_eq!(pages(&ix, "scan.pdf"), Some(3));
        assert_eq!(ix.indexed_text("scan.pdf").unwrap(), None);
        assert_eq!(ix.unread_pdfs().unwrap(), 1);
        assert_eq!(ix.claim_pdfs(16).unwrap()[0].rel_path, "other.pdf");
        assert_eq!(
            ix.get_file("scan.pdf").unwrap().map(|f| f.kind),
            Some(FileKind::Pdf)
        );
    }

    #[test]
    fn a_claim_nobody_gave_back_is_not_claimed_again() {
        let (_vault, cache, mut ix) = vault(&["crash.pdf"]);
        assert_eq!(ix.claim_pdfs(16).unwrap().len(), 1);
        drop(ix);
        let mut ix = Index::open(&cache.path().join("index.db")).unwrap();
        assert!(ix.claim_pdfs(16).unwrap().is_empty());
        assert_eq!(pages(&ix, "crash.pdf"), Some(-1));
    }
}
