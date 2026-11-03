//! Our own ink through a save, its transforms, the eraser's cuts and the name that marks it ours.

use super::*;

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
    doc.add_ink(0, &[(20.0, 20.0), (60.0, 40.0), (100.0, 20.0)], red, 1.0)
        .unwrap();
    assert_eq!(doc.annotation_count(0).unwrap(), before + 1);

    let strokes = doc.inks(0).unwrap();
    assert_eq!(strokes.len(), 1, "{strokes:?}");
    let start = strokes[0].points[0];
    assert!(
        (start.0 - 20.0).abs() < 0.5 && (start.1 - 20.0).abs() < 0.5,
        "{start:?}"
    );

    // It survives the file, which is the whole point of drawing into the PDF.
    let back = reopen(&dir, &doc);
    assert_eq!(back.annotation_count(0).unwrap(), before + 1);
    assert_eq!(back.inks(0).unwrap().len(), 1);
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
    back.take_ink(0, strokes[0].at).unwrap();
    assert_eq!(back.annotation_count(0).unwrap(), before);
    assert!(back.inks(0).unwrap().is_empty());
}

/// A small zigzag drawn slowly at two pixels a point: what is stored still reaches every corner
/// and the end the pen lifted at, within a pixel.
#[test]
fn a_small_drawing_keeps_its_corners() {
    let Some((_dir, mut doc)) = open_tiny() else {
        return;
    };
    let corners = [
        (20.0, 30.0),
        (22.0, 24.0),
        (24.0, 30.0),
        (26.0, 24.0),
        (27.0, 27.0),
    ];
    // A report every tenth of a point, as a slow pen makes them.
    let mut points: Vec<(f32, f32)> = corners
        .windows(2)
        .flat_map(|w| {
            let ((ax, ay), (bx, by)) = (w[0], w[1]);
            (0..30).map(move |i| {
                let t = i as f32 / 30.0;
                (ax + t * (bx - ax), ay + t * (by - ay))
            })
        })
        .collect();
    points.push(corners[4]);
    let pen = InkStyle {
        width: 1.0,
        rgba: [0, 0, 0, 255],
        multiply: false,
    };
    let scale = 2.0;
    doc.add_ink(0, &points, pen, scale).unwrap();
    let stored = &doc.inks(0).unwrap()[0].points;
    for corner in corners {
        assert!(hit(stored, corner, 1.0 / scale), "{corner:?} in {stored:?}");
    }
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
    assert_eq!(back.inks(0).unwrap().len(), 1);
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
    doc.transform_ink(0, before.at, m).unwrap();
    let inks = doc.inks(0).unwrap();
    assert_eq!(inks.len(), 1);
    let moved = &inks[0];
    let last = doc.annotation_count(0).unwrap() - 1;
    assert_eq!(
        (moved.at, doc.cos.index_of(0, moved.at).unwrap()),
        (before.at, last)
    );
    let near = |a: f32, b: f32| (a - b).abs() < 0.05;
    assert!(near(moved.bounds.left, before.bounds.left + 10.0));
    assert!(near(moved.bounds.top, before.bounds.top + 5.0));
    assert!(near(moved.points[0].0, 30.0) && near(moved.points[0].1, 25.0));
    assert_eq!(moved.style, style);

    doc.transform_ink(0, moved.at, invert(m)).unwrap();
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
    let points = doc.inks(0).unwrap().remove(0).points;
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

/// A stroke written before the strokes were named by pdfium: one path inside its `/Rect`, without
/// an `/InkList`, a name or an author.
fn unnamed_ink_pdf() -> Vec<u8> {
    let stream = "0 0 1 RG 2 w 1 J 1 j 20 80 m 60 60 l S";
    pdf_of(&[
        "<</Type/Catalog/Pages 2 0 R>>".to_string(),
        "<</Type/Pages/Kids[3 0 R]/Count 1>>".to_string(),
        "<</Type/Page/Parent 2 0 R/MediaBox[0 0 200 100]/Annots[4 0 R]>>".to_string(),
        "<</Type/Annot/Subtype/Ink/Rect[17 57 63 83]/C[0 0 1]/F 4/AP<</N 5 0 R>>>>".to_string(),
        format!(
            "<</Type/XObject/Subtype/Form/BBox[17 57 63 83]/Length {}>>stream\n{stream}\nendstream",
            stream.len()
        ),
    ])
}

/// Our strokes are ours by their `/NM`, `accent-` and their object number, though they carry an
/// `/InkList` as another editor's do; one from before they were named is ours by its shape. Ours
/// is drawn again where Select moves it, into its own object, under our name.
#[test]
fn our_stroke_is_ours_by_its_name() {
    let Some((_dir, mut doc)) = open_tiny() else {
        return;
    };
    let pen = InkStyle {
        width: 2.0,
        rgba: [0, 0, 255, 255],
        multiply: false,
    };
    doc.add_ink(0, &[(20.0, 20.0), (60.0, 40.0)], pen, 1.0)
        .unwrap();
    let pdf = accent_pdf::Pdf::open(std::sync::Arc::new(doc.save().unwrap())).unwrap();
    let page = pdf.get(pdf.pages().unwrap()[0]).unwrap();
    let annots = page.as_dict().unwrap().get(b"Annots").unwrap().clone();
    let annots = match annots {
        accent_pdf::Object::Ref(list) => pdf.get(list).unwrap(),
        direct => direct,
    };
    let stroke = annots
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .reference()
        .unwrap();
    let dict = pdf.get(stroke).unwrap().as_dict().unwrap().clone();
    let name = format!("accent-{}", stroke.num).into_bytes();
    assert_eq!(dict.get(b"NM"), Some(&accent_pdf::Object::String(name)));
    assert!(dict.get(b"InkList").is_some());

    // Another editor's would keep its dictionary, and gain no name.
    let drawn_again = |doc: &mut PdfDoc| {
        let at = doc.inks(0).unwrap()[0].at;
        doc.transform_ink(0, at, [1.0, 0.0, 0.0, 1.0, 5.0, 0.0])
            .unwrap();
        let name = format!("accent-{}", at.num).into_bytes();
        let named = doc.cos.dict(at).unwrap().get(b"NM").cloned();
        (doc.inks(0).unwrap()[0].at == at) && named == Some(accent_pdf::Object::String(name))
    };
    assert!(drawn_again(&mut doc), "ours, drawn again");
    let (_old, mut old) = open_pdf(&unnamed_ink_pdf()).unwrap();
    assert!(old.inks(0).unwrap()[0].cuttable);
    assert!(drawn_again(&mut old), "ours from before");
}

#[test]
fn thin_catmull_rom_and_hit() {
    // Thinning keeps the ends and drops what sits inside the step.
    let pts = [(0.0, 0.0), (0.5, 0.0), (3.0, 0.0)];
    assert_eq!(thin(&pts, 1.0), vec![(0.0, 0.0), (3.0, 0.0)]);
    // The end stays even within the step, or a stroke ends short of where the pen lifted.
    let pts = [(0.0, 0.0), (1.0, 0.0), (1.5, 0.0)];
    assert_eq!(thin(&pts, 1.0), pts);
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
    let cut = doc.cut_ink(0, drawn.at, (100.0, 20.0), (100.0, 80.0), 4.0);
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
    let at = inks[0].at;
    assert!(
        doc.cut_ink(0, at, (0.0, 0.0), (10.0, 0.0), 4.0)
            .unwrap()
            .is_none()
    );
    assert_eq!(doc.annotation_count(0).unwrap(), before + 2);
}

/// A stroke of ours turned a quarter about its middle is drawn again along the turned path, its
/// `/Rect` made again round it, in the file as in the document; Undo turns it back and Redo
/// turns it again.
#[test]
fn a_turned_stroke_is_drawn_along_the_turned_path() {
    let Some((dir, mut doc)) = open_tiny() else {
        return;
    };
    let style = InkStyle {
        width: 2.0,
        rgba: [0, 0, 255, 255],
        multiply: false,
    };
    let line = Shape::Line {
        a: (20.0, 30.0),
        b: (60.0, 30.0),
    };
    doc.add_shape(0, line, style).unwrap();
    let mut ink = Ink::default();
    let (id, _) = ink.named(&doc, 0).remove(0);
    let m = rotation((40.0, 30.0), 90.0);
    let at = ink.at(id).unwrap();
    doc.transform_ink(0, at, m).unwrap();
    ink.moved(0, at, m);
    let near = |a: (f32, f32), b: (f32, f32)| (a.0 - b.0).abs() < 0.05 && (a.1 - b.1).abs() < 0.05;
    let ends = |doc: &PdfDoc| {
        let i = doc.inks(0).unwrap().remove(0);
        (i.points[0], i.points[i.points.len() - 1], i.bounds)
    };
    // Clockwise as the page is read: the left end goes up.
    let (first, last, b) = ends(&doc);
    assert!(
        near(first, (40.0, 10.0)) && near(last, (40.0, 50.0)),
        "{first:?} {last:?}"
    );
    assert!(
        near((b.left, b.top), (38.0, 8.0)) && near((b.right, b.bottom), (42.0, 52.0)),
        "{b:?}"
    );
    let (first, last, _) = ends(&reopen(&dir, &doc));
    assert!(
        near(first, (40.0, 10.0)) && near(last, (40.0, 50.0)),
        "saved turned"
    );

    ink.walk(&mut doc, false);
    let (first, last, _) = ends(&doc);
    assert!(
        near(first, (20.0, 30.0)) && near(last, (60.0, 30.0)),
        "{first:?} {last:?}"
    );
    ink.walk(&mut doc, true);
    let (first, last, _) = ends(&doc);
    assert!(
        near(first, (40.0, 10.0)) && near(last, (40.0, 50.0)),
        "{first:?} {last:?}"
    );
}

/// What `take_ink` keeps is enough for `redraw_ink` to put the same stroke back, which is what an
/// erase, and the undo of one, both come down to.
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
    doc.add_ink(0, &[(20.0, 20.0), (60.0, 40.0), (100.0, 20.0)], style, 1.0)
        .unwrap();
    let drawn = doc.inks(0).unwrap().remove(0);
    let (kept, area) = doc.take_ink(0, drawn.at).unwrap();
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
    let place = doc.cos.index_of(0, back.at).unwrap();
    assert_eq!(
        (back.at, place),
        (drawn.at, before),
        "the same object, at the end"
    );
    assert_eq!(back.style, drawn.style);
    assert!(near(back.bounds, drawn.bounds), "{:?}", back.bounds);
    assert_eq!(back.points.len(), drawn.points.len());

    // Only a drawn path can be taken: the highlight is refused, and still there.
    let highlight = doc.cos.annots(0).unwrap()[0].reference().unwrap();
    assert!(doc.take_ink(0, highlight).is_err());
    assert_eq!(doc.annotation_count(0).unwrap(), before + 1);
}
