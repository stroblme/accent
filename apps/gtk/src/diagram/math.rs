//! Labels with formulas, typeset by WebKit.
//!
//! Pango has no mathematics, and draw.io itself draws a label as HTML with MathJax in it. So a
//! label holding a formula is written out as HTML with each formula as MathML — core's
//! converter, the one the note preview uses — laid out by a WebKit view that sits on the canvas
//! unseen, and painted as a picture of the whole label: its wrapping, its baselines and its
//! bullets are the browser's, as they are in draw.io. A label with no formula never comes here.
//!
//! Everything a page asks for goes into one document: one load, one snapshot cut into a picture
//! per label, as many labels as one snapshot can hold, the rest waiting for the next. The canvas
//! paints the label's source meanwhile.
//!
//! One typesetter serves the whole app ([`shared`]): every diagram tab and every note's embed of
//! a diagram, a WebKit view being a web process of its own. Its view is held by no window, which
//! WebKit lays out and snapshots all the same.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::rc::Rc;

use accent_drawio::{Align, Color, Font, Marks, Run};
use adw::prelude::*;
use gtk::{gdk, glib};
use webkit6::prelude::*;

/// The page zoom the labels are rendered at: a formula stays sharp up to this canvas zoom and
/// is scaled down, not up, below it.
pub(super) const RENDER_ZOOM: f64 = 3.0;

/// The tallest snapshot a batch is taken as, in texture pixels, which bounds what is held while
/// it is cut up: 300 small formulas in one would make a picture of nearly 200 MB.
const MAX_PICTURE: f64 = 8192.0;

/// How long after a batch fails to wait for WebKit to say its process died.
const LOST_GRACE: std::time::Duration = std::time::Duration::from_millis(500);

/// A label to typeset: its HTML and the width it wraps at, if it wraps.
pub struct Label {
    pub key: u64,
    pub html: String,
}

/// A typeset label: its picture, and its size in page units.
#[derive(Clone)]
pub struct Rendered {
    pub texture: gdk::Texture,
    pub size: (f64, f64),
}

pub struct Typesetter {
    view: webkit6::WebView,
    done: RefCell<HashMap<u64, Option<Rendered>>>,
    queued: RefCell<Vec<Label>>,
    /// The labels of the document loaded now, in its order; empty while nothing is in flight.
    batch: RefCell<Vec<Label>>,
    /// Whether WebKit's process died under a batch since one last came back; see
    /// [`Typesetter::lost`].
    lost: Cell<bool>,
    scheduled: Cell<bool>,
    /// Who paints again when labels come in; one that answers `false` is gone and dropped.
    on_ready: RefCell<Vec<Box<dyn Fn() -> bool>>>,
}

thread_local! {
    static SHARED: std::cell::OnceCell<Rc<Typesetter>> = const { std::cell::OnceCell::new() };
}

/// The app's one typesetter, made the first time a diagram with formulas asks.
// ponytail: what it has typeset is kept for the app's life, every diagram's formulas together;
// dropping the pictures no view has painted for a while is the upgrade if that ever weighs.
pub fn shared() -> Rc<Typesetter> {
    SHARED.with(|t| {
        t.get_or_init(|| Typesetter::new(&gtk::Overlay::new()))
            .clone()
    })
}

impl Typesetter {
    /// A typesetter whose view lives, unseen and untouchable, in `host`.
    fn new(host: &gtk::Overlay) -> Rc<Typesetter> {
        let settings = webkit6::Settings::new();
        settings.set_enable_javascript_markup(false);
        settings.set_enable_media(false);
        settings.set_enable_webgl(false);
        settings.set_enable_page_cache(false);
        let view = webkit6::WebView::builder()
            .network_session(&webkit6::NetworkSession::new_ephemeral())
            .settings(&settings)
            .build();
        view.set_halign(gtk::Align::Start);
        view.set_valign(gtk::Align::Start);
        view.set_size_request(1200, 800);
        view.set_can_target(false);
        view.set_can_focus(false);
        view.set_opacity(0.0);
        view.set_zoom_level(RENDER_ZOOM);
        // No colour of its own: the snapshot is taken with a transparent background.
        view.set_background_color(&gdk::RGBA::new(0.0, 0.0, 0.0, 0.0));
        host.add_overlay(&view);
        host.set_measure_overlay(&view, false);
        host.set_clip_overlay(&view, true);
        let typesetter = Rc::new(Typesetter {
            view,
            done: RefCell::new(HashMap::new()),
            queued: RefCell::new(Vec::new()),
            batch: RefCell::new(Vec::new()),
            lost: Cell::new(false),
            scheduled: Cell::new(false),
            on_ready: RefCell::new(Vec::new()),
        });
        let weak = Rc::downgrade(&typesetter);
        typesetter.view.connect_load_changed(move |_, event| {
            if event == webkit6::LoadEvent::Finished
                && let Some(t) = weak.upgrade()
            {
                glib::spawn_future_local(async move { t.collect().await });
            }
        });
        let weak = Rc::downgrade(&typesetter);
        typesetter
            .view
            .connect_web_process_terminated(move |_, reason| {
                tracing::warn!("the diagram formulas' web process ended: {reason:?}");
                if let Some(t) = weak.upgrade()
                    && reason != webkit6::WebProcessTerminationReason::TerminatedByApi
                {
                    t.lost();
                }
            });
        typesetter
    }

    /// WebKit's process died under the view — a crash, or past its memory limit — and the batch
    /// in flight will never come back, which left every formula after it as its source until the
    /// diagram was opened again. It goes again on a new process, once until a batch comes back:
    /// a batch that brings the process down again stays as source, and the rest go on.
    fn lost(&self) {
        let batch: Vec<Label> = self.batch.borrow_mut().drain(..).collect();
        match self.lost.replace(true) {
            false => {
                self.queued.borrow_mut().splice(0..0, batch);
            }
            true => {
                let mut done = self.done.borrow_mut();
                done.extend(batch.iter().map(|l| (l.key, None)));
            }
        }
        self.finish();
    }

    /// Whether anything is waiting to be typeset or in the middle of it.
    pub fn busy(&self) -> bool {
        !self.batch.borrow().is_empty() || !self.queued.borrow().is_empty()
    }

    /// Call `f` whenever labels come in, until it answers `false`.
    pub fn connect_ready(&self, f: impl Fn() -> bool + 'static) {
        self.on_ready.borrow_mut().push(Box::new(f));
    }

    /// The label typeset under `key`: `Some(None)` for one WebKit could not render, `None` for
    /// one not rendered yet.
    pub fn get(&self, key: u64) -> Option<Option<Rendered>> {
        self.done.borrow().get(&key).cloned()
    }

    /// Typeset `label` when WebKit is next free. Asking twice is asking once.
    pub fn ask(self: &Rc<Self>, label: Label) {
        let known = self.done.borrow().contains_key(&label.key)
            || self.batch.borrow().iter().any(|l| l.key == label.key)
            || self.queued.borrow().iter().any(|l| l.key == label.key);
        if known {
            return;
        }
        self.queued.borrow_mut().push(label);
        // From an idle: this is asked while the canvas paints, which is no time to load a page.
        if !self.scheduled.replace(true) {
            let weak = Rc::downgrade(self);
            glib::idle_add_local_once(move || {
                if let Some(t) = weak.upgrade() {
                    t.scheduled.set(false);
                    t.run();
                }
            });
        }
    }

    /// Load everything queued as one document, unless a load is already in flight.
    fn run(&self) {
        if !self.batch.borrow().is_empty() {
            return;
        }
        let labels: Vec<Label> = self.queued.borrow_mut().drain(..).collect();
        if labels.is_empty() {
            return;
        }
        *self.batch.borrow_mut() = labels;
        self.load();
    }

    /// Load the batch as one document.
    fn load(&self) {
        let mut body = String::new();
        for (i, label) in self.batch.borrow().iter().enumerate() {
            body.push_str(&format!(
                "<div class=\"w\"><div class=\"l\" id=\"l{i}\">{}</div></div>",
                label.html
            ));
        }
        let page = format!(
            "<!doctype html><html><head><meta charset=\"utf-8\"><style>\
             html, body {{ margin: 0; padding: 0; background: transparent; }}\
             .w {{ display: block; margin: 0 0 24px 0; }}\
             .l {{ display: inline-block; line-height: 1.2; }}\
             math {{ font-family: 'Latin Modern Math', 'STIX Two Math', math; }}\
             ul {{ margin: 0; padding-left: 1.2em; }}\
             </style></head><body>{body}</body></html>"
        );
        self.view.load_html(&page, None);
    }

    /// The document is laid out: measure every label, take one picture, and cut it up.
    async fn collect(self: Rc<Self>) {
        let keys: Vec<u64> = self.batch.borrow().iter().map(|l| l.key).collect();
        if keys.is_empty() {
            return;
        }
        let measured = self
            .view
            .evaluate_javascript_future(MEASURE, None, None)
            .await
            .map(|v| v.to_str().to_string());
        // A document taller than one picture holds keeps the labels that fit, loaded again on
        // their own; the others go back to wait for the next batch.
        if let Ok(m) = &measured
            && self.holds(&keys)
        {
            let limit = MAX_PICTURE / (RENDER_ZOOM * f64::from(self.view.scale_factor()));
            let fit = fitting(&parse_boxes(m).1, limit);
            if fit < keys.len() {
                let rest = self.batch.borrow_mut().split_off(fit);
                self.queued.borrow_mut().splice(0..0, rest);
                return self.load();
            }
        }
        let shot = self
            .view
            .snapshot_future(
                webkit6::SnapshotRegion::FullDocument,
                webkit6::SnapshotOptions::TRANSPARENT_BACKGROUND,
            )
            .await;
        // The process died meanwhile and the batch went again: this one is not ours any more.
        if !self.holds(&keys) {
            return;
        }
        let (measured, texture) = match (measured, shot) {
            (Ok(m), Ok(t)) => (m, t),
            (m, t) => {
                tracing::warn!("diagram formulas not typeset: {:?} {:?}", m.err(), t.err());
                // A process that died fails these first and says so a moment later: that batch
                // is `lost`'s to send again, not this one's to give up on.
                glib::timeout_future(LOST_GRACE).await;
                if !self.holds(&keys) {
                    return;
                }
                for key in &keys {
                    self.done.borrow_mut().insert(*key, None);
                }
                return self.finish();
            }
        };
        let (doc_width, boxes) = parse_boxes(&measured);
        let factor = f64::from(texture.width()) / doc_width.max(1.0);
        // Each label cut out into a picture of its own and the snapshot let go: a crop of it
        // would hold all of it, and the cairo renderer would read all of it for every label.
        let size = (texture.width() as usize, texture.height() as usize);
        let mut pixels = vec![0; size.0 * size.1 * 4];
        texture.download(&mut pixels, size.0 * 4);
        for (i, key) in keys.iter().enumerate() {
            let rendered = boxes.get(i).and_then(|&(x, y, w, h)| {
                let picked = (x * factor, y * factor, w * factor, h * factor);
                let (bytes, pw, ph) = cut(&pixels, size, picked)?;
                let picture = gdk::MemoryTexture::new(
                    pw as i32,
                    ph as i32,
                    gdk::MemoryFormat::B8g8r8a8Premultiplied,
                    &glib::Bytes::from_owned(bytes),
                    pw * 4,
                );
                Some(Rendered {
                    texture: picture.upcast(),
                    size: (w, h),
                })
            });
            self.done.borrow_mut().insert(*key, rendered);
        }
        self.lost.set(false);
        self.finish();
    }

    /// Whether the batch in flight is still the one of `keys`.
    fn holds(&self, keys: &[u64]) -> bool {
        self.batch
            .borrow()
            .iter()
            .map(|l| l.key)
            .eq(keys.iter().copied())
    }

    fn finish(&self) {
        self.batch.borrow_mut().clear();
        self.on_ready.borrow_mut().retain(|f| f());
        self.run();
    }
}

/// Every label's box in the document, and the document's width, as `w;x,y,w,h;…`.
const MEASURE: &str = "(() => { const out = [document.documentElement.scrollWidth]; \
     document.querySelectorAll('.l').forEach(e => { const r = e.getBoundingClientRect(); \
     out.push([r.left + scrollX, r.top + scrollY, r.width, r.height].join(',')); }); \
     return out.join(';'); })()";

fn parse_boxes(measured: &str) -> (f64, Vec<(f64, f64, f64, f64)>) {
    let mut parts = measured.split(';');
    let width = parts.next().and_then(|w| w.parse().ok()).unwrap_or(0.0);
    let boxes = parts
        .filter_map(|b| {
            let v: Vec<f64> = b.split(',').filter_map(|n| n.parse().ok()).collect();
            (v.len() == 4).then(|| (v[0], v[1], v[2], v[3]))
        })
        .collect();
    (width, boxes)
}

/// How many labels, from the first, one picture of the document holds: those ending within
/// `limit` CSS pixels of its top, and the first whatever its size.
fn fitting(boxes: &[(f64, f64, f64, f64)], limit: f64) -> usize {
    boxes
        .iter()
        .take_while(|&&(_, y, _, h)| y + h <= limit)
        .count()
        .max(1)
}

/// The pixels of the box `(x, y, w, h)`, in pixels of a picture `size` big at four bytes a
/// pixel (`texture.download`'s), with its width and height; `None` where it holds none.
fn cut(
    pixels: &[u8],
    size: (usize, usize),
    (x, y, w, h): (f64, f64, f64, f64),
) -> Option<(Vec<u8>, usize, usize)> {
    let at = |v: f64, max: usize| (v.round().max(0.0) as usize).min(max);
    let (left, top) = (at(x, size.0), at(y, size.1));
    let (right, bottom) = (at(x + w, size.0), at(y + h, size.1));
    if right <= left || bottom <= top {
        return None;
    }
    let stride = size.0 * 4;
    let bytes = (top..bottom)
        .flat_map(|row| &pixels[row * stride + left * 4..row * stride + right * 4])
        .copied()
        .collect();
    Some((bytes, right - left, bottom - top))
}

/// A label as the HTML WebKit lays out: the label's font, colour and alignment on its box, the
/// runs as spans, each formula as MathML (its source, if it does not parse), wrapped at `width`
/// page units or not at all.
pub fn label_html(runs: &[Run], font: &Font, align: Align, width: Option<f64>) -> String {
    // `Arial,Helvetica` is a list of two families, as draw.io's default is: each is quoted on its
    // own, or CSS reads the whole thing as one family name nothing answers to.
    let families: Vec<String> = font
        .family
        .split(',')
        .map(|f| format!("'{}'", f.trim().replace('\'', "")))
        .collect();
    let mut style = format!(
        "font-family: {}, sans-serif; font-size: {}px; color: {};",
        families.join(", "),
        font.size,
        colour(font.color)
    );
    if font.bold {
        style.push_str(" font-weight: bold;");
    }
    if font.italic {
        style.push_str(" font-style: italic;");
    }
    if font.underline {
        style.push_str(" text-decoration: underline;");
    }
    style.push_str(match align {
        Align::Left => " text-align: left;",
        Align::Center => " text-align: center;",
        Align::Right => " text-align: right;",
    });
    match width {
        Some(w) => style.push_str(&format!(" width: {w}px; white-space: normal;")),
        None => style.push_str(" white-space: nowrap;"),
    }
    let mut body = String::new();
    for run in runs {
        match run {
            Run::Text { text, marks } => {
                let span = marks_style(marks);
                match span.is_empty() {
                    true => body.push_str(&escape(text)),
                    false => {
                        body.push_str(&format!("<span style=\"{span}\">{}</span>", escape(text)))
                    }
                }
            }
            Run::Math { tex, display } => match accent_core::markdown::mathml(tex, *display) {
                Some(mathml) => body.push_str(&mathml),
                None => body.push_str(&escape(&format!("\\({tex}\\)"))),
            },
            Run::Break => body.push_str("<br>"),
            Run::Bullet => body.push_str("• "),
        }
    }
    format!("<div style=\"{style}\">{body}</div>")
}

/// Which `Label` key `html` is typeset under.
pub fn key_of(html: &str) -> u64 {
    let mut h = DefaultHasher::new();
    html.hash(&mut h);
    h.finish()
}

fn marks_style(marks: &Marks) -> String {
    let mut s = String::new();
    if marks.bold {
        s.push_str("font-weight: bold;");
    }
    if marks.italic {
        s.push_str("font-style: italic;");
    }
    if marks.underline {
        s.push_str("text-decoration: underline;");
    }
    if let Some(c) = marks.color {
        s.push_str(&format!("color: {};", colour(c)));
    }
    if let Some(size) = marks.size {
        s.push_str(&format!("font-size: {size}px;"));
    }
    s
}

fn colour(c: Color) -> String {
    format!(
        "rgba({}, {}, {}, {})",
        c.r,
        c.g,
        c.b,
        f64::from(c.a) / 255.0
    )
}

fn escape(text: &str) -> String {
    glib::markup_escape_text(text).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_label_becomes_html_with_its_formula_as_mathml() {
        let font = Font {
            size: 14.0,
            family: "Arial,Helvetica".into(),
            color: Color::rgb(0, 0, 0),
            bold: false,
            italic: false,
            underline: false,
        };
        let runs = [
            Run::Text {
                text: "a < b".into(),
                marks: Marks {
                    bold: true,
                    ..Marks::default()
                },
            },
            Run::Math {
                tex: "x^2".into(),
                display: false,
            },
        ];
        let html = label_html(&runs, &font, Align::Center, Some(100.0));
        assert!(html.contains("width: 100px"), "{html}");
        assert!(
            html.contains("font-family: 'Arial', 'Helvetica', sans-serif;"),
            "{html}"
        );
        assert!(
            html.contains("<span style=\"font-weight: bold;\">a &lt; b</span>"),
            "{html}"
        );
        assert!(html.contains("<math"), "{html}");
        assert_ne!(
            key_of(&html),
            key_of(&label_html(&runs, &font, Align::Left, None))
        );
    }

    #[test]
    fn measured_boxes_are_read_back() {
        let (w, boxes) = parse_boxes("1200;0,0,100.5,20;0,44,50,10");
        assert_eq!(w, 1200.0);
        assert_eq!(boxes, [(0.0, 0.0, 100.5, 20.0), (0.0, 44.0, 50.0, 10.0)]);
    }

    #[test]
    fn a_batch_keeps_the_labels_one_picture_holds() {
        let boxes = [
            (0.0, 0.0, 50.0, 20.0),
            (0.0, 44.0, 50.0, 20.0),
            (0.0, 88.0, 50.0, 20.0),
        ];
        assert_eq!(fitting(&boxes, 1000.0), 3);
        assert_eq!(fitting(&boxes, 70.0), 2);
        // A label taller than any picture goes on its own.
        assert_eq!(fitting(&[(0.0, 0.0, 50.0, 5000.0)], 70.0), 1);
    }

    #[test]
    fn a_label_is_cut_out_of_the_picture() {
        // A 3 × 2 picture whose every pixel holds its index four times.
        let pixels: Vec<u8> = (0..6u8).flat_map(|i| [i; 4]).collect();
        let (bytes, w, h) = cut(&pixels, (3, 2), (0.6, 0.0, 2.0, 2.0)).unwrap();
        assert_eq!((w, h), (2, 2));
        assert_eq!(bytes, [1, 1, 1, 1, 2, 2, 2, 2, 4, 4, 4, 4, 5, 5, 5, 5]);
        // Past the picture's edge it stops there, and a box with nothing in it is no picture.
        assert_eq!(
            cut(&pixels, (3, 2), (2.0, 1.0, 5.0, 5.0)).unwrap().0,
            [5; 4]
        );
        assert!(cut(&pixels, (3, 2), (1.0, 1.0, 0.2, 1.0)).is_none());
    }
}
