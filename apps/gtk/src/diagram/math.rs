//! Labels with formulas, typeset by WebKit.
//!
//! Pango has no mathematics, and draw.io itself draws a label as HTML with MathJax in it. So a
//! label holding a formula is written out as HTML with each formula as MathML — core's
//! converter, the one the note preview uses — laid out by a WebKit view that sits on the canvas
//! unseen, and painted as a picture of the whole label: its wrapping, its baselines and its
//! bullets are the browser's, as they are in draw.io. A label with no formula never comes here.
//!
//! Everything a page asks for goes into one document: one load, one snapshot, one crop per
//! label. The canvas paints the label's source meanwhile.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::rc::Rc;

use accent_drawio::{Align, Color, Font, Marks, Run};
use adw::prelude::*;
use gtk::{gdk, glib, graphene};
use webkit6::prelude::*;

/// The page zoom the labels are rendered at: a formula stays sharp up to this canvas zoom and
/// is scaled down, not up, below it.
const RENDER_ZOOM: f64 = 3.0;

/// A label to typeset: its HTML and the width it wraps at, if it wraps.
pub struct Label {
    pub key: u64,
    pub html: String,
}

/// A typeset label: where in `texture` it is, and its size in page units.
#[derive(Clone)]
pub struct Rendered {
    pub texture: gdk::Texture,
    pub crop: graphene::Rect,
    pub size: (f64, f64),
}

pub struct Typesetter {
    view: webkit6::WebView,
    done: RefCell<HashMap<u64, Option<Rendered>>>,
    queued: RefCell<Vec<Label>>,
    /// The keys of the document loaded now, in its order; empty while nothing is in flight.
    batch: RefCell<Vec<u64>>,
    scheduled: Cell<bool>,
    on_ready: RefCell<Option<Box<dyn Fn()>>>,
}

impl Typesetter {
    /// A typesetter whose view lives, unseen and untouchable, in `host`.
    pub fn new(host: &gtk::Overlay) -> Rc<Typesetter> {
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
            scheduled: Cell::new(false),
            on_ready: RefCell::new(None),
        });
        let weak = Rc::downgrade(&typesetter);
        typesetter.view.connect_load_changed(move |_, event| {
            if event == webkit6::LoadEvent::Finished
                && let Some(t) = weak.upgrade()
            {
                glib::spawn_future_local(async move { t.collect().await });
            }
        });
        typesetter
    }

    /// Whether anything is waiting to be typeset or in the middle of it.
    pub fn busy(&self) -> bool {
        !self.batch.borrow().is_empty() || !self.queued.borrow().is_empty()
    }

    pub fn connect_ready(&self, f: impl Fn() + 'static) {
        *self.on_ready.borrow_mut() = Some(Box::new(f));
    }

    /// The label typeset under `key`: `Some(None)` for one WebKit could not render, `None` for
    /// one not rendered yet.
    pub fn get(&self, key: u64) -> Option<Option<Rendered>> {
        self.done.borrow().get(&key).cloned()
    }

    /// Typeset `label` when WebKit is next free. Asking twice is asking once.
    pub fn ask(self: &Rc<Self>, label: Label) {
        let known = self.done.borrow().contains_key(&label.key)
            || self.batch.borrow().contains(&label.key)
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
        let mut body = String::new();
        for (i, label) in labels.iter().enumerate() {
            body.push_str(&format!(
                "<div class=\"w\"><div class=\"l\" id=\"l{i}\">{}</div></div>",
                label.html
            ));
        }
        *self.batch.borrow_mut() = labels.iter().map(|l| l.key).collect();
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
        let keys = self.batch.borrow().clone();
        if keys.is_empty() {
            return;
        }
        let measured = self
            .view
            .evaluate_javascript_future(MEASURE, None, None)
            .await
            .map(|v| v.to_str().to_string());
        let shot = self
            .view
            .snapshot_future(
                webkit6::SnapshotRegion::FullDocument,
                webkit6::SnapshotOptions::TRANSPARENT_BACKGROUND,
            )
            .await;
        let (measured, texture) = match (measured, shot) {
            (Ok(m), Ok(t)) => (m, t),
            (m, t) => {
                tracing::warn!("diagram formulas not typeset: {:?} {:?}", m.err(), t.err());
                for key in &keys {
                    self.done.borrow_mut().insert(*key, None);
                }
                return self.finish();
            }
        };
        let (doc_width, boxes) = parse_boxes(&measured);
        let factor = f64::from(texture.width()) / doc_width.max(1.0);
        for (i, key) in keys.iter().enumerate() {
            let rendered = boxes.get(i).map(|&(x, y, w, h)| Rendered {
                texture: texture.clone(),
                crop: graphene::Rect::new(
                    (x * factor) as f32,
                    (y * factor) as f32,
                    (w * factor) as f32,
                    (h * factor) as f32,
                ),
                size: (w, h),
            });
            self.done.borrow_mut().insert(*key, rendered);
        }
        self.finish();
    }

    fn finish(&self) {
        self.batch.borrow_mut().clear();
        if let Some(f) = self.on_ready.borrow().as_ref() {
            f();
        }
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
}
