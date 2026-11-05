//! Highlights and comments, read from a file and saved into one.

use super::*;

#[test]
fn exported_highlights_read_back_after_a_reopen() {
    let Some((dir, mut doc)) = open_tiny() else {
        return;
    };
    let hl = Highlight {
        page: 0,
        quads: vec![Rect {
            left: 18.0,
            top: 36.0,
            right: 140.0,
            bottom: 64.0,
        }],
        color: [255, 255, 0, 255],
        contents: Some("check this".to_string()),
    };
    assert_eq!(doc.add_highlights(std::slice::from_ref(&hl)).unwrap(), 1);
    // The same quads again are the export the reader asked for twice.
    assert_eq!(doc.add_highlights(std::slice::from_ref(&hl)).unwrap(), 0);

    let back = reopen(&dir, &doc);
    let hls = back.highlights().unwrap();
    assert_eq!(hls.len(), 1, "{hls:?}");
    assert_eq!(hls[0].page, 0);
    assert_eq!(hls[0].contents.as_deref(), Some("check this"));
    assert_eq!(hls[0].color, [255, 255, 0, 255]);
    assert!(same_quads(&hls[0].quads, &hl.quads), "{:?}", hls[0].quads);
    // And it tints the page, the way the fixture's own highlight does.
    let img = back.render_page(0, 1.0, Theme::Plain).unwrap();
    let i = ((50 * img.width + 80) * 4) as usize;
    let px = [img.data[i], img.data[i + 1], img.data[i + 2]];
    assert!(
        px[0] > 200 && px[1] > 200 && px[2] < 120,
        "yellowish: {px:?}"
    );

    // A page-level read sees the same one, and the other page has none.
    assert_eq!(back.highlights_on(0).unwrap().len(), 1);
    assert!(back.highlights_on(1).unwrap().is_empty());
}

/// Export as PDF: the copy carries the ink and the note highlights, and the document it was made
/// from gains nothing.
#[test]
fn a_copy_carries_the_highlights_and_leaves_the_document_alone() {
    let Some((_dir, mut doc)) = open_tiny_with(true) else {
        return;
    };
    let pen = InkStyle {
        width: 2.0,
        rgba: [0, 0, 255, 255],
        multiply: false,
    };
    doc.add_ink(0, &[(20.0, 20.0), (60.0, 40.0)], pen, 1.0)
        .unwrap();
    let hl = Highlight {
        page: 1,
        quads: vec![Rect {
            left: 18.0,
            top: 50.0,
            right: 120.0,
            bottom: 66.0,
        }],
        color: [53, 132, 228, 255],
        contents: None,
    };

    let copy = doc_of(doc.copy_with_highlights(&[hl]).unwrap()).unwrap();
    assert_eq!(copy.page_count(), doc.page_count());
    assert_eq!(copy.highlights().unwrap().len(), 2);
    assert_eq!(copy.inks(0).unwrap().len(), 1);
    assert_eq!(doc.highlights().unwrap().len(), 1);
    // Nothing to add is the document as it stands.
    assert_eq!(doc.copy_with_highlights(&[]).unwrap(), doc.save().unwrap());
    let plain = doc_of(doc.copy_with_highlights(&[]).unwrap()).unwrap();
    assert_eq!(plain.highlights().unwrap().len(), 1);
    assert_eq!(plain.inks(0).unwrap().len(), 1);
}

/// Our own highlight has its appearance stream in the file at once, saved before anything has
/// rendered it, and keeps it once something has: filled in its colour, multiplied onto the page.
#[test]
fn an_exported_highlight_is_saved_with_its_appearance() {
    let Some((_dir, mut doc)) = open_tiny() else {
        return;
    };
    let quads = vec![Rect {
        left: 18.0,
        top: 36.0,
        right: 140.0,
        bottom: 64.0,
    }];
    let highlight = Highlight {
        page: 0,
        quads,
        color: [255, 255, 0, 255],
        contents: Some("Hello accent".to_string()),
    };
    doc.add_highlights(&[highlight]).unwrap();
    let ours = |file: Vec<u8>| {
        let pdf = accent_pdf::Pdf::open(std::sync::Arc::new(file)).unwrap();
        let page = pdf.get(pdf.pages().unwrap()[0]).unwrap();
        let annots = match page.as_dict().unwrap().get(b"Annots").unwrap() {
            accent_pdf::Object::Ref(list) => pdf.get(*list).unwrap(),
            direct => direct.clone(),
        };
        let mark = annots
            .as_array()
            .unwrap()
            .last()
            .unwrap()
            .reference()
            .unwrap();
        let mark = pdf.get(mark).unwrap().as_dict().unwrap().clone();
        let Some(accent_pdf::Object::Dict(ap)) = mark.get(b"AP") else {
            return false;
        };
        let Some(accent_pdf::Object::Ref(normal)) = ap.get(b"N") else {
            return false;
        };
        let accent_pdf::Object::Stream(drawn) = pdf.get(*normal).unwrap() else {
            return false;
        };
        let drawn = String::from_utf8(drawn.raw).unwrap();
        drawn.contains("1 1 0 rg") && drawn.contains("\nf\n")
    };
    assert!(ours(doc.save().unwrap()));
    doc.render_tile(0, 2.0, 0, 0, 100, 100, Theme::Plain)
        .unwrap();
    assert!(ours(doc.save().unwrap()));
}

#[test]
fn reads_existing_highlight_annotations() {
    let Some((_d, doc)) = open_tiny_with(true) else {
        return;
    };
    let hls = doc.highlights().unwrap();
    assert_eq!(hls.len(), 1, "{hls:?}");
    let hl = &hls[0];
    assert_eq!(hl.page, 0);
    assert_eq!(hl.contents.as_deref(), Some("check this"));
    assert_eq!(hl.color, [255, 255, 0, 255], "yellow /C [1 1 0]");
    assert_eq!(hl.quads.len(), 1, "one /QuadPoints quad");
    // Page is 100pt tall; the quad spans y 36..64 bottom-up, so 36..64 top-down becomes
    // top = 100 - 64 = 36, bottom = 100 - 36 = 64.
    let q = hl.quads[0];
    assert!(
        (q.left - 18.0).abs() < 0.5 && (q.right - 140.0).abs() < 0.5,
        "{q:?}"
    );
    assert!(
        (q.top - 36.0).abs() < 0.5 && (q.bottom - 64.0).abs() < 0.5,
        "{q:?}"
    );
    assert_eq!(
        q.quad_corners(),
        [
            (q.left, q.top),
            (q.right, q.top),
            (q.left, q.bottom),
            (q.right, q.bottom)
        ]
    );
    // A highlight must actually tint the render.
    let img = doc.render_page(0, 1.0, Theme::Plain).unwrap();
    let px = |x: u32, y: u32| {
        let i = ((y * img.width + x) * 4) as usize;
        [img.data[i], img.data[i + 1], img.data[i + 2]]
    };
    let inside = px(80, 50);
    assert!(
        inside[0] > 200 && inside[1] > 200 && inside[2] < 120,
        "yellowish: {inside:?}"
    );
}

/// Reading highlights *after* rendering used to segfault: the render makes pdfium synthesise
/// an appearance stream, after which pdfium-render's colour accessors take a fallback that
/// casts the annotation handle to a page-object handle. See [`annotation_color`].
#[test]
fn highlights_survive_a_prior_render() {
    let Some((_d, doc)) = open_tiny_with(true) else {
        return;
    };
    doc.render_page(0, 2.0, Theme::Plain).unwrap();
    let hls = doc.highlights().unwrap();
    assert_eq!(hls.len(), 1);
    assert_eq!(
        hls[0].color,
        [255, 255, 0, 255],
        "still yellow after a render"
    );
    assert_eq!(hls[0].quads.len(), 1);
    // And again, now that the appearance stream definitely exists.
    assert_eq!(doc.highlights().unwrap(), hls);
}

/// A highlight's `/Contents` is a comment over its quads; the link beside it is none.
#[test]
fn a_highlight_with_contents_is_a_comment() {
    let Some((_d, doc)) = open_tiny_with(true) else {
        return;
    };
    let comments = doc.comments(0).unwrap();
    assert_eq!(comments.len(), 1, "{comments:?}");
    assert_eq!(comments[0].text, "check this");
    assert_eq!(comments[0].author, None);
    let [q] = comments[0].areas[..] else {
        panic!("{comments:?}");
    };
    assert!(
        (q.left - 18.0).abs() < 0.5 && (q.top - 36.0).abs() < 0.5 && (q.bottom - 64.0).abs() < 0.5,
        "{q:?}"
    );
    assert!(doc.comments(1).unwrap().is_empty());
}

/// A sticky note is one comment by its author over its `/Rect`, which pdfium paints an icon in
/// though the note has no appearance stream; its popup and a hidden note are not comments.
#[test]
fn a_sticky_note_is_a_comment_by_its_author() {
    let Some((_d, doc)) = open_pdf(&commented_pdf()) else {
        return;
    };
    let comments = doc.comments(0).unwrap();
    assert_eq!(comments.len(), 1, "{comments:?}");
    assert_eq!(comments[0].text, "Is this right?");
    assert_eq!(comments[0].author.as_deref(), Some("Grace"));
    let [r] = comments[0].areas[..] else {
        panic!("{comments:?}");
    };
    assert!(
        (r.left - 150.0).abs() < 0.5 && (r.top - 60.0).abs() < 0.5 && (r.bottom - 80.0).abs() < 0.5,
        "{r:?}"
    );
    let img = doc.render_page(0, 1.0, Theme::Plain).unwrap();
    let inked = (60..80).any(|y: u32| {
        (150..170).any(|x: u32| {
            let i = ((y * img.width + x) * 4) as usize;
            img.data[i..i + 3].iter().any(|&c| c < 200)
        })
    });
    assert!(inked, "pdfium paints the note's icon");
}
