//! Ink another editor wrote, erased, moved, turned and cut while it keeps its own look.

use super::*;

/// A 200 × 100 pt page with another editor's `/Ink`: two strokes, metadata, and a popup.
/// Its appearance stream uses `/BBox [0 0 50 50]` and a quarter-turn `/Matrix`, then fits into
/// `/Rect` at (100, 20). The `/InkList` instead uses page coordinates: red (140, 30)–(110, 60),
/// blue (110, 35)–(140, 65). Without `two_colours`, both strokes are red.
fn foreign_ink_pdf(two_colours: bool) -> Vec<u8> {
    let blue = if two_colours { "0 0 1 RG " } else { "" };
    let stream = format!("1 0 0 RG 2 w 10 10 m 40 40 l S {blue}15 40 m 45 10 l S");
    pdf_of(&[
        "<</Type/Catalog/Pages 2 0 R>>".to_string(),
        "<</Type/Pages/Kids[3 0 R]/Count 1>>".to_string(),
        "<</Type/Page/Parent 2 0 R/MediaBox[0 0 200 100]/Annots[4 0 R 6 0 R]>>".to_string(),
        "<</Type/Annot/Subtype/Ink/Rect[100 20 150 70]/C[1 0 0]\
         /InkList[[140 30 110 60][110 35 140 65]]/Contents(two lines)/T(Ada)\
         /M(D:20260930120000Z)/IT/InkHighlight/Popup 6 0 R/AP<</N 5 0 R>>/F 4>>"
            .to_string(),
        format!(
            "<</Type/XObject/Subtype/Form/BBox[0 0 50 50]/Matrix[0 1 -1 0 50 0]\
             /Length {}>>stream\n{stream}\nendstream",
            stream.len()
        ),
        "<</Type/Annot/Subtype/Popup/Rect[150 50 200 100]/Parent 4 0 R>>".to_string(),
    ])
}

/// Ink drawn in a space of its own answers to the pointer where it shows, by its `/InkList`,
/// which is in the page's own points, and not where its appearance stream draws it.
#[test]
fn ink_drawn_in_its_own_space_is_hit_where_it_shows() {
    let Some((_dir, doc)) = open_pdf(&foreign_ink_pdf(true)) else {
        return;
    };
    let inks = doc.inks(0).unwrap();
    // Across both lines where they show, and above them.
    assert!(
        swept(&inks[0].points, (100.0, 55.0), (150.0, 55.0), 2.0),
        "{:?}",
        inks[0].points
    );
    assert!(!swept(&inks[0].points, (0.0, 75.0), (60.0, 75.0), 4.0));
}

/// Which of red, green and blue stands out at a point of a page rendered at 4 px per point,
/// top-left page origin, if any does: none on the white paper.
fn hue_at(page: &RgbaImage, (x, y): (f32, f32)) -> Option<usize> {
    let at = ((y * 4.0) as usize * page.width as usize + (x * 4.0) as usize) * 4;
    let px = &page.data[at..at + 3];
    let low = px.iter().min().copied().unwrap_or(0);
    (0..3).find(|&c| px[c] > low.saturating_add(100))
}

/// A quarter of the way along each line of `foreign_ink_pdf` where it shows, top-left page
/// origin, with the channel it is drawn in: red, then blue. Clear of where the two cross.
const MARKS: [((f32, f32), usize); 2] = [((132.5, 62.5), 0), ((117.5, 57.5), 2)];

/// Another editor's stroke erased leaves `/Annots` with its popup, and Undo puts both back where
/// they were, as they were: its dictionary — popup, date and intent included — its appearance
/// stream with its `/Matrix` and `/BBox`, and its popup are the objects they were, and the page
/// is drawn as it was. Redo takes them out again.
#[test]
fn a_foreign_stroke_erased_and_undone_is_the_same_stroke() {
    let Some((_dir, mut doc)) = open_pdf(&foreign_ink_pdf(true)) else {
        return;
    };
    let saved = doc.save().unwrap();
    let drawn = doc.render_page(0, 4.0, Theme::Plain).unwrap();
    assert!(
        MARKS
            .iter()
            .all(|&(at, hue)| hue_at(&drawn, at) == Some(hue))
    );
    let mut ink = Ink::default();
    let (id, shape) = ink.named(&doc, 0).remove(0);
    assert!(!shape.cuttable, "its paths differ in colour");
    let at = ink.at(id).unwrap();
    let (was, _) = doc.take_ink(0, at).unwrap();
    ink.erased(0, at, was, Vec::new(), false);
    assert!(doc.inks(0).unwrap().is_empty());
    assert_eq!(
        doc.annotation_count(0).unwrap(),
        0,
        "the stroke and its popup"
    );
    let erased = doc.render_page(0, 4.0, Theme::Plain).unwrap();
    assert!(MARKS.iter().all(|&(at, _)| hue_at(&erased, at).is_none()));

    ink.walk(&mut doc, false);
    let again = doc.save().unwrap();
    for num in [4, 5, 6] {
        assert_eq!(object(&again, num), object(&saved, num), "object {num}");
    }
    let annots = |file: &[u8]| match object(file, 3).as_dict().unwrap().get(b"Annots") {
        Some(&accent_pdf::Object::Ref(list)) => object(file, list.num),
        list => list.cloned().unwrap(),
    };
    assert_eq!(annots(&again), annots(&saved), "where they were");
    let back = doc.render_page(0, 4.0, Theme::Plain).unwrap();
    assert!(back.data == drawn.data, "drawn as it was");
    assert_eq!(ink.named(&doc, 0).remove(0).0, id, "under its name");
    ink.walk(&mut doc, true);
    assert!(doc.inks(0).unwrap().is_empty());
}

/// Another editor's stroke moved with Select keeps its place, its appearance stream and every key
/// but its `/Rect` and `/InkList`, which the map moves; the viewer fits the stream into the moved
/// `/Rect`, so its red and blue lines show where the map put them, still turned as they were.
/// Undo moves it back to the very bytes it had.
#[test]
fn a_foreign_stroke_moves_by_its_rect_alone() {
    let Some((_dir, mut doc)) = open_pdf(&foreign_ink_pdf(true)) else {
        return;
    };
    let saved = doc.save().unwrap();
    // Halved, and moved right and down.
    let m = [0.5, 0.0, 0.0, 0.5, 60.0, 20.0];
    let mut ink = Ink::default();
    let (id, before) = ink.named(&doc, 0).remove(0);
    let at = ink.at(id).unwrap();
    let place = doc.cos.index_of(0, at).unwrap();
    doc.transform_ink(0, at, m).unwrap();
    ink.moved(0, at, m);
    assert_eq!(doc.cos.index_of(0, at).unwrap(), place, "in its place");
    assert_eq!(doc.annotation_count(0).unwrap(), 2);
    let after = doc.inks(0).unwrap().remove(0);
    let moved: Vec<(f32, f32)> = before.points.iter().map(|&q| apply(m, q)).collect();
    assert_eq!((after.at, after.points), (at, moved));
    let want = Rect::from_corners((110.0, 35.0), (135.0, 60.0));
    assert_eq!(after.bounds, want);

    let page = doc.render_page(0, 4.0, Theme::Plain).unwrap();
    for (at, hue) in MARKS {
        assert_eq!(hue_at(&page, apply(m, at)), Some(hue), "{at:?} moved");
    }
    assert_eq!(hue_at(&page, MARKS[0].0), None, "nothing left where it was");
    // Only `/Rect` and `/InkList` differ.
    let moved = doc.save().unwrap();
    assert_eq!(object(&moved, 5), object(&saved, 5));
    let keys = |file: &[u8]| {
        let mut dict = object(file, 4).as_dict().unwrap().clone();
        dict.remove(b"Rect");
        dict.remove(b"InkList");
        dict
    };
    assert_eq!(keys(&moved), keys(&saved));
    assert_ne!(object(&moved, 4), object(&saved, 4));

    ink.walk(&mut doc, false);
    assert_eq!(object(&doc.save().unwrap(), 4), object(&saved, 4));
}

/// Another editor's stroke turned with Select turns as a picture: its `/InkList` turned, its
/// appearance a copy of its stream whose `/Matrix` puts the form where the turn does, its other
/// keys and its old stream untouched. Its red and blue lines show turned, and Undo draws them
/// where they were.
#[test]
fn a_foreign_stroke_turns_as_a_picture() {
    let Some((_dir, mut doc)) = open_pdf(&foreign_ink_pdf(true)) else {
        return;
    };
    let saved = doc.save().unwrap();
    let mut ink = Ink::default();
    let (id, before) = ink.named(&doc, 0).remove(0);
    let b = before.bounds;
    let m = rotation(((b.left + b.right) / 2.0, (b.top + b.bottom) / 2.0), 90.0);
    let at = ink.at(id).unwrap();
    let place = doc.cos.index_of(0, at).unwrap();
    doc.transform_ink(0, at, m).unwrap();
    ink.moved(0, at, m);
    assert_eq!(doc.cos.index_of(0, at).unwrap(), place, "in its place");
    let after = doc.inks(0).unwrap().remove(0);
    let turned = before.points.iter().map(|&q| apply(m, q));
    for (got, want) in after.points.iter().zip(turned) {
        assert!((got.0 - want.0).abs() < 0.01 && (got.1 - want.1).abs() < 0.01);
    }
    let page = doc.render_page(0, 4.0, Theme::Plain).unwrap();
    for (at, hue) in MARKS {
        assert_eq!(hue_at(&page, apply(m, at)), Some(hue), "{at:?} turned");
    }
    assert_eq!(hue_at(&page, MARKS[0].0), None, "nothing left where it was");
    let turned = doc.save().unwrap();
    assert_eq!(object(&turned, 5), object(&saved, 5), "its own stream");
    let keys = |file: &[u8]| {
        let mut dict = object(file, 4).as_dict().unwrap().clone();
        for key in [&b"Rect"[..], b"InkList", b"AP"] {
            dict.remove(key);
        }
        dict
    };
    assert_eq!(keys(&turned), keys(&saved));

    ink.walk(&mut doc, false);
    let page = doc.render_page(0, 4.0, Theme::Plain).unwrap();
    for (at, hue) in MARKS {
        assert_eq!(hue_at(&page, at), Some(hue), "{at:?} back");
    }
}

/// Another editor's stroke of one colour is cut along its `/InkList`, where it shows: each line
/// the pass crossed is left as two pieces of ours in its colour and width, the original leaves
/// with its popup, and Undo puts both back as they were.
#[test]
fn a_foreign_stroke_is_cut_along_its_ink_list() {
    let Some((_dir, mut doc)) = open_pdf(&foreign_ink_pdf(false)) else {
        return;
    };
    let saved = doc.save().unwrap();
    let mut ink = Ink::default();
    let (id, shape) = ink.named(&doc, 0).remove(0);
    assert!(shape.cuttable);
    let at = ink.at(id).unwrap();
    // Across both lines, between the marks and where they cross.
    let cut = doc.cut_ink(0, at, (100.0, 50.0), (150.0, 50.0), 2.0);
    let cut = cut.unwrap().expect("the pass crossed both lines");
    assert!(matches!(cut.was, Drawn::Foreign { .. }));
    assert_eq!(cut.left.len(), 4);
    let left = cut.left.into_iter().map(|piece| (fresh_id(), piece));
    ink.erased(0, at, cut.was, left.collect(), false);
    let inks = doc.inks(0).unwrap();
    assert_eq!(inks.len(), 4);
    for piece in &inks {
        assert_eq!(piece.style.rgba, [255, 0, 0, 255]);
        assert!((piece.style.width - 2.0).abs() < 0.01, "{:?}", piece.style);
    }

    ink.walk(&mut doc, false);
    assert_eq!(doc.annotation_count(0).unwrap(), 2);
    let again = doc.save().unwrap();
    for num in [4, 5, 6] {
        assert_eq!(object(&again, num), object(&saved, num), "object {num}");
    }
}

/// Another editor's stroke turned by Select at an angle other than a quarter keeps its width, and
/// so do the pieces the eraser cuts from it.
#[test]
fn a_foreign_stroke_turned_keeps_its_width() {
    let Some((_dir, mut doc)) = open_pdf(&foreign_ink_pdf(false)) else {
        return;
    };
    let before = doc.inks(0).unwrap().remove(0);
    assert!(
        (before.style.width - 2.0).abs() < 0.01,
        "{:?}",
        before.style
    );
    let b = before.bounds;
    let m = rotation(((b.left + b.right) / 2.0, (b.top + b.bottom) / 2.0), 45.0);
    doc.transform_ink(0, before.at, m).unwrap();
    let turned = doc.inks(0).unwrap().remove(0);
    assert!(
        (turned.style.width - 2.0).abs() < 0.01,
        "{:?}",
        turned.style
    );
    // Across both lines, as `a_foreign_stroke_is_cut_along_its_ink_list` cuts them unturned.
    let (from, to) = (apply(m, (100.0, 50.0)), apply(m, (150.0, 50.0)));
    let cut = doc.cut_ink(0, turned.at, from, to, 2.0).unwrap();
    assert_eq!(cut.expect("the pass crossed both lines").left.len(), 4);
    for piece in doc.inks(0).unwrap() {
        assert!((piece.style.width - 2.0).abs() < 0.01, "{:?}", piece.style);
    }
}

/// Another editor's stroke whose paths differ in colour is not cut: its pieces could not all be
/// drawn as it was. A cut leaves it alone.
#[test]
fn a_foreign_stroke_of_two_colours_is_not_cut() {
    let Some((_dir, mut doc)) = open_pdf(&foreign_ink_pdf(true)) else {
        return;
    };
    let inks = doc.inks(0).unwrap();
    assert_eq!(inks.len(), 1, "{inks:?}");
    assert!(!inks[0].cuttable);
    // Straight across both lines where they show, which a cuttable stroke would lose pieces to.
    assert!(swept(&inks[0].points, (100.0, 50.0), (150.0, 50.0), 4.0));
    let cut = doc.cut_ink(0, inks[0].at, (100.0, 50.0), (150.0, 50.0), 4.0);
    assert!(cut.unwrap().is_none());
    assert_eq!(doc.annotation_count(0).unwrap(), 2);
}
