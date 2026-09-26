//! How an image is shown under the theme: recoloured onto the page's paper and ink when it reads
//! as a document, as it is when it does not, and the other way round once the reader inverts it.
//!
//! The image tab and the preview both come through here, so a figure looks the same in either.
//! The decision is [`Look::palette`], snapshotted on the main thread where the theme lives; the
//! decoding, the classifying and the recolouring run on a worker ([`show`], [`serve`]), with
//! GDK's own decoders and `accent_core::recolour`.

use std::cell::OnceCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

use accent_core::recolour::{self, Verdict};
use gtk::prelude::*;
use gtk::{gdk, glib};

use crate::theme::{self, Page};

/// What the theme in force asks of images, as a worker can carry it.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Look {
    /// The theme's page, `None` in Light, which leaves every image alone.
    theme: Option<Page>,
    /// Where an image the theme leaves alone goes when it is inverted.
    inverted_to: Page,
}

impl Look {
    /// The look now. On the main thread only: `theme.rs` keeps the choice there.
    pub fn now() -> Look {
        Look {
            theme: theme::page_colours(adw::StyleManager::default().is_dark()),
            inverted_to: theme::dark_page(),
        }
    }

    /// The paper and ink to recolour an image onto, or `None` to show it as it is.
    ///
    /// The theme recolours a document and leaves a photo alone, the way it treats a PDF page;
    /// inverting turns that round, onto the theme's page or, in Light, which has none, the dark
    /// one.
    pub fn palette(&self, document: bool, inverted: bool) -> Option<Page> {
        let recolour = (self.theme.is_some() && document) != inverted;
        recolour.then(|| self.theme.unwrap_or(self.inverted_to))
    }

    /// Whether the answer turns on what the image shows. Not in Light, which treats every image
    /// alike, so there nothing is ever classified.
    fn asks(&self, inverted: bool) -> bool {
        self.palette(true, inverted) != self.palette(false, inverted)
    }
}

/// What an image tab shows: the file as decoded, kept so a restyle need not read it again, and
/// that texture under the look it was asked for.
pub struct Shown {
    pub original: gdk::Texture,
    pub texture: gdk::Texture,
}

/// Decode `path`, unless `original` already holds it, and recolour it as `look` asks. On a
/// worker: a large scan takes a while to decode, and longer to recolour.
pub fn show(
    path: &Path,
    original: Option<gdk::Texture>,
    look: Look,
    inverted: bool,
) -> Result<Shown, glib::Error> {
    let original = match original {
        Some(texture) => texture,
        None => gdk::Texture::from_filename(path)?,
    };
    let texture = match svg(path) {
        // An SVG is recoloured as a vector, through a filter, and drawn again from its text.
        true => look
            .palette(true, inverted)
            .and_then(|page| svg_filtered(path, page))
            .and_then(|svg| gdk::Texture::from_bytes(&glib::Bytes::from_owned(svg)).ok())
            .unwrap_or_else(|| original.clone()),
        false => recoloured(path, look, inverted, || Some(original.clone()))
            .map_or_else(|| original.clone(), |t| t.upcast()),
    };
    Ok(Shown { original, texture })
}

/// What the preview hands WebKit for an image: the file as it is, or new bytes of a type.
pub enum Served {
    File,
    Bytes(glib::Bytes, &'static str),
}

/// [`show`] for the preview: only an image that is recoloured is decoded, recoloured and encoded
/// again, as a PNG; everything else, and anything that fails on the way, is served as the file.
pub fn serve(path: &Path, look: Look, inverted: bool) -> Served {
    if svg(path) {
        return match look
            .palette(true, inverted)
            .and_then(|page| svg_filtered(path, page))
        {
            Some(svg) => Served::Bytes(glib::Bytes::from_owned(svg), "image/svg+xml"),
            None => Served::File,
        };
    }
    match recoloured(path, look, inverted, || {
        gdk::Texture::from_filename(path).ok()
    }) {
        Some(texture) => Served::Bytes(texture.save_to_png_bytes(), "image/png"),
        None => Served::File,
    }
}

/// What the classifier said about `path`, while the file is as it was then: `None` where it has
/// not been asked, which in Light is every image.
pub fn verdict(path: &Path) -> Option<Verdict> {
    let stamp = stamp(path)?;
    let cache = verdicts().lock().ok()?;
    cache
        .get(path)
        .filter(|(at, _)| *at == stamp)
        .map(|(_, v)| *v)
}

/// Straight RGBA8 pixels and their size.
struct Pixels {
    data: Vec<u8>,
    width: u32,
    height: u32,
}

impl Pixels {
    fn texture(self) -> gdk::MemoryTexture {
        gdk::MemoryTexture::new(
            self.width as i32,
            self.height as i32,
            gdk::MemoryFormat::R8g8b8a8,
            &glib::Bytes::from_owned(self.data),
            self.width as usize * 4,
        )
    }
}

/// A raster image recoloured as `look` asks, or `None` to show it as it is. `decode` is asked for
/// the image only when the answer turns on what it shows or it is to be recoloured, and once.
fn recoloured(
    path: &Path,
    look: Look,
    inverted: bool,
    decode: impl Fn() -> Option<gdk::Texture>,
) -> Option<gdk::MemoryTexture> {
    let texture = OnceCell::new();
    let decoded = || texture.get_or_init(&decode).clone();
    let document = match gif(path) || !look.asks(inverted) {
        true => false,
        false => classified(path, || decoded().map(|t| measure(&t))).is_some_and(|v| v.document),
    };
    let (paper, ink) = look.palette(document, inverted)?;
    let mut pixels = pixels(&decoded()?)?;
    recolour::recolour(&mut pixels.data, paper, ink);
    Some(pixels.texture())
}

/// The classifier's verdict on `texture`. One too large to look at is not a document, unmeasured.
fn measure(texture: &gdk::Texture) -> Verdict {
    match pixels(texture) {
        Some(p) => recolour::classify(&p.data, p.width, p.height),
        None => Verdict {
            paper: 0.0,
            colours: 0,
            document: false,
        },
    }
}

/// `texture`'s pixels, or `None` above [`recolour::MAX_PIXELS`]: such an image is left alone
/// rather than held twice.
fn pixels(texture: &gdk::Texture) -> Option<Pixels> {
    let (width, height) = (texture.width() as u32, texture.height() as u32);
    if u64::from(width) * u64::from(height) > recolour::MAX_PIXELS {
        return None;
    }
    let mut downloader = gdk::TextureDownloader::new(texture);
    downloader.set_format(gdk::MemoryFormat::R8g8b8a8);
    let (bytes, stride) = downloader.download_bytes();
    // `recolour` wants the rows packed, as a download nearly always is; a padded one is cut
    // down to its pixels.
    let row = width as usize * 4;
    let data = match stride == row {
        true => bytes.to_vec(),
        false => bytes
            .chunks(stride)
            .flat_map(|r| &r[..row])
            .copied()
            .collect(),
    };
    Some(Pixels {
        data,
        width,
        height,
    })
}

/// [`verdict`], measured by `measure` when there is none yet. A measurement that could not be
/// taken is not kept.
fn classified(path: &Path, measure: impl FnOnce() -> Option<Verdict>) -> Option<Verdict> {
    if let Some(verdict) = verdict(path) {
        return Some(verdict);
    }
    let verdict = measure()?;
    if let (Some(stamp), Ok(mut cache)) = (stamp(path), verdicts().lock()) {
        cache.insert(path.to_path_buf(), (stamp, verdict));
    }
    Some(verdict)
}

/// Every verdict this process has taken, by the file's path on this machine. Process-wide, so the
/// tab and the preview classify a file once between them.
fn verdicts() -> &'static Mutex<HashMap<PathBuf, (Stamp, Verdict)>> {
    static VERDICTS: OnceLock<Mutex<HashMap<PathBuf, (Stamp, Verdict)>>> = OnceLock::new();
    VERDICTS.get_or_init(Mutex::default)
}

/// Which version of a file a verdict was taken on: its mtime and size.
type Stamp = (SystemTime, u64);

fn stamp(path: &Path) -> Option<Stamp> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

/// An SVG's text with the recolouring filter around its content.
fn svg_filtered(path: &Path, (paper, ink): Page) -> Option<Vec<u8>> {
    let text = std::fs::read_to_string(path).ok()?;
    recolour::recolour_svg(&text, paper, ink).map(String::into_bytes)
}

/// Line art, always recoloured: the vector counterpart of a scan.
fn svg(path: &Path) -> bool {
    extension(path) == "svg"
}

/// Never recoloured unasked: likely an animation, which is its own content.
fn gif(path: &Path) -> bool {
    extension(path) == "gif"
}

fn extension(path: &Path) -> String {
    path.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    const DARK: Page = ([29, 29, 32], [235, 235, 235]);
    const CREAM: Page = ([253, 246, 227], [101, 123, 131]);

    #[test]
    fn a_theme_recolours_a_document_and_inverting_turns_that_round() {
        let light = Look {
            theme: None,
            inverted_to: DARK,
        };
        let cream = Look {
            theme: Some(CREAM),
            inverted_to: DARK,
        };
        // (look, document, inverted) -> palette
        let table = [
            (light, false, false, None),
            (light, true, false, None),
            (light, false, true, Some(DARK)),
            (light, true, true, Some(DARK)),
            (cream, false, false, None),
            (cream, true, false, Some(CREAM)),
            (cream, false, true, Some(CREAM)),
            (cream, true, true, None),
        ];
        for (look, document, inverted, palette) in table {
            assert_eq!(
                look.palette(document, inverted),
                palette,
                "{look:?} document={document} inverted={inverted}"
            );
        }
        // Light treats every image alike, so it never has one classified.
        assert!(!light.asks(false) && !light.asks(true));
        assert!(cream.asks(false) && cream.asks(true));
    }
}
