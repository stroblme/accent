//! Where the pages sit at one zoom, and the reading position that survives a change of it.
//!
//! Pure, so the arithmetic that decides what is on screen is testable without a display.

use gtk::graphene;

/// Between pages, and around the column. The 12 of DESIGN.md's spacing scale.
const GAP: f32 = 12.0;

/// Points to CSS pixels at zoom 1.0. A PDF point is 1/72 inch and a CSS pixel 1/96.
pub(super) const PT_TO_PX: f32 = 96.0 / 72.0;

pub const MIN_SCALE: f64 = 0.1;
pub const MAX_SCALE: f64 = 8.0;

/// How the page is sized to the window. Defined in core, because the session remembers it.
pub use accent_core::config::PdfZoom;

/// The status bar's readout for a zoom. A PDF always has one, so there is always something to
/// click to get back to Fit Width.
pub fn zoom_label(zoom: PdfZoom) -> Option<String> {
    match zoom {
        PdfZoom::FitWidth => Some("Fit Width".to_string()),
        PdfZoom::FitPage => Some("Fit Page".to_string()),
        PdfZoom::Scale(z) => Some(format!("{} %", (z * 100.0).round() as i32)),
    }
}

/// Where a page sits in the scrolled content, in CSS pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// Every page's place at one scale, plus the size of the content they add up to.
#[derive(Debug, Clone, Default)]
pub struct Layout {
    /// CSS pixels per PDF point.
    pub scale: f32,
    pub pages: Vec<PageRect>,
    pub width: f32,
    pub height: f32,
}

impl Layout {
    /// A point in the scrolled content as a point on `page`, in that page's own points. The one
    /// definition of the transform; [`Layout::rect_of`] is its inverse.
    pub fn to_page(&self, page: usize, cx: f32, cy: f32) -> Option<(f32, f32)> {
        let rect = self.pages.get(page)?;
        Some(((cx - rect.x) / self.scale, (cy - rect.y) / self.scale))
    }

    /// A rectangle of `page`, in that page's own points, as one in the scrolled content — what
    /// every overlay is painted into.
    pub fn rect_of(&self, page: &PageRect, r: &accent_core::pdf::Rect) -> graphene::Rect {
        graphene::Rect::new(
            page.x + r.left * self.scale,
            page.y + r.top * self.scale,
            r.width() * self.scale,
            r.height() * self.scale,
        )
    }
}

/// A reading position that survives a zoom, a resize and a reload: which page, and the fraction
/// of it at the top-left of the viewport.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Anchor {
    pub page: usize,
    pub u: f32,
    pub v: f32,
}

impl Default for Anchor {
    fn default() -> Self {
        Anchor {
            page: 0,
            u: 0.0,
            v: 0.0,
        }
    }
}

impl Anchor {
    /// The same place in a document that has since gained or lost pages.
    pub fn clamped(self, pages: usize) -> Anchor {
        Anchor {
            page: self.page.min(pages.saturating_sub(1)),
            ..self
        }
    }
}

/// The two ends of a drag, each a page and a point on it in that page's own points.
///
/// The two need not be the same page, and `to` may be earlier in the document than `from`: a
/// drag runs in whichever direction the reader pulls it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Span {
    pub from: (usize, (f32, f32)),
    pub to: (usize, (f32, f32)),
}

/// Lay the pages out in one column at `scale`, centred in `viewport_w`.
///
/// Pure, so the arithmetic that decides what is on screen is testable without a display.
pub fn layout(sizes: &[(f32, f32)], scale: f32, viewport_w: f32) -> Layout {
    let widest = sizes.iter().map(|(w, _)| w * scale).fold(0.0, f32::max);
    let width = (widest + GAP * 2.0).max(viewport_w);
    let mut pages = Vec::with_capacity(sizes.len());
    let mut y = GAP;
    for (w, h) in sizes {
        let (w, h) = (w * scale, h * scale);
        pages.push(PageRect {
            x: ((width - w) / 2.0).max(GAP),
            y,
            w,
            h,
        });
        y += h + GAP;
    }
    Layout {
        scale,
        pages,
        width,
        height: y.max(1.0),
    }
}

/// The scale a zoom mode asks for, given the viewport it has to fit into.
pub fn fit_scale(sizes: &[(f32, f32)], zoom: PdfZoom, vw: f32, vh: f32) -> f32 {
    let widest = sizes.iter().map(|(w, _)| *w).fold(1.0, f32::max);
    let tallest = sizes.iter().map(|(_, h)| *h).fold(1.0, f32::max);
    let width = ((vw - GAP * 2.0).max(1.0)) / widest;
    match zoom {
        PdfZoom::FitWidth => width,
        // The smaller of the two, so the whole page really is on screen.
        PdfZoom::FitPage => width.min(((vh - GAP * 2.0).max(1.0)) / tallest),
        PdfZoom::Scale(z) => z as f32 * PT_TO_PX,
    }
}

/// One zoom step in or out from the zoom `from`. The arithmetic is the document's, so a page
/// steps in the same tenths a note does however far a fit mode left it from one.
///
/// `from` is a zoom, not a layout scale: reading the step back out of [`Layout::scale`] means
/// dividing an `f32` by [`PT_TO_PX`], and past 230 % the drift that leaves is larger than
/// [`crate::zoom::stepped_zoom`]'s epsilon, so the next tenth is the one the page is already at and
/// the zoom stops moving.
pub fn stepped(from: f64, out: bool) -> PdfZoom {
    PdfZoom::Scale(crate::zoom::stepped_zoom(from, out).clamp(MIN_SCALE, MAX_SCALE))
}

/// Where a reading position resumes from, given the zoom it resumes into.
///
/// Under [`PdfZoom::FitPage`] one page is one screen, so the page lands at its top. Keeping the
/// fraction of it the viewport happened to be left at is what made Fit Page look broken: the
/// scale was right, but the same half of one page and half of the next stayed on screen.
pub fn resume_at(zoom: PdfZoom, anchor: Anchor) -> Anchor {
    match zoom {
        PdfZoom::FitPage => Anchor {
            u: 0.0,
            v: 0.0,
            ..anchor
        },
        _ => anchor,
    }
}

/// A scale clamped to what is worth rendering: below the floor nothing is legible, above the
/// ceiling one page is hundreds of megabytes of tiles.
pub fn clamp_scale(scale: f32) -> f32 {
    scale.clamp((MIN_SCALE as f32) * PT_TO_PX, (MAX_SCALE as f32) * PT_TO_PX)
}

/// The content offset `(x, y)` as a reading position: which page the top-left of the viewport is
/// in, and how far into it.
pub fn anchor_at(layout: &Layout, x: f64, y: f64) -> Anchor {
    // A pixel down, so an offset resting exactly on a page's top edge is that page rather than
    // the gap above it.
    let page = page_at(layout, y + 1.0);
    match layout.pages.get(page) {
        Some(rect) => Anchor {
            page,
            u: ((x - f64::from(rect.x)) / f64::from(rect.w.max(1.0))) as f32,
            v: ((y - f64::from(rect.y)) / f64::from(rect.h.max(1.0))) as f32,
        },
        None => Anchor::default(),
    }
}

/// Where a reading position sits in the content, or `None` for a page this layout has not got.
pub fn offset_of(layout: &Layout, anchor: Anchor) -> Option<(f64, f64)> {
    let rect = layout.pages.get(anchor.page)?;
    Some((
        f64::from(rect.x + anchor.u * rect.w),
        f64::from(rect.y + anchor.v * rect.h),
    ))
}

/// Which page a content coordinate falls in, or the nearest one above it.
pub(super) fn page_at(layout: &Layout, y: f64) -> usize {
    layout
        .pages
        .iter()
        .rposition(|rect| f64::from(rect.y) <= y)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn letter(n: usize) -> Vec<(f32, f32)> {
        vec![(612.0, 792.0); n]
    }

    #[test]
    fn pages_stack_with_a_gap_and_centre_in_the_viewport() {
        let out = layout(&letter(3), 1.0, 1000.0);
        assert_eq!(out.pages.len(), 3);
        assert_eq!(out.pages[0].y, GAP);
        assert_eq!(out.pages[1].y, GAP + 792.0 + GAP);
        // Centred: the same margin either side.
        assert_eq!(out.pages[0].x, (1000.0 - 612.0) / 2.0);
        assert_eq!(out.width, 1000.0);
        assert_eq!(out.height, GAP + (792.0 + GAP) * 3.0);
    }

    #[test]
    fn a_page_wider_than_the_viewport_sets_the_content_width() {
        let out = layout(&letter(1), 2.0, 500.0);
        assert_eq!(out.width, 612.0 * 2.0 + GAP * 2.0);
        assert_eq!(out.pages[0].x, GAP);
    }

    #[test]
    fn fit_page_takes_the_smaller_axis() {
        let sizes = letter(2);
        // Wide and short: height is what binds.
        let scale = fit_scale(&sizes, PdfZoom::FitPage, 2000.0, 400.0);
        assert!((scale - (400.0 - GAP * 2.0) / 792.0).abs() < 1e-6);
        let width = fit_scale(&sizes, PdfZoom::FitWidth, 2000.0, 400.0);
        assert!(width > scale);
    }

    #[test]
    fn fit_page_puts_a_whole_page_on_screen() {
        let sizes = letter(3);
        let (vw, vh) = (900.0, 700.0);
        let fitted = layout(&sizes, fit_scale(&sizes, PdfZoom::FitPage, vw, vh), vw);
        // The reader was halfway down page two when Fit Page was asked for.
        let was = Anchor {
            page: 1,
            u: 0.0,
            v: 0.5,
        };
        let page = fitted.pages[1];
        let whole = |top: f64| {
            top <= f64::from(page.y) && f64::from(page.y + page.h) <= top + f64::from(vh)
        };
        let (_, top) = offset_of(&fitted, resume_at(PdfZoom::FitPage, was)).expect("page two");
        assert!(whole(top), "page two is not wholly on screen from {top}");
        // Resuming where the reader was, which is what every other zoom does, leaves half of it
        // above the viewport and half of page three below: that is what Fit Page looked like.
        let (_, kept) = offset_of(&fitted, was).expect("page two");
        assert!(
            !whole(kept),
            "nothing to fix: page two already fits from {kept}"
        );
        assert_eq!(resume_at(PdfZoom::FitWidth, was), was);
    }

    fn scale(zoom: PdfZoom) -> f64 {
        let PdfZoom::Scale(zoom) = zoom else {
            panic!("a step is always a fixed scale")
        };
        zoom
    }

    #[test]
    fn zoom_steps_in_tenths_and_clamps() {
        assert_eq!(scale(stepped(1.0, false)), 1.1);
        assert_eq!(scale(stepped(1.0, true)), 0.9);
        // A page fitted to the window sits off a tenth: the next one, not a tenth further.
        assert_eq!(scale(stepped(1.37, false)), 1.4);
        assert_eq!(scale(stepped(1.37, true)), 1.3);
        assert_eq!(clamp_scale(1000.0), 8.0 * PT_TO_PX);
        assert_eq!(clamp_scale(0.0), 0.1 * PT_TO_PX);
    }

    #[test]
    fn a_step_reaches_both_ends_of_the_range_without_sticking() {
        // Stepping used to be read back out of the laid-out `f32` scale, where past 230 % the
        // rounding made every step land on the zoom the page was already at.
        assert_eq!(scale(stepped(2.3, false)), 2.4);
        let mut zoom = MIN_SCALE;
        for _ in 0..200 {
            let next = scale(stepped(zoom, false));
            assert!(next > zoom || next == MAX_SCALE, "stuck at {zoom}");
            zoom = next;
        }
        assert_eq!(zoom, MAX_SCALE);
        for _ in 0..200 {
            let next = scale(stepped(zoom, true));
            assert!(next < zoom || next == MIN_SCALE, "stuck at {zoom}");
            zoom = next;
        }
        assert_eq!(zoom, MIN_SCALE);
    }

    #[test]
    fn an_anchor_is_the_same_place_after_a_resize() {
        let sizes = letter(5);
        let fit = |width: f32| {
            layout(
                &sizes,
                fit_scale(&sizes, PdfZoom::FitWidth, width, 700.0),
                width,
            )
        };
        let (wide, narrow) = (fit(1000.0), fit(500.0));
        let anchor = Anchor {
            page: 2,
            u: 0.0,
            v: 1.0 / 3.0,
        };
        let (_, was) = offset_of(&wide, anchor).unwrap();
        let (_, now) = offset_of(&narrow, anchor).unwrap();
        // Half the column is half the height, so the raw offset means something else entirely:
        // it lands two pages further down. The anchor is what survives the resize.
        assert!(now < was);
        assert_eq!(anchor_at(&narrow, 0.0, now).page, anchor.page);
        assert!((anchor_at(&narrow, 0.0, now).v - anchor.v).abs() < 1e-4);
        assert_ne!(anchor_at(&narrow, 0.0, was).page, anchor.page);
    }

    #[test]
    fn an_anchor_survives_a_document_that_lost_pages() {
        let anchor = Anchor {
            page: 9,
            u: 0.5,
            v: 0.25,
        };
        assert_eq!(anchor.clamped(4).page, 3);
        assert_eq!(anchor.clamped(4).v, 0.25);
    }
}
