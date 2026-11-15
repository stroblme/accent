// Derived from draw.io mxgraph/src/shape/mxRectangleShape.js and mxgraph/src/shape/mxSwimlane.js (Apache-2.0, Copyright (c) 2006-2026 JGraph Holdings Ltd / draw.io AG), ported to Rust and modified for accent; see crates/drawio/NOTICE.
//! A rounded corner that grows with its shape until `absoluteArcSize=1` gives it a size of its
//! own, and the `arcSize` that keeps it where it is drawn when that key is switched.

use super::mxgraph;
use crate::geom::Rect;
use crate::style::Resolved;

/// The shapes whose `arcSize` is a share of their size unless `absoluteArcSize=1` makes it
/// twice the radius in units (`mxRectangleShape.paintBackground`, `mxSwimlane.getSwimlaneArcSize`).
// ponytail: a folder reads its `arcSize` as a 0–1 share and with `absoluteArcSize` as the radius
// itself, which the Properties pane's Arc Size row does not offer yet, so it is left out. So are
// a process and an internal storage, whose bars and lines keep off a corner read as a percentage
// whatever `absoluteArcSize` says (Shapes.js 2006-2011, 4080-4085), so they would move.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Corner {
    /// A rectangle and the shapes drawn on one: the share is a percentage of the shorter side.
    Rectangle,
    /// A percentage of three times the title bar's depth.
    Swimlane,
}

impl Corner {
    pub fn of(shape: &str) -> Option<Corner> {
        match shape {
            "label" | "rectangle" | "rect" | "plus" => Some(Corner::Rectangle),
            "swimlane" => Some(Corner::Swimlane),
            _ => None,
        }
    }

    /// The radius the corner is drawn at in `b` when rounded.
    fn radius(self, b: Rect, style: &Resolved) -> f64 {
        match self {
            Corner::Rectangle => mxgraph::rectangle_corner(b, style)
                .min(b.w / 2.0)
                .min(b.h / 2.0),
            Corner::Swimlane => mxgraph::swimlane_corner(b, style),
        }
    }

    /// How much radius one unit of `arcSize` gives in `b`, as a share of the shape or, `fixed`,
    /// as a length.
    fn per_unit(self, b: Rect, style: &Resolved, fixed: bool) -> f64 {
        match (self, fixed) {
            (_, true) => 0.5,
            (Corner::Rectangle, false) => b.w.min(b.h) / 100.0,
            (Corner::Swimlane, false) => 3.0 * mxgraph::swimlane_start(b, style) / 100.0,
        }
    }

    /// The `arcSize` that draws the corner in `b` where `style` draws it now once
    /// `absoluteArcSize` is `fixed`, to a hundredth; `None` where the shape is too thin to have
    /// a share of.
    pub fn kept(self, b: Rect, style: &Resolved, fixed: bool) -> Option<f64> {
        let unit = self.per_unit(b, style, fixed);
        (unit > 0.0).then(|| (self.radius(b, style) / unit * 100.0).round() / 100.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shapes::tests::style;

    #[test]
    fn a_corner_keeps_its_radius_either_way() {
        let wide = Rect::new(0.0, 0.0, 100.0, 40.0);
        // 15 % of 40 is a radius of 6, which a fixed corner gives with 12.
        let relative = style("rounded=1;", false);
        assert_eq!(Corner::Rectangle.kept(wide, &relative, true), Some(12.0));
        let fixed = style("rounded=1;absoluteArcSize=1;arcSize=12;", false);
        assert_eq!(Corner::Rectangle.kept(wide, &fixed, false), Some(15.0));
        // Fixed, two sizes draw one radius, which is a different share of each.
        let tall = Rect::new(0.0, 0.0, 300.0, 120.0);
        assert_eq!(Corner::Rectangle.kept(tall, &fixed, false), Some(5.0));
        assert_eq!(Corner::Rectangle.kept(tall, &fixed, true), Some(12.0));
        // The thirds a share leaves are written to a hundredth.
        let odd = Rect::new(0.0, 0.0, 90.0, 30.0);
        assert_eq!(Corner::Rectangle.kept(odd, &fixed, false), Some(20.0));
        let three = Rect::new(0.0, 0.0, 90.0, 36.0);
        assert_eq!(Corner::Rectangle.kept(three, &fixed, false), Some(16.67));
    }

    #[test]
    fn an_elliptical_corner_becomes_the_round_one_that_fits() {
        // 60 % of the 40 side is a radius of 24, which a 40 high box draws 24 across and 20
        // down; a fixed corner is round, at most half the shorter side.
        let wide = Rect::new(0.0, 0.0, 100.0, 40.0);
        let s = style("rounded=1;arcSize=60;", false);
        assert_eq!(Corner::Rectangle.kept(wide, &s, true), Some(40.0));
    }

    #[test]
    fn a_swimlane_s_corner_is_a_share_of_its_title() {
        let lane = Rect::new(0.0, 0.0, 200.0, 100.0);
        // 15 % of three times the 20 deep title: a radius of 9.
        let s = style("swimlane;rounded=1;startSize=20;", false);
        assert_eq!(Corner::Swimlane.kept(lane, &s, true), Some(18.0));
        let s = style(
            "swimlane;rounded=1;startSize=20;absoluteArcSize=1;arcSize=18;",
            false,
        );
        assert_eq!(Corner::Swimlane.kept(lane, &s, false), Some(15.0));
        let flat = style("swimlane;rounded=1;startSize=0;", false);
        assert_eq!(Corner::Swimlane.kept(lane, &flat, false), None);
    }

    #[test]
    fn only_shapes_whose_corner_grows_with_them_have_one() {
        assert_eq!(Corner::of("label"), Some(Corner::Rectangle));
        assert_eq!(Corner::of("plus"), Some(Corner::Rectangle));
        assert_eq!(Corner::of("swimlane"), Some(Corner::Swimlane));
        // A polygon's corner is already in units, a folder's share is its own, and a process's
        // bars would move.
        for shape in [
            "hexagon",
            "rhombus",
            "ellipse",
            "folder",
            "process",
            "internalStorage",
            "mxgraph.aws4.lambda",
        ] {
            assert_eq!(Corner::of(shape), None, "{shape}");
        }
    }
}
