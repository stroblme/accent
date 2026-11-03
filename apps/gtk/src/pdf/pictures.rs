//! Pictures from outside a PDF, read into what a paste writes: a JPEG as it is, anything else
//! decoded by GDK and deflated (`accent_core::pdf::Picture`). Run on a worker: a large image
//! takes a while to decode and to deflate.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::Path;

use accent_core::orientation;
use accent_core::pdf::{Matrix, Picture, picture_size, then, upright};
use gtk::gdk;
use gtk::prelude::*;

use crate::look;

/// How much larger than its own size an SVG is drawn, so it stays sharp zoomed in a little.
const SVG_SCALE: i32 = 2;

/// A picture read for a paste: what it writes, the turn its EXIF asks for, its size upright in
/// pixels, which is what it lands at, and what tells it from another pasted before.
pub struct Incoming {
    pub picture: Picture,
    tag: u8,
    natural: (u32, u32),
    pub key: u64,
}

impl Incoming {
    /// A JPEG's bytes as they are, turned as its EXIF says when it lands; `None` for one a PDF
    /// reader may not draw.
    pub fn jpeg(bytes: Vec<u8>) -> Option<Incoming> {
        let tag = orientation::read(&mut std::io::Cursor::new(&bytes));
        let picture = Picture::jpeg(bytes)?;
        let (w, h) = (picture.width, picture.height);
        let natural = if tag >= 5 { (h, w) } else { (w, h) };
        Some(Incoming::of(picture, tag, natural))
    }

    /// A texture's pixels, upright as they are, at `natural` pixels.
    fn texture(texture: &gdk::Texture, natural: (u32, u32)) -> Option<Incoming> {
        let pixels = look::pixels(texture)?;
        let picture = Picture::pixels(pixels.width, pixels.height, &pixels.data);
        Some(Incoming::of(picture, 1, natural))
    }

    /// A pasted or dropped image, upright.
    pub fn pasted(texture: &gdk::Texture) -> Option<Incoming> {
        let natural = (texture.width() as u32, texture.height() as u32);
        Incoming::texture(texture, natural)
    }

    /// The image at `path`: a JPEG as it is, an SVG drawn at twice its size and landing at its
    /// own, anything else decoded by GDK and turned the way up its EXIF says.
    pub fn file(path: &Path) -> Option<Incoming> {
        let bytes = std::fs::read(path)
            .inspect_err(|e| tracing::debug!("reading {}: {e}", path.display()))
            .ok()?;
        let svg = (path.extension()).is_some_and(|e| e.eq_ignore_ascii_case("svg"));
        if svg {
            let (texture, (w, h)) = look::rasterise(&bytes, SVG_SCALE, 1.0)
                .inspect_err(|e| tracing::debug!("drawing {}: {e}", path.display()))
                .ok()?;
            return Incoming::texture(&texture, (w as u32, h as u32));
        }
        if let Some(jpeg) = Incoming::jpeg(bytes) {
            return Some(jpeg);
        }
        let texture = look::decode(path)
            .inspect_err(|e| tracing::debug!("decoding {}: {e}", path.display()))
            .ok()?;
        Incoming::pasted(&texture)
    }

    fn of(picture: Picture, tag: u8, natural: (u32, u32)) -> Incoming {
        let mut hasher = DefaultHasher::new();
        picture.data.hash(&mut hasher);
        let key = hasher.finish();
        Incoming {
            picture,
            tag,
            natural,
            key,
        }
    }

    /// Where its unit square goes to land centred on `centre` of a page of `page` points: at its
    /// natural size, up to half the page each way, and wholly on the paper.
    pub fn place(&self, (x, y): (f32, f32), page: (f32, f32)) -> Matrix {
        let (w, h) = picture_size(self.natural, page);
        let x = x.clamp(w / 2.0, (page.0 - w / 2.0).max(w / 2.0));
        let y = y.clamp(h / 2.0, (page.1 - h / 2.0).max(h / 2.0));
        then(
            upright(self.tag),
            [w, 0.0, 0.0, h, x - w / 2.0, y - h / 2.0],
        )
    }
}
