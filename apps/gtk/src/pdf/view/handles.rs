//! The selection's touch handles: an accent disc under each end once a finger or a pen has
//! selected, as GTK's text views raise theirs after a touch selection (`gtktexthandle.c`). A drag
//! on one moves that end through the selection's own path, with the loupe over it, and a mouse
//! press, a key, the keyboard going elsewhere or a selection let go puts them away.

use adw::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{graphene, gsk};

use super::PdfView;
use crate::pdf::Span;
use crate::pdf::tools::Hand;
use crate::theme;

/// A handle's disc, in logical pixels: libadwaita's `cursor-handle > contents`, round but for
/// the corner that meets the end of the text.
const DISC: f32 = 20.0;

/// How far round its disc a press still takes a handle: `cursor-handle`'s padding, which is its
/// touch area, 20 px across and 24 px down.
const REACH: (f32, f32) = (20.0, 24.0);

/// A handle in the hand: the end of the selection that stays, as a page and a point on it, where
/// the moving end's glyph had its middle in the view when the hand took hold, and that hand.
#[derive(Debug, Clone, Copy)]
pub struct Grip {
    keep: (usize, (f32, f32)),
    from: (f64, f64),
    hand: Hand,
}

impl PdfView {
    /// Raise the handles, or put them away.
    pub(super) fn show_handles(&self, up: bool) {
        if self.imp().handles.replace(up) != up {
            self.queue_draw();
        }
    }

    /// The selection's first and last glyph while its handles are up, each as its page and box.
    fn ends(&self) -> Option<[(usize, accent_core::pdf::Rect); 2]> {
        if !self.imp().handles.get() {
            return None;
        }
        let selection = self.imp().selection.borrow();
        let mut boxes =
            (selection.iter()).flat_map(|(page, boxes)| boxes.iter().map(move |b| (*page, *b)));
        let first = boxes.next()?;
        Some([first, boxes.last().unwrap_or(first)])
    }

    /// The two discs, the start's then the end's, in content coordinates: hanging left of the
    /// first glyph's bottom-left corner and right of the last glyph's bottom-right, as libadwaita
    /// hangs `cursor-handle.top` and `.bottom`. None for an end scrolled out of the view, unless
    /// a handle is in the hand (`gtk_text_view_set_handle_position`).
    fn discs(&self) -> [Option<graphene::Rect>; 2] {
        let Some(ends) = self.ends() else {
            return [None, None];
        };
        let layout = self.imp().layout.borrow();
        let (ox, oy) = self.scroll_offset();
        let (vw, vh) = (f64::from(self.width()), f64::from(self.height()));
        let held = self.imp().grip.get().is_some();
        let disc = |(page, glyph): (usize, accent_core::pdf::Rect), start: bool| {
            let r = layout.rect_of(layout.pages.get(page)?, &glyph);
            let x = if start { r.x() } else { r.x() + r.width() };
            let y = r.y() + r.height();
            let (vx, vy) = (f64::from(x) - ox, f64::from(y) - oy);
            let shown = held || ((0.0..=vw).contains(&vx) && (0.0..=vh).contains(&vy));
            let left = if start { x - DISC } else { x };
            shown.then(|| graphene::Rect::new(left, y, DISC, DISC))
        };
        [disc(ends[0], true), disc(ends[1], false)]
    }

    /// Paint the handles, in content coordinates, in the accent libadwaita fills them with.
    pub(super) fn paint_handles(&self, snapshot: &gtk::Snapshot) {
        let accent = theme::accent();
        let (round, square) = (
            graphene::Size::new(DISC / 2.0, DISC / 2.0),
            graphene::Size::zero(),
        );
        for (disc, start) in self.discs().into_iter().zip([true, false]) {
            let Some(disc) = disc else {
                continue;
            };
            // The square corner at the text: the start's top right, the end's top left.
            let (left, right) = if start {
                (round, square)
            } else {
                (square, round)
            };
            snapshot.push_rounded_clip(&gsk::RoundedRect::new(disc, left, right, round, round));
            snapshot.append_color(&accent, &disc);
            snapshot.pop();
        }
    }

    /// Which handle a press at `(x, y)` in the view takes, the start's being 0: one whose disc or
    /// the reach round it holds the press, the nearer when both do.
    pub(super) fn grip_at(&self, x: f64, y: f64) -> Option<usize> {
        let (ox, oy) = self.scroll_offset();
        let (cx, cy) = ((x + ox) as f32, (y + oy) as f32);
        let near = |d: &graphene::Rect| {
            let (mx, my) = (d.x() + DISC / 2.0, d.y() + DISC / 2.0);
            let reach = (DISC / 2.0 + REACH.0, DISC / 2.0 + REACH.1);
            ((cx - mx).abs() <= reach.0 && (cy - my).abs() <= reach.1)
                .then(|| (cx - mx).hypot(cy - my))
        };
        let discs = self.discs();
        let reached = (0..2).filter_map(|end| Some((end, near(discs[end].as_ref()?)?)));
        reached
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(end, _)| end)
    }

    /// Take hold of the handle a press by `hand` at `(x, y)` is on, if it is on one.
    pub(super) fn take_grip(&self, x: f64, y: f64, hand: Hand) -> bool {
        let (Some(end), Some(ends)) = (self.grip_at(x, y), self.ends()) else {
            return false;
        };
        let middle = |(page, b): (usize, accent_core::pdf::Rect)| {
            (page, ((b.left + b.right) / 2.0, (b.top + b.bottom) / 2.0))
        };
        let (moving, keep) = (middle(ends[end]), middle(ends[1 - end]));
        let from = {
            let layout = self.imp().layout.borrow();
            let Some(page) = layout.pages.get(moving.0) else {
                return false;
            };
            let (ox, oy) = self.scroll_offset();
            let (px, py) = moving.1;
            let at = (page.x + px * layout.scale, page.y + py * layout.scale);
            (f64::from(at.0) - ox, f64::from(at.1) - oy)
        };
        self.imp().grip.set(Some(Grip { keep, from, hand }));
        true
    }

    /// The hand on a handle has moved `(dx, dy)` since it took hold: that end goes to the glyph
    /// it has carried the glyph's middle to, the other staying, as `gtktexthandle.c` keeps the
    /// hand's offset from the end it took, and under a finger or a pen the loupe shows it there.
    /// Past the other end the two trade places, as a drag pulled back past its start does.
    pub(super) fn drag_grip(&self, dx: f64, dy: f64) {
        let Some(grip) = self.imp().grip.get() else {
            return;
        };
        let to = (grip.from.0 + dx, grip.from.1 + dy);
        let Some(at) = self.nearest_page_point(to.0, to.1) else {
            return;
        };
        if let Some(f) = self.imp().on_select.borrow().as_ref() {
            f(
                self,
                Span {
                    from: grip.keep,
                    to: at,
                },
            );
        }
        if grip.hand != Hand::Mouse {
            self.loupe(Some(to));
        }
    }

    /// Let go of the handle in the hand; whether there was one.
    pub(super) fn drop_grip(&self) -> bool {
        let held = self.imp().grip.take().is_some();
        if held {
            self.loupe(None);
            self.queue_draw();
        }
        held
    }

    /// The handles as a drill sees them: `none`, or the middle of each disc in the view, the
    /// start's then the end's, `-` for one out of the view.
    #[cfg(feature = "bench")]
    pub fn handles_report(&self) -> String {
        let (ox, oy) = self.scroll_offset();
        let discs = self.discs();
        if discs.iter().all(Option::is_none) {
            return "none".to_string();
        }
        let at = |d: &Option<graphene::Rect>| match d {
            Some(d) => format!(
                "({:.0},{:.0})",
                f64::from(d.x() + DISC / 2.0) - ox,
                f64::from(d.y() + DISC / 2.0) - oy
            ),
            None => "-".to_string(),
        };
        format!("start={} end={}", at(&discs[0]), at(&discs[1]))
    }

    /// The middle of a handle's disc in the view, the start's being 0. Only drills ask.
    #[cfg(feature = "bench")]
    pub fn bench_disc(&self, end: usize) -> Option<(f64, f64)> {
        let (ox, oy) = self.scroll_offset();
        let d = self.discs()[end]?;
        let (x, y) = (d.x() + DISC / 2.0, d.y() + DISC / 2.0);
        Some((f64::from(x) - ox, f64::from(y) - oy))
    }

    /// A hand pressing, dragging and letting go of a handle, as the view's gesture does: no finger
    /// reaches a headless window. Only `ACCENT_BENCH_PDF=loupe` drives them.
    #[cfg(feature = "bench")]
    pub fn bench_grip(&self, x: f64, y: f64, hand: Hand) -> bool {
        self.take_grip(x, y, hand)
    }

    #[cfg(feature = "bench")]
    pub fn bench_grip_drag(&self, dx: f64, dy: f64) {
        self.drag_grip(dx, dy);
    }

    #[cfg(feature = "bench")]
    pub fn bench_grip_lift(&self) {
        self.drop_grip();
    }
}
