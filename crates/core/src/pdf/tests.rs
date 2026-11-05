use std::ops::Range;

mod doc;
mod foreign_ink;
mod highlights;
mod ink;
mod pages;
mod pictures;
mod reads;
mod text;
mod writer;

use super::ink::{Seg, flatten, segments_of};
use super::text::{item_offset, line_groups};
use super::*;
use crate::search::Options;

/// A two-page PDF built by hand so the tests need no fixture: a line of Helvetica on each
/// page, an internal link from page 1 to page 2, a URI link on page 2, and a two-level
/// outline. With `highlight`, page 1 also carries a `/Highlight` annotation over its line —
/// listed *before* the link in `/Annots`, which is what trips a links-by-annotation-index
/// implementation.
fn tiny_pdf(highlight: bool) -> Vec<u8> {
    let content1 = "BT /F1 24 Tf 20 40 Td (Hello accent) Tj ET";
    let content2 = "BT /F1 18 Tf 20 40 Td (Second page) Tj ET";
    // Object 6 is written either way so the numbering below never shifts; without
    // `highlight` nothing references it and pdfium never sees it.
    let annots1 = if highlight {
        "/Annots[6 0 R 7 0 R]"
    } else {
        "/Annots[7 0 R]"
    };
    let objs = vec![
        "<</Type/Catalog/Pages 2 0 R/Outlines 8 0 R>>".to_string(),
        "<</Type/Pages/Kids[3 0 R 9 0 R]/Count 2>>".to_string(),
        format!(
            "<</Type/Page/Parent 2 0 R/MediaBox[0 0 200 100]\
             /Resources<</Font<</F1 4 0 R>>>>/Contents 5 0 R{annots1}>>"
        ),
        "<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>".to_string(),
        format!(
            "<</Length {}>>stream\n{content1}\nendstream",
            content1.len()
        ),
        // /QuadPoints is in the PDF spec's order: upper-left, upper-right, lower-left,
        // lower-right, bottom-left page origin.
        "<</Type/Annot/Subtype/Highlight/Rect[18 36 140 64]\
         /QuadPoints[18 64 140 64 18 36 140 36]/C[1 1 0]/CA 1\
         /Contents(check this)/F 4>>"
            .to_string(),
        "<</Type/Annot/Subtype/Link/Rect[20 30 140 60]/Border[0 0 0]\
         /Dest[9 0 R /XYZ 0 80 0]>>"
            .to_string(),
        "<</Type/Outlines/First 12 0 R/Last 12 0 R/Count 2>>".to_string(),
        "<</Type/Page/Parent 2 0 R/MediaBox[0 0 200 100]\
         /Resources<</Font<</F1 4 0 R>>>>/Contents 10 0 R/Annots[11 0 R]>>"
            .to_string(),
        format!(
            "<</Length {}>>stream\n{content2}\nendstream",
            content2.len()
        ),
        "<</Type/Annot/Subtype/Link/Rect[10 10 100 30]/Border[0 0 0]\
         /A<</S/URI/URI(https://example.org)>>>>"
            .to_string(),
        "<</Title(Second)/Parent 8 0 R/First 13 0 R/Last 13 0 R/Count 1\
         /Dest[9 0 R /XYZ 0 80 0]>>"
            .to_string(),
        "<</Title(Child)/Parent 12 0 R/Dest[9 0 R /XYZ 0 60 0]>>".to_string(),
    ];
    pdf_of(&objs)
}

/// A PDF of these objects, numbered from 1, the first being the catalog.
fn pdf_of(objs: &[String]) -> Vec<u8> {
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

/// One 200 x 100 pt page with a sticky note another reader left — a `/Text` by Grace at
/// (150, 60)–(170, 80) with its `/Popup` and no `/AP` — and a hidden one beside it.
fn commented_pdf() -> Vec<u8> {
    pdf_of(&[
        "<</Type/Catalog/Pages 2 0 R>>".to_string(),
        "<</Type/Pages/Kids[3 0 R]/Count 1>>".to_string(),
        "<</Type/Page/Parent 2 0 R/MediaBox[0 0 200 100]/Annots[4 0 R 5 0 R 6 0 R]>>".to_string(),
        "<</Type/Annot/Subtype/Text/Rect[150 20 170 40]/Contents(Is this right?)/T(Grace)\
         /C[1 1 0]/Popup 5 0 R/F 4>>"
            .to_string(),
        "<</Type/Annot/Subtype/Popup/Rect[100 40 200 100]/Parent 4 0 R>>".to_string(),
        "<</Type/Annot/Subtype/Text/Rect[10 20 30 40]/Contents(gone)/F 2>>".to_string(),
    ])
}

/// Write the tiny PDF into a tempdir and open it, or `None` if pdfium is missing.
fn open_tiny_with(highlight: bool) -> Option<(tempfile::TempDir, PdfDoc)> {
    open_pdf(&tiny_pdf(highlight))
}

/// A document over `bytes`, read as every document is, from a file: here one with no name, which
/// goes when the document does.
fn doc_of(bytes: Vec<u8>) -> Result<PdfDoc> {
    let mut file = tempfile::tempfile().unwrap();
    std::io::Write::write_all(&mut file, &bytes).unwrap();
    PdfDoc::from_file(file)
}

/// Write these bytes into a tempdir and open them, or `None` if pdfium is missing.
fn open_pdf(bytes: &[u8]) -> Option<(tempfile::TempDir, PdfDoc)> {
    if !available() {
        eprintln!("skipping: no libpdfium in {}", library_dir().display());
        return None;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tiny.pdf");
    std::fs::write(&path, bytes).unwrap();
    Some((dir, PdfDoc::open(&path).unwrap()))
}

pub(super) fn open_tiny() -> Option<(tempfile::TempDir, PdfDoc)> {
    open_tiny_with(false)
}

/// Save the document into its own temporary directory and open the copy.
pub(super) fn reopen(dir: &tempfile::TempDir, doc: &PdfDoc) -> PdfDoc {
    let out = dir.path().join("out.pdf");
    std::fs::write(&out, doc.save().unwrap()).unwrap();
    PdfDoc::open(&out).unwrap()
}

/// Each page's text, trimmed, in the document's order.
pub(super) fn page_texts(doc: &PdfDoc) -> Vec<String> {
    (0..doc.page_count())
        .map(|p| {
            let glyphs = doc.page_text(p).unwrap();
            glyphs
                .iter()
                .map(|g| g.ch)
                .collect::<String>()
                .trim()
                .to_string()
        })
        .collect()
}

/// Object `num` of a saved file as its newest revision has it.
fn object(file: &[u8], num: u32) -> accent_pdf::Object {
    let pdf = accent_pdf::Pdf::open(std::sync::Arc::new(file.to_vec())).unwrap();
    let r = accent_pdf::Ref { num, generation: 0 };
    pdf.get(r).unwrap()
}
