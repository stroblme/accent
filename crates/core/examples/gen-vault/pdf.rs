//! PDFs written object by object, with the cross-reference table a reader needs.

use super::Gen;

impl Gen {
    /// A real single-page PDF: catalog, pages, page, Helvetica font, one text stream, xref.
    pub(super) fn pdf(&mut self, name: &str) -> Vec<u8> {
        let line = self.words(6);
        let content = format!(
            "BT /F1 18 Tf 60 720 Td ({name}) Tj ET\nBT /F1 11 Tf 60 690 Td ({line}) Tj ET\n"
        );
        let objs = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>".to_string(),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
            format!("<< /Length {} >>\nstream\n{content}endstream", content.len()),
        ];
        Self::pdf_of(&objs)
    }

    /// `n` labelled A4 pages for paging, thumbnail and page-edit tests; `n` must be at least 3.
    /// No random draws are consumed, preserving the other fixtures; final bulk fill still
    /// reaches `--files`.
    ///
    /// The outline starts on page 2 so navigation can reach a page before its first bookmark:
    /// Introduction on page 2, Details beneath it on page 3, and Appendix on the last page.
    pub(super) fn pages_pdf(n: usize) -> Vec<u8> {
        // 1 the catalog, 2 the page tree, 3 the font, then each page and its text, then the
        // outline's root and its three entries.
        let kids: Vec<String> = (0..n).map(|i| format!("{} 0 R", 4 + 2 * i)).collect();
        let (root, page) = (4 + 2 * n, |p: usize| 4 + 2 * (p - 1));
        let mut objs = vec![
            format!("<< /Type /Catalog /Pages 2 0 R /Outlines {root} 0 R >>"),
            format!("<< /Type /Pages /Kids [{}] /Count {n} >>", kids.join(" ")),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
        ];
        for i in 0..n {
            let content = format!("BT /F1 48 Tf 60 700 Td (Page {}) Tj ET\n", i + 1);
            objs.push(format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Resources << /Font << /F1 3 0 R >> >> /Contents {} 0 R >>",
                5 + 2 * i
            ));
            objs.push(format!(
                "<< /Length {} >>\nstream\n{content}endstream",
                content.len()
            ));
        }
        let (intro, details, appendix) = (root + 1, root + 2, root + 3);
        objs.extend([
            format!("<< /Type /Outlines /First {intro} 0 R /Last {appendix} 0 R /Count 3 >>"),
            format!(
                "<< /Title (Introduction) /Parent {root} 0 R /Next {appendix} 0 R \
                 /First {details} 0 R /Last {details} 0 R /Count 1 /Dest [{} 0 R /XYZ 0 842 0] >>",
                page(2)
            ),
            format!(
                "<< /Title (Details) /Parent {intro} 0 R /Dest [{} 0 R /XYZ 0 842 0] >>",
                page(3)
            ),
            format!(
                "<< /Title (Appendix) /Parent {root} 0 R /Prev {intro} 0 R \
                 /Dest [{} 0 R /XYZ 0 842 0] >>",
                page(n)
            ),
        ]);
        Self::pdf_of(&objs)
    }

    /// Annotation fixture: a 400 × 300 pt page with a commented highlight by Ada, a commented
    /// underline with a link, a strike-out, Grace's sticky note and popup, a hidden note, and
    /// `/FreeText`. Only typed text has an appearance stream. A blank page and a page with
    /// Grace's two-line note follow. `ACCENT_BENCH_PDF=comments:` depends on their positions.
    pub(super) fn comments_pdf() -> Vec<u8> {
        let content = "BT /F1 14 Tf 40 250 Td (Ada highlighted this line.) Tj ET\n\
                       BT /F1 14 Tf 40 220 Td (This line is underlined, with a link.) Tj ET\n\
                       BT /F1 14 Tf 40 190 Td (This line is struck out.) Tj ET\n";
        let typed = "BT /F1 12 Tf 4 10 Td (Typed on the page.) Tj ET\n";
        let quad = |x1: u32, y0: u32| {
            let y1 = y0 + 18;
            format!("/Rect [38 {y0} {x1} {y1}] /QuadPoints [38 {y1} {x1} {y1} 38 {y0} {x1} {y0}]")
        };
        let objs = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R 15 0 R 16 0 R] /Count 3 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 400 300] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R /Annots [6 0 R 7 0 R 8 0 R 9 0 R 10 0 R 11 0 R 12 0 R 13 0 R] >>".to_string(),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
            format!("<< /Length {} >>\nstream\n{content}endstream", content.len()),
            format!("<< /Type /Annot /Subtype /Highlight {} /C [1 1 0] /T (Ada) /Contents (Worth a second look.) /F 4 >>", quad(194, 246)),
            format!("<< /Type /Annot /Subtype /Underline {} /C [0 0.6 0] /Contents (Underlined without a name.) /F 4 >>", quad(250, 216)),
            "<< /Type /Annot /Subtype /Link /Rect [222 216 246 234] /Border [0 0 0] /Dest [3 0 R /XYZ 0 300 0] >>".to_string(),
            format!("<< /Type /Annot /Subtype /StrikeOut {} /C [1 0 0] /F 4 >>", quad(176, 186)),
            "<< /Type /Annot /Subtype /Text /Rect [340 240 360 260] /Name /Comment /C [1 0.8 0] /T (Grace) /Contents (Is this right?) /Popup 11 0 R /F 4 >>".to_string(),
            "<< /Type /Annot /Subtype /Popup /Rect [300 140 400 240] /Parent 10 0 R >>".to_string(),
            "<< /Type /Annot /Subtype /Text /Rect [340 200 360 220] /Contents (Hidden.) /F 2 >>".to_string(),
            "<< /Type /Annot /Subtype /FreeText /Rect [40 80 240 110] /DA (/Helv 12 Tf 0 g) /Contents (Typed on the page.) /AP << /N 14 0 R >> /F 4 >>".to_string(),
            format!(
                "<< /Type /XObject /Subtype /Form /BBox [0 0 200 30] /Resources << /Font << /F1 4 0 R >> >> /Length {} >>\nstream\n{typed}endstream",
                typed.len()
            ),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 400 300] >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 400 300] /Annots [17 0 R] >>".to_string(),
            "<< /Type /Annot /Subtype /Text /Rect [180 140 200 160] /Name /Comment /T (Grace) /Contents (Second thoughts.\\nOn the last page.) /F 4 >>".to_string(),
        ];
        Self::pdf_of(&objs)
    }

    /// These objects as a PDF, numbered from 1 with the catalog first, and their xref.
    fn pdf_of(objs: &[String]) -> Vec<u8> {
        let mut out = Vec::with_capacity(1024);
        out.extend_from_slice(b"%PDF-1.4\n");
        let mut offsets = Vec::with_capacity(objs.len());
        for (i, o) in objs.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
        }
        let xref = out.len();
        out.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes(),
        );
        for off in &offsets {
            out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objs.len() + 1
            )
            .as_bytes(),
        );
        out
    }
}
