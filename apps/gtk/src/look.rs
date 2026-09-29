//! How an image is shown under the theme: recoloured onto the page's paper and ink when it reads
//! as a document, as it is when it does not, and onto the other half's page once the reader
//! inverts it, as a PDF page is.
//!
//! The image tab and the preview both come through here, so a figure looks the same in either.
//! The decision is [`Look::palette`], snapshotted on the main thread where the theme lives; the
//! decoding, the classifying and the recolouring run on a worker ([`show`], [`serve`]), with
//! GDK's own decoders and `accent_core::recolour`.

use std::cell::{Cell, OnceCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

use accent_core::orientation;
use accent_core::recolour::{self, Verdict};
use gtk::prelude::*;
use gtk::subclass::prelude::ObjectSubclassIsExt;
use gtk::{gdk, gdk_pixbuf, gio, glib};

use crate::theme::{self, Page};

/// What the theme in force asks of images, as a worker can carry it.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Look {
    /// The page of the half on screen, `None` in Light, which leaves every image alone.
    theme: Option<Page>,
    /// The other half's page, which an inverted image goes onto, as an inverted PDF page does.
    opposite: Option<Page>,
}

impl Look {
    /// The look now. On the main thread only: `theme.rs` keeps the choice there.
    pub fn now() -> Look {
        let dark = adw::StyleManager::default().is_dark();
        Look {
            theme: theme::page_colours(dark),
            opposite: theme::page_colours(!dark),
        }
    }

    /// The look of a printout or an export: every image as its file is, whatever the theme on
    /// screen and whatever the reader inverted.
    pub fn paper() -> Look {
        Look {
            theme: None,
            opposite: None,
        }
    }

    /// The paper and ink to recolour an image onto, or `None` to show it as it is.
    ///
    /// The PDF's rule (`PdfTab::restyle`): the theme recolours a document and leaves a photo
    /// alone, and an inverted image, whatever it shows, is recoloured as the other half of the
    /// theme would — so under Dark, whose other half is Light, it is shown as it is.
    pub fn palette(&self, document: bool, inverted: bool) -> Option<Page> {
        match inverted {
            true => self.opposite,
            false => self.theme.filter(|_| document),
        }
    }

    /// Whether an image recoloured onto `page` has its transparent parts filled with the paper:
    /// on any page but the window's own, which would otherwise show through in the wrong colour.
    pub fn fills(&self, page: Page) -> bool {
        self.theme != Some(page)
    }

    /// Whether the answer turns on what the image shows: only for the theme's own recolouring,
    /// so Light and an inverted image never have one classified.
    fn asks(&self, inverted: bool) -> bool {
        self.palette(true, inverted) != self.palette(false, inverted)
    }
}

/// What an image tab shows: the file as decoded, kept so a restyle need not read it again, and
/// that texture under the look it was asked for.
pub struct Shown {
    pub original: gdk::Texture,
    pub texture: gdk::Texture,
    /// The size an SVG is shown at, its own, which its texture has more pixels than: it is drawn
    /// at the display's scale and at a zoom past 100 %. `None` for a raster, which has only its
    /// own pixels.
    pub size: Option<(i32, i32)>,
}

impl Shown {
    /// What the picture is handed: the texture, at its logical size.
    pub fn paintable(&self) -> gdk::Paintable {
        match self.size {
            None => self.texture.clone().upcast(),
            Some((width, height)) => Scaled::new(&self.texture, width, height).upcast(),
        }
    }
}

/// The zoom an image is drawn at while its picture is zoomed to `zoom`, `None` being fitted: an
/// SVG's past 100 %, so a deep zoom stays sharp. A raster has only its own pixels, and anything
/// fitted or zoomed out is drawn at its own size and shown smaller.
pub fn drawn_zoom(path: &Path, zoom: Option<f64>) -> f64 {
    match (svg(path), zoom) {
        (true, Some(zoom)) => zoom.max(1.0),
        _ => 1.0,
    }
}

/// Decode `path`, unless `original` already holds it, and recolour it as `look` asks, an SVG drawn
/// at `scale` (the display's) and `zoom` ([`drawn_zoom`]). On a worker: a large scan takes a
/// while to decode, and longer to recolour.
pub fn show(
    path: &Path,
    original: Option<gdk::Texture>,
    look: Look,
    inverted: bool,
    scale: i32,
    zoom: f64,
) -> Result<Shown, glib::Error> {
    if svg(path) {
        return show_svg(path, look, inverted, scale, zoom);
    }
    let original = match original {
        Some(texture) => texture,
        None => decode(path)?,
    };
    let texture = recoloured(path, look, inverted, || Some(original.clone()))
        .map_or_else(|| original.clone(), |t| t.upcast());
    Ok(Shown {
        original,
        texture,
        size: None,
    })
}

/// [`show`] for an SVG, drawn again from its text every time, at the display's scale as GTK draws
/// one: it is recoloured as a vector, through a filter, and a drawing costs little.
fn show_svg(
    path: &Path,
    look: Look,
    inverted: bool,
    scale: i32,
    zoom: f64,
) -> Result<Shown, glib::Error> {
    let (text, _) = gio::File::for_path(path).load_bytes(gio::Cancellable::NONE)?;
    let (original, size) = rasterise(&text, scale, zoom)?;
    let texture = look
        .palette(true, inverted)
        .and_then(|page| svg_filtered(path, page, look.fills(page)))
        .and_then(|svg| rasterise(&svg, scale, zoom).ok())
        .map_or_else(|| original.clone(), |(texture, _)| texture);
    Ok(Shown {
        original,
        texture,
        size: Some(size),
    })
}

/// The most pixels an SVG is drawn at for a zoom: 16 megapixels, 64 MB, about twice a 4K
/// display's. A deeper zoom into a larger drawing enlarges that rather than drawing it at any
/// size; the display's scale alone is never cut.
const MAX_ZOOMED: f64 = 16e6;

/// An SVG drawn at `scale` times its own size, through gdk-pixbuf, as GTK draws a picture's, and
/// `zoom` times that as far as [`MAX_ZOOMED`] goes; with that own size.
fn rasterise(svg: &[u8], scale: i32, zoom: f64) -> Result<(gdk::Texture, (i32, i32)), glib::Error> {
    let loader = gdk_pixbuf::PixbufLoader::new();
    let size = Rc::new(Cell::new((0, 0)));
    let own = size.clone();
    loader.connect_size_prepared(move |loader, w, h| {
        own.set((w, h));
        let (w, h, scale) = (f64::from(w), f64::from(h), f64::from(scale));
        let zoom = zoom
            .min((MAX_ZOOMED / (w * h * scale * scale)).sqrt())
            .max(1.0);
        let times = scale * zoom;
        loader.set_size((w * times).round() as i32, (h * times).round() as i32);
    });
    loader.write(svg)?;
    loader.close()?;
    let pixbuf = loader
        .pixbuf()
        .ok_or_else(|| glib::Error::new(gdk_pixbuf::PixbufError::Failed, "no image"))?;
    let format = match pixbuf.has_alpha() {
        true => gdk::MemoryFormat::R8g8b8a8,
        false => gdk::MemoryFormat::R8g8b8,
    };
    let texture = gdk::MemoryTexture::new(
        pixbuf.width(),
        pixbuf.height(),
        format,
        &pixbuf.read_pixel_bytes(),
        pixbuf.rowstride() as usize,
    );
    Ok((texture.upcast(), size.get()))
}

/// What the preview hands WebKit for an image: the file as it is, or new bytes of a type.
pub enum Served {
    File,
    Bytes(glib::Bytes, &'static str),
}

/// [`show`] for the preview: only an image that is recoloured, or that its EXIF turns where WebKit
/// would not, is decoded and encoded again, as a PNG; everything else, and anything that fails on
/// the way, is served as the file.
pub fn serve(path: &Path, look: Look, inverted: bool) -> Served {
    if svg(path) {
        return match look
            .palette(true, inverted)
            .and_then(|page| svg_filtered(path, page, look.fills(page)))
        {
            Some(svg) => Served::Bytes(glib::Bytes::from_owned(svg), "image/svg+xml"),
            None => Served::File,
        };
    }
    // WebKit turns a JPEG the way its EXIF says, and no other image.
    let turned = || !matches!(extension(path).as_str(), "jpg" | "jpeg") && tag(path) != 1;
    match recoloured(path, look, inverted, || decode(path).ok()) {
        Some(texture) => Served::Bytes(texture.save_to_png_bytes(), "image/png"),
        None if turned() => match decode(path) {
            Ok(texture) => Served::Bytes(texture.save_to_png_bytes(), "image/png"),
            Err(_) => Served::File,
        },
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

/// `path` decoded by GDK and turned the way up its EXIF says, which GDK's decoders ignore. One
/// too large to hold twice is left as it is, as the recolouring leaves it.
fn decode(path: &Path) -> Result<gdk::Texture, glib::Error> {
    let texture = gdk::Texture::from_filename(path)?;
    let tag = match extension(path).as_str() {
        "tif" | "tiff" => after_libtiff(tag(path)),
        _ => tag(path),
    };
    let Some(p) = (tag != 1).then(|| pixels(&texture)).flatten() else {
        return Ok(texture);
    };
    let (data, width, height) = orientation::apply(&p.data, p.width, p.height, tag);
    Ok(Pixels {
        data,
        width,
        height,
    }
    .texture()
    .upcast())
}

/// The EXIF Orientation tag of the image at `path`, 1 where there is none.
fn tag(path: &Path) -> u8 {
    std::fs::File::open(path).map_or(1, |mut file| orientation::read(&mut file))
}

/// What is left of a TIFF's `tag` once GDK has decoded it. GDK hands a TIFF that is not upright
/// to libtiff's `TIFFReadRGBAImageOriented`, which does the tag's mirroring but not its quarter
/// turn: 2 to 4 come out upright, and 5 to 8 wanting a turn about one diagonal or the other.
fn after_libtiff(tag: u8) -> u8 {
    match tag {
        5 | 7 => 5,
        6 | 8 => 7,
        _ => 1,
    }
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
    if look.fills((paper, ink)) {
        recolour::onto_white(&mut pixels.data);
    }
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

/// An SVG's text with the recolouring filter around its content, drawn over its own paper when
/// `fill` asks for it ([`Look::fills`]).
fn svg_filtered(path: &Path, (paper, ink): Page, fill: bool) -> Option<Vec<u8>> {
    let text = std::fs::read_to_string(path).ok()?;
    match fill {
        true => recolour::recolour_svg_on_paper(&text, paper, ink),
        false => recolour::recolour_svg(&text, paper, ink),
    }
    .map(String::into_bytes)
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

mod imp {
    use std::cell::{Cell, OnceCell};

    use gtk::prelude::*;
    use gtk::subclass::prelude::*;
    use gtk::{gdk, glib};

    #[derive(Default)]
    pub struct Scaled {
        pub texture: OnceCell<gdk::Texture>,
        pub size: Cell<(i32, i32)>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Scaled {
        const NAME: &'static str = "AccentScaled";
        type Type = super::Scaled;
        type Interfaces = (gdk::Paintable,);
    }

    impl ObjectImpl for Scaled {}

    impl PaintableImpl for Scaled {
        fn flags(&self) -> gdk::PaintableFlags {
            gdk::PaintableFlags::STATIC_SIZE | gdk::PaintableFlags::STATIC_CONTENTS
        }

        fn intrinsic_width(&self) -> i32 {
            self.size.get().0
        }

        fn intrinsic_height(&self) -> i32 {
            self.size.get().1
        }

        fn snapshot(&self, snapshot: &gdk::Snapshot, width: f64, height: f64) {
            if let Some(texture) = self.texture.get() {
                texture.snapshot(snapshot, width, height);
            }
        }
    }
}

glib::wrapper! {
    /// A texture of more pixels than it is to be shown at, sized as it is to be shown: GTK's own
    /// `GtkScaler`, which it keeps private.
    pub struct Scaled(ObjectSubclass<imp::Scaled>) @implements gdk::Paintable;
}

impl Scaled {
    fn new(texture: &gdk::Texture, width: i32, height: i32) -> Scaled {
        let scaled: Scaled = glib::Object::new();
        let _ = scaled.imp().texture.set(texture.clone());
        scaled.imp().size.set((width, height));
        scaled
    }

    /// The texture as drawn, for a drill.
    #[cfg(feature = "bench")]
    pub fn texture(&self) -> gdk::Texture {
        self.imp().texture.get().cloned().expect("set in `new`")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DARK: Page = ([29, 29, 32], [235, 235, 235]);
    const CREAM: Page = ([253, 246, 227], [101, 123, 131]);
    const NIGHT: Page = ([0, 43, 54], [131, 148, 150]);

    /// The PDF's rule: the theme recolours a document, and inverting puts any image on the other
    /// half's page, or shows it plain where that half has none. Only a page that is not the
    /// window's own gets its transparent parts filled.
    #[test]
    fn a_theme_recolours_a_document_and_inverting_takes_the_other_half() {
        let light = Look {
            theme: None,
            opposite: Some(DARK),
        };
        let dark = Look {
            theme: Some(DARK),
            opposite: None,
        };
        let cream = Look {
            theme: Some(CREAM),
            opposite: Some(NIGHT),
        };
        // (look, document, inverted) -> (palette, filled)
        let table = [
            (light, false, false, None),
            (light, true, false, None),
            (light, false, true, Some((DARK, true))),
            (light, true, true, Some((DARK, true))),
            (dark, false, false, None),
            (dark, true, false, Some((DARK, false))),
            (dark, false, true, None),
            (dark, true, true, None),
            (cream, true, false, Some((CREAM, false))),
            (cream, false, true, Some((NIGHT, true))),
        ];
        for (look, document, inverted, want) in table {
            let got = look.palette(document, inverted).map(|p| (p, look.fills(p)));
            assert_eq!(
                got, want,
                "{look:?} document={document} inverted={inverted}"
            );
        }
        // Paper shows every image as its file, inverted or not.
        for (document, inverted) in [(false, false), (true, false), (false, true), (true, true)] {
            assert_eq!(Look::paper().palette(document, inverted), None);
        }
        // Only a theme's own recolouring turns on what the image shows.
        assert!(!light.asks(false) && dark.asks(false) && cream.asks(false));
        assert!(!light.asks(true) && !dark.asks(true) && !cream.asks(true));
    }

    /// A TIFF comes out of [`decode`] upright whatever its tag, GDK having done part of the turn
    /// ([`after_libtiff`]): a 3 × 2 grey image, each pixel its own shade.
    #[test]
    fn a_tiff_is_decoded_upright() {
        let shades = [10u8, 20, 30, 40, 50, 60];
        for tag in 1..=8u16 {
            // Little-endian: the header, the pixels, then the directory.
            let entries: [(u16, u16, u32); 10] = [
                (256, 3, 3), // width
                (257, 3, 2), // height
                (258, 3, 8), // bits per sample
                (259, 3, 1), // no compression
                (262, 3, 1), // black is zero
                (273, 4, 8), // the strip's offset
                (274, 3, u32::from(tag)),
                (277, 3, 1), // samples per pixel
                (278, 3, 2), // rows per strip
                (279, 4, 6), // the strip's length
            ];
            let mut file = [&b"II*\0"[..], &14u32.to_le_bytes(), &shades].concat();
            file.extend(10u16.to_le_bytes());
            for (id, kind, value) in entries {
                file.extend(
                    [
                        &id.to_le_bytes()[..],
                        &kind.to_le_bytes(),
                        &1u32.to_le_bytes(),
                    ]
                    .concat(),
                );
                file.extend(value.to_le_bytes());
            }
            file.extend(0u32.to_le_bytes());
            let path =
                std::env::temp_dir().join(format!("accent-{}-{tag}.tif", std::process::id()));
            std::fs::write(&path, file).unwrap();
            let decoded = pixels(&decode(&path).unwrap()).unwrap();
            std::fs::remove_file(&path).unwrap();

            let grey: Vec<u8> = shades.iter().flat_map(|&v| [v, v, v, 255]).collect();
            let (want, width, height) = orientation::apply(&grey, 3, 2, tag as u8);
            let got = (decoded.data, decoded.width, decoded.height);
            assert_eq!(got, (want, width, height), "tag {tag}");
        }
    }
}
