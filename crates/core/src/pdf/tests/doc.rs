//! Opening a document and reading back its file state, geometry, links, outline, information
//! and renders.

use super::*;

/// pdfium reads a document from its file as it needs it, so a file written into in place — as
/// pdflatex writes its output — is no longer the document that was opened, while one replaced by
/// a rename, as every save here is, leaves it reading the file it opened.
#[test]
fn a_document_knows_when_its_file_is_written_into() {
    let Some((dir, doc)) = open_tiny() else {
        return;
    };
    let path = dir.path().join("tiny.pdf");
    assert!(doc.intact());
    let beside = dir.path().join("beside.pdf");
    std::fs::write(&beside, tiny_pdf(true)).unwrap();
    std::fs::rename(&beside, &path).unwrap();
    assert!(doc.intact(), "a rename leaves the opened file alone");
    let doc = PdfDoc::open(&path).unwrap();
    std::fs::write(&path, tiny_pdf(false)).unwrap();
    assert!(!doc.intact(), "written into in place");
}

#[test]
fn opens_and_reports_geometry() {
    let Some((_d, doc)) = open_tiny() else { return };
    assert_eq!(doc.page_count(), 2);
    let (w, h) = doc.page_size(0).unwrap();
    assert!(
        (w - 200.0).abs() < 0.5 && (h - 100.0).abs() < 0.5,
        "{w}x{h}"
    );
}

#[test]
fn a_pdf_date_reads_with_or_without_its_parts() {
    assert_eq!(parse_date("D:20231015143000+02'00'"), Some(1_697_373_000));
    assert_eq!(parse_date("D:20231015143000Z"), Some(1_697_380_200));
    assert_eq!(parse_date("D:20231015163000+0200"), Some(1_697_380_200));
    assert_eq!(parse_date("D:2023"), Some(1_672_531_200));
    assert_eq!(parse_date("20231015"), Some(1_697_328_000));
    assert_eq!(parse_date("D:19991231235959-00'00'"), Some(946_684_799));
    for garbage in [
        "",
        "yesterday",
        "D:202",
        "D:20231345",
        "D:2023101525",
        "D:2023x",
    ] {
        assert_eq!(parse_date(garbage), None, "{garbage:?}");
    }
}

/// The information dictionary as a document carries it, blank entries and a date with a zone
/// among them.
#[test]
fn a_documents_info_is_its_dictionary() {
    if !available() {
        eprintln!("skipping: no libpdfium");
        return;
    }
    let objs = [
        "<</Type/Catalog/Pages 2 0 R>>".to_string(),
        "<</Type/Pages/Kids[3 0 R]/Count 1>>".to_string(),
        "<</Type/Page/Parent 2 0 R/MediaBox[0 0 200 100]>>".to_string(),
        "<</Title(Field Notes)/Author( )/Creator(Writer)\
         /CreationDate(D:20231015143000+02'00')>>"
            .to_string(),
    ];
    let bytes = String::from_utf8(pdf_of(&objs)).unwrap();
    let bytes = bytes.replace("/Root 1 0 R>>", "/Root 1 0 R/Info 4 0 R>>");
    let doc = doc_of(bytes.into_bytes()).unwrap();
    let info = doc.info();
    assert_eq!(info.title.as_deref(), Some("Field Notes"));
    assert_eq!(info.author, None);
    assert_eq!(info.creator.as_deref(), Some("Writer"));
    assert_eq!(info.producer, None);
    assert_eq!(info.created, Some(1_697_373_000));
    assert_eq!(info.version.as_deref(), Some("1.4"));
}

/// pdfium aborts the process if two threads call into it at once; the lock in this module is
/// the only thing preventing that, since pdfium-render 0.9 no longer serialises calls itself.
#[test]
fn concurrent_use_does_not_abort() {
    let Some((dir, doc)) = open_tiny() else {
        return;
    };
    let path = dir.path().join("tiny.pdf");
    std::thread::scope(|s| {
        for _ in 0..4 {
            s.spawn(|| {
                for _ in 0..5 {
                    let other = PdfDoc::open(&path).unwrap();
                    assert_eq!(other.page_count(), 2);
                    doc.render_page(0, 1.0, adwaita_dark()).unwrap();
                    assert!(!doc.page_text(0).unwrap().is_empty());
                    doc.highlights().unwrap();
                }
            });
        }
    });
}

#[test]
fn render_tile_matches_the_full_render() {
    let Some((_d, doc)) = open_tiny_with(true) else {
        return;
    };
    // 200x100pt at 2 px/pt is a 400x200 px page.
    let full = doc.render_page(0, 2.0, Theme::Plain).unwrap();
    let (x, y, w, h) = (60u32, 40u32, 120u32, 80u32);
    let tile = doc
        .render_tile(0, 2.0, x as i32, y as i32, w as i32, h as i32, Theme::Plain)
        .unwrap();
    assert_eq!((tile.width, tile.height), (w, h));
    // Byte-identical, not approximate: the tile is the same render translated by a whole
    // number of pixels, so pdfium rasterises it onto the same device grid.
    for row in 0..h {
        let src = (((y + row) * full.width + x) * 4) as usize;
        let dst = ((row * w) * 4) as usize;
        let n = (w * 4) as usize;
        assert_eq!(
            &full.data[src..src + n],
            &tile.data[dst..dst + n],
            "row {row}"
        );
    }
    // A tile running off the page keeps only what is left of it; one starting off it fails.
    let clamped = doc
        .render_tile(0, 2.0, 380, 190, 100, 100, Theme::Plain)
        .unwrap();
    assert_eq!((clamped.width, clamped.height), (20, 10));
    assert!(
        doc.render_tile(0, 2.0, 400, 0, 10, 10, Theme::Plain)
            .is_err()
    );
}

/// A retained render page must follow edits, and switching pages must not reuse another page's
/// content. Compare with a fresh reader of the saved bytes after each change.
#[test]
fn rendered_pages_follow_edits_and_page_switches() {
    let Some((_dir, mut doc)) = open_tiny() else {
        return;
    };
    let render = |doc: &PdfDoc, page| {
        doc.render_tile(page, 2.0, 0, 0, 400, 200, Theme::Plain)
            .unwrap()
            .data
    };
    let before = render(&doc, 0);
    let style = InkStyle {
        width: 5.0,
        rgba: [255, 0, 0, 255],
        multiply: false,
    };
    let (_, at) = doc
        .add_ink(0, &[(20.0, 20.0), (80.0, 20.0)], style, 1.0)
        .unwrap();
    let after = render(&doc, 0);
    assert_ne!(before, after);
    let fresh = doc_of(doc.save().unwrap()).unwrap();
    assert_eq!(after, render(&fresh, 0));
    assert_eq!(render(&doc, 1), render(&fresh, 1));
    assert_eq!(after, render(&doc, 0));
    doc.take_ink(0, at).unwrap();
    assert_eq!(before, render(&doc, 0));
}

#[test]
fn links_read_dest_and_uri() {
    let Some((_d, doc)) = open_tiny_with(true) else {
        return;
    };
    // Page 1 lists the highlight before the link, which is what used to yield the link twice.
    let first = doc.links(0).unwrap();
    assert_eq!(first.len(), 1, "{first:?}");
    // /Rect[20 30 140 60] on a 100pt page: top = 100 - 60, bottom = 100 - 30.
    let r = first[0].rect;
    assert!(
        (r.left - 20.0).abs() < 0.5 && (r.top - 40.0).abs() < 0.5 && (r.bottom - 70.0).abs() < 0.5,
        "{r:?}"
    );
    // /Dest [page 2 /XYZ 0 80 0] on a 100pt page: top-left y = 100 - 80 = 20.
    let LinkTarget::Page { page, top } = first[0].target else {
        panic!("{:?}", first[0].target);
    };
    assert_eq!(page, 1);
    assert!(top.is_some_and(|t| (t - 20.0).abs() < 0.5), "{top:?}");

    let second = doc.links(1).unwrap();
    assert_eq!(second.len(), 1, "{second:?}");
    assert_eq!(
        second[0].target,
        LinkTarget::Uri("https://example.org".to_string())
    );
}

#[test]
fn outline_is_flat_with_depths() {
    let Some((_d, doc)) = open_tiny() else { return };
    let got: Vec<_> = doc
        .outline()
        .unwrap()
        .into_iter()
        .map(|o| (o.title, o.depth, o.page))
        .collect();
    assert_eq!(
        got,
        vec![
            ("Second".to_string(), 0, Some(1)),
            ("Child".to_string(), 1, Some(1)),
        ]
    );
}

// The Adwaita dark view background with its foreground, the pair a dark theme renders onto.
const ADWAITA: ([u8; 3], [u8; 3]) = ([0x1d, 0x1d, 0x20], [0xeb, 0xeb, 0xeb]);

fn adwaita_dark() -> Theme {
    Theme::Recolour {
        paper: ADWAITA.0,
        ink: ADWAITA.1,
    }
}
