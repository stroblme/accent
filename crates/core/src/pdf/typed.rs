//! Text typed onto a page: a `/FreeText` of ours (ISO 32000-1 §12.5.6.6) in Helvetica, which
//! every reader has, so none is embedded. Its appearance stream is written here, so every reader
//! draws it as accent does, and its `/Contents` and `/DA` are what Firefox and Acrobat edit it by.

use accent_pdf::{Content, Object, Ref, form, helvetica_width, winansi};
use serde::{Deserialize, Serialize};

use super::annot::{OURS, PRINT};
use super::cos::{Objects, decoded, dict, name, reals, text};
use super::{PdfDoc, Rect};
use crate::{Error, Result};

/// How typed text is set: its size in points, and its colour.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TextStyle {
    pub size: f32,
    pub rgb: [u8; 3],
}

/// Text typed onto a page: the top-left corner of its box in top-left page points, what it says,
/// a line to each `\n`, and how it is set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Typed {
    pub at: (f32, f32),
    pub text: String,
    pub style: TextStyle,
}

/// A text of ours as the tools see it: its object, what it is, and its `/Rect`.
#[derive(Debug, Clone, PartialEq)]
pub struct TypedText {
    pub at: Ref,
    pub typed: Typed,
    pub bounds: Rect,
}

/// How far apart the lines are, in font sizes: Helvetica's usual leading.
pub const LEADING: f32 = 1.2;
/// The room round the text inside its box, in font sizes.
pub const PADDING: f32 = 0.2;
/// How far below the top of its line a line's baseline is, in font sizes: where a line
/// [`LEADING`] tall puts it for a Helvetica-like face, half the leading above the ascent, which
/// leaves as much room above a capital as below the baseline.
pub const BASELINE: f32 = 0.95;
/// What the appearance stream and `/DA` call the font.
const FONT: &[u8] = b"Helv";

/// Whether `c` can be typed: a line break, or a character Helvetica's WinAnsi encoding holds.
pub fn typeable(c: char) -> bool {
    c == '\n' || winansi(c).is_some()
}

impl Typed {
    /// The box the text is set in: its widest line across and its lines down, with [`PADDING`]
    /// round them.
    pub fn bounds(&self) -> Rect {
        let size = self.style.size;
        let lines = self.text.split('\n');
        let widest = lines.clone().map(|l| width(l, size)).fold(0.0, f32::max);
        let pad = 2.0 * PADDING * size;
        let (x, y) = self.at;
        Rect {
            left: x,
            top: y,
            right: x + widest + pad,
            bottom: y + lines.count() as f32 * LEADING * size + pad,
        }
    }
}

/// How wide `line` sets in Helvetica at `size`.
fn width(line: &str, size: f32) -> f32 {
    let thousandths: u32 = (line.chars().filter_map(winansi))
        .map(|code| u32::from(helvetica_width(code)))
        .sum();
    thousandths as f32 * size / 1000.0
}

impl PdfDoc {
    /// `text` typed onto `page` with its box's top-left corner at `at`, as a `/FreeText` at the
    /// end of the page's `/Annots`, what Helvetica cannot set left out; the box it covers, and its
    /// object.
    pub fn add_text(
        &mut self,
        page: usize,
        at: (f32, f32),
        text: &str,
        style: TextStyle,
    ) -> Result<(Rect, Ref)> {
        let text = text.chars().filter(|&c| typeable(c)).collect();
        self.typed(page, &Typed { at, text, style }, None)
    }

    /// The text of ours `at` on `page` made to say `text` in `style`, where it is and in its
    /// place in `/Annots`: what it was, and the box it covered and now covers.
    pub fn retype(
        &mut self,
        page: usize,
        at: Ref,
        text: &str,
        style: TextStyle,
    ) -> Result<(Typed, Rect)> {
        let (was, bounds) = self.typed_at(page, at)?;
        let text = text.chars().filter(|&c| typeable(c)).collect();
        let now = Typed {
            text,
            style,
            ..was.clone()
        };
        let (area, _) = self.typed(page, &now, Some(at))?;
        Ok((was, bounds.union(area)))
    }

    /// Every text of ours on `page`, in `/Annots` order.
    pub fn texts(&self, page: usize) -> Result<Vec<TypedText>> {
        let (_, height) = self.page_size(page)?;
        Ok((self.cos.annots(page)?.iter())
            .filter_map(|entry| {
                let (at, typed, bounds) = self.cos.typed(entry, height)?;
                Some(TypedText { at, typed, bounds })
            })
            .collect())
    }

    /// The text of ours `at` on `page`: what it is, and its `/Rect`. Refused for anything else, so
    /// a stroke cannot be retyped, nor a text that left the page.
    pub(super) fn typed_at(&self, page: usize, at: Ref) -> Result<(Typed, Rect)> {
        let (_, height) = self.page_size(page)?;
        self.cos.index_of(page, at)?;
        let (_, typed, bounds) = (self.cos.typed(&Object::Ref(at), height))
            .ok_or_else(|| Error::Pdf(format!("object {} of page {page} is no text", at.num)))?;
        Ok((typed, bounds))
    }

    /// `typed` written onto `page`, into the annotation `into` or a new one, the document opened
    /// again; the box it covers, and its object.
    pub(super) fn typed(
        &mut self,
        page: usize,
        typed: &Typed,
        into: Option<Ref>,
    ) -> Result<(Rect, Ref)> {
        let (_, height) = self.page_size(page)?;
        let written = self.cos.put_text(page, height, typed, into)?;
        self.reopen()?;
        Ok(written)
    }
}

impl Objects {
    /// `typed` as a `/FreeText` of ours on page `page`, `height` points tall: written over the
    /// annotation `into`, keeping what else it carries but the rich text an editor may have
    /// added, which would contradict it; or for `None` added at the end of the page's `/Annots`.
    /// The box it covers, and its object.
    ///
    /// The appearance draws in the page's own space, its `/BBox` being the `/Rect`, as an ink
    /// stroke's does; `/DA` says the font, size and colour, which an editor sets it again in.
    pub(super) fn put_text(
        &mut self,
        page: usize,
        height: f32,
        typed: &Typed,
        into: Option<Ref>,
    ) -> Result<(Rect, Ref)> {
        let bounds = typed.bounds();
        let rect = [
            bounds.left,
            height - bounds.bottom,
            bounds.right,
            height - bounds.top,
        ];
        let TextStyle { size, rgb } = typed.style;
        let colour = rgb.map(|c| f32::from(c) / 255.0);
        let set = |content: &mut Content| {
            content.font(FONT, size).fill_rgb(colour);
        };
        let mut drawing = Content::default();
        drawing.save().begin_text();
        set(&mut drawing);
        let pad = PADDING * size;
        drawing.next_line((bounds.left + pad, rect[3] - pad - BASELINE * size));
        for (i, line) in typed.text.split('\n').enumerate() {
            if i > 0 {
                drawing.next_line((0.0, -LEADING * size));
            }
            drawing.show(&line.chars().filter_map(winansi).collect::<Vec<u8>>());
        }
        drawing.end_text().restore();
        let font = dict([
            (b"Type", name(b"Font")),
            (b"Subtype", name(b"Type1")),
            (b"BaseFont", name(b"Helvetica")),
            (b"Encoding", name(b"WinAnsiEncoding")),
        ]);
        let resources = dict([(b"Font", Object::Dict(dict([(FONT, Object::Dict(font))])))]);
        let appearance = form(rect, drawing.finish(), resources);
        let appearance = self.update.add(Object::Stream(appearance));
        let mut da = Content::default();
        set(&mut da);
        // The same operators on one line.
        let da = da.finish();
        let da = (da.trim_ascii_end().iter())
            .map(|&b| if b == b'\n' { b' ' } else { b })
            .collect();

        let written = [
            (&b"Type"[..], name(b"Annot")),
            (b"Subtype", name(b"FreeText")),
            (b"Rect", reals(rect)),
            (b"Contents", text(&typed.text)),
            (b"DA", Object::String(da)),
            (b"Border", Object::Array(vec![Object::Int(0); 3])),
            (b"F", Object::Int(PRINT)),
            (b"AP", Object::Dict(dict([(b"N", Object::Ref(appearance))]))),
        ];
        let Some(at) = into else {
            let at = self.append_annot(page, dict(written), None)?;
            self.name_ours(at)?;
            return Ok((bounds, at));
        };
        let mut annot = self.dict(at)?;
        for (key, value) in written {
            annot.set(key, value);
        }
        annot.remove(b"RC");
        annot.remove(b"DS");
        self.update.set(at, Object::Dict(annot));
        Ok((bounds, at))
    }

    /// The annotation `entry` names as a text of ours, on a page `height` points tall: its
    /// object, what it says and how, and its `/Rect`. `None` for any other, or one hidden.
    pub(super) fn typed(&self, entry: &Object, height: f32) -> Option<(Ref, Typed, Rect)> {
        let at = entry.reference()?;
        let annot = self.dict(at).ok()?;
        let ours = matches!(annot.get(b"NM"), Some(Object::String(nm)) if nm.starts_with(OURS));
        let hidden = annot.get(b"F").and_then(Object::as_int).unwrap_or(0) & 2 != 0;
        if annot.get(b"Subtype") != Some(&name(b"FreeText")) || !ours || hidden {
            return None;
        }
        let file = self.area(annot.get(b"Rect")?)?;
        let bounds = Rect::from_corners(
            (file.left, height - file.bottom),
            (file.right, height - file.top),
        );
        let string = |key: &[u8]| match self.resolve(annot.get(key)?).ok()? {
            Object::String(s) => Some(s),
            _ => None,
        };
        let typed = Typed {
            at: (bounds.left, bounds.top),
            text: string(b"Contents").map(|s| decoded(&s)).unwrap_or_default(),
            style: string(b"DA").and_then(|da| style_of(&da))?,
        };
        Some((at, typed, bounds))
    }
}

/// The size and colour a `/DA` string sets: `/Helv 14 Tf 1 0 0 rg`, or a grey's `0 g`.
fn style_of(da: &[u8]) -> Option<TextStyle> {
    let words: Vec<&str> = std::str::from_utf8(da).ok()?.split_whitespace().collect();
    let op = |op: &str| words.iter().position(|w| *w == op);
    let number = |at: Option<usize>| words.get(at?)?.parse::<f32>().ok();
    let channel = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    let size = number(op("Tf")?.checked_sub(1))?;
    let rgb = match (op("rg"), op("g")) {
        (Some(at), _) => [3, 2, 1].map(|back| number(at.checked_sub(back)).map_or(0, channel)),
        (None, Some(at)) => [number(at.checked_sub(1)).map_or(0, channel); 3],
        _ => [0; 3],
    };
    Some(TextStyle { size, rgb })
}

#[cfg(test)]
mod tests {
    use super::super::tests::{open_tiny, reopen};
    use super::super::{Ink, Theme};
    use super::*;

    const RED: TextStyle = TextStyle {
        size: 14.0,
        rgb: [255, 0, 0],
    };

    /// Whether the page, rendered at 2 px a point, has a red pixel inside `r`.
    fn red_in(doc: &PdfDoc, r: Rect) -> bool {
        let page = doc.render_page(1, 2.0, Theme::Plain).unwrap();
        let w = page.width as usize;
        page.data
            .as_chunks::<4>()
            .0
            .iter()
            .enumerate()
            .any(|(i, px)| {
                let (x, y) = ((i % w) as f32 / 2.0, (i / w) as f32 / 2.0);
                r.contains((x, y)) && px[0] > 200 && px[1] < 100 && px[2] < 100
            })
    }

    /// Typed text survives the file as it was typed, Latin-1 and the euro included, in its own
    /// size, colour and box, drawn there; it is no stroke and no comment, and what Helvetica
    /// cannot set is left out.
    #[test]
    fn typed_text_round_trips_through_save() {
        let Some((dir, mut doc)) = open_tiny() else {
            return;
        };
        let (area, at) = doc
            .add_text(1, (110.0, 4.0), "Grüße → 5 €\nzwei", RED)
            .unwrap();
        let typed = Typed {
            at: (110.0, 4.0),
            text: "Grüße  5 €\nzwei".to_string(),
            style: RED,
        };
        assert_eq!(area, typed.bounds());
        let back = reopen(&dir, &doc);
        for doc in [&doc, &back] {
            let texts = doc.texts(1).unwrap();
            assert_eq!(texts.len(), 1);
            // After the page's link.
            assert_eq!((texts[0].at, &texts[0].typed), (at, &typed));
            assert_eq!(doc.cos.index_of(1, at).unwrap(), 1);
            assert!((texts[0].bounds.right - area.right).abs() < 1e-3);
            assert!(doc.inks(1).unwrap().is_empty(), "no stroke");
            assert!(doc.comments(1).unwrap().is_empty(), "no comment");
            assert!(red_in(doc, area), "drawn in its box");
        }
        assert!(!red_in(
            &back,
            Rect::from_corners((0.0, 0.0), (100.0, 100.0))
        ));
    }

    /// Typing, typing again, a move and an erase are a step each, which Undo walks back and Redo
    /// forward again.
    #[test]
    fn typed_text_edits_walk_back_and_forth() {
        let Some((_dir, mut doc)) = open_tiny() else {
            return;
        };
        let mut ink = Ink::default();
        let said = |doc: &PdfDoc| -> Vec<(String, i32)> {
            let texts = doc.texts(1).unwrap().into_iter();
            texts.map(|t| (t.typed.text, t.typed.at.0 as i32)).collect()
        };
        let (_, at) = doc.add_text(1, (20.0, 10.0), "one", RED).unwrap();
        ink.drew(1, at);
        let id = ink.named_texts(&doc, 1)[0].0;
        assert_eq!(ink.at(id), Some(at));
        let (was, _) = doc.retype(1, at, "two", RED).unwrap();
        ink.retyped(1, at, was);
        let right = [1.0, 0.0, 0.0, 1.0, 30.0, 0.0];
        let place = doc.cos.index_of(1, at).unwrap();
        doc.transform_ink(1, at, right).unwrap();
        ink.moved(1, at, right);
        let place_now = doc.cos.index_of(1, at).unwrap();
        assert_eq!(place_now, place, "a text moves in its place");
        let (gone, _) = doc.take_ink(1, at).unwrap();
        ink.erased(1, at, gone, Vec::new(), false);
        assert!(said(&doc).is_empty());

        let one = ("one".to_string(), 20);
        let two = |x| ("two".to_string(), x);
        let mut walk = |forwards| {
            ink.walk(&mut doc, forwards);
            said(&doc)
        };
        let back: Vec<_> = (0..4).map(|_| walk(false)).collect();
        assert_eq!(
            back,
            [vec![two(50)], vec![two(20)], vec![one.clone()], vec![]]
        );
        let forward: Vec<_> = (0..4).map(|_| walk(true)).collect();
        assert_eq!(forward, [vec![one], vec![two(20)], vec![two(50)], vec![]]);
    }

    #[test]
    fn a_default_appearance_reads_as_its_size_and_colour() {
        let style = |da: &str| style_of(da.as_bytes());
        assert_eq!(style("/Helv 14 Tf 1 0 0 rg"), Some(RED));
        let grey = style("0.5 g /Helv 9.5 Tf").unwrap();
        assert_eq!((grey.size, grey.rgb), (9.5, [128; 3]));
        assert_eq!(style("1 0 0 rg"), None);
    }
}
