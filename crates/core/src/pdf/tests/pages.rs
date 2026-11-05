//! Page edits on a blank or an opened document, its pages appended, inserted, moved, imported
//! and deleted.

use super::*;

#[test]
fn blank_pdf_is_one_page_of_the_size_it_was_asked_for() {
    if !available() {
        eprintln!("skipping: no libpdfium");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let made = |name: &str, size| {
        let path = dir.path().join(name);
        std::fs::write(&path, blank_pdf(size).unwrap()).unwrap();
        let doc = PdfDoc::open(&path).unwrap();
        assert_eq!(doc.page_count(), 1);
        doc.page_size(0).unwrap()
    };
    let (w, h) = made("sketch.pdf", A4);
    assert!(
        (w - 595.3).abs() < 1.0 && (h - 841.9).abs() < 1.0,
        "{w}x{h}"
    );
    // Anything but A4 goes through the same call: landscape and square are page sizes, not modes.
    let (w, h) = made("wide.pdf", (A4.1, A4.0));
    assert!(
        (w - 841.9).abs() < 1.0 && (h - 595.3).abs() < 1.0,
        "{w}x{h}"
    );
}

#[test]
fn an_appended_page_is_the_size_of_the_last_one_and_takes_ink() {
    if !available() {
        eprintln!("skipping: no libpdfium");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("notepad.pdf");
    let square = (400.0, 400.0);
    std::fs::write(&path, blank_pdf(square).unwrap()).unwrap();

    let mut doc = PdfDoc::open(&path).unwrap();
    doc.insert_page(1).unwrap();
    assert_eq!(doc.page_count(), 2);
    assert_eq!(doc.page_sizes().unwrap(), vec![square, square]);

    // The page a stroke lands on is the machinery the pen already uses: an appended page must be
    // no different from the one the document was created with.
    let style = InkStyle {
        width: 2.0,
        rgba: [0, 0, 0, 255],
        multiply: false,
    };
    let (area, _) = doc
        .add_ink(1, &[(10.0, 10.0), (100.0, 120.0)], style, 1.0)
        .unwrap();
    assert!(area.width() > 0.0 && area.height() > 0.0, "{area:?}");
    assert_eq!(doc.annotation_count(1).unwrap(), 1);

    // And it survives the one save path there is, which is what a reader will open.
    std::fs::write(&path, doc.save().unwrap()).unwrap();
    let reopened = PdfDoc::open(&path).unwrap();
    assert_eq!(reopened.page_sizes().unwrap(), vec![square, square]);
    assert_eq!(reopened.inks(1).unwrap().len(), 1);
}

/// Undo of a delete puts the page's own object back into the page tree, with no copy and no
/// snapshot: its link into the document and the bookmarks naming it lead to it again, in the saved
/// file too, whose update never wrote the page itself.
#[test]
fn a_deleted_page_comes_back_with_its_links_and_bookmark() {
    let Some((dir, mut doc)) = open_tiny() else {
        return;
    };
    let original = std::fs::read(dir.path().join("tiny.pdf")).unwrap();
    let targets = |doc: &PdfDoc| {
        let link = doc
            .links(0)
            .unwrap()
            .into_iter()
            .find_map(|l| match l.target {
                LinkTarget::Page { page, .. } => Some(page),
                LinkTarget::Uri(_) => None,
            });
        let marks: Vec<_> = doc.outline().unwrap().iter().map(|o| o.page).collect();
        (link, marks)
    };
    let mut ink = Ink::default();
    ink.edit_pages(&mut doc, PageEdit::delete(1)).unwrap();
    assert_eq!(page_texts(&doc), ["Hello accent"]);
    assert_eq!(targets(&doc), (None, vec![None, None]));

    ink.walk(&mut doc, false);
    let saved = doc.save().unwrap();
    for doc in [&doc, &doc_of(saved.clone()).unwrap()] {
        assert_eq!(page_texts(doc), ["Hello accent", "Second page"]);
        assert_eq!(targets(doc), (Some(1), vec![Some(1), Some(1)]));
    }
    let page = accent_pdf::Ref {
        num: 9,
        generation: 0,
    };
    let pdf = accent_pdf::Pdf::open(std::sync::Arc::new(saved.clone())).unwrap();
    assert_eq!(pdf.pages().unwrap()[1], page, "the page itself");
    let section = &saved[original.len()..];
    assert!(memchr::memmem::find(section, b"\n9 0 obj").is_none());
}

/// Moving a page moves the page itself, so the bookmark and the link that point at it follow it
/// — in the document and in the file it is saved to. A copy and a delete would leave both
/// pointing at a page that is gone.
#[test]
fn a_moved_page_keeps_its_bookmark_and_the_link_into_it() {
    let Some((dir, mut doc)) = open_tiny() else {
        return;
    };
    doc.move_page(1, 0).unwrap();
    for doc in [&doc, &reopen(&dir, &doc)] {
        assert_eq!(page_texts(doc), ["Second page", "Hello accent"]);
        let bookmarks: Vec<_> = doc.outline().unwrap().iter().map(|o| o.page).collect();
        assert_eq!(bookmarks, [Some(0), Some(0)]);
        let links = doc.links(1).unwrap();
        assert!(
            matches!(links[0].target, LinkTarget::Page { page: 0, .. }),
            "{links:?}"
        );
    }
    assert!(doc.move_page(2, 0).is_err(), "there is no third page");
}

#[test]
fn a_page_inserted_between_two_is_blank_and_sized_like_the_one_before() {
    let Some((dir, mut doc)) = open_tiny() else {
        return;
    };
    doc.insert_page(1).unwrap();
    let doc = reopen(&dir, &doc);
    assert_eq!(page_texts(&doc), ["Hello accent", "", "Second page"]);
    assert_eq!(doc.page_size(1).unwrap(), doc.page_size(0).unwrap());
    // The bookmark into the page that moved down follows it.
    assert_eq!(doc.outline().unwrap()[0].page, Some(2));
}

/// pdfium's copy of a page leaves a pop-up's `/Parent` naming the markup in the document it came
/// from, which a save follows once that document has closed. Cut loose, the copy saves, and its
/// comment still reads.
#[test]
fn an_imported_page_saves_once_its_document_has_closed() {
    let Some((dir, mut doc)) = open_tiny() else {
        return;
    };
    let (_src, source) = open_pdf(&commented_pdf()).unwrap();
    assert_eq!(doc.import_pages(&source, 1).unwrap(), 1);
    drop(source);
    let doc = reopen(&dir, &doc);
    assert_eq!(page_texts(&doc), ["Hello accent", "", "Second page"]);
    let comments = doc.comments(1).unwrap();
    let said: Vec<_> = comments.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(said, ["Is this right?"]);
}

#[test]
fn a_page_is_deleted_and_the_last_one_is_kept() {
    let Some((dir, mut doc)) = open_tiny() else {
        return;
    };
    doc.delete_page(0).unwrap();
    let mut doc = reopen(&dir, &doc);
    assert_eq!(page_texts(&doc), ["Second page"]);
    assert!(doc.delete_page(0).is_err(), "a PDF keeps one page");
    assert_eq!(doc.page_count(), 1);
}
