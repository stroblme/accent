//! The pass that reads the vault's PDFs' text into the index, behind the walk: a thread and an
//! index connection of its own, at the lowest priority, so no note waits on it (DESIGN.md,
//! Architecture, PDF text).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use accent_core::index::{Index, UnreadPdf};
use accent_core::pdf_text::{self, Extractor, Text};

use crate::{Event, Result};

/// PDFs claimed at a time: few, so a stop gives little back and another process sharing the index
/// takes its share, but not a transaction a file.
const CLAIM: usize = 16;

/// Text written in one transaction at most, so a walk's or a save's write waits on none for long.
const FLUSH: usize = 256 * 1024;

/// How often the pass says how far it has got.
const TICK: Duration = Duration::from_millis(250);

/// What a vault with PDFs and nothing to read them says, once.
const NO_READER: &str = "PDF text is not searched: pdftotext (poppler-utils) is not installed \
                         where the vault is";

/// The running pass, which reads whatever PDFs the index holds unread each time it is woken.
pub(crate) struct PdfText {
    wake: Sender<()>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl PdfText {
    /// Start the pass on `index`, its own connection, reading with `with`; it waits to be woken.
    pub(crate) fn spawn(
        index: Index,
        events: Sender<Event>,
        with: Option<Extractor>,
    ) -> Result<PdfText> {
        let (wake, woken) = channel();
        let stop = Arc::new(AtomicBool::new(false));
        let thread = std::thread::Builder::new()
            .name("accent-pdf-text".to_string())
            .spawn({
                let stop = stop.clone();
                move || run(index, &woken, &events, with.as_ref(), &stop)
            })
            .map_err(|e| accent_core::Error::io("the PDF text reader", e))?;
        Ok(PdfText {
            wake,
            stop,
            thread: Some(thread),
        })
    }

    /// What the vault's worker wakes the pass with: after a walk that finished, and after a
    /// change to a PDF.
    pub(crate) fn waker(&self) -> Sender<()> {
        self.wake.clone()
    }

    /// End the pass for as long as the vault is open: the file being read is given back unread.
    pub(crate) fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.wake.send(());
    }
}

impl Drop for PdfText {
    fn drop(&mut self) {
        self.stop();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run(
    mut index: Index,
    woken: &Receiver<()>,
    events: &Sender<Event>,
    with: Option<&Extractor>,
    stop: &AtomicBool,
) {
    lower_priority();
    let mut told = false;
    while woken.recv().is_ok() {
        // A burst of wakes is one pass.
        while woken.try_recv().is_ok() {}
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let Some(with) = with else {
            if !told && index.unread_pdfs().is_ok_and(|n| n > 0) {
                told = true;
                tracing::warn!("{NO_READER}");
                let _ = events.send(Event::Error(NO_READER.to_string()));
            }
            continue;
        };
        if let Err(e) = pass(&mut index, events, with, stop) {
            tracing::warn!("reading the vault's PDF text: {e}");
        }
    }
}

/// Read every PDF the index holds unread, saying how far it has got and, last, that it has ended.
fn pass(
    index: &mut Index,
    events: &Sender<Event>,
    with: &Extractor,
    stop: &AtomicBool,
) -> accent_core::Result<()> {
    let mut total = index.unread_pdfs()?;
    if total == 0 {
        return Ok(());
    }
    let say = |done: usize, total: usize, running| {
        let _ = events.send(Event::PdfText {
            done,
            total: total.max(done),
            running,
        });
    };
    let (mut done, mut said) = (0, Instant::now());
    say(done, total, true);
    let read = (|| -> accent_core::Result<()> {
        loop {
            let claimed = index.claim_pdfs(CLAIM)?;
            if claimed.is_empty() {
                return Ok(());
            }
            let (mut read, mut bytes): (Vec<(UnreadPdf, Text)>, usize) = (Vec::new(), 0);
            let mut claimed = claimed.into_iter();
            while let Some(pdf) = claimed.next() {
                let text = match stop.load(Ordering::Relaxed) {
                    true => Err(accent_core::Error::Cancelled),
                    false => pdf_text::read(with, &pdf.path, &|| stop.load(Ordering::Relaxed)),
                };
                let text = match text {
                    Ok(text) => text,
                    Err(accent_core::Error::Cancelled) => {
                        let back: Vec<UnreadPdf> = std::iter::once(pdf).chain(claimed).collect();
                        return index.put_pdf_texts(&read, &back);
                    }
                    // Encrypted, damaged or no PDF at all: read, and nothing to find in it.
                    Err(e) => {
                        tracing::debug!("{}: no text indexed: {e}", pdf.rel_path);
                        Text::default()
                    }
                };
                bytes += text.body.len();
                read.push((pdf, text));
                done += 1;
                if bytes >= FLUSH {
                    index.put_pdf_texts(&read, &[])?;
                    (read, bytes) = (Vec::new(), 0);
                }
                if said.elapsed() >= TICK {
                    say(done, total, true);
                    said = Instant::now();
                }
            }
            index.put_pdf_texts(&read, &[])?;
            // What changed meanwhile joins the count.
            total = done + index.unread_pdfs()?;
        }
    })();
    say(done, total, false);
    read
}

/// The pass's thread, and a `pdftotext` it starts, at the lowest priority there is.
fn lower_priority() {
    // SAFETY: plain calls on the calling thread; on Linux `PRIO_PROCESS` with 0 is the calling
    // thread alone, which a child it starts inherits.
    #[cfg(target_os = "linux")]
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, 0, 19);
    }
    #[cfg(target_os = "macos")]
    unsafe {
        libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_UTILITY, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{BUDGET, Fixture, wait_for};
    use crate::{Vault, VaultConfig};

    /// A PDF of one Helvetica line a page, written by hand as `accent_core::pdf_text`'s tests
    /// write theirs.
    fn text_pdf(pages: &[&str]) -> Vec<u8> {
        let n = pages.len();
        let kids: Vec<String> = (0..n).map(|i| format!("{} 0 R", 4 + 2 * i)).collect();
        let mut objs = vec![
            "<</Type/Catalog/Pages 2 0 R>>".to_string(),
            format!("<</Type/Pages/Kids[{}]/Count {n}>>", kids.join(" ")),
            "<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>".to_string(),
        ];
        for (i, text) in pages.iter().enumerate() {
            let content = format!("BT /F1 12 Tf 20 60 Td ({text}) Tj ET");
            objs.push(format!(
                "<</Type/Page/Parent 2 0 R/MediaBox[0 0 300 100]\
                 /Resources<</Font<</F1 3 0 R>>>>/Contents {} 0 R>>",
                5 + 2 * i
            ));
            objs.push(format!(
                "<</Length {}>>stream\n{content}\nendstream",
                content.len()
            ));
        }
        let mut out = String::from("%PDF-1.4\n");
        let mut offsets = Vec::new();
        for (i, o) in objs.iter().enumerate() {
            offsets.push(out.len());
            out.push_str(&format!("{} 0 obj\n{o}\nendobj\n", i + 1));
        }
        let xref = out.len();
        out.push_str(&format!(
            "xref\n0 {}\n0000000000 65535 f \n",
            objs.len() + 1
        ));
        for off in &offsets {
            out.push_str(&format!("{off:010} 00000 n \n"));
        }
        out.push_str(&format!(
            "trailer\n<</Size {}/Root 1 0 R>>\nstartxref\n{xref}\n%%EOF\n",
            objs.len() + 1
        ));
        out.into_bytes()
    }

    fn pages_of(vault: &Vault, query: &str) -> Vec<Option<u32>> {
        let hits = vault.search(query, 10, false).unwrap();
        hits.into_iter().map(|h| h.page).collect()
    }

    #[test]
    fn a_pdfs_pages_are_searched_once_the_pass_has_read_them_and_again_when_it_changes() {
        if accent_core::pdf_text::extractor().is_none() {
            return eprintln!("skipping: neither libpdfium nor pdftotext is here");
        }
        let root = tempfile::tempdir().unwrap();
        let pdf = root.path().join("paper.pdf");
        std::fs::write(
            &pdf,
            text_pdf(&["intro", "methods", "the zebrafish result"]),
        )
        .unwrap();
        let f = Fixture::open_dir(root, VaultConfig::default());
        let ended = |e: &Event| matches!(e, Event::PdfText { running: false, .. });
        assert!(
            matches!(
                f.wait(ended),
                Some(Event::PdfText {
                    done: 1,
                    total: 1,
                    ..
                })
            ),
            "the pass never read the PDF"
        );
        assert_eq!(pages_of(&f.vault, "zebrafish"), [Some(3)]);

        std::fs::write(&pdf, text_pdf(&["zebrafish first", "then nothing"])).unwrap();
        assert!(
            f.wait(ended).is_some(),
            "the changed PDF was not read again"
        );
        assert_eq!(pages_of(&f.vault, "zebrafish"), [Some(1)]);
        let body = f.vault.indexed_text("paper.pdf").unwrap().unwrap();
        assert_eq!(body, "zebrafish first\x0cthen nothing");
    }

    #[test]
    fn a_vault_with_nothing_to_read_its_pdfs_says_so_once_and_claims_none() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("scan.pdf"), b"%PDF-1.4\n").unwrap();
        let cache = tempfile::tempdir().unwrap();
        let db = cache.path().join("index.db");
        let mut index = Index::open(&db).unwrap();
        index.reconcile(root.path(), |_| {}).unwrap();
        let (events, heard) = channel();
        let pass = PdfText::spawn(Index::open(&db).unwrap(), events, None).unwrap();
        for _ in 0..3 {
            pass.waker().send(()).unwrap();
        }
        let said = |e: &Event| matches!(e, Event::Error(m) if m == NO_READER);
        assert!(wait_for(&heard, said, BUDGET).is_some());
        drop(pass);
        assert!(heard.try_iter().all(|e| !said(&e)), "said more than once");
        assert_eq!(index.unread_pdfs().unwrap(), 1, "nothing claimed");
    }
}
