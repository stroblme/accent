//! An SVG picture drawn by librsvg at the size it is shown. The canvas draws a texture of the
//! part on screen at the display's pixels, rendered on a worker once the view has settled
//! (`zoom::ZOOM_SETTLE`), the copy before scaled or the picture as decoded (`paint::Picture`)
//! showing meanwhile, so a frame only ever draws textures. An export draws it as vectors.
//! A picture of any kind is decoded on a worker too, the first time the canvas or the Outline's
//! thumbnails paint it ([`Queue`]).

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::pin::Pin;
use std::rc::Rc;

use accent_drawio::Rect;
use gtk::prelude::*;
use gtk::{cairo, gdk, glib, gsk};
use librsvg_rebind::prelude::*;

use super::paint::grect;

/// The most pixels one copy holds: 16 megapixels, 64 MB, as an SVG in an image tab (`look`).
const MOST: f64 = 16e6;

/// The most pixels the copies a frame draws and asks for hold together: 64 megapixels, 256 MB,
/// as the PDF's tile cache does.
pub const BUDGET: f64 = 64e6;

/// Where the canvas paints a frame: what of its content is on screen (content coordinates), the
/// display's pixels per content unit, and the frame's number.
#[derive(Clone, Copy)]
pub struct Canvas {
    pub visible: Rect,
    pub device: f64,
    pub frame: u64,
}

/// A copy of a picture: its box `size` device pixels big with its top-left at `origin`, and
/// `part` of it, in the same pixels, whole ones.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Want {
    pub size: (f64, f64),
    pub origin: (f64, f64),
    pub part: Rect,
}

impl Want {
    pub fn pixels(&self) -> f64 {
        self.part.w * self.part.h
    }
}

/// A copy drawn, the frame that last drew it, and the last that drew it for the box it was
/// drawn for.
struct Drawn {
    want: Want,
    texture: gdk::Texture,
    used: Cell<u64>,
    fitted: Cell<u64>,
}

/// An SVG picture's text, its own size, and the copies drawn of it.
pub struct Svg {
    text: glib::Bytes,
    own: (f64, f64),
    copies: RefCell<Vec<Drawn>>,
}

impl Svg {
    pub fn new(text: glib::Bytes, own: (f64, f64)) -> Svg {
        Svg {
            text,
            own,
            copies: RefCell::default(),
        }
    }

    /// The text and own size a worker draws a copy from.
    pub fn source(&self) -> (glib::Bytes, (f64, f64)) {
        (self.text.clone(), self.own)
    }

    /// Keep a copy drawn for `want`, in place of one drawn for the same box.
    pub fn land(&self, want: Want, texture: gdk::Texture, frame: u64) {
        let mut copies = self.copies.borrow_mut();
        copies.retain(|c| c.want.size != want.size || c.want.origin != want.origin);
        copies.push(Drawn {
            want,
            texture,
            used: Cell::new(frame),
            fitted: Cell::new(frame),
        });
    }

    /// The pixels of the copies frame `frame` drew for their own boxes, which a new copy's
    /// room is counted against.
    pub fn held(&self, frame: u64) -> f64 {
        let copies = self.copies.borrow();
        let fitted = copies.iter().filter(|c| c.fitted.get() == frame);
        fitted.map(|c| c.want.pixels()).sum()
    }

    /// What each copy was drawn for, for a drill.
    #[cfg(feature = "bench")]
    pub fn copies(&self) -> Vec<Want> {
        self.copies.borrow().iter().map(|c| c.want).collect()
    }

    /// Let go of the copies frame `frame` did not draw: the picture is off screen, or a newer
    /// copy took their place.
    pub fn sweep(&self, frame: u64) {
        self.copies.borrow_mut().retain(|c| c.used.get() == frame);
    }

    /// Paint the picture at `dest` (content coordinates): a copy drawn for this box, moved by
    /// whole pixels or not at all, or else the latest one scaled to it, and `base` (the picture
    /// as decoded) wherever no copy reaches. Whether `need` is still to be drawn: no copy of
    /// this box covers what is on screen.
    pub fn paint(
        &self,
        snapshot: &gtk::Snapshot,
        base: &gdk::Texture,
        dest: &Rect,
        need: Option<(Want, Rect)>,
        frame: u64,
    ) -> bool {
        let copies = self.copies.borrow();
        let fits = |c: &&Drawn| {
            need.is_some_and(|(want, seen)| {
                let (dx, dy) = (
                    want.origin.0 - c.want.origin.0,
                    want.origin.1 - c.want.origin.1,
                );
                c.want.size == want.size
                    && dx.fract() == 0.0
                    && dy.fract() == 0.0
                    && c.want.part.translate(dx, dy).contains_rect(&seen)
            })
        };
        let fitting = copies.iter().find(fits);
        let Some(copy) = fitting.or(copies.last()) else {
            snapshot.append_scaled_texture(base, gsk::ScalingFilter::Linear, &grect(dest));
            return need.is_some();
        };
        copy.used.set(frame);
        if fitting.is_some() {
            copy.fitted.set(frame);
        }
        // The copy's part, from the box it was drawn for onto this one.
        let w = &copy.want;
        let (sx, sy) = (dest.w / w.size.0, dest.h / w.size.1);
        let at = Rect::new(
            dest.x + (w.part.x - w.origin.0) * sx,
            dest.y + (w.part.y - w.origin.1) * sy,
            w.part.w * sx,
            w.part.h * sy,
        );
        for band in around(dest, &at) {
            snapshot.push_clip(&grect(&band));
            snapshot.append_scaled_texture(base, gsk::ScalingFilter::Linear, &grect(dest));
            snapshot.pop();
        }
        snapshot.append_scaled_texture(&copy.texture, gsk::ScalingFilter::Linear, &grect(&at));
        fitting.is_none() && need.is_some()
    }

    /// Draw the picture as vectors onto `cr` into `dest`, as an export takes it.
    pub fn vectors(&self, cr: &cairo::Context, dest: &Rect) {
        let Ok(handle) = librsvg_rebind::Handle::from_data(&self.text) else {
            return;
        };
        cr.translate(dest.x, dest.y);
        stretched(cr, &handle, self.own, (dest.w, dest.h));
    }
}

/// The canvas's frames and the copies they asked for, drawn on a worker once they have asked
/// for the same for [`crate::zoom::ZOOM_SETTLE`].
#[derive(Clone, Default)]
pub struct Renders(Rc<Asked>);

#[derive(Default)]
struct Asked {
    frame: Cell<u64>,
    wanted: RefCell<Vec<(Rc<Svg>, Want)>>,
    settle: RefCell<Option<glib::SourceId>>,
}

impl Renders {
    /// A new frame of `widget`, showing `visible` (content coordinates) on its display.
    pub fn frame(&self, widget: &gtk::Widget, visible: Rect) -> Canvas {
        let frame = self.0.frame.get() + 1;
        self.0.frame.set(frame);
        let surface = widget.native().and_then(|n| n.surface());
        let device = surface.map_or(f64::from(widget.scale_factor()), |s| s.scale());
        Canvas {
            visible,
            device,
            frame,
        }
    }

    /// The copies a frame of `widget` lacks: drawn once frames have asked for the same for a
    /// while, `widget` drawn again as each lands.
    pub fn ask(&self, wants: Vec<(Rc<Svg>, Want)>, widget: &gtk::Widget) {
        let asked = &self.0;
        let same = {
            let wanted = asked.wanted.borrow();
            wanted.len() == wants.len()
                && wanted
                    .iter()
                    .zip(&wants)
                    .all(|((a, x), (b, y))| Rc::ptr_eq(a, b) && x == y)
        };
        if same {
            return;
        }
        if let Some(id) = asked.settle.take() {
            id.remove();
        }
        let none = wants.is_empty();
        *asked.wanted.borrow_mut() = wants;
        if none {
            return;
        }
        let (renders, widget) = (self.clone(), widget.downgrade());
        let id = glib::timeout_add_local_once(crate::zoom::ZOOM_SETTLE, move || {
            *renders.0.settle.borrow_mut() = None;
            glib::spawn_future_local(async move { renders.draw(widget).await });
        });
        *asked.settle.borrow_mut() = Some(id);
    }

    /// Draw what is asked for one copy at a time, each while it still is.
    async fn draw(self, widget: glib::WeakRef<gtk::Widget>) {
        let wanted = self.0.wanted.borrow().clone();
        for (svg, want) in wanted {
            let still = |(s, w): &(Rc<Svg>, Want)| Rc::ptr_eq(s, &svg) && *w == want;
            if !self.0.wanted.borrow().iter().any(still) {
                continue;
            }
            let (text, own) = svg.source();
            let drawn = crate::work::off_thread("svg picture", move || render(&text, own, want));
            let Some(texture) = drawn.await.flatten() else {
                continue;
            };
            let Some(widget) = widget.upgrade() else {
                return;
            };
            svg.land(want, texture, self.0.frame.get());
            widget.queue_draw();
        }
    }
}

/// A diagram's pictures decoded on a worker, one after another in the order they were first
/// painted, as [`Renders`] draws its copies, so no frame waits for a page of many pictures.
#[derive(Clone, Default)]
pub struct Queue(Rc<RefCell<Jobs>>);

#[derive(Default)]
struct Jobs {
    waiting: VecDeque<Pin<Box<dyn Future<Output = ()>>>>,
    running: bool,
}

impl Queue {
    /// Run `job` once the jobs before it are done.
    pub fn push(&self, job: impl Future<Output = ()> + 'static) {
        let mut jobs = self.0.borrow_mut();
        jobs.waiting.push_back(Box::pin(job));
        if std::mem::replace(&mut jobs.running, true) {
            return;
        }
        let queue = self.clone();
        glib::spawn_future_local(async move {
            loop {
                let next = queue.0.borrow_mut().waiting.pop_front();
                let Some(job) = next else {
                    break;
                };
                job.await;
            }
            queue.0.borrow_mut().running = false;
        });
    }
}

/// What a picture at `dest` (content coordinates, before its turn and flips) asks for on
/// `canvas`, and the part of it on screen: its box at the display's pixels, the part on screen
/// with half a view round it, or all of a turned one. `None` when none of it is on screen, or
/// what it would draw passes [`MOST`].
pub fn want(
    dest: &Rect,
    rotation: f64,
    flips: (bool, bool),
    canvas: &Canvas,
) -> Option<(Want, Rect)> {
    let d = canvas.device;
    let size = (dest.w * d, dest.h * d);
    let origin = (dest.x * d, dest.y * d);
    let whole = outward(&Rect::new(origin.0, origin.1, size.0, size.1));
    let view = Rect::new(
        canvas.visible.x * d,
        canvas.visible.y * d,
        canvas.visible.w * d,
        canvas.visible.h * d,
    );
    let (seen, part) = match rotation % 360.0 == 0.0 {
        true => {
            // What of it is on screen, mirrored as its flips mirror it, with half a view round it.
            let seen = mirrored(&clip(&whole, &view)?, &whole, flips);
            let room = Rect::new(
                seen.x - view.w / 2.0,
                seen.y - view.h / 2.0,
                seen.w + view.w,
                seen.h + view.h,
            );
            (seen, clip(&whole, &outward(&room))?)
        }
        false => (whole, whole),
    };
    let seen = outward(&seen);
    let part = match part.w * part.h > MOST {
        true => seen,
        false => part,
    };
    let want = Want { size, origin, part };
    (want.pixels() <= MOST).then_some((want, seen))
}

/// `text` drawn as `want` asks, on a worker: its part of the box at device pixels.
pub fn render(text: &glib::Bytes, own: (f64, f64), want: Want) -> Option<gdk::Texture> {
    let handle = librsvg_rebind::Handle::from_data(text).ok()?;
    let (w, h) = (want.part.w as i32, want.part.h as i32);
    let mut surface = cairo::ImageSurface::create(cairo::Format::ARgb32, w, h).ok()?;
    {
        let cr = cairo::Context::new(&surface).ok()?;
        cr.translate(want.origin.0 - want.part.x, want.origin.1 - want.part.y);
        stretched(&cr, &handle, own, want.size);
    }
    surface.flush();
    let stride = surface.stride() as usize;
    let bytes = glib::Bytes::from(&*surface.data().ok()?);
    // Cairo's ARGB32 in the byte order of every platform GTK builds for (`pdf::preview`).
    let format = gdk::MemoryFormat::B8g8r8a8Premultiplied;
    Some(gdk::MemoryTexture::new(w, h, format, &bytes, stride).upcast())
}

/// Draw `handle` from the origin stretched from its `own` size to `size`, as draw.io stretches a
/// picture to its cell.
fn stretched(
    cr: &cairo::Context,
    handle: &librsvg_rebind::Handle,
    own: (f64, f64),
    size: (f64, f64),
) {
    cr.scale(size.0 / own.0, size.1 / own.1);
    let all = librsvg_rebind::Rectangle::new(0.0, 0.0, own.0, own.1);
    if let Err(e) = handle.render_document(cr, &all) {
        tracing::debug!("librsvg drew no picture: {e}");
    }
}

/// Where `a` and `b` overlap, if they do.
fn clip(a: &Rect, b: &Rect) -> Option<Rect> {
    let (x, y) = (a.x.max(b.x), a.y.max(b.y));
    let (r, bottom) = (a.right().min(b.right()), a.bottom().min(b.bottom()));
    (r > x && bottom > y).then(|| Rect::new(x, y, r - x, bottom - y))
}

/// `r` out to whole pixels.
fn outward(r: &Rect) -> Rect {
    let (x, y) = (r.x.floor(), r.y.floor());
    Rect::new(x, y, r.right().ceil() - x, r.bottom().ceil() - y)
}

/// `r`, a part of `whole`, where it is before `whole` is mirrored by `flips`.
fn mirrored(r: &Rect, whole: &Rect, (h, v): (bool, bool)) -> Rect {
    let c = whole.centre();
    Rect::new(
        if h { 2.0 * c.x - r.right() } else { r.x },
        if v { 2.0 * c.y - r.bottom() } else { r.y },
        r.w,
        r.h,
    )
}

/// The parts of `dest` round `inner`, which lies within it: above, below, left and right.
fn around(dest: &Rect, inner: &Rect) -> impl Iterator<Item = Rect> {
    [
        Rect::new(dest.x, dest.y, dest.w, inner.y - dest.y),
        Rect::new(
            dest.x,
            inner.bottom(),
            dest.w,
            dest.bottom() - inner.bottom(),
        ),
        Rect::new(dest.x, inner.y, inner.x - dest.x, inner.h),
        Rect::new(
            inner.right(),
            inner.y,
            dest.right() - inner.right(),
            inner.h,
        ),
    ]
    .into_iter()
    .filter(|r| r.w > 0.5 && r.h > 0.5)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canvas(visible: Rect) -> Canvas {
        Canvas {
            visible,
            device: 2.0,
            frame: 1,
        }
    }

    #[test]
    fn a_queue_runs_its_jobs_one_at_a_time_in_the_order_pushed() {
        let context = glib::MainContext::new();
        context
            .with_thread_default(|| {
                let (queue, log) = (Queue::default(), Rc::new(RefCell::new(Vec::new())));
                let (open, gate) = futures_channel::oneshot::channel::<()>();
                let (a, b, c) = (log.clone(), log.clone(), log.clone());
                queue.push(async move {
                    a.borrow_mut().push("a starts");
                    let _ = gate.await;
                    a.borrow_mut().push("a ends");
                });
                queue.push(async move { b.borrow_mut().push("b") });
                while context.iteration(false) {}
                assert_eq!(*log.borrow(), ["a starts"]);
                // Pushed while one runs.
                queue.push(async move { c.borrow_mut().push("c") });
                let _ = open.send(());
                while context.iteration(false) {}
                assert_eq!(*log.borrow(), ["a starts", "a ends", "b", "c"]);
            })
            .expect("a main context of the test's own");
    }

    #[test]
    fn a_picture_on_screen_asks_for_its_box_at_the_displays_pixels() {
        let dest = Rect::new(10.0, 20.0, 100.0, 50.0);
        let (want, seen) = want(
            &dest,
            0.0,
            (false, false),
            &canvas(Rect::new(0.0, 0.0, 800.0, 600.0)),
        )
        .unwrap();
        assert_eq!(want.size, (200.0, 100.0));
        assert_eq!(want.part, Rect::new(20.0, 40.0, 200.0, 100.0));
        assert_eq!(seen, want.part);
    }

    #[test]
    fn a_picture_past_the_view_asks_for_what_is_on_screen_and_half_a_view_round_it() {
        let dest = Rect::new(-1000.0, 0.0, 4000.0, 100.0);
        let (want, seen) = want(
            &dest,
            0.0,
            (false, false),
            &canvas(Rect::new(0.0, 0.0, 400.0, 300.0)),
        )
        .unwrap();
        assert_eq!(seen, Rect::new(0.0, 0.0, 800.0, 200.0));
        assert_eq!(want.part, Rect::new(-400.0, 0.0, 1600.0, 200.0));
    }

    #[test]
    fn a_flipped_picture_asks_for_the_part_its_flip_puts_on_screen() {
        let dest = Rect::new(0.0, 0.0, 1000.0, 10.0);
        let (_, seen) = want(
            &dest,
            0.0,
            (true, false),
            &canvas(Rect::new(0.0, 0.0, 100.0, 10.0)),
        )
        .unwrap();
        assert_eq!(seen, Rect::new(1800.0, 0.0, 200.0, 20.0));
    }

    #[test]
    fn a_picture_off_screen_asks_for_nothing() {
        let dest = Rect::new(900.0, 0.0, 100.0, 100.0);
        assert!(
            want(
                &dest,
                0.0,
                (false, false),
                &canvas(Rect::new(0.0, 0.0, 800.0, 600.0))
            )
            .is_none()
        );
    }
}
