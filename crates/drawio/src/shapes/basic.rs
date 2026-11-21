// Derived from draw.io shapes/mxBasic.js (Apache-2.0, Copyright (c) 2006-2026 JGraph Holdings Ltd / draw.io AG), ported to Rust and modified for accent; see crates/drawio/NOTICE.
//! The shapes of draw.io's Basic palette that it draws in code (`mxBasic.js`).

use super::{Part, quarter};
use crate::geom::{PathCmd, Point, Rect};
use crate::style::Resolved;

/// `mxgraph.basic.diag_round_rect`: a box with its top-left and bottom-right corners rounded,
/// twice `dx` across (`mxShapeBasicDiagRoundRect`, mxBasic.js 1241-1285).
pub(super) fn diag_round_rect(b: Rect, style: &Resolved) -> Vec<Part> {
    let (w, h) = (b.w, b.h);
    let d = (style.num("dx", 0.5).clamp(0.0, w) * 2.0)
        .min(w * 0.5)
        .min(h * 0.5);
    let p = |x: f64, y: f64| Point::new(b.x + x, b.y + y);
    vec![Part::body(vec![
        PathCmd::MoveTo(p(d, 0.0)),
        PathCmd::LineTo(p(w, 0.0)),
        PathCmd::LineTo(p(w, h - d)),
        quarter(p(w, h - d), p(w, h), p(w - d, h)),
        PathCmd::LineTo(p(0.0, h)),
        PathCmd::LineTo(p(0.0, d)),
        quarter(p(0.0, d), p(0.0, 0.0), p(d, 0.0)),
        PathCmd::Close,
    ])]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shapes::tests::style;

    #[test]
    fn a_diagonal_round_rect_rounds_two_corners_by_twice_dx() {
        let b = Rect::new(0.0, 0.0, 100.0, 60.0);
        let ends = |s: &str| -> Vec<Point> {
            diag_round_rect(b, &style(s, false))[0]
                .path
                .iter()
                .filter_map(|c| match c {
                    PathCmd::MoveTo(p) | PathCmd::LineTo(p) | PathCmd::CurveTo(_, _, p) => Some(*p),
                    _ => None,
                })
                .collect()
        };
        let pts = |v: &[(f64, f64)]| v.iter().map(|&(x, y)| Point::new(x, y)).collect::<Vec<_>>();
        assert_eq!(
            ends("shape=mxgraph.basic.diag_round_rect;dx=6;"),
            pts(&[
                (12.0, 0.0),
                (100.0, 0.0),
                (100.0, 48.0),
                (88.0, 60.0),
                (0.0, 60.0),
                (0.0, 12.0),
                (12.0, 0.0)
            ])
        );
        // At most half the shorter side, and 0.5 when the style gives none.
        assert_eq!(ends("dx=100;")[0], Point::new(30.0, 0.0));
        assert_eq!(ends("")[0], Point::new(1.0, 0.0));
    }
}
