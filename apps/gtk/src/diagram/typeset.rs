//! Typeset formula labels with WebKit because Pango has no math layout.
//!
//! Snapshot whole HTML labels ([`label_html`](super::math::label_html)) to preserve browser
//! wrapping, baselines and bullets. Batch labels into bounded snapshots; show source text while
//! waiting. Plain labels stay in Pango.
//!
//! One shared typesetter ([`shared`]) serves tabs and note embeds to avoid a web
//! process per diagram; its view needs no window for layout or snapshots.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};

use adw::prelude::*;
use gtk::{gdk, glib};
use webkit6::prelude::*;

use super::math::{Label, RENDER_ZOOM, Rendered};

/// Snapshot-height limit in texture pixels, bounding memory while batches are sliced.
const MAX_PICTURE: f64 = 8192.0;

/// How long after a batch fails to wait for WebKit to say its process died.
const LOST_GRACE: std::time::Duration = std::time::Duration::from_millis(500);

pub struct Typesetter {
    /// Made when a batch needs one, and let go with its web process once nothing has been asked
    /// for a while: see [`Typesetter::finish`].
    view: RefCell<Option<webkit6::WebView>>,
    /// Itself, for the handlers of a view made later.
    me: Weak<Typesetter>,
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
    /// The pending release of the view, from the last batch back.
    idle: crate::widgets::Debounce,
}

thread_local! {
    static SHARED: std::cell::OnceCell<Rc<Typesetter>> = const { std::cell::OnceCell::new() };
}

/// The app's one typesetter, made the first time a diagram with formulas asks; `None` only in a
/// build without WebKit ([`super::math`]).
// ponytail: what it has typeset is kept for the app's life, every diagram's formulas together;
// dropping the pictures no view has painted for a while is the upgrade if that ever weighs.
pub fn shared() -> Option<Rc<Typesetter>> {
    Some(SHARED.with(|t| t.get_or_init(Typesetter::new).clone()))
}

impl Typesetter {
    fn new() -> Rc<Typesetter> {
        Rc::new_cyclic(|me| Typesetter {
            view: RefCell::default(),
            me: me.clone(),
            done: RefCell::new(HashMap::new()),
            queued: RefCell::new(Vec::new()),
            batch: RefCell::new(Vec::new()),
            lost: Cell::new(false),
            scheduled: Cell::new(false),
            on_ready: RefCell::new(Vec::new()),
            idle: crate::widgets::Debounce::new(crate::preview::idle_for()),
        })
    }

    /// The view, made the first time a batch needs one since the last was let go.
    fn view(&self, filter: &webkit6::UserContentFilter) -> webkit6::WebView {
        let mut view = self.view.borrow_mut();
        view.get_or_insert_with(|| self.make_view(filter)).clone()
    }

    /// Create a hidden view isolated from input and the network by the preview filter,
    /// since label HTML comes from files.
    fn make_view(&self, filter: &webkit6::UserContentFilter) -> webkit6::WebView {
        let host = gtk::Overlay::new();
        let settings = webkit6::Settings::new();
        settings.set_enable_javascript_markup(false);
        settings.set_enable_media(false);
        settings.set_enable_webgl(false);
        settings.set_enable_page_cache(false);
        let content = webkit6::UserContentManager::new();
        content.add_filter(filter);
        let view = webkit6::WebView::builder()
            .network_session(&webkit6::NetworkSession::new_ephemeral())
            .user_content_manager(&content)
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
        let weak = self.me.clone();
        view.connect_load_changed(move |_, event| {
            if event == webkit6::LoadEvent::Finished
                && let Some(t) = weak.upgrade()
            {
                glib::spawn_future_local(async move { t.collect().await });
            }
        });
        let weak = self.me.clone();
        view.connect_web_process_terminated(move |_, reason| {
            tracing::warn!("the diagram formulas' web process ended: {reason:?}");
            if let Some(t) = weak.upgrade()
                && reason != webkit6::WebProcessTerminationReason::TerminatedByApi
            {
                t.lost();
            }
        });
        view
    }

    /// Recover an interrupted batch after WebKit process death. Retry once in a new
    /// process until a batch succeeds; if the retry also crashes, retain source labels
    /// and continue with later batches.
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

    /// How many labels have been typeset into pictures. Only drills ask.
    #[cfg(feature = "bench")]
    pub fn typeset(&self) -> usize {
        self.done.borrow().values().filter(|r| r.is_some()).count()
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
        self.idle.cancel();
        self.load();
    }

    /// Load the batch as one document once the network filter is in; without one, the batch
    /// stays as its source.
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
             math {{ font-family: 'Latin Modern Math', 'STIX Two Math', 'DejaVu Math TeX Gyre', math; }}\
             ul {{ margin: 0; padding-left: 1.2em; }}\
             </style></head><body>{body}</body></html>"
        );
        let me = self.me.clone();
        crate::preview::network_filter(move |filter| {
            let Some(t) = me.upgrade() else {
                return;
            };
            match filter {
                Ok(filter) => t.view(filter).load_html(&page, None),
                Err(_) => {
                    let batch: Vec<Label> = t.batch.borrow_mut().drain(..).collect();
                    t.done
                        .borrow_mut()
                        .extend(batch.iter().map(|l| (l.key, None)));
                    t.finish();
                }
            }
        });
    }

    /// The document is laid out: measure every label, take one picture, and cut it up.
    async fn collect(self: Rc<Self>) {
        let keys: Vec<u64> = self.batch.borrow().iter().map(|l| l.key).collect();
        if keys.is_empty() {
            return;
        }
        // There is one: it is what said the batch had loaded.
        let Some(view) = self.view.borrow().clone() else {
            return;
        };
        let measured = view
            .evaluate_javascript_future(MEASURE, None, None)
            .await
            .map(|v| v.to_str().to_string());
        // A document taller than one picture holds keeps the labels that fit, loaded again on
        // their own; the others go back to wait for the next batch.
        if let Ok(m) = &measured
            && self.holds(&keys)
        {
            let limit = MAX_PICTURE / (RENDER_ZOOM * f64::from(view.scale_factor()));
            let fit = fitting(&parse_boxes(m).1, limit);
            if fit < keys.len() {
                let rest = self.batch.borrow_mut().split_off(fit);
                self.queued.borrow_mut().splice(0..0, rest);
                return self.load();
            }
        }
        let shot = view
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

    /// Notify listeners and load pending labels. When idle for [`crate::preview::idle_for`],
    /// release the view, web process and network session, retaining rendered labels.
    /// The next batch creates a new view.
    fn finish(&self) {
        self.batch.borrow_mut().clear();
        self.on_ready.borrow_mut().retain(|f| f());
        self.run();
        if !self.busy() {
            let me = self.me.clone();
            self.idle.call(move || {
                if let Some(t) = me.upgrade() {
                    t.view.take();
                }
            });
        }
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

#[cfg(test)]
mod tests {
    use super::*;

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
