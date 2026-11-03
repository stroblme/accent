//! Apply the same image appearance in tabs and previews.
//!
//! Theme recolouring puts document-like images on the page's paper and ink, leaving photos
//! unchanged. Inversion uses the opposite theme half, as PDF pages do. Disabling recolouring
//! leaves images unchanged and puts inverted images on the theme page.
//!
//! [`Look::palette`] snapshots the theme on the main thread; workers decode, classify and
//! recolour through GDK and `accent_core::recolour` ([`show`], [`serve`]).

use std::cell::{Cell, OnceCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime};

use accent_core::recolour::{self, Verdict};
use accent_core::{animation, orientation};
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
    /// Whether the theme recolours images, and the diagrams a note embeds, unasked (Preferences).
    images: bool,
    diagrams: bool,
}

impl Look {
    /// The look now. On the main thread only: `theme.rs` keeps the choice there.
    pub fn now() -> Look {
        let dark = adw::StyleManager::default().is_dark();
        let kinds = theme::recolour();
        Look {
            theme: theme::page_colours(dark),
            opposite: theme::page_colours(!dark),
            images: kinds.images,
            diagrams: kinds.diagrams,
        }
    }

    /// The look of a printout or an export: every image as its file is, whatever the theme on
    /// screen and whatever the reader inverted.
    pub fn paper() -> Look {
        Look {
            theme: None,
            opposite: None,
            images: false,
            diagrams: false,
        }
    }

    /// Return recolouring paper and ink, or `None` to show the image unchanged.
    ///
    /// Theme recolouring affects documents, leaving photos alone. Inversion applies
    /// `accent_core::recolour::onto` regardless of content; Dark's opposite Light half shows
    /// images unchanged when recolouring is enabled.
    pub fn palette(&self, document: bool, inverted: bool) -> Option<Page> {
        let own = self.theme.filter(|_| document || inverted);
        recolour::onto(self.images, inverted, own, self.opposite)
    }

    /// [`Self::palette`] for a diagram a note embeds, under the diagrams' switch: the canvas's
    /// rule (`DiagramTab::restyle`).
    fn diagram(&self, inverted: bool) -> Option<Page> {
        recolour::onto(self.diagrams, inverted, self.theme, self.opposite)
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

/// What an image tab shows: the file as decoded, and that texture under the look it was asked for.
pub struct Shown {
    /// A raster's decoding, kept so a restyle need not read it again; `None` for an SVG, which a
    /// restyle draws again from its text, so it is held only as shown.
    pub original: Option<gdk::Texture>,
    pub texture: gdk::Texture,
    /// SVG logical size, distinct from texture pixels rendered for display scale and zoom. `None` for rasters.
    pub size: Option<(i32, i32)>,
    /// An animation's frames, recoloured as `texture`, its first, is; none for a still.
    pub frames: Vec<Frame>,
}

/// A frame of an animation, and how long it shows: `None` for good, the last of one that plays
/// once.
pub type Frame = (gdk::Texture, Option<Duration>);

impl Shown {
    /// What the picture is handed: the texture at its logical size, or the animation playing.
    pub fn paintable(&self) -> gdk::Paintable {
        match self.size {
            None if !self.frames.is_empty() => Animated::new(self.frames.clone()).upcast(),
            None => self.texture.clone().upcast(),
            Some((width, height)) => Scaled::new(&self.texture, width, height).upcast(),
        }
    }
}

/// Choose SVG render zoom above 100 %, capped by [`MAX_ZOOMED`] at display `scale`.
/// Fit and zoomed-out views render at intrinsic size; rasters have only their original pixels.
pub fn drawn_zoom(path: &Path, zoom: Option<f64>, size: Option<(i32, i32)>, scale: i32) -> f64 {
    match (svg(path), zoom) {
        (true, Some(zoom)) => size.map_or(zoom.max(1.0), |size| capped(zoom, size, scale)),
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
    // An animation keeps no decoding: a restyle reads its frames again.
    if original.is_none()
        && let Some(frames) = frames(path, look, inverted)
    {
        return Ok(Shown {
            original: None,
            texture: frames[0].0.clone(),
            size: None,
            frames,
        });
    }
    let original = match original {
        Some(texture) => texture,
        None => decode(path)?,
    };
    let texture = recoloured(path, look, inverted, || Some(original.clone()))
        .map_or_else(|| original.clone(), |t| t.upcast());
    Ok(Shown {
        original: Some(original),
        texture,
        size: None,
        frames: Vec::new(),
    })
}

/// Render SVG text through a vector recolouring filter at display scale.
/// Create a plain texture only when shown to avoid another large allocation at deep zoom.
fn show_svg(
    path: &Path,
    look: Look,
    inverted: bool,
    scale: i32,
    zoom: f64,
) -> Result<Shown, glib::Error> {
    let recoloured = look
        .palette(true, inverted)
        .and_then(|page| svg_filtered(path, page, look.fills(page)))
        .and_then(|svg| rasterise(&svg, scale, zoom).ok());
    let (texture, size) = match recoloured {
        Some(drawn) => drawn,
        None => {
            let (text, _) = gio::File::for_path(path).load_bytes(gio::Cancellable::NONE)?;
            rasterise(&text, scale, zoom)?
        }
    };
    Ok(Shown {
        original: None,
        texture,
        size: Some(size),
        frames: Vec::new(),
    })
}

/// SVG zoom budget: 16 megapixels or 64 MB. Beyond it, enlarge the existing render.
/// Display scale alone is never reduced.
const MAX_ZOOMED: f64 = 16e6;

/// `zoom`, at least 1, as far as [`MAX_ZOOMED`] lets a drawing of `size` go at `scale`.
fn capped(zoom: f64, (width, height): (i32, i32), scale: i32) -> f64 {
    let pixels = f64::from(width) * f64::from(height) * f64::from(scale * scale);
    zoom.min((MAX_ZOOMED / pixels).sqrt()).max(1.0)
}

/// Rasterise an SVG through gdk-pixbuf at display `scale` and capped `zoom`, returning its intrinsic size.
fn rasterise(svg: &[u8], scale: i32, zoom: f64) -> Result<(gdk::Texture, (i32, i32)), glib::Error> {
    let loader = gdk_pixbuf::PixbufLoader::new();
    let size = Rc::new(Cell::new((0, 0)));
    let own = size.clone();
    loader.connect_size_prepared(move |loader, w, h| {
        own.set((w, h));
        let times = f64::from(scale) * capped(zoom, (w, h), scale);
        let (w, h) = (f64::from(w) * times, f64::from(h) * times);
        loader.set_size(w.round() as i32, h.round() as i32);
    });
    loader.write(svg)?;
    loader.close()?;
    let pixbuf = loader
        .pixbuf()
        .ok_or_else(|| glib::Error::new(gdk_pixbuf::PixbufError::Failed, "no image"))?;
    Ok((texture(&pixbuf, &pixbuf.read_pixel_bytes()), size.get()))
}

/// `pixbuf` as a texture of its pixels, `bytes`.
fn texture(pixbuf: &gdk_pixbuf::Pixbuf, bytes: &glib::Bytes) -> gdk::Texture {
    let format = match pixbuf.has_alpha() {
        true => gdk::MemoryFormat::R8g8b8a8,
        false => gdk::MemoryFormat::R8g8b8,
    };
    let stride = pixbuf.rowstride() as usize;
    gdk::MemoryTexture::new(pixbuf.width(), pixbuf.height(), format, bytes, stride).upcast()
}

/// What an exported page holds for an image: the file as it is, or new bytes of a type.
pub enum Served {
    File,
    Bytes(glib::Bytes, &'static str),
}

/// Serve an exported page's images unchanged unless recolouring, EXIF correction or unsupported
/// formats require PNG. Failures fall back to the original file.
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
    // A browser draws no TIFF, and turns a JPEG the way its EXIF says but no other image.
    let converted = || match extension(path).as_str() {
        "tif" | "tiff" => true,
        "jpg" | "jpeg" => false,
        _ => tag(path) != 1,
    };
    // An animation as it is: recoloured, it would be a still.
    let recoloured = (!animated(path))
        .then(|| recoloured(path, look, inverted, || decode(path).ok()))
        .flatten();
    match recoloured {
        Some(texture) => Served::Bytes(texture.save_to_png_bytes(), "image/png"),
        None if converted() => match decode(path) {
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

/// Remaining TIFF orientation after libtiff decoding: tags 2–4 are upright, 5–8 still need diagonal turns.
/// `TIFFReadRGBAImageOriented` performs mirroring but not the required quarter turn.
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

/// Recolour a raster for `look`, or return `None` to preserve it.
/// Decode at most once and only when classification or recolouring needs pixels.
fn recoloured(
    path: &Path,
    look: Look,
    inverted: bool,
    decode: impl Fn() -> Option<gdk::Texture>,
) -> Option<gdk::MemoryTexture> {
    let texture = OnceCell::new();
    let decoded = || texture.get_or_init(&decode).clone();
    let page = recolouring(path, look, inverted, decoded)?;
    paint(&decoded()?, look, page)
}

/// The page `look` puts the raster at `path` onto, `None` to leave it as it is; its pixels,
/// `decoded`, are asked for only where the classifier has to look at them.
fn recolouring(
    path: &Path,
    look: Look,
    inverted: bool,
    decoded: impl Fn() -> Option<gdk::Texture>,
) -> Option<Page> {
    look.palette(true, inverted)?;
    let document = look.asks(inverted)
        && classified(path, || decoded().map(|t| measure(&t))).is_some_and(|v| v.document);
    look.palette(document, inverted)
}

/// How many pixels an animation's frames may hold in all, 64 MB of them: one past it shows its
/// first frame, as a still does.
const FRAMES: i64 = 16_000_000;

/// The frames of the animation at `path` as gdk-pixbuf plays it, each recoloured as a still
/// would be, judged by the first; `None` for a still, for a file gdk-pixbuf plays no animation of
/// (glycin's loaders read no APNG) and for one past [`FRAMES`]. Reads the file, so on a worker.
fn frames(path: &Path, look: Look, inverted: bool) -> Option<Vec<Frame>> {
    if !animated(path) {
        return None;
    }
    let animation = gdk_pixbuf::PixbufAnimation::from_file(path).ok()?;
    // A clock of its own, stepped a frame's delay at a time, so each frame is read once.
    let mut clock = SystemTime::UNIX_EPOCH;
    let iter = animation.iter(Some(clock));
    let (mut frames, mut first, mut pixels) = (Vec::new(), None::<glib::Bytes>, 0);
    loop {
        let pixbuf = iter.pixbuf();
        let bytes = pixbuf.read_pixel_bytes();
        // glycin's loader (gdk-pixbuf 2.44) learns which frame is its last only by looking past
        // it, and then hands back the first frame's own pixels.
        if first.as_ref().is_some_and(|f| f.as_ptr() == bytes.as_ptr()) {
            break;
        }
        pixels += i64::from(pixbuf.width()) * i64::from(pixbuf.height());
        if pixels > FRAMES {
            return None;
        }
        let delay = iter.delay_time();
        frames.push((texture(&pixbuf, &bytes), delay));
        first.get_or_insert(bytes);
        // gdk-pixbuf's own loaders know their last frame from the start.
        match delay {
            Some(delay) if !iter.on_currently_loading_frame() => {
                clock += delay;
                // Just past the frame's end, as gdk-pixbuf's glycin loader before 2.44.5
                // (Fedora 44 ships 2.44.4) moves on only once that end has passed.
                iter.advance(clock + Duration::from_millis(1));
            }
            _ => break,
        }
    }
    if frames.len() < 2 {
        return None;
    }
    let first = frames[0].0.clone();
    let Some(page) = recolouring(path, look, inverted, || Some(first.clone())) else {
        return Some(frames);
    };
    let recoloured = |(texture, delay): Frame| match paint(&texture, look, page) {
        Some(painted) => (painted.upcast(), delay),
        None => (texture, delay),
    };
    Some(frames.into_iter().map(recoloured).collect())
}

/// `texture` recoloured onto `page`, its transparent parts filled where [`Look::fills`] says.
fn paint(texture: &gdk::Texture, look: Look, (paper, ink): Page) -> Option<gdk::MemoryTexture> {
    let mut pixels = pixels(texture)?;
    if look.fills((paper, ink)) {
        recolour::onto_white(&mut pixels.data);
    }
    recolour::recolour(&mut pixels.data, paper, ink);
    Some(pixels.texture())
}

/// A picture with no file behind it, a notebook's output: `bytes` decoded, or an SVG's text drawn
/// at `scale` and `zoom`, recoloured as [`show`] recolours an image file (the image rule), and
/// whether the look changed it. Nothing is cached, there being no file to keep a verdict for. On a
/// worker.
pub fn show_bytes(
    bytes: &[u8],
    svg: bool,
    look: Look,
    scale: i32,
    zoom: f64,
) -> Result<(Shown, bool), glib::Error> {
    if svg {
        let filtered = look
            .palette(true, false)
            .and_then(|page| filtered(&String::from_utf8_lossy(bytes), page, look.fills(page)));
        let drawn = filtered.as_deref().map_or(bytes, str::as_bytes);
        let (texture, size) = rasterise(drawn, scale, zoom)?;
        let shown = Shown {
            original: None,
            texture,
            size: Some(size),
            frames: Vec::new(),
        };
        return Ok((shown, filtered.is_some()));
    }
    let original = gdk::Texture::from_bytes(&glib::Bytes::from(bytes))?;
    let document = look.asks(false) && measure(&original).document;
    let recoloured = look
        .palette(document, false)
        .and_then(|page| paint(&original, look, page));
    let shown = Shown {
        original: Some(original.clone()),
        texture: recoloured.clone().map_or(original, |t| t.upcast()),
        size: None,
        frames: Vec::new(),
    };
    Ok((shown, recoloured.is_some()))
}

/// A diagram's page a note embeds, as its SVG `svg` (`diagram::embed`), drawn at `scale` and `zoom`
/// as [`show`] draws an SVG file, recoloured under the diagrams' switch. On a worker.
pub fn show_diagram(
    svg: &str,
    look: Look,
    inverted: bool,
    scale: i32,
    zoom: f64,
) -> Result<Shown, glib::Error> {
    let filtered = look
        .diagram(inverted)
        .and_then(|page| filtered(svg, page, look.fills(page)));
    let (texture, size) = rasterise(filtered.as_deref().unwrap_or(svg).as_bytes(), scale, zoom)?;
    Ok(Shown {
        original: None,
        texture,
        size: Some(size),
        frames: Vec::new(),
    })
}

/// The classifier's verdict on `texture`. One too large to look at is not a document, unmeasured.
pub(crate) fn measure(texture: &gdk::Texture) -> Verdict {
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

/// Process-wide file-classification cache shared by image tabs and previews.
fn verdicts() -> &'static Mutex<HashMap<PathBuf, (Stamp, Verdict)>> {
    static VERDICTS: OnceLock<Mutex<HashMap<PathBuf, (Stamp, Verdict)>>> = OnceLock::new();
    VERDICTS.get_or_init(Mutex::default)
}

/// Which version of a file a verdict was taken on: its mtime and size.
pub(crate) type Stamp = (SystemTime, u64);

pub(crate) fn stamp(path: &Path) -> Option<Stamp> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

/// An SVG's text with the recolouring filter around its content, drawn over its own paper when
/// `fill` asks for it ([`Look::fills`]).
fn svg_filtered(path: &Path, page: Page, fill: bool) -> Option<Vec<u8>> {
    let text = std::fs::read_to_string(path).ok()?;
    filtered(&text, page, fill).map(String::into_bytes)
}

/// [`svg_filtered`] of an SVG's text.
fn filtered(text: &str, (paper, ink): Page, fill: bool) -> Option<String> {
    match fill {
        true => recolour::recolour_svg_on_paper(text, paper, ink),
        false => recolour::recolour_svg(text, paper, ink),
    }
}

/// Line art, always recoloured: the vector counterpart of a scan.
fn svg(path: &Path) -> bool {
    extension(path) == "svg"
}

/// Whether `path` is an animated GIF, WebP or PNG, which [`frames`] plays and an export carries
/// as it is. Reads the file, so off the main thread.
fn animated(path: &Path) -> bool {
    std::fs::File::open(path).is_ok_and(animation::animated)
}

/// A GIF of two 1 × 1 frames, red then blue, a tenth of a second each, looping.
#[cfg(any(test, feature = "bench"))]
pub(crate) const TWO_FRAMES: &[u8] = b"GIF89a\x01\0\x01\0\x80\0\0\xE6\x1E\x1E\x1E\x1E\xE6\
    \x21\xFF\x0BNETSCAPE2.0\x03\x01\0\0\0\
    \x21\xF9\x04\x04\x0A\0\0\0\x2C\0\0\0\0\x01\0\x01\0\0\x02\x02\x44\x01\0\
    \x21\xF9\x04\x04\x0A\0\0\0\x2C\0\0\0\0\x01\0\x01\0\0\x02\x02\x4C\x01\0\x3B";

fn extension(path: &Path) -> String {
    path.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

mod imp {
    use std::cell::{Cell, OnceCell, RefCell};

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

    #[derive(Default)]
    pub struct Animated {
        pub frames: OnceCell<Vec<super::Frame>>,
        /// The frame on show.
        pub at: Cell<usize>,
        /// The timer to the next frame.
        pub next: RefCell<Option<glib::SourceId>>,
        /// Whether it was drawn since the timer last moved it on.
        pub drawn: Cell<bool>,
    }

    impl Animated {
        pub fn frame(&self) -> &gdk::Texture {
            &self.frames.get().expect("set in `new`")[self.at.get()].0
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Animated {
        const NAME: &'static str = "AccentAnimated";
        type Type = super::Animated;
        type Interfaces = (gdk::Paintable,);
    }

    impl ObjectImpl for Animated {
        fn dispose(&self) {
            if let Some(next) = self.next.take() {
                next.remove();
            }
        }
    }

    impl PaintableImpl for Animated {
        fn flags(&self) -> gdk::PaintableFlags {
            gdk::PaintableFlags::STATIC_SIZE
        }

        fn intrinsic_width(&self) -> i32 {
            self.frame().width()
        }

        fn intrinsic_height(&self) -> i32 {
            self.frame().height()
        }

        fn current_image(&self) -> gdk::Paintable {
            self.frame().clone().upcast()
        }

        fn snapshot(&self, snapshot: &gdk::Snapshot, width: f64, height: f64) {
            self.frame().snapshot(snapshot, width, height);
            self.drawn.set(true);
            if self.next.borrow().is_none() {
                self.obj().play();
            }
        }
    }
}

glib::wrapper! {
    /// Show a high-resolution texture at its logical size, matching GTK's private GtkScaler.
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

glib::wrapper! {
    /// An animation's frames in turn, each for its delay, while it is drawn: one nothing draws —
    /// scrolled away, its tab behind another or closed — stops on its frame, its timer gone, and
    /// goes on once drawn again.
    pub struct Animated(ObjectSubclass<imp::Animated>) @implements gdk::Paintable;
}

impl Animated {
    fn new(frames: Vec<Frame>) -> Animated {
        let animated: Animated = glib::Object::new();
        let _ = animated.imp().frames.set(frames);
        animated
    }

    /// The timer to the next frame, after this one's delay, unless this is the last of an
    /// animation that plays once. When it fires, the next frame shows if this one was drawn
    /// meanwhile, and the animation stops if not.
    fn play(&self) {
        let imp = self.imp();
        let frames = imp.frames.get().expect("set in `new`");
        let Some(delay) = frames[imp.at.get()].1 else {
            return;
        };
        let animated = self.downgrade();
        let next = glib::timeout_add_local_once(delay, move || {
            let Some(animated) = animated.upgrade() else {
                return;
            };
            let imp = animated.imp();
            imp.next.take();
            if imp.drawn.replace(false) {
                let count = imp.frames.get().map_or(1, Vec::len);
                imp.at.set((imp.at.get() + 1) % count);
                animated.invalidate_contents();
                animated.play();
            }
        });
        imp.next.replace(Some(next));
    }

    /// The frame on show, and whether its timer runs, for a drill.
    #[cfg(feature = "bench")]
    pub fn playing(&self) -> (usize, bool) {
        (self.imp().at.get(), self.imp().next.borrow().is_some())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    const DARK: Page = ([29, 29, 32], [235, 235, 235]);
    const CREAM: Page = ([253, 246, 227], [101, 123, 131]);
    const NIGHT: Page = ([0, 43, 54], [131, 148, 150]);

    /// Verify document recolouring, opposite-half inversion and transparent-paper filling.
    /// Only pages differing from the window background fill transparent areas.
    #[test]
    fn a_theme_recolours_a_document_and_inverting_takes_the_other_half() {
        let light = Look {
            theme: None,
            opposite: Some(DARK),
            images: true,
            diagrams: true,
        };
        let dark = Look {
            theme: Some(DARK),
            opposite: None,
            images: true,
            diagrams: true,
        };
        let cream = Look {
            theme: Some(CREAM),
            opposite: Some(NIGHT),
            images: true,
            diagrams: true,
        };
        // Images left alone: none recoloured, and an inverted one, photo or not, on the theme's.
        let off = Look {
            images: false,
            ..dark
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
            (off, true, false, None),
            (off, false, true, Some((DARK, false))),
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

    /// The shades of [`grey_tiff`]'s six pixels, row by row.
    const SHADES: [u8; 6] = [10, 20, 30, 40, 50, 60];

    /// A 3 × 2 grey TIFF, each pixel its own shade, stored the way `tag` says to turn it.
    pub(crate) fn grey_tiff(tag: u16) -> Vec<u8> {
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
        let mut file = [&b"II*\0"[..], &14u32.to_le_bytes(), &SHADES].concat();
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
        file
    }

    /// An animation shows each of its frames in turn, each recoloured as a still would be:
    /// inverted under Light, onto Dark's page. A still has none.
    #[test]
    fn an_animation_shows_every_frame_recoloured() {
        let light = Look {
            theme: None,
            opposite: Some(DARK),
            images: true,
            diagrams: true,
        };
        let dir = std::env::temp_dir();
        let (gif, tif) = (
            dir.join(format!("accent-{}.gif", std::process::id())),
            dir.join(format!("accent-{}-still.tif", std::process::id())),
        );
        std::fs::write(&gif, TWO_FRAMES).unwrap();
        std::fs::write(&tif, grey_tiff(1)).unwrap();
        let plain = show(&gif, None, light, false, 1, 1.0).unwrap();
        let inverted = show(&gif, None, light, true, 1, 1.0).unwrap();
        let still = show(&tif, None, light, false, 1, 1.0).unwrap();
        std::fs::remove_file(&gif).unwrap();
        std::fs::remove_file(&tif).unwrap();

        let delays: Vec<_> = plain.frames.iter().map(|f| f.1).collect();
        assert_eq!(delays, [Some(Duration::from_millis(100)); 2]);
        let shade = |frame: &Frame| pixels(&frame.0).unwrap().data[..3].to_vec();
        let shades = |shown: &Shown| shown.frames.iter().map(shade).collect::<Vec<_>>();
        assert_eq!(shades(&plain), [[230, 30, 30], [30, 30, 230]]);
        let (plain, inverted) = (shades(&plain), shades(&inverted));
        assert!(inverted[0] != plain[0] && inverted[1] != plain[1] && inverted[0] != inverted[1]);
        assert!(still.frames.is_empty());
    }

    /// A TIFF comes out of [`decode`] upright whatever its tag, GDK having done part of the turn
    /// ([`after_libtiff`]).
    #[test]
    fn a_tiff_is_decoded_upright() {
        for tag in 1..=8u16 {
            let path =
                std::env::temp_dir().join(format!("accent-{}-{tag}.tif", std::process::id()));
            std::fs::write(&path, grey_tiff(tag)).unwrap();
            let decoded = pixels(&decode(&path).unwrap()).unwrap();
            std::fs::remove_file(&path).unwrap();

            let grey: Vec<u8> = SHADES.iter().flat_map(|&v| [v, v, v, 255]).collect();
            let (want, width, height) = orientation::apply(&grey, 3, 2, tag as u8);
            let got = (decoded.data, decoded.width, decoded.height);
            assert_eq!(got, (want, width, height), "tag {tag}");
        }
    }
}
