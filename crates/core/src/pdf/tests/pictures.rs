//! Pictures pasted onto a page: written, read back, moved, turned, taken off and copied.

use super::*;
use accent_pdf::Object;

const RED_JPEG: &[u8] = include_bytes!("red-8x8.jpg");

/// Where a picture lands on the tiny PDF's second page: (140, 10) to (180, 30), clear of its
/// text.
const PLACE: Matrix = [40.0, 0.0, 0.0, 20.0, 140.0, 10.0];

/// 4 × 2 pixels, the left half red and the right half clear (blue, were its alpha lost).
fn half_red() -> Picture {
    let rgba: Vec<u8> = (0..8)
        .flat_map(|i| match i % 4 < 2 {
            true => [255, 0, 0, 255],
            false => [0, 0, 255, 0],
        })
        .collect();
    Picture::pixels(4, 2, &rgba)
}

/// The colour at a point of `page`'s second page rendered at 2 px a point, top-left origin.
fn colour_at(doc: &PdfDoc, (x, y): (f32, f32)) -> [u8; 3] {
    let page = doc.render_page(1, 2.0, Theme::Plain).unwrap();
    let at = ((y * 2.0) as usize * page.width as usize + (x * 2.0) as usize) * 4;
    [page.data[at], page.data[at + 1], page.data[at + 2]]
}

fn red(c: [u8; 3]) -> bool {
    c[0] > 200 && c[1] < 60 && c[2] < 60
}

fn white(c: [u8; 3]) -> bool {
    c.iter().all(|&v| v > 240)
}

fn close(a: Rect, b: Rect) -> bool {
    let near = |p: f32, q: f32| (p - q).abs() < 1e-3;
    near(a.left, b.left) && near(a.top, b.top) && near(a.right, b.right) && near(a.bottom, b.bottom)
}

/// The image the picture `at` of the second page paints.
fn image_of(doc: &PdfDoc, at: accent_pdf::Ref) -> accent_pdf::Ref {
    let (_, height) = doc.page_size(1).unwrap();
    doc.cos.picture(&Object::Ref(at), height).unwrap().1
}

/// A picture survives the file where it was put, its clear half clear, drawn by pdfium; it is no
/// stroke, no text and no comment.
#[test]
fn a_picture_round_trips_through_save_and_pdfium() {
    let Some((dir, mut doc)) = open_tiny() else {
        return;
    };
    let picture = half_red();
    assert!(picture.alpha.is_some());
    let clip = Clip::Image {
        place: PLACE,
        picture,
    };
    let (area, objects) = doc.paste(1, &[clip], IDENTITY).unwrap();
    let placed = Rect::from_corners((140.0, 10.0), (180.0, 30.0));
    assert!(close(area, placed), "{area:?}");
    let back = reopen(&dir, &doc);
    for doc in [&doc, &back] {
        let pictures = doc.pictures(1).unwrap();
        assert_eq!(pictures.len(), 1);
        assert_eq!(pictures[0].at, objects[0]);
        assert!(close(pictures[0].bounds, placed));
        assert!(doc.inks(1).unwrap().is_empty() && doc.texts(1).unwrap().is_empty());
        assert!(doc.comments(1).unwrap().is_empty(), "no comment");
        assert!(red(colour_at(doc, (150.0, 20.0))));
        assert!(
            white(colour_at(doc, (170.0, 20.0))),
            "clear where it is clear"
        );
    }
}

/// A JPEG goes into the file as it was, for every reader to decode.
#[test]
fn a_jpeg_is_written_as_it_is() {
    let Some((dir, mut doc)) = open_tiny() else {
        return;
    };
    let picture = Picture::jpeg(RED_JPEG.to_vec()).unwrap();
    let clip = Clip::Image {
        place: PLACE,
        picture,
    };
    let (_, objects) = doc.paste(1, &[clip], IDENTITY).unwrap();
    let back = reopen(&dir, &doc);
    let Object::Stream(image) = back.cos.get(image_of(&back, objects[0])).unwrap() else {
        panic!("no image stream");
    };
    let filter = image.dict.get(b"Filter");
    assert_eq!(filter, Some(&Object::Name(b"DCTDecode".to_vec())));
    assert_eq!(image.raw, RED_JPEG);
    assert!(red(colour_at(&back, (160.0, 20.0))));
}

/// A picture moves and turns in its place among the page's annotations, under a stroke drawn
/// after it, and a delete's Undo puts it back there; Undo walks the turn and the move back.
#[test]
fn a_picture_moves_turns_and_comes_back_in_its_place() {
    let Some((_dir, mut doc)) = open_tiny() else {
        return;
    };
    let mut ink = Ink::default();
    let names = [fresh_id()];
    let clip = Clip::Image {
        place: PLACE,
        picture: half_red(),
    };
    ink.paste(&mut doc, 1, &[clip], IDENTITY, &names).unwrap();
    let line = Shape::Line {
        a: (10.0, 80.0),
        b: (60.0, 80.0),
    };
    let style = InkStyle {
        width: 2.0,
        rgba: [0, 0, 0, 255],
        multiply: false,
    };
    let (_, stroke) = doc.add_shape(1, line, style).unwrap();
    ink.drew(1, stroke);
    let at = ink.at(names[0]).unwrap();
    let place = doc.cos.index_of(1, at).unwrap();
    let bounds = |doc: &PdfDoc| doc.pictures(1).unwrap().first().map(|p| p.bounds);

    ink.transform(&mut doc, 1, &names, [1.0, 0.0, 0.0, 1.0, -10.0, 0.0])
        .unwrap();
    let moved = Rect::from_corners((130.0, 10.0), (170.0, 30.0));
    assert!(close(bounds(&doc).unwrap(), moved));
    assert_eq!(
        doc.cos.index_of(1, at).unwrap(),
        place,
        "moved in its place"
    );
    // A quarter turn clockwise about its middle takes its red half to the top.
    ink.transform(&mut doc, 1, &names, rotation((150.0, 20.0), 90.0))
        .unwrap();
    let turned = Rect::from_corners((140.0, 0.0), (160.0, 40.0));
    assert!(close(bounds(&doc).unwrap(), turned));
    assert!(red(colour_at(&doc, (150.0, 10.0))));
    assert!(white(colour_at(&doc, (150.0, 30.0))));
    assert_eq!(doc.cos.index_of(1, at).unwrap(), place);

    ink.delete(&mut doc, 1, &names).unwrap();
    assert_eq!(bounds(&doc), None);
    ink.walk(&mut doc, false);
    assert!(close(bounds(&doc).unwrap(), turned));
    assert_eq!(doc.cos.index_of(1, at).unwrap(), place, "back in its place");
    ink.walk(&mut doc, false);
    ink.walk(&mut doc, false);
    let placed = Rect::from_corners((140.0, 10.0), (180.0, 30.0));
    assert!(close(bounds(&doc).unwrap(), placed));
}

/// A copy of a picture pastes as another picture of the same image, written once, in this session
/// and in the next.
#[test]
fn a_copied_picture_pastes_sharing_its_image() {
    let Some((dir, mut doc)) = open_tiny() else {
        return;
    };
    let mut ink = Ink::default();
    let first = [fresh_id()];
    let clip = Clip::Image {
        place: PLACE,
        picture: half_red(),
    };
    ink.paste(&mut doc, 1, &[clip], IDENTITY, &first).unwrap();
    let copied = |ink: &Ink, doc: &mut PdfDoc, ids: &[u32]| -> Vec<Clip> {
        let clips = ink.clips(doc, 1, ids).into_iter().flat_map(|(_, c)| c);
        let json = serde_json::to_string(&clips.collect::<Vec<_>>()).unwrap();
        serde_json::from_str(&json).unwrap()
    };
    let clips = copied(&ink, &mut doc, &first);
    let down = [1.0, 0.0, 0.0, 1.0, 0.0, 40.0];
    ink.paste(&mut doc, 1, &clips, down, &[fresh_id()]).unwrap();
    let pictures = doc.pictures(1).unwrap();
    assert_eq!(pictures.len(), 2);
    let shared = image_of(&doc, pictures[0].at);
    assert_eq!(image_of(&doc, pictures[1].at), shared);

    let mut back = reopen(&dir, &doc);
    let mut ink = Ink::default();
    let ids: Vec<u32> = ink
        .named_pictures(&back, 1)
        .iter()
        .map(|(id, _)| *id)
        .collect();
    let clips = copied(&ink, &mut back, &ids[..1]);
    let up = [1.0, 0.0, 0.0, 1.0, 0.0, -5.0];
    ink.paste(&mut back, 1, &clips, up, &[fresh_id()]).unwrap();
    let pictures = back.pictures(1).unwrap();
    assert_eq!(pictures.len(), 3);
    assert!(pictures.iter().all(|p| image_of(&back, p.at) == shared));
}
