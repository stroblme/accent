//! A PDF's text for the index, a page at a time: through pdfium where this build links it, and
//! through poppler's `pdftotext` where it does not, which is a remote vault's static server.
//!
//! Both read the same shape: each page's text, its trailing whitespace trimmed, the pages joined by
//! form feeds (U+000C), which is how `pdftotext` writes them and how the index finds a match's page.

use std::ffi::OsStr;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::index::MAX_INDEXED_BODY;
use crate::{Error, Result};

/// The most pages of one PDF read: a scanned book of thousands holds no text to find anyway, and
/// each page costs a load.
pub const MAX_PAGES: usize = 2000;

/// How long `pdftotext` may spend on one file before it is given up on.
const DEADLINE: Duration = Duration::from_secs(60);

/// How often a running `pdftotext` is checked for a stop or its deadline.
const TICK: Duration = Duration::from_millis(50);

/// What reads a PDF's text on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Extractor {
    #[cfg(feature = "pdf")]
    Pdfium,
    /// The `pdftotext` program at this path.
    Pdftotext(PathBuf),
}

/// One PDF's text as the index holds it: the pages read, joined by form feeds, at most
/// [`MAX_INDEXED_BODY`] bytes of whole pages, and how many pages that is.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Text {
    pub body: String,
    pub pages: usize,
}

impl Text {
    /// Take one more page; `false` once no more are wanted, the caps reached. A first page past
    /// the byte cap keeps what fits of it.
    fn push(&mut self, page: &str) -> bool {
        // pdfium's mark of a word hyphenated across a line, whose line break it has taken out
        // already: dropped, the word is whole again, as pdfium's own search reads it.
        let page = (page.replace("\r\n", "\n").replace(['\r', '\x0c'], "\n")).replace('\u{2}', "");
        let page = page.trim_end();
        let sep = usize::from(self.pages > 0);
        if self.body.len() + sep + page.len() > MAX_INDEXED_BODY as usize {
            if self.pages == 0 {
                self.body
                    .push_str(&page[..page.floor_char_boundary(MAX_INDEXED_BODY as usize)]);
                self.pages = 1;
            }
            return false;
        }
        if sep == 1 {
            self.body.push('\x0c');
        }
        self.body.push_str(page);
        self.pages += 1;
        self.pages < MAX_PAGES
    }
}

/// The extractor this machine has: pdfium where this build links it and the library is found,
/// otherwise a `pdftotext` on `PATH`, otherwise none.
pub fn extractor() -> Option<Extractor> {
    #[cfg(feature = "pdf")]
    if crate::pdf::available() {
        return Some(Extractor::Pdfium);
    }
    pdftotext_on(&std::env::var_os("PATH")?)
}

/// The `pdftotext` in the first of `path`'s folders that holds one.
pub fn pdftotext_on(path: &OsStr) -> Option<Extractor> {
    std::env::split_paths(path)
        .map(|dir| dir.join("pdftotext"))
        .find(|exe| {
            std::fs::metadata(exe).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
        .map(Extractor::Pdftotext)
}

/// The text of the PDF at `path`. [`Error::Cancelled`] once `stop` says so, checked between
/// pages; any other error is a PDF that does not open (encrypted, damaged, not a PDF).
pub fn read(with: &Extractor, path: &Path, stop: &(dyn Fn() -> bool + Sync)) -> Result<Text> {
    let mut text = Text::default();
    match with {
        #[cfg(feature = "pdf")]
        Extractor::Pdfium => {
            let doc = crate::pdf::PdfDoc::open(path)?;
            for page in 0..doc.page_count() {
                if stop() {
                    return Err(Error::Cancelled);
                }
                // A page pdfium cannot load reads as blank: its neighbours are still worth having.
                if !text.push(&doc.page_plain_text(page).unwrap_or_default()) {
                    break;
                }
            }
        }
        Extractor::Pdftotext(exe) => pdftotext(exe, path, stop, &mut text)?,
    }
    Ok(text)
}

/// The pages `pdftotext` writes for `path`, taken as each one ends in its form feed, the program
/// killed on a stop, past its [`DEADLINE`] or once `text` wants no more. A page it had not finished
/// is left out.
fn pdftotext(
    exe: &Path,
    path: &Path,
    stop: &(dyn Fn() -> bool + Sync),
    text: &mut Text,
) -> Result<()> {
    let mut child = Command::new(exe)
        .args(["-q", "-enc", "UTF-8", "-l", &MAX_PAGES.to_string()])
        .arg(path)
        .arg("-")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| Error::io(exe.display(), e))?;
    let mut stdout = child.stdout.take().expect("stdout is piped");
    let child = Mutex::new(child);
    let read = AtomicBool::new(false);
    let stopped = std::thread::scope(|s| {
        let watch = s.spawn(|| {
            let start = Instant::now();
            while !read.load(Ordering::Relaxed) {
                let cancel = stop();
                if cancel || start.elapsed() > DEADLINE {
                    let _ = child.lock().unwrap_or_else(|e| e.into_inner()).kill();
                    return cancel;
                }
                std::thread::sleep(TICK);
            }
            false
        });
        let (mut pending, mut chunk) = (Vec::new(), vec![0u8; 64 * 1024]);
        'read: while let Ok(n @ 1..) = stdout.read(&mut chunk) {
            pending.extend_from_slice(&chunk[..n]);
            while let Some(end) = pending.iter().position(|&b| b == b'\x0c') {
                let page = String::from_utf8_lossy(&pending[..end]).into_owned();
                pending.drain(..=end);
                if !text.push(&page) {
                    break 'read;
                }
            }
            // A page of text past twice the cap is a page no cap can be cut from whole.
            if pending.len() > 2 * MAX_INDEXED_BODY as usize {
                text.push(&String::from_utf8_lossy(&pending));
                break;
            }
        }
        read.store(true, Ordering::Relaxed);
        watch.join().unwrap_or(false)
    });
    let mut child = child.into_inner().unwrap_or_else(|e| e.into_inner());
    // Whatever it still has to write, nobody reads.
    let _ = child.kill();
    let status = child.wait().map_err(|e| Error::io(exe.display(), e))?;
    if stopped {
        return Err(Error::Cancelled);
    }
    if !status.success() && text.pages == 0 {
        return Err(Error::Pdf(format!(
            "pdftotext cannot read {}: {status}",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A PDF of one page per entry, each holding its text in a line of Helvetica, or nothing at all
    /// for an empty one: a page a scanner made has no text either.
    fn text_pdf(pages: &[&str]) -> Vec<u8> {
        let n = pages.len();
        let kids: Vec<String> = (0..n).map(|i| format!("{} 0 R", 4 + 2 * i)).collect();
        let mut objs = vec![
            "<</Type/Catalog/Pages 2 0 R>>".to_string(),
            format!("<</Type/Pages/Kids[{}]/Count {n}>>", kids.join(" ")),
            "<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>".to_string(),
        ];
        for (i, text) in pages.iter().enumerate() {
            let content = match text.is_empty() {
                true => String::new(),
                false => format!("BT /F1 12 Tf 20 60 Td ({text}) Tj ET"),
            };
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

    /// Every extractor this machine has, so each test runs against all of them.
    fn extractors() -> Vec<Extractor> {
        let mut all = Vec::new();
        #[cfg(feature = "pdf")]
        if crate::pdf::available() {
            all.push(Extractor::Pdfium);
        }
        all.extend(std::env::var_os("PATH").and_then(|p| pdftotext_on(&p)));
        if all.is_empty() {
            eprintln!("skipping: neither libpdfium nor pdftotext is here");
        }
        all
    }

    fn write(dir: &tempfile::TempDir, name: &str, bytes: &[u8]) -> PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn pages_are_read_in_order_and_joined_by_form_feeds() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            &dir,
            "three.pdf",
            &text_pdf(&["first page", "", "third zebrafish"]),
        );
        for with in extractors() {
            let text = read(&with, &path, &|| false).unwrap();
            assert_eq!(text.pages, 3, "{with:?}");
            assert_eq!(text.body, "first page\x0c\x0cthird zebrafish", "{with:?}");
        }
    }

    #[test]
    fn a_pdf_without_text_reads_as_its_pages_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(&dir, "scan.pdf", &text_pdf(&["", ""]));
        for with in extractors() {
            let text = read(&with, &path, &|| false).unwrap();
            assert_eq!((text.pages, text.body.trim()), (2, ""), "{with:?}");
        }
    }

    #[test]
    fn a_file_that_is_no_pdf_is_an_error_and_a_stop_is_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        let junk = write(&dir, "junk.pdf", b"not a pdf at all");
        let good = write(&dir, "good.pdf", &text_pdf(&["text"]));
        for with in extractors() {
            assert!(read(&with, &junk, &|| false).is_err(), "{with:?}");
            assert_eq!(
                read(&with, &good, &|| true),
                Err(Error::Cancelled),
                "{with:?}"
            );
        }
    }

    #[test]
    fn the_caps_keep_whole_pages_and_cut_only_a_first_page() {
        let mut text = Text::default();
        let half = "a".repeat(MAX_INDEXED_BODY as usize / 2);
        assert!(text.push(&half));
        assert!(
            !text.push(&format!("{half}b")),
            "the second page no longer fits"
        );
        assert_eq!((text.pages, text.body.len()), (1, half.len()));

        let mut text = Text::default();
        assert!(!text.push(&"é".repeat(MAX_INDEXED_BODY as usize)));
        assert_eq!(text.pages, 1);
        assert!(text.body.len() <= MAX_INDEXED_BODY as usize && text.body.ends_with('é'));

        let mut text = Text::default();
        let pushed = (0..MAX_PAGES + 5).take_while(|_| text.push("p")).count();
        assert_eq!((pushed, text.pages), (MAX_PAGES - 1, MAX_PAGES));
    }

    #[test]
    fn line_ends_and_stray_form_feeds_are_folded_into_the_page() {
        let mut text = Text::default();
        text.push("one\r\ntwo\rthree\x0cfour pa\u{2}rameters  \n\n");
        assert_eq!(text.body, "one\ntwo\nthree\nfour parameters");
    }

    #[test]
    fn pdftotext_is_found_on_the_path_given_and_nowhere_else() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(pdftotext_on(dir.path().as_os_str()), None);
        let exe = write(&dir, "pdftotext", b"#!/bin/sh\n");
        assert_eq!(pdftotext_on(dir.path().as_os_str()), None, "not executable");
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            pdftotext_on(dir.path().as_os_str()),
            Some(Extractor::Pdftotext(exe))
        );
    }
}
