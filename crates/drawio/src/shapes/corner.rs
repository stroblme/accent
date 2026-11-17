// Derived from draw.io mxgraph/src/shape/mxRectangleShape.js, mxgraph/src/shape/mxSwimlane.js and js/grapheditor/Shapes.js (Apache-2.0, Copyright (c) 2006-2026 JGraph Holdings Ltd / draw.io AG), ported to Rust and modified for accent; see crates/drawio/NOTICE.
//! A rounded corner that grows with its shape until `absoluteArcSize=1` gives it a size of its
//! own, and the `arcSize` that keeps it where it is drawn when that key is switched.

use super::{grapheditor, mxgraph};
use crate::geom::Rect;
use crate::style::Resolved;

/// The shapes whose `arcSize` is a share of their size unless `absoluteArcSize=1` makes it a
/// length in units (`mxRectangleShape.paintBackground`, `mxSwimlane.getSwimlaneArcSize`,
/// `FolderShape.paintVertexShape`).
// ponytail: a process and an internal storage are left out, whose bars and lines keep off a
// corner read as a percentage whatever `absoluteArcSize` says (Shapes.js 2006-2011, 4080-4085),
// so they would move.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Corner {
    /// A rectangle and the shapes drawn on one: the share is a percentage of the shorter side,
    /// and the length twice the radius.
    Rectangle,
    /// A percentage of three times the title bar's depth, and the length twice the radius.
    Swimlane,
    /// A 0–1 share of the shorter side, 0.1 by default, and the length the radius itself.
    Folder,
}

impl Corner {
    pub fn of(shape: &str) -> Option<Corner> {
        match shape {
            "label" | "rectangle" | "rect" | "plus" => Some(Corner::Rectangle),
            "swimlane" => Some(Corner::Swimlane),
            "folder" => Some(Corner::Folder),
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
            Corner::Folder => grapheditor::folder_radius(b, style),
        }
    }

    /// How much radius one unit of `arcSize` gives in `b`, as a share of the shape or, `fixed`,
    /// as a length.
    fn per_unit(self, b: Rect, style: &Resolved, fixed: bool) -> f64 {
        match (self, fixed) {
            (Corner::Folder, true) => 1.0,
            (Corner::Folder, false) => b.w.min(b.h),
            (_, true) => 0.5,
            (Corner::Rectangle, false) => b.w.min(b.h) / 100.0,
            (Corner::Swimlane, false) => 3.0 * mxgraph::swimlane_start(b, style) / 100.0,
        }
    }

    /// The `arcSize` that draws the corner in `b` where `style` draws it now once
    /// `absoluteArcSize` is `fixed`, to a hundredth, a folder's share to a ten-thousandth, as fine
    /// as a percentage to a hundredth; `None` where the shape is too thin to have a share of.
    pub fn kept(self, b: Rect, style: &Resolved, fixed: bool) -> Option<f64> {
        let unit = self.per_unit(b, style, fixed);
        let places = match (self, fixed) {
            (Corner::Folder, false) => 1e4,
            _ => 1e2,
        };
        (unit > 0.0).then(|| (self.radius(b, style) / unit * places).round() / places)
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
    fn a_folder_s_corner_is_a_share_of_its_shorter_side_or_its_radius() {
        // 0.1 of the 60 side is a radius of 6, which a fixed corner gives with 6 itself.
        let folder = Rect::new(0.0, 0.0, 120.0, 60.0);
        let s = style("folder;rounded=1;", false);
        assert_eq!(Corner::Folder.kept(folder, &s, true), Some(6.0));
        let fixed = style("folder;rounded=1;absoluteArcSize=1;arcSize=6.5;", false);
        assert_eq!(Corner::Folder.kept(folder, &fixed, false), Some(0.1083));
        // Within half the body under the 20 high tab, as draw.io draws it.
        let big = style("folder;rounded=1;arcSize=0.5;", false);
        assert_eq!(Corner::Folder.kept(folder, &big, true), Some(20.0));
    }

    #[test]
    fn only_shapes_whose_corner_grows_with_them_have_one() {
        assert_eq!(Corner::of("label"), Some(Corner::Rectangle));
        assert_eq!(Corner::of("plus"), Some(Corner::Rectangle));
        assert_eq!(Corner::of("swimlane"), Some(Corner::Swimlane));
        assert_eq!(Corner::of("folder"), Some(Corner::Folder));
        // A polygon's corner is already in units, and a process's bars would move.
        for shape in [
            "hexagon",
            "rhombus",
            "ellipse",
            "process",
            "internalStorage",
            "mxgraph.aws4.lambda",
        ] {
            assert_eq!(Corner::of(shape), None, "{shape}");
        }
    }
}
