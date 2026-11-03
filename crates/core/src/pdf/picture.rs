//! Pictures on a page: a `/Stamp` of ours whose appearance paints one image XObject (ISO 32000-1
//! §8.9.5, §12.5.6.12), which is what a paste or a drop of an image writes. A JPEG goes in as it
//! is (`/DCTDecode`), anything else as deflated RGB (`/FlateDecode`), with a soft mask for its
//! alpha only where some pixel is translucent (§11.6.5.3).
//!
//! The stamp's form paints the image into the unit square, and its `/Matrix` puts that where the
//! picture is, so a move, a stretch or a turn writes the small form again and never the image.

use std::borrow::Cow;

use accent_pdf::{Content, Object, Ref, Stream, form};
use serde::{Deserialize, Serialize};

use super::annot::{OURS, PRINT};
use super::cos::{Objects, dict, name, reals};
use super::ink::{Matrix, apply, then};
use super::{PdfDoc, Rect};
use crate::{Error, Result};

/// The longest side a picture other than a JPEG keeps, in pixels: past it, it is scaled down.
pub const MAX_SIDE: u32 = 4096;

/// How many page points a pixel covers at a picture's natural size: 96 pixels an inch, as a
/// browser and an office suite place an image.
const POINTS_PER_PIXEL: f32 = 0.75;

/// What the stamp's resources call its image.
const IMAGE: &[u8] = b"Im0";

/// The picture's unit square, its top-left corner at (0, 0) and y down, as the image space of
/// ISO 32000-1 §8.9.4, whose first row is at the top and y up; and back again.
const UNIT_DOWN: Matrix = [1.0, 0.0, 0.0, -1.0, 0.0, 1.0];

/// An image as a PDF holds it: its size in pixels, its colours, and its data as its filter has it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Picture {
    pub width: u32,
    pub height: u32,
    /// One grey channel, else red, green and blue.
    pub gray: bool,
    /// `data` is a JPEG file, written as it is; else the samples, deflated.
    pub jpeg: bool,
    #[serde(with = "base64")]
    pub data: Vec<u8>,
    /// A byte of alpha a pixel, deflated, where some pixel is translucent: the image's `/SMask`.
    #[serde(default, with = "base64_some")]
    pub alpha: Option<Vec<u8>>,
}

impl Picture {
    /// A JPEG file as it is, which every reader decodes (`/DCTDecode`): `None` for anything but a
    /// baseline, extended or progressive JPEG of 8-bit grey or colour, which is all a PDF reader
    /// is sure to draw.
    pub fn jpeg(bytes: Vec<u8>) -> Option<Picture> {
        let (width, height, components) = frame(&bytes)?;
        Some(Picture {
            width,
            height,
            gray: components == 1,
            jpeg: true,
            data: bytes,
            alpha: None,
        })
    }

    /// Straight RGBA8 pixels, `width` × `height`, scaled down past [`MAX_SIDE`] and deflated,
    /// with a mask only where some pixel is translucent.
    pub fn pixels(width: u32, height: u32, rgba: &[u8]) -> Picture {
        let (rgba, width, height) = shrunk(rgba, width, height);
        let pixels = rgba.as_chunks::<4>().0;
        let rgb: Vec<u8> = pixels.iter().flat_map(|&[r, g, b, _]| [r, g, b]).collect();
        let translucent = pixels.iter().any(|px| px[3] < 255);
        let alpha =
            translucent.then(|| deflate(&pixels.iter().map(|px| px[3]).collect::<Vec<_>>()));
        Picture {
            width,
            height,
            gray: false,
            jpeg: false,
            data: deflate(&rgb),
            alpha,
        }
    }

    /// What tells two pictures apart, so a picture pasted again shares the image it was written
    /// as: everything it holds, hashed.
    pub(super) fn key(&self) -> blake3::Hash {
        let mut hasher = blake3::Hasher::new();
        hasher
            .update(&self.width.to_le_bytes())
            .update(&self.height.to_le_bytes())
            .update(&[u8::from(self.gray), u8::from(self.jpeg)])
            .update(&(self.data.len() as u64).to_le_bytes())
            .update(&self.data);
        if let Some(alpha) = &self.alpha {
            hasher.update(alpha);
        }
        hasher.finalize()
    }
}

/// A picture of ours as the tools see it: its object, where its unit square goes on the page,
/// in top-left page points with the picture's own top-left corner at (0, 0), and the box round
/// that.
#[derive(Debug, Clone, PartialEq)]
pub struct PlacedPicture {
    pub at: Ref,
    pub place: Matrix,
    pub bounds: Rect,
}

/// The corners of a picture `place` puts on the page: top-left, top-right, bottom-right and
/// bottom-left, the order its outline goes round.
pub fn picture_corners(place: Matrix) -> [(f32, f32); 4] {
    [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)].map(|p| apply(place, p))
}

/// The box round a picture `place` puts on the page.
pub(super) fn picture_bounds(place: Matrix) -> Rect {
    Rect::from_corners((0.0, 0.0), (1.0, 1.0)).under(place)
}

/// The map that turns a picture's unit square the way its EXIF Orientation `tag` (1–8) says, so
/// it shows upright: a tag of 5 to 8 swaps its width and height.
pub fn upright(tag: u8) -> Matrix {
    match tag {
        2 => [-1.0, 0.0, 0.0, 1.0, 1.0, 0.0],
        3 => [-1.0, 0.0, 0.0, -1.0, 1.0, 1.0],
        4 => [1.0, 0.0, 0.0, -1.0, 0.0, 1.0],
        5 => [0.0, 1.0, 1.0, 0.0, 0.0, 0.0],
        6 => [0.0, 1.0, -1.0, 0.0, 1.0, 0.0],
        7 => [0.0, -1.0, -1.0, 0.0, 1.0, 1.0],
        8 => [0.0, -1.0, 1.0, 0.0, 0.0, 1.0],
        _ => super::IDENTITY,
    }
}

/// How large, in page points, a picture of `pixels` lands on a page of `page` points: at 96
/// pixels an inch, shrunk to fit half the page's width and half its height, never enlarged.
pub fn picture_size(pixels: (u32, u32), page: (f32, f32)) -> (f32, f32) {
    let (w, h) = (
        pixels.0 as f32 * POINTS_PER_PIXEL,
        pixels.1 as f32 * POINTS_PER_PIXEL,
    );
    let fit = (page.0 / 2.0 / w).min(page.1 / 2.0 / h).min(1.0);
    (w * fit, h * fit)
}

/// The width, height and number of components of a JPEG's frame, `None` for anything but an
/// 8-bit grey or colour one in baseline, extended or progressive coding (`SOF0`–`SOF2`).
fn frame(jpeg: &[u8]) -> Option<(u32, u32, u8)> {
    let mut at = 2;
    if !jpeg.starts_with(&[0xff, 0xd8]) {
        return None;
    }
    // Segment by segment, each a marker and its length, up to the frame header.
    while let [0xff, marker, hi, lo, ..] = *jpeg.get(at..)? {
        let length = usize::from(u16::from_be_bytes([hi, lo]));
        match marker {
            0xc0..=0xc2 => {
                let header = jpeg.get(at + 4..at + 10)?;
                let height = u16::from_be_bytes([header[1], header[2]]);
                let width = u16::from_be_bytes([header[3], header[4]]);
                let usable =
                    header[0] == 8 && matches!(header[5], 1 | 3) && width > 0 && height > 0;
                return usable.then_some((u32::from(width), u32::from(height), header[5]));
            }
            // Any other frame is coding a reader need not know; the scan is past every header.
            0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf | 0xda => return None,
            // Fill bytes before a marker.
            0xff => at += 1,
            _ => at += 2 + length,
        }
    }
    None
}

/// `rgba` scaled down to fit [`MAX_SIDE`], each pixel the average of the box of pixels it
/// covers, its colour weighted by their alpha so a transparent pixel's colour does not bleed in.
fn shrunk(rgba: &[u8], width: u32, height: u32) -> (Cow<'_, [u8]>, u32, u32) {
    let longest = width.max(height);
    if longest <= MAX_SIDE {
        return (Cow::Borrowed(rgba), width, height);
    }
    let scaled = |n: u32| (u64::from(n) * u64::from(MAX_SIDE) / u64::from(longest)).max(1) as u32;
    let (w, h) = (scaled(width), scaled(height));
    // The source pixels `t` of `target` covers, of `source`.
    let span = |t: u32, target: u32, source: u32| {
        let from = (u64::from(t) * u64::from(source) / u64::from(target)) as u32;
        let to = (u64::from(t + 1) * u64::from(source)).div_ceil(u64::from(target)) as u32;
        (from, to.clamp(from + 1, source))
    };
    let mut out = Vec::with_capacity(w as usize * h as usize * 4);
    for ty in 0..h {
        let (y0, y1) = span(ty, h, height);
        for tx in 0..w {
            let (x0, x1) = span(tx, w, width);
            let mut sum = [0u64; 4];
            for y in y0..y1 {
                let row = (y as usize * width as usize + x0 as usize) * 4;
                let row = &rgba[row..row + (x1 - x0) as usize * 4];
                for &[r, g, b, a] in row.as_chunks::<4>().0 {
                    let a = u64::from(a);
                    sum[0] += u64::from(r) * a;
                    sum[1] += u64::from(g) * a;
                    sum[2] += u64::from(b) * a;
                    sum[3] += a;
                }
            }
            let n = u64::from((x1 - x0) * (y1 - y0));
            let colour = |c: usize| (sum[c] + sum[3] / 2).checked_div(sum[3]).unwrap_or(0) as u8;
            out.extend([
                colour(0),
                colour(1),
                colour(2),
                ((sum[3] + n / 2) / n) as u8,
            ]);
        }
    }
    (Cow::Owned(out), w, h)
}

/// `bytes` deflated as `/FlateDecode` reads them, in a zlib wrapper.
fn deflate(bytes: &[u8]) -> Vec<u8> {
    miniz_oxide::deflate::compress_to_vec_zlib(bytes, 6)
}

impl Objects {
    /// `picture` as an image XObject of the session, its mask one of its own: written once, a
    /// picture written before being shared.
    pub(super) fn put_picture(&mut self, picture: &Picture) -> Ref {
        let key = picture.key();
        if let Some(&at) = self.pictures.get(&key) {
            return at;
        }
        let image = |gray: bool, filter: &[u8], raw: Vec<u8>| Stream {
            dict: dict([
                (b"Type", name(b"XObject")),
                (b"Subtype", name(b"Image")),
                (b"Width", Object::Int(i64::from(picture.width))),
                (b"Height", Object::Int(i64::from(picture.height))),
                (
                    b"ColorSpace",
                    name(if gray { b"DeviceGray" } else { b"DeviceRGB" }),
                ),
                (b"BitsPerComponent", Object::Int(8)),
                (b"Filter", name(filter)),
            ]),
            raw,
        };
        let filter: &[u8] = if picture.jpeg {
            b"DCTDecode"
        } else {
            b"FlateDecode"
        };
        let mut written = image(picture.gray, filter, picture.data.clone());
        if let Some(alpha) = &picture.alpha {
            let mask = image(true, b"FlateDecode", alpha.clone());
            let mask = self.update.add(Object::Stream(mask));
            written.dict.set(b"SMask", Object::Ref(mask));
        }
        let at = self.update.add(Object::Stream(written));
        self.pictures.insert(key, at);
        at
    }

    /// A picture of ours painting `image` where `place` puts its unit square on page `page`,
    /// `height` points tall: written over the stamp `into`, which keeps its place in `/Annots`,
    /// or for `None` added at the end of them. The box it covers, and its object.
    pub(super) fn put_stamp(
        &mut self,
        page: usize,
        height: f32,
        image: Ref,
        place: Matrix,
        into: Option<Ref>,
    ) -> Result<(Rect, Ref)> {
        let flip = [1.0, 0.0, 0.0, -1.0, 0.0, height];
        let matrix = then(then(UNIT_DOWN, place), flip);
        let file = picture_bounds(matrix);
        let rect = [file.left, file.top, file.right, file.bottom];
        let mut drawing = Content::default();
        drawing.paint(IMAGE);
        let images = dict([(IMAGE, Object::Ref(image))]);
        let resources = dict([(b"XObject", Object::Dict(images))]);
        let mut appearance = form([0.0, 0.0, 1.0, 1.0], drawing.finish(), resources);
        appearance.dict.set(b"Matrix", reals(matrix));
        let appearance = self.update.add(Object::Stream(appearance));
        let ap = Object::Dict(dict([(b"N", Object::Ref(appearance))]));
        let bounds = picture_bounds(place);
        let Some(at) = into else {
            let stamp = dict([
                (b"Type", name(b"Annot")),
                (b"Subtype", name(b"Stamp")),
                (b"Rect", reals(rect)),
                (b"F", Object::Int(PRINT)),
                (b"AP", ap),
            ]);
            let at = self.append_annot(page, stamp, None)?;
            self.name_ours(at)?;
            return Ok((bounds, at));
        };
        let mut stamp = self.dict(at)?;
        stamp.set(b"Rect", reals(rect));
        stamp.set(b"AP", ap);
        self.update.set(at, Object::Dict(stamp));
        Ok((bounds, at))
    }

    /// The annotation `entry` names as a picture of ours, on a page `height` points tall, where
    /// it shows, and its image; `None` for any other, or one hidden. Read from the stamp and its
    /// form, never the image.
    pub(super) fn picture(&self, entry: &Object, height: f32) -> Option<(PlacedPicture, Ref)> {
        let at = entry.reference()?;
        let stamp = self.dict(at).ok()?;
        let ours = matches!(stamp.get(b"NM"), Some(Object::String(nm)) if nm.starts_with(OURS));
        let hidden = stamp.get(b"F").and_then(Object::as_int).unwrap_or(0) & 2 != 0;
        if stamp.get(b"Subtype") != Some(&name(b"Stamp")) || !ours || hidden {
            return None;
        }
        let resolved = |o: &Object| self.resolve(o).ok();
        let rect = self.area(stamp.get(b"Rect")?)?;
        let normal = resolved(resolved(stamp.get(b"AP")?)?.as_dict()?.get(b"N")?)?;
        let Object::Stream(appearance) = normal else {
            return None;
        };
        // Where every reader shows it: an editor that moved it by its `/Rect` moved it too.
        let (_, matrix) = self.fitted(&appearance.dict, rect)?;
        let resources = resolved(appearance.dict.get(b"Resources")?)?;
        let images = resolved(resources.as_dict()?.get(b"XObject")?)?;
        let image = images.as_dict()?.get(IMAGE)?.reference()?;
        let flip = [1.0, 0.0, 0.0, -1.0, 0.0, height];
        let place = then(then(UNIT_DOWN, matrix), flip);
        let bounds = picture_bounds(place);
        Some((PlacedPicture { at, place, bounds }, image))
    }

    /// The image `image` of a picture of ours, as [`Picture`] holds it: its bytes as the file
    /// has them, which is what a copy carries.
    pub(super) fn picture_data(&self, image: Ref) -> Result<Picture> {
        let not_ours = || Error::Pdf(format!("object {} is no picture of ours", image.num));
        let Object::Stream(stream) = self.get(image)? else {
            return Err(not_ours());
        };
        let value = |key: &[u8]| stream.dict.get(key).and_then(|o| self.resolve(o).ok());
        let size = |key: &[u8]| value(key)?.as_int().and_then(|n| u32::try_from(n).ok());
        let (Some(width), Some(height)) = (size(b"Width"), size(b"Height")) else {
            return Err(not_ours());
        };
        let jpeg = match value(b"Filter") {
            Some(Object::Name(f)) if f == b"DCTDecode" => true,
            Some(Object::Name(f)) if f == b"FlateDecode" => false,
            _ => return Err(not_ours()),
        };
        let alpha = match value(b"SMask") {
            Some(Object::Stream(mask)) => Some(mask.raw),
            _ => None,
        };
        Ok(Picture {
            width,
            height,
            gray: value(b"ColorSpace") == Some(name(b"DeviceGray")),
            jpeg,
            data: stream.raw,
            alpha,
        })
    }
}

impl PdfDoc {
    /// Every picture of ours on `page`, in `/Annots` order, read without its image.
    pub fn pictures(&self, page: usize) -> Result<Vec<PlacedPicture>> {
        let (_, height) = self.page_size(page)?;
        Ok((self.cos.annots(page)?.iter())
            .filter_map(|entry| self.cos.picture(entry, height))
            .map(|(placed, _)| placed)
            .collect())
    }

    /// The picture of ours `at` on `page`, and its image. Refused for anything else, or one that
    /// left the page.
    pub(super) fn picture_at(&self, page: usize, at: Ref) -> Result<(PlacedPicture, Ref)> {
        let (_, height) = self.page_size(page)?;
        self.cos.index_of(page, at)?;
        (self.cos.picture(&Object::Ref(at), height))
            .ok_or_else(|| Error::Pdf(format!("object {} of page {page} is no picture", at.num)))
    }
}

/// A byte string as base64 text, which is how a copy's picture goes through JSON.
mod base64 {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let text = String::deserialize(d)?;
        STANDARD.decode(text).map_err(D::Error::custom)
    }
}

/// [`base64`] for bytes that may be absent.
mod base64_some {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &Option<Vec<u8>>, s: S) -> Result<S::Ok, S::Error> {
        match bytes {
            Some(bytes) => super::base64::serialize(bytes, s),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Vec<u8>>, D::Error> {
        #[derive(Deserialize)]
        struct Encoded(#[serde(with = "super::base64")] Vec<u8>);
        Ok(Option::<Encoded>::deserialize(d)?.map(|Encoded(bytes)| bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED_JPEG: &[u8] = include_bytes!("tests/red-8x8.jpg");

    fn inflated(bytes: &[u8]) -> Vec<u8> {
        miniz_oxide::inflate::decompress_to_vec_zlib(bytes).unwrap()
    }

    /// A JPEG keeps its bytes and is read for its size and colours; anything a reader may not
    /// draw, or no JPEG at all, is refused.
    #[test]
    fn a_jpeg_header_gives_its_size_and_colours() {
        let picture = Picture::jpeg(RED_JPEG.to_vec()).unwrap();
        assert_eq!(
            (picture.width, picture.height, picture.gray, picture.jpeg),
            (8, 8, false, true)
        );
        assert_eq!((picture.data.as_slice(), picture.alpha), (RED_JPEG, None));
        let at = RED_JPEG.windows(2).position(|w| w == [0xff, 0xc0]).unwrap();
        let mut twelve_bit = RED_JPEG.to_vec();
        twelve_bit[at + 4] = 12;
        assert_eq!(Picture::jpeg(twelve_bit), None);
        assert_eq!(Picture::jpeg(b"\x89PNG\r\n\x1a\n".to_vec()), None);
    }

    /// Opaque pixels go as RGB alone; one translucent pixel gives the picture its alpha.
    #[test]
    fn pixels_get_a_mask_only_when_translucent() {
        let opaque = Picture::pixels(2, 1, &[255, 0, 0, 255, 0, 0, 255, 255]);
        assert_eq!(inflated(&opaque.data), [255, 0, 0, 0, 0, 255]);
        assert_eq!(
            (opaque.alpha, opaque.jpeg, opaque.gray),
            (None, false, false)
        );
        let clear = Picture::pixels(2, 1, &[255, 0, 0, 255, 0, 0, 0, 0]);
        assert_eq!(inflated(&clear.alpha.unwrap()), [255, 0]);
    }

    /// Past the cap a picture is scaled down to it, each pixel its box's average, the colour of a
    /// transparent pixel no part of it.
    #[test]
    fn a_picture_past_the_cap_is_scaled_down() {
        let (w, h) = (2 * MAX_SIDE, 2);
        let rgba: Vec<u8> = (0..w * h)
            .flat_map(|i| match i % 2 {
                0 => [255, 0, 0, 255],
                _ => [0, 255, 0, 0],
            })
            .collect();
        let picture = Picture::pixels(w, h, &rgba);
        assert_eq!((picture.width, picture.height), (MAX_SIDE, 1));
        assert_eq!(&inflated(&picture.data)[..6], [255, 0, 0, 255, 0, 0]);
        assert_eq!(&inflated(&picture.alpha.unwrap())[..2], [128, 128]);
    }

    /// Each EXIF orientation takes the stored picture's top-left corner where it shows.
    #[test]
    fn upright_turns_the_unit_square_as_exif_says() {
        let top_left = |tag| apply(upright(tag), (0.0, 0.0));
        let corners: Vec<_> = (1..=8).map(top_left).collect();
        assert_eq!(
            corners,
            [
                (0.0, 0.0),
                (1.0, 0.0),
                (1.0, 1.0),
                (0.0, 1.0),
                (0.0, 0.0),
                (1.0, 0.0),
                (1.0, 1.0),
                (0.0, 1.0),
            ]
        );
        // A quarter turn clockwise takes the top edge to the right one.
        assert_eq!(apply(upright(6), (1.0, 0.0)), (1.0, 1.0));
        assert_eq!(apply(upright(5), (1.0, 0.0)), (0.0, 1.0));
    }

    /// A picture lands at 96 dpi, shrunk to fit half the page either way, never enlarged.
    #[test]
    fn a_picture_lands_at_96_dpi_up_to_half_the_page() {
        let a4 = (595.0, 842.0);
        assert_eq!(picture_size((200, 100), a4), (150.0, 75.0));
        assert_eq!(picture_size((2000, 1000), a4), (297.5, 148.75));
        let (w, h) = picture_size((100, 4000), a4);
        assert!((h - 421.0).abs() < 1e-3 && (w - 10.525).abs() < 1e-3);
    }

    /// A picture goes through JSON as base64, and comes back as it was.
    #[test]
    fn a_picture_round_trips_through_json() {
        let picture = Picture::pixels(1, 1, &[1, 2, 3, 4]);
        let json = serde_json::to_string(&picture).unwrap();
        assert_eq!(serde_json::from_str::<Picture>(&json).unwrap(), picture);
        let jpeg = Picture::jpeg(RED_JPEG.to_vec()).unwrap();
        let json = serde_json::to_string(&jpeg).unwrap();
        assert_eq!(serde_json::from_str::<Picture>(&json).unwrap(), jpeg);
    }
}
