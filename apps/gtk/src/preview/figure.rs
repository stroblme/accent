//! Pictures in the page: a vault image (`![[cat.png]]`), a diagram's page (`![[flow.drawio]]`),
//! a markdown or an HTML image. Each is a [`Figure`] in the text, loaded as its tab shows the
//! file: through the asset resolver, which holds it to the vault (refresh N12), on a worker, and
//! recoloured as the theme and Preferences say (`look`). A picture on the web is never fetched:
//! its alt text stands in, dimmed. Over a picture the menu offers Invert Image Colours.
//!
//! What is loaded is kept by the address the page names it with, as WebKit's page asked for it,
//! so a block written again shows its pictures at once, and a new look recolours each from the
//! file as decoded.

use super::Keys;
use super::native::Page;
use super::text::{Href, Ink, Object, Writer};
use crate::assets::{self, Resolve};
use crate::look::{self, Look};
use accent_core::markdown::{self, percent_encode};
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, gio, glib, graphene, gsk};
use std::cell::{Cell, OnceCell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

/// The side of the sign a picture that cannot be shown leaves, in pixels at zoom 1.
const MISSING: f64 = 16.0;

pub(super) fn image(w: &mut Writer, target: &str) {
    let uri = format!("accent://file/{}", percent_encode(target));
    picture(w, &uri, target, format!("![[{target}]]"), "image");
}

/// A diagram's page, inside the link a click follows to that page.
pub(super) fn diagram(w: &mut Writer, target: &str, anchor: Option<&str>) {
    let (page, href, source) = match anchor {
        Some(a) => (
            format!("?page={}", percent_encode(a)),
            format!("{target}#{a}"),
            format!("![[{target}#{a}]]"),
        ),
        None => (String::new(), target.to_string(), format!("![[{target}]]")),
    };
    let uri = format!("accent://file/{}{page}", percent_encode(target));
    w.open_link(Href::Open(href));
    picture(w, &uri, target, source, "diagram");
    w.close_link();
}

/// `![alt](dest)`, org's `[[file:x.png]]` too: `dest` is relative to the note, as a link is.
pub(super) fn markdown_image(w: &mut Writer, dest: &str, alt: &str) {
    let uri = resolved(w.page, dest);
    let name = if alt.is_empty() { dest } else { alt };
    picture(w, &uri, name, format!("![{alt}]({dest})"), "image");
}

pub(super) fn html_image(w: &mut Writer, src: &str, alt: &str) {
    let uri = resolved(w.page, src);
    let name = if alt.is_empty() { src } else { alt };
    let source = format!("<img src=\"{src}\" alt=\"{alt}\">");
    picture(w, &uri, name, source, "image");
}

/// `dest` against the note's folder, as WebKit resolved a page's address.
fn resolved(page: &Page, dest: &str) -> String {
    let base = markdown::base_uri(&page.note.borrow().0);
    glib::Uri::resolve_relative(Some(&base), dest, glib::UriFlags::NONE)
        .map_or_else(|_| dest.to_string(), |uri| uri.to_string())
}

/// The picture at `uri`, or, for one the vault does not hold (the web's), `name` dimmed. A table
/// cell, which holds text, gets the name.
fn picture(w: &mut Writer, uri: &str, name: &str, source: String, kind: &str) {
    let page = w.page;
    if w.in_cell() || markdown::accent_file(uri).is_none() {
        w.push(&page.tags.figure);
        w.text(name);
        w.pop();
        return;
    }
    let figure = Figure::new(uri, page.ink.clone());
    figure.fit(page.zoom.get(), page.text_width());
    page.figures.0.show(&figure, page);
    // A picture in a link is not underlined, as a browser underlines none.
    let link = w.tags.iter().position(|t| *t == page.tags.link);
    if let Some(i) = link {
        w.tags.remove(i);
    }
    w.push(&page.figures.solid(page));
    w.paintable(&figure, 0, Object::new(format!("{kind} {name}"), source));
    w.pop();
    if let Some(i) = link {
        w.tags.insert(i, page.tags.link.clone());
    }
}

/// The menu's own item over the picture at `offset`, if any: Invert Image Colours, greyed out
/// over an animation, which is never recoloured.
pub(super) fn menu_item(page: &Page, offset: i32) -> Option<gio::MenuItem> {
    let figure = page.buffer.iter_at_offset(offset).paintable()?;
    let loaded = figure
        .downcast::<Figure>()
        .ok()?
        .imp()
        .loaded
        .borrow()
        .clone()?;
    let (key, (shown, _)) = (loaded.key.as_ref()?, loaded.shown.as_ref()?);
    Some(match shown.animated {
        true => gio::MenuItem::new(
            Some(&format!("Invert Image Colours {}", look::ANIMATED_LABEL)),
            Some("preview.invert-animation"),
        ),
        false => {
            let item = gio::MenuItem::new(Some("Invert Image Colours"), None);
            item.set_action_and_target_value(Some("preview.invert-image"), Some(&key.to_variant()));
            item
        }
    })
}

/// Whether the look the pictures are drawn in changed with the theme, which
/// [`forget_images`] then draws them in.
pub(super) fn restyle(page: &Page) -> bool {
    let look = Look::now();
    page.figures.0.look.replace(look) != look
}

/// Every picture recoloured in the look now, from the file as decoded (an SVG drawn again),
/// each shown as it was until then; then `then`.
pub(super) fn forget_images(page: &Page, then: impl FnOnce() + 'static) {
    let state = &page.figures.0;
    state.prune();
    let again: Vec<(String, Option<gdk::Texture>)> = (state.loaded.borrow().iter())
        .map(|(uri, l)| {
            (
                uri.clone(),
                l.shown.as_ref().and_then(|s| s.0.original.clone()),
            )
        })
        .chain(state.loading.borrow().keys().map(|uri| (uri.clone(), None)))
        .collect();
    for (uri, original) in again {
        state.load(uri, page, original);
    }
    then();
}

/// The pictures of `key` (a file, or a folder under it) read again: it changed, came, went, moved
/// or was inverted. So are those not found or still being read whose address may name it.
/// Whether one of `key`'s is shown.
pub(super) fn forget_image(page: &Page, key: &str) -> bool {
    let state = &page.figures.0;
    state.prune();
    let folder = format!("{key}/");
    let mut shown = false;
    let again: Vec<String> = (state.loaded.borrow().iter())
        .filter(|(uri, l)| match l.key.as_deref() {
            Some(k) => {
                let hit = k == key || k.starts_with(&folder);
                shown |= hit;
                hit
            }
            None => may_name(uri, key),
        })
        .map(|(uri, _)| uri.clone())
        .chain(
            state
                .loading
                .borrow()
                .keys()
                .filter(|uri| may_name(uri, key))
                .cloned(),
        )
        .collect();
    for uri in again {
        state.loaded.borrow_mut().remove(&uri);
        state.load(uri, page, None);
    }
    shown
}

/// Every picture fitted to the zoom and the text's width; an SVG or a diagram drawn for another
/// zoom drawn again for this one, as an image tab draws one.
pub(super) fn resize(page: &Page) {
    let (zoom, width) = (page.zoom.get(), page.text_width());
    let state = &page.figures.0;
    for figure in state.live() {
        if figure.fit(zoom, width) {
            figure.invalidate_size();
        }
    }
    let loading = state.loading.borrow().clone();
    let again: Vec<String> = (state.loaded.borrow().iter())
        .filter(|(uri, l)| {
            let svg = l.shown.as_ref().is_some_and(|s| s.0.size.is_some());
            svg && l.zoom != zoom.max(1.0) && !loading.contains_key(*uri)
        })
        .map(|(uri, _)| uri.clone())
        .collect();
    for uri in again {
        state.load(uri, page, None);
    }
}

/// Whether the picture at `uri` may be the vault file `key` (or under the folder `key`) before
/// the vault has said which file it names: `![[cat.png]]` names `Attachments/cat.png` too.
fn may_name(uri: &str, key: &str) -> bool {
    let Some(rel) = markdown::accent_file(uri) else {
        return false;
    };
    let name = |p: &str| p.rsplit('/').next().unwrap_or(p).to_string();
    rel == key || rel.starts_with(&format!("{key}/")) || name(&rel) == name(key)
}

// ------------------------------------------------------------------------------------ figures

/// A picture as loaded: the vault key of its file, and what it shows with what is drawn of it,
/// `None` for no such file or one that does not read.
#[derive(Default)]
struct Loaded {
    key: Option<String>,
    shown: Option<(look::Shown, gdk::Paintable)>,
    /// The zoom an SVG was drawn for.
    zoom: f64,
}

/// A page's pictures: how they are found and drawn, and each one loaded, by its address.
pub(super) struct Figures(Rc<State>);

struct State {
    resolve: Arc<Resolve>,
    inverted: Keys,
    web: Keys,
    /// The look the pictures were last drawn in.
    look: Cell<Look>,
    loaded: RefCell<HashMap<String, Rc<Loaded>>>,
    /// The addresses being read, each with its latest read's ticket: an earlier read lands
    /// nowhere.
    loading: RefCell<HashMap<String, u64>>,
    tickets: Cell<u64>,
    figures: RefCell<Vec<glib::WeakRef<Figure>>>,
    /// Line height 1 for a picture or a formula: the prose's 1.6 would lead its whole height.
    solid: OnceCell<gtk::TextTag>,
    #[cfg(feature = "bench")]
    reads: Cell<u32>,
}

impl Figures {
    /// The pictures of the page `view` shows, found through `resolve` and drawn as `inverted` and
    /// `web` say (the window's keys); Invert Image Colours hands `on_invert` a picture's key.
    pub(super) fn new(
        view: &impl IsA<gtk::Widget>,
        resolve: impl Fn(&str) -> Option<(String, PathBuf)> + Send + Sync + 'static,
        inverted: Keys,
        web: Keys,
        on_invert: impl Fn(&str) + 'static,
    ) -> Figures {
        let invert = gio::SimpleAction::new("invert-image", Some(glib::VariantTy::STRING));
        invert.connect_activate(move |_, key| {
            if let Some(key) = key.and_then(|k| k.str()) {
                on_invert(key);
            }
        });
        let animation = gio::SimpleAction::new("invert-animation", None);
        animation.set_enabled(false);
        let actions = gio::SimpleActionGroup::new();
        actions.add_action(&invert);
        actions.add_action(&animation);
        view.insert_action_group("preview", Some(&actions));
        Figures(Rc::new(State {
            resolve: Arc::new(resolve),
            inverted,
            web,
            look: Cell::new(Look::now()),
            loaded: RefCell::default(),
            loading: RefCell::default(),
            tickets: Cell::new(0),
            figures: RefCell::default(),
            solid: OnceCell::new(),
            #[cfg(feature = "bench")]
            reads: Cell::new(0),
        }))
    }

    /// See [`State::solid`]; made after the page's tags, so it wins over them.
    pub(super) fn solid(&self, page: &Page) -> gtk::TextTag {
        self.0
            .solid
            .get_or_init(|| {
                let table = page.buffer.tag_table();
                let tag = gtk::TextTag::builder().line_height(1.0).build();
                table.add(&tag);
                tag.set_priority(table.size() - 1);
                tag
            })
            .clone()
    }
}

impl State {
    /// Show what is loaded of `figure`'s address, or read it.
    fn show(self: &Rc<Self>, figure: &Figure, page: &Page) {
        {
            let mut figures = self.figures.borrow_mut();
            // Before the list grows, so dropping the dead costs nothing in the long run.
            if figures.len() == figures.capacity() {
                figures.retain(|f| f.upgrade().is_some());
            }
            figures.push(figure.downgrade());
        }
        let uri = figure.uri();
        if let Some(loaded) = self.loaded.borrow().get(uri) {
            figure.imp().loaded.replace(Some(loaded.clone()));
            return;
        }
        if !self.loading.borrow().contains_key(uri) {
            self.load(uri.to_string(), page, None);
        }
    }

    fn live(&self) -> Vec<Figure> {
        self.figures
            .borrow()
            .iter()
            .filter_map(|f| f.upgrade())
            .collect()
    }

    /// Forget what no picture on the page shows.
    fn prune(&self) {
        let shown: HashSet<String> = self.live().iter().map(|f| f.uri().to_string()).collect();
        self.loaded
            .borrow_mut()
            .retain(|uri, _| shown.contains(uri));
    }

    /// Read `uri` for `page`'s note in the look now, from `original` where the file is decoded
    /// already, and show it in every picture of it once read.
    fn load(self: &Rc<Self>, uri: String, page: &Page, original: Option<gdk::Texture>) {
        let ticket = self.tickets.get() + 1;
        self.tickets.set(ticket);
        self.loading.borrow_mut().insert(uri.clone(), ticket);
        #[cfg(feature = "bench")]
        self.reads.set(self.reads.get() + 1);
        let read = Read {
            uri: uri.clone(),
            note: page.note.borrow().0.clone(),
            resolve: self.resolve.clone(),
            look: self.look.get(),
            inverted: self.inverted.borrow().clone(),
            web: self.web.borrow().clone(),
            scale: page.view.scale_factor(),
            zoom: page.zoom.get().max(1.0),
        };
        let state = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let loaded = read.read(original).await;
            let Some(state) = state.upgrade() else {
                return;
            };
            if state.loading.borrow().get(&uri) != Some(&ticket) {
                return;
            }
            state.loading.borrow_mut().remove(&uri);
            let loaded = Rc::new(loaded);
            state
                .loaded
                .borrow_mut()
                .insert(uri.clone(), loaded.clone());
            for figure in state.live().iter().filter(|f| f.uri() == uri) {
                figure.imp().loaded.replace(Some(loaded.clone()));
                figure.invalidate_size();
            }
        });
    }
}

/// What reading a picture takes off the main thread.
struct Read {
    uri: String,
    note: String,
    resolve: Arc<Resolve>,
    look: Look,
    inverted: HashSet<String>,
    web: HashSet<String>,
    scale: i32,
    zoom: f64,
}

impl Read {
    /// The picture: a file's on a worker, a diagram's page drawn on the main thread as the
    /// diagram's tab draws it, then recoloured on a worker.
    async fn read(self, original: Option<gdk::Texture>) -> Loaded {
        let Read {
            uri,
            note,
            resolve,
            look,
            inverted,
            web,
            scale,
            zoom,
        } = self;
        let page = assets::diagram_page(&uri);
        let file = crate::work::off_thread("picture", move || {
            let (key, path) = assets::asset_of(&*resolve, &note, &uri)?;
            let inverted = inverted.contains(&key);
            let shown = (!accent_core::path::is_diagram(&key))
                .then(|| look::show(&path, original, look, inverted, scale, zoom).ok());
            Some((key, path, inverted, shown))
        });
        let Some(Some((key, path, inverted, shown))) = file.await else {
            return Loaded {
                zoom,
                ..Loaded::default()
            };
        };
        let shown = match shown {
            Some(shown) => shown,
            None => {
                match crate::diagram::embed::svg(&path, page.as_deref(), web.contains(&key)).await {
                    Some(svg) => {
                        let svg = svg.to_string();
                        let drawn = crate::work::off_thread("diagram picture", move || {
                            look::show_diagram(&svg, look, inverted, scale, zoom).ok()
                        });
                        drawn.await.flatten()
                    }
                    None => None,
                }
            }
        };
        Loaded {
            key: Some(key),
            shown: shown.map(|s| {
                let paintable = s.paintable();
                (s, paintable)
            }),
            zoom,
        }
    }
}

// ------------------------------------------------------------------------------------- figure

glib::wrapper! {
    /// A picture in the text: nothing while it is read, then the picture at its size times the
    /// zoom, no wider than the text, or a missing-image sign where there is none to show. Nothing
    /// fades in (refresh T5).
    pub struct Figure(ObjectSubclass<imp::Figure>) @implements gdk::Paintable;
}

impl Figure {
    fn new(uri: &str, ink: Rc<Ink>) -> Figure {
        let figure: Figure = glib::Object::new();
        let _ = figure.imp().uri.set(uri.to_string());
        let _ = figure.imp().ink.set(ink);
        figure
    }

    fn uri(&self) -> &str {
        self.imp().uri.get().map_or("", String::as_str)
    }

    /// Fit to `zoom` and a text `width`; whether that changed what it asks for.
    fn fit(&self, zoom: f64, width: i32) -> bool {
        self.imp().fit.replace((zoom, width)) != (zoom, width)
    }
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Figure {
        pub(super) uri: OnceCell<String>,
        pub(super) ink: OnceCell<Rc<Ink>>,
        /// `None` while it is read.
        pub(super) loaded: RefCell<Option<Rc<Loaded>>>,
        /// The zoom and the text's width.
        pub(super) fit: Cell<(f64, i32)>,
    }

    impl Figure {
        /// Its size on the page: the picture's times the zoom, shrunk to the text's width.
        fn size(&self) -> (i32, i32) {
            let natural = match self.loaded.borrow().as_deref() {
                None => return (0, 0),
                Some(Loaded {
                    shown: Some((_, p)),
                    ..
                }) => (
                    f64::from(p.intrinsic_width()),
                    f64::from(p.intrinsic_height()),
                ),
                Some(_) => (MISSING, MISSING),
            };
            let (zoom, width) = self.fit.get();
            let (w, h) = (natural.0 * zoom, natural.1 * zoom);
            let shrink = (f64::from(width) / w).min(1.0);
            ((w * shrink).round() as i32, (h * shrink).round() as i32)
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Figure {
        const NAME: &'static str = "AccentPreviewFigure";
        type Type = super::Figure;
        type Interfaces = (gdk::Paintable,);
    }

    impl ObjectImpl for Figure {}

    impl PaintableImpl for Figure {
        fn intrinsic_width(&self) -> i32 {
            self.size().0
        }

        fn intrinsic_height(&self) -> i32 {
            self.size().1
        }

        fn snapshot(&self, snapshot: &gdk::Snapshot, width: f64, height: f64) {
            let Some(snapshot) = snapshot.downcast_ref::<gtk::Snapshot>() else {
                return;
            };
            let loaded = self.loaded.borrow();
            match loaded.as_deref().and_then(|l| l.shown.as_ref()) {
                // Mipmapped, so a photo shrunk to the column is not grainy.
                Some((_, p)) => match p.downcast_ref::<gdk::Texture>() {
                    Some(texture) => {
                        let rect = graphene::Rect::new(0.0, 0.0, width as f32, height as f32);
                        snapshot.append_scaled_texture(
                            texture,
                            gsk::ScalingFilter::Trilinear,
                            &rect,
                        );
                    }
                    None => p.snapshot(snapshot, width, height),
                },
                None if loaded.is_some() => missing(snapshot, width, height, self.ink.get()),
                None => {}
            }
        }
    }

    /// The theme's missing-image sign, dimmed as a figure's name is.
    fn missing(snapshot: &gtk::Snapshot, width: f64, height: f64, ink: Option<&Rc<Ink>>) {
        let (Some(display), Some(ink)) = (gdk::Display::default(), ink) else {
            return;
        };
        let icon = gtk::IconTheme::for_display(&display).lookup_icon(
            "image-missing-symbolic",
            &[],
            width as i32,
            1,
            gtk::TextDirection::None,
            gtk::IconLookupFlags::empty(),
        );
        let colour = crate::highlight::dim(ink.fg.get(), ink.page.get(), 0.55);
        icon.snapshot_symbolic(snapshot, width, height, &[colour]);
    }
}

/// A picture on the page as a drill reads it: its address and vault key, whether it is being
/// read, whether it shows the file, whether that is an animation, its size, its own size at the
/// zoom and whether its size is that shrunk to the text's width now, two of its pixels as drawn
/// (`r,g,b`), and its middle in the view's coordinates.
#[cfg(feature = "bench")]
pub struct Probe {
    pub uri: String,
    pub key: Option<String>,
    pub pending: bool,
    pub loaded: bool,
    pub animated: bool,
    pub size: (i32, i32),
    pub natural: (i32, i32),
    pub fits: bool,
    pub corner: String,
    pub centre: String,
    pub at: (i32, i32),
}

/// Every picture on the page, in order.
#[cfg(feature = "bench")]
pub(super) fn probe(page: &Page) -> Vec<Probe> {
    let mut out = Vec::new();
    let mut at = page.buffer.start_iter();
    loop {
        if let Some(figure) = at.paintable().and_then(|p| p.downcast::<Figure>().ok()) {
            out.push(probed(page, &figure, &at));
        }
        if !at.forward_find_char(|c| c == '\u{fffc}', None) {
            return out;
        }
    }
}

#[cfg(feature = "bench")]
fn probed(page: &Page, figure: &Figure, at: &gtk::TextIter) -> Probe {
    let loaded = figure.imp().loaded.borrow().clone();
    let shown = loaded.as_ref().and_then(|l| l.shown.as_ref());
    let size = (figure.intrinsic_width(), figure.intrinsic_height());
    let zoomed = |n: i32| (f64::from(n) * page.zoom.get()).round() as i32;
    let natural = shown.map_or((0, 0), |(_, p)| {
        (zoomed(p.intrinsic_width()), zoomed(p.intrinsic_height()))
    });
    let fits = size.0 == natural.0.min(page.text_width());
    let pixel = |fx: f64, fy: f64| {
        let Some((shown, _)) = shown else {
            return "-".to_string();
        };
        let texture = &shown.texture;
        let mut downloader = gdk::TextureDownloader::new(texture);
        downloader.set_format(gdk::MemoryFormat::R8g8b8a8);
        let (bytes, stride) = downloader.download_bytes();
        let (w, h) = (texture.width() as usize, texture.height() as usize);
        let x = ((fx * w as f64) as usize).min(w - 1);
        let y = ((fy * h as f64) as usize).min(h - 1);
        let i = y * stride + x * 4;
        format!("{},{},{}", bytes[i], bytes[i + 1], bytes[i + 2])
    };
    // (2, 2) on the page, as WebKit's probe read it.
    let corner = pixel(
        2.0 / f64::from(size.0.max(1)),
        2.0 / f64::from(size.1.max(1)),
    );
    let rect = page.view.iter_location(at);
    let (x, y) = page.view.buffer_to_window_coords(
        gtk::TextWindowType::Widget,
        rect.x() + rect.width() / 2,
        rect.y() + rect.height() / 2,
    );
    Probe {
        uri: figure.uri().to_string(),
        key: loaded.as_ref().and_then(|l| l.key.clone()),
        pending: page.figures.0.loading.borrow().contains_key(figure.uri()),
        loaded: shown.is_some(),
        animated: shown.is_some_and(|s| s.0.animated),
        size,
        natural,
        fits,
        corner,
        centre: pixel(0.5, 0.5),
        at: (x, y),
    }
}

/// How many reads the page's pictures took: none for a render that shows only what it showed.
#[cfg(feature = "bench")]
pub(super) fn reads(page: &Page) -> u32 {
    page.figures.0.reads.get()
}
