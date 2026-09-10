use std::ops::Range;

use super::ink::{Seg, flatten, segments_of};
use super::text::{item_offset, line_groups};
use super::*;

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

/// One 200 x 100 pt page carrying an `/Ink` the way another editor may write one: its
/// appearance stream draws a line from (10, 10) to (40, 40) in a box of its own, `[0 0 50 50]`,
/// which the viewer fits onto the annotation's `/Rect` at (100, 20). Read back, the line is in
/// that box's space and nowhere near where it shows.
fn foreign_ink_pdf() -> Vec<u8> {
    let stream = "1 0 0 RG 2 w 10 10 m 40 40 l S";
    pdf_of(&[
        "<</Type/Catalog/Pages 2 0 R>>".to_string(),
        "<</Type/Pages/Kids[3 0 R]/Count 1>>".to_string(),
        "<</Type/Page/Parent 2 0 R/MediaBox[0 0 200 100]/Annots[4 0 R]>>".to_string(),
        "<</Type/Annot/Subtype/Ink/Rect[100 20 150 70]/C[1 0 0]/InkList[[110 30 140 60]]\
         /AP<</N 5 0 R>>/F 4>>"
            .to_string(),
        format!(
            "<</Type/XObject/Subtype/Form/BBox[0 0 50 50]/Length {}>>stream\n{stream}\nendstream",
            stream.len()
        ),
    ])
}

/// Write the tiny PDF into a tempdir and open it, or `None` if pdfium is missing.
fn open_tiny_with(highlight: bool) -> Option<(tempfile::TempDir, PdfDoc)> {
    open_pdf(&tiny_pdf(highlight))
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

fn open_tiny() -> Option<(tempfile::TempDir, PdfDoc)> {
    open_tiny_with(false)
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
fn glyphs_are_non_empty_and_in_reading_order() {
    let Some((_d, doc)) = open_tiny() else { return };
    let glyphs = doc.page_text(0).unwrap();
    assert!(!glyphs.is_empty());
    let text: String = glyphs.iter().map(|g| g.ch).collect();
    assert!(text.contains("Hello accent"), "got {text:?}");
    // Single line of left-to-right text: boxes advance, and none is degenerate.
    let inked: Vec<_> = glyphs.iter().filter(|g| !g.ch.is_whitespace()).collect();
    assert!(inked.windows(2).all(|w| w[0].rect.left <= w[1].rect.left));
    assert!(inked.iter().all(|g| g.rect.height() > 0.0));
    // Indices are pdfium's and must line up with the vector positions we index by.
    assert!(glyphs.iter().enumerate().all(|(i, g)| g.index == i));
}

#[test]
fn text_in_rect_reads_the_line() {
    let Some((_d, doc)) = open_tiny() else { return };
    let whole = Rect {
        left: 0.0,
        top: 0.0,
        right: 200.0,
        bottom: 100.0,
    };
    assert!(doc.text_in_rect(0, whole).contains("Hello"));
}

#[test]
fn selection_link_has_the_obsidian_shape() {
    let Some((_d, doc)) = open_tiny() else { return };
    let sel = Selection {
        page: 0,
        start: 0,
        end: 40,
    };
    let glyphs = doc.page_text(0).unwrap();
    let out = selection_link(&glyphs, "notes/paper.pdf", &sel);
    // Stand-in for ^\[\[.+\.pdf#page=\d+&selection=\d+,\d+,\d+,\d+\]\]$ without a regex dep.
    let body = out
        .link
        .strip_prefix("[[")
        .and_then(|s| s.strip_suffix("]]"))
        .expect("wrapped in [[ ]]");
    let (file, rest) = body.split_once("#page=").expect("#page=");
    assert!(file.ends_with(".pdf") && !file.is_empty());
    let (page, sel_part) = rest.split_once("&selection=").expect("&selection=");
    assert!(page.parse::<u32>().is_ok(), "page {page:?}");
    let nums: Vec<_> = sel_part.split(',').collect();
    assert_eq!(nums.len(), 4, "{sel_part:?}");
    assert!(
        nums.iter().all(|n| n.parse::<usize>().is_ok()),
        "{sel_part:?}"
    );

    assert!(out.text.starts_with("Hello"), "{:?}", out.text);
    assert!(!out.quads.is_empty());
}

/// Save the document into its own temporary directory and open the copy.
fn reopen(dir: &tempfile::TempDir, doc: &PdfDoc) -> PdfDoc {
    let out = dir.path().join("out.pdf");
    std::fs::write(&out, doc.save().unwrap()).unwrap();
    PdfDoc::open(&out).unwrap()
}

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

#[test]
fn ink_round_trips_through_save() {
    let Some((dir, mut doc)) = open_tiny() else {
        return;
    };
    let before = doc.annotation_count(0).unwrap();
    let red = InkStyle {
        width: 4.0,
        rgba: [255, 0, 0, 255],
        multiply: false,
    };
    doc.add_ink(0, &[(20.0, 20.0), (60.0, 40.0), (100.0, 20.0)], red)
        .unwrap();
    assert_eq!(doc.annotation_count(0).unwrap(), before + 1);

    let strokes = doc.ink_paths(0).unwrap();
    assert_eq!(strokes.len(), 1, "{strokes:?}");
    let start = strokes[0].1[0];
    assert!(
        (start.0 - 20.0).abs() < 0.5 && (start.1 - 20.0).abs() < 0.5,
        "{start:?}"
    );

    // It survives the file, which is the whole point of drawing into the PDF.
    let back = reopen(&dir, &doc);
    assert_eq!(back.annotation_count(0).unwrap(), before + 1);
    assert_eq!(back.ink_paths(0).unwrap().len(), 1);
    // And it is drawn: the stroke passes through the middle of the page in red.
    let img = back.render_page(0, 1.0, Theme::Plain).unwrap();
    let red = img
        .data
        .as_chunks::<4>()
        .0
        .iter()
        .any(|px| px[0] > 200 && px[1] < 100 && px[2] < 100);
    assert!(red, "the stroke is drawn");

    // Erasing takes the whole stroke and leaves what was there before.
    let mut back = back;
    back.take_ink(0, strokes[0].0).unwrap();
    assert_eq!(back.annotation_count(0).unwrap(), before);
    assert!(back.ink_paths(0).unwrap().is_empty());
}

#[test]
fn shape_round_trips_through_save() {
    let Some((dir, mut doc)) = open_tiny() else {
        return;
    };
    let before = doc.annotation_count(0).unwrap();
    let red = InkStyle {
        width: 4.0,
        rgba: [255, 0, 0, 255],
        multiply: false,
    };
    let rect = Rect {
        left: 20.0,
        top: 20.0,
        right: 80.0,
        bottom: 60.0,
    };
    doc.add_shape(0, Shape::Rect(rect), red).unwrap();
    assert_eq!(doc.annotation_count(0).unwrap(), before + 1);
    let inks = doc.inks(0).unwrap();
    assert_eq!(inks.len(), 1, "{inks:?}");
    let near = |a: f32, b: f32| (a - b).abs() < 0.5;
    assert!(near(inks[0].points[0].0, 20.0) && near(inks[0].points[0].1, 20.0));
    // The closing edge is walked too, so the left side answers to the eraser.
    assert!(hit(&inks[0].points, (20.0, 40.0), 1.0));
    let b = inks[0].bounds;
    assert!(
        near(b.left, 17.0) && near(b.top, 17.0) && near(b.right, 83.0) && near(b.bottom, 63.0),
        "{b:?}"
    );
    assert_eq!(inks[0].style, red);

    let back = reopen(&dir, &doc);
    assert_eq!(back.ink_paths(0).unwrap().len(), 1);
    let img = back.render_page(0, 1.0, Theme::Plain).unwrap();
    let red = img
        .data
        .as_chunks::<4>()
        .0
        .iter()
        .any(|px| px[0] > 200 && px[1] < 100 && px[2] < 100);
    assert!(red, "the shape is drawn");
}

#[test]
fn transform_ink_moves_the_bounds_and_undoes_itself() {
    let Some((_dir, mut doc)) = open_tiny() else {
        return;
    };
    let style = InkStyle {
        width: 2.0,
        rgba: [0, 0, 255, 255],
        multiply: false,
    };
    let line = Shape::Line {
        a: (20.0, 20.0),
        b: (60.0, 20.0),
    };
    doc.add_shape(0, line, style).unwrap();
    let before = doc.inks(0).unwrap().remove(0);
    let m = [1.0, 0.0, 0.0, 1.0, 10.0, 5.0];
    doc.transform_ink(0, before.index, m).unwrap();
    let inks = doc.inks(0).unwrap();
    assert_eq!(inks.len(), 1);
    let moved = &inks[0];
    assert_eq!(moved.index, doc.annotation_count(0).unwrap() - 1);
    let near = |a: f32, b: f32| (a - b).abs() < 0.05;
    assert!(near(moved.bounds.left, before.bounds.left + 10.0));
    assert!(near(moved.bounds.top, before.bounds.top + 5.0));
    assert!(near(moved.points[0].0, 30.0) && near(moved.points[0].1, 25.0));
    assert_eq!(moved.style, style);

    doc.transform_ink(0, moved.index, invert(m)).unwrap();
    let back = doc.inks(0).unwrap().remove(0);
    assert!(near(back.bounds.left, before.bounds.left) && near(back.bounds.top, before.bounds.top));
    assert!(
        near(back.bounds.right, before.bounds.right)
            && near(back.bounds.bottom, before.bounds.bottom)
    );
}

#[test]
fn flattened_ink_hits_a_circles_rim() {
    let Some((_dir, mut doc)) = open_tiny() else {
        return;
    };
    let style = InkStyle {
        width: 2.0,
        rgba: [0, 0, 0, 255],
        multiply: false,
    };
    let circle = Shape::Circle {
        centre: (100.0, 50.0),
        radius: 30.0,
    };
    doc.add_shape(0, circle, style).unwrap();
    let (_, points) = doc.ink_paths(0).unwrap().remove(0);
    // The rim at 45°, the control polygon's corner 4 pt outside it, and the centre.
    assert!(hit(&points, (121.2, 28.8), 1.0));
    assert!(!hit(&points, (83.4, 20.0), 3.0));
    assert!(!hit(&points, (100.0, 50.0), 25.0));
}

#[test]
fn flatten_samples_a_bezier_and_a_matrix_inverts() {
    let segs = [
        Seg::Move((0.0, 0.0)),
        Seg::Bezier((0.0, 0.0), (10.0, 0.0), (10.0, 0.0)),
        Seg::Close,
    ];
    let flat = flatten(&segs);
    assert!(flat.iter().all(|p| p.1 == 0.0), "{flat:?}");
    assert_eq!(flat[flat.len() - 2], (10.0, 0.0));
    assert_eq!(flat[flat.len() - 1], (0.0, 0.0));

    let s = [2.0, 0.0, 0.0, 0.5, 3.0, 4.0];
    let p = (7.0, -2.0);
    let back = apply(invert(s), apply(s, p));
    assert!(
        (back.0 - p.0).abs() < 1e-5 && (back.1 - p.1).abs() < 1e-5,
        "{back:?}"
    );
}

#[test]
fn blank_pdf_is_one_a4_page() {
    if !available() {
        eprintln!("skipping: no libpdfium");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sketch.pdf");
    std::fs::write(&path, blank_pdf().unwrap()).unwrap();
    let doc = PdfDoc::open(&path).unwrap();
    assert_eq!(doc.page_count(), 1);
    let (w, h) = doc.page_size(0).unwrap();
    assert!(
        (w - 595.3).abs() < 1.0 && (h - 841.9).abs() < 1.0,
        "{w}x{h}"
    );
}

#[test]
fn thin_catmull_rom_and_hit() {
    // Thinning keeps the ends and drops what sits inside the step.
    let pts = [(0.0, 0.0), (0.5, 0.0), (3.0, 0.0)];
    assert_eq!(thin(&pts, 1.0), vec![(0.0, 0.0), (3.0, 0.0)]);
    // A stroke that never moved is a dot, not an empty path.
    assert_eq!(thin(&[(1.0, 2.0), (1.0, 2.0)], 1.0), vec![(1.0, 2.0)]);
    // No points is no curve, rather than an underflow.
    assert!(catmull_rom(&[]).is_empty());
    assert!(thin(&[], 1.0).is_empty());

    // Three points on a line give two segments that stay on it and end where they should.
    let segs = catmull_rom(&[(0.0, 0.0), (10.0, 0.0), (20.0, 0.0)]);
    assert_eq!(segs.len(), 2);
    assert!(segs.iter().flatten().all(|p| p.1 == 0.0), "{segs:?}");
    assert_eq!(segs[0][2], (10.0, 0.0));
    assert_eq!(segs[1][2], (20.0, 0.0));
    assert!(catmull_rom(&[(1.0, 1.0)]).is_empty());

    let line = [(0.0, 0.0), (10.0, 0.0)];
    assert!(hit(&line, (5.0, 3.0), 4.0));
    assert!(!hit(&line, (5.0, 5.0), 4.0));
    // Past the end of the segment, not just off its side.
    assert!(!hit(&line, (20.0, 0.0), 4.0));
    assert!(hit(&[(0.0, 0.0)], (2.0, 0.0), 4.0));
}

/// A drag reports once a frame, so a quick pass lands one report either side of a thin stroke
/// and neither of them near it: the line between the two is what crosses it.
#[test]
fn a_quick_pass_takes_the_stroke_it_steps_over() {
    let stroke = [(50.0, 0.0), (50.0, 100.0)];
    let (before, after) = ((40.0, 50.0), (60.0, 50.0));
    assert!(!hit(&stroke, before, 4.0) && !hit(&stroke, after, 4.0));
    assert!(swept(&stroke, before, after, 4.0));
    // Alongside it 5 pt away, and across the line it would make past its end, are both misses.
    assert!(!swept(&stroke, (45.0, 10.0), (45.0, 90.0), 4.0));
    assert!(!swept(&stroke, (40.0, 110.0), (60.0, 110.0), 4.0));
    // A dot is taken by a pass beside it, not only by one that ends on it.
    assert!(swept(&[(10.0, 10.0)], (0.0, 12.0), (20.0, 12.0), 4.0));
    assert!(!hit(&[(10.0, 10.0)], (0.0, 12.0), 4.0));
}

/// A flattened curve stays on the curve between its points as well as at them, which is what lets
/// a cut draw what it leaves as straight lines: a 100 pt circle's chords keep within a tenth of a
/// point of its rim.
#[test]
fn a_flattened_circle_stays_on_its_rim() {
    let segs = segments_of(Shape::Circle {
        centre: (0.0, 0.0),
        radius: 100.0,
    });
    let flat = flatten(&segs);
    for w in flat.windows(2) {
        let mid = ((w[0].0 + w[1].0) / 2.0, (w[0].1 + w[1].1) / 2.0);
        let off = (mid.0.hypot(mid.1) - 100.0).abs();
        assert!(off < 0.1, "{off} pt off the rim at {mid:?}");
    }
}

/// A pass across a stroke takes what lies within its reach and leaves the rest as runs of their
/// own; a pass that misses leaves the stroke alone, and a crumb is not worth keeping.
#[test]
fn a_cut_leaves_what_the_pass_did_not_cover() {
    let near = |run: &[(f32, f32)], want: &[(f32, f32)]| {
        run.len() == want.len()
            && run
                .iter()
                .zip(want)
                .all(|(a, b)| (a.0 - b.0).abs() < 0.01 && (a.1 - b.1).abs() < 0.01)
    };
    let line = [(0.0, 0.0), (100.0, 0.0)];
    // Straight down across the middle, taking 5 pt either side of the pass.
    let runs = cut(&line, (50.0, -20.0), (50.0, 20.0), 5.0).unwrap();
    assert_eq!(runs.len(), 2, "{runs:?}");
    assert!(near(&runs[0], &[(0.0, 0.0), (45.0, 0.0)]), "{runs:?}");
    assert!(near(&runs[1], &[(55.0, 0.0), (100.0, 0.0)]), "{runs:?}");
    // Along the whole of it: nothing is left. Beside it: nothing is taken.
    assert_eq!(
        cut(&line, (-10.0, 0.0), (110.0, 0.0), 5.0),
        Some(Vec::new())
    );
    assert_eq!(cut(&line, (0.0, 20.0), (100.0, 20.0), 5.0), None);
    // Half a point left past the pass at the end is a crumb, and goes with it.
    let runs = cut(&line, (96.0, -20.0), (96.0, 20.0), 3.5).unwrap();
    assert!(
        near(&runs[0], &[(0.0, 0.0), (92.5, 0.0)]) && runs.len() == 1,
        "{runs:?}"
    );

    // A closed path cut once is one piece, running on round from the cut to the cut.
    let rect = [
        (0.0, 0.0),
        (100.0, 0.0),
        (100.0, 50.0),
        (0.0, 50.0),
        (0.0, 0.0),
    ];
    let runs = cut(&rect, (50.0, -20.0), (50.0, 20.0), 5.0).unwrap();
    let round = [
        (55.0, 0.0),
        (100.0, 0.0),
        (100.0, 50.0),
        (0.0, 50.0),
        (0.0, 0.0),
        (45.0, 0.0),
    ];
    assert!(runs.len() == 1 && near(&runs[0], &round), "{runs:?}");
}

/// Cutting through pdfium: the line goes, and what is left of it comes back as two strokes of
/// its own in the same style.
#[test]
fn a_cut_line_comes_back_in_two() {
    let Some((_dir, mut doc)) = open_tiny() else {
        return;
    };
    let before = doc.annotation_count(0).unwrap();
    let style = InkStyle {
        width: 2.0,
        rgba: [0, 0, 255, 255],
        multiply: false,
    };
    let line = Shape::Line {
        a: (20.0, 50.0),
        b: (180.0, 50.0),
    };
    doc.add_shape(0, line, style).unwrap();
    let drawn = doc.inks(0).unwrap().remove(0);
    assert!(drawn.cuttable);
    // Down across x = 100 with a 4 pt eraser: it reaches the line's edge 1 pt further out.
    let cut = doc.cut_ink(0, drawn.index, (100.0, 20.0), (100.0, 80.0), 4.0);
    let cut = cut.unwrap().expect("the pass crossed the line");
    assert_eq!(cut.left.len(), 2);
    let inks = doc.inks(0).unwrap();
    assert_eq!(doc.annotation_count(0).unwrap(), before + 2);
    let ends: Vec<(f32, f32)> = inks
        .iter()
        .map(|i| (i.points[0].0, i.points[i.points.len() - 1].0))
        .collect();
    assert_eq!(inks.len(), 2, "{ends:?}");
    assert!((ends[0].0 - 20.0).abs() < 0.01 && (ends[0].1 - 95.0).abs() < 0.01);
    assert!((ends[1].0 - 105.0).abs() < 0.01 && (ends[1].1 - 180.0).abs() < 0.01);
    assert!(inks.iter().all(|i| i.style == style), "{inks:?}");
    // A pass that misses touches nothing.
    let at = inks[0].index;
    assert!(
        doc.cut_ink(0, at, (0.0, 0.0), (10.0, 0.0), 4.0)
            .unwrap()
            .is_none()
    );
    assert_eq!(doc.annotation_count(0).unwrap(), before + 2);
}

/// Ink drawn in a space of its own is read there, so a cut would draw its pieces somewhere else:
/// it is not cuttable, and a cut leaves it alone.
#[test]
fn ink_drawn_in_its_own_space_is_not_cut() {
    let Some((_dir, mut doc)) = open_pdf(&foreign_ink_pdf()) else {
        return;
    };
    let inks = doc.inks(0).unwrap();
    assert_eq!(inks.len(), 1, "{inks:?}");
    assert!(
        !inks[0].cuttable,
        "{:?} is outside {:?}",
        inks[0].points, inks[0].bounds
    );
    // Straight across the line where it was read, which a cuttable stroke would lose a piece to.
    assert!(swept(&inks[0].points, (0.0, 75.0), (60.0, 75.0), 4.0));
    let cut = doc.cut_ink(0, inks[0].index, (0.0, 75.0), (60.0, 75.0), 4.0);
    assert!(cut.unwrap().is_none());
    assert_eq!(doc.annotation_count(0).unwrap(), 1);
}

/// What `take_ink` keeps is enough for `redraw_ink` to draw the same stroke again, which is what
/// an erase, and the undo of one, both come down to.
#[test]
fn a_taken_stroke_draws_again_as_it_was() {
    // With the fixture's highlight, which sits at index 0 ahead of the link.
    let Some((_dir, mut doc)) = open_tiny_with(true) else {
        return;
    };
    let before = doc.annotation_count(0).unwrap();
    let style = InkStyle {
        width: 3.0,
        rgba: [0, 128, 0, 255],
        multiply: false,
    };
    doc.add_ink(0, &[(20.0, 20.0), (60.0, 40.0), (100.0, 20.0)], style)
        .unwrap();
    let drawn = doc.inks(0).unwrap().remove(0);
    let (kept, area) = doc.take_ink(0, drawn.index).unwrap();
    assert_eq!(doc.annotation_count(0).unwrap(), before);
    assert!(doc.inks(0).unwrap().is_empty());
    let near = |a: Rect, b: Rect| {
        [
            a.left - b.left,
            a.top - b.top,
            a.right - b.right,
            a.bottom - b.bottom,
        ]
        .iter()
        .all(|d| d.abs() < 0.01)
    };
    assert!(near(area, drawn.bounds), "{area:?} {:?}", drawn.bounds);

    doc.redraw_ink(0, &kept).unwrap();
    let back = doc.inks(0).unwrap().remove(0);
    assert_eq!(back.index, before, "drawn again at the end");
    assert_eq!(back.style, drawn.style);
    assert!(near(back.bounds, drawn.bounds), "{:?}", back.bounds);
    assert_eq!(back.points.len(), drawn.points.len());

    // Only a drawn path can be taken: the highlight is refused, and still there.
    assert!(doc.take_ink(0, 0).is_err());
    assert_eq!(doc.annotation_count(0).unwrap(), before + 1);
}

#[test]
fn selection_quads_round_trips_selection_link() {
    let Some((_d, doc)) = open_tiny() else { return };
    let glyphs = doc.page_text(0).unwrap();
    let sel = Selection {
        page: 0,
        start: 0,
        end: 12,
    };
    let out = selection_link(&glyphs, "paper.pdf", &sel);
    let anchor = out.link.split_once('#').unwrap().1.trim_end_matches("]]");
    let (page, nums) = crate::markdown::pdf_anchor(anchor).unwrap();
    assert_eq!(page, 0);

    let (range, quads) = selection_quads(&glyphs, nums.unwrap()).unwrap();
    assert_eq!(range, 0..12);
    assert!(same_quads(&quads, &out.quads), "{quads:?} {:?}", out.quads);

    // Numbers past the page's lines are a link from another engine, not a panic.
    assert!(selection_quads(&glyphs, [99, 0, 99, 3]).is_none());
    assert!(selection_quads(&glyphs, [0, 0, 0, 0]).is_none());
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

#[test]
fn search_is_case_insensitive_and_empty_safe() {
    let Some((_d, doc)) = open_tiny() else { return };
    let hits = doc.search(1, "second").unwrap();
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert!(
        hits[0].iter().any(|r| r.width() > 0.0 && r.height() > 0.0),
        "{:?}",
        hits[0]
    );
    assert!(doc.search(1, "").unwrap().is_empty(), "empty query");
    assert!(doc.search(0, "second").unwrap().is_empty(), "other page");
}

// The two pairs the app asks for: the Adwaita dark view background with its foreground, and
// Solarized light's cream paper with its slate ink.
const ADWAITA: ([u8; 3], [u8; 3]) = ([0x1d, 0x1d, 0x20], [0xeb, 0xeb, 0xeb]);
const SOLARIZED_LIGHT: ([u8; 3], [u8; 3]) = ([0xfd, 0xf6, 0xe3], [0x65, 0x7b, 0x83]);

fn adwaita_dark() -> Theme {
    Theme::Recolour {
        paper: ADWAITA.0,
        ink: ADWAITA.1,
    }
}

#[test]
fn recolour_maps_paper_and_ink_to_the_theme() {
    for (paper, ink) in [ADWAITA, SOLARIZED_LIGHT] {
        // Tolerance of 1 per channel: the ramp runs through f32 and truncates on the way out.
        let near = |got: [u8; 4], want: [u8; 3]| (0..3).all(|i| got[i].abs_diff(want[i]) <= 1);

        let white = recolour_pixel([255, 255, 255, 255], paper, ink);
        assert!(near(white, paper), "white -> {white:?}, want {paper:?}");
        assert_eq!(white[3], 255, "alpha preserved");

        let black = recolour_pixel([0, 0, 0, 255], paper, ink);
        assert!(near(black, ink), "black -> {black:?}, want {ink:?}");
    }
}

#[test]
fn recolour_keeps_a_saturated_colour_recognisable() {
    for (paper, ink) in [ADWAITA, SOLARIZED_LIGHT] {
        // A yellow highlight must remain a yellow highlight, not turn grey.
        let yellow = recolour_pixel([255, 255, 0, 255], paper, ink);
        assert!(
            yellow[0] > yellow[2] + 40 && yellow[1] > yellow[2] + 40,
            "{yellow:?} on paper {paper:?}"
        );

        let red = recolour_pixel([255, 0, 0, 255], paper, ink);
        assert!(
            red[0] > red[1] + 40 && red[0] > red[2] + 40,
            "stays reddish: {red:?} on paper {paper:?}"
        );
    }
}

#[test]
fn line_groups_split_on_a_new_line() {
    let g = |ch, left: f32, top: f32| Glyph {
        ch,
        rect: Rect {
            left,
            top,
            right: left + 5.0,
            bottom: top + 10.0,
        },
        index: 0,
    };
    let glyphs = [g('a', 0.0, 0.0), g('b', 5.0, 0.0), g('c', 0.0, 12.0)];
    assert_eq!(line_groups(&glyphs), vec![0..2, 2..3]);
    assert_eq!(item_offset(&line_groups(&glyphs), 2), (1, 0));
    assert_eq!(line_groups(&[]), Vec::<Range<usize>>::new());
}
