// Derived from draw.io js/grapheditor/Shapes.js (Apache-2.0, Copyright (c) 2006-2026 JGraph Holdings Ltd / draw.io AG), ported to Rust and modified for accent; see crates/drawio/NOTICE.
//! How far in a shape's `size` reaches, a share of its width until `fixedSize=1` makes it a
//! length, and the `size` that keeps it where it is drawn when that key is switched.

use crate::geom::Rect;
use crate::style::Resolved;

/// How a shape reads `size` (draw.io's `fixedSize` shapes): a length, `fixed` by default and
/// within the width or half of it (`half`), or a share of the width, `share` by default and at
/// most `limit`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Inset {
    fixed: f64,
    half: bool,
    share: f64,
    limit: f64,
}

impl Inset {
    /// A process's bars (Shapes.js 1976-2035), whose fixed default is the share's 0.1, so a
    /// fixed process with no `size` has its bars a tenth of a unit in.
    pub(super) const PROCESS: Inset = Inset::new(0.1, false, 0.1, 1.0);
    pub(super) const PARALLELOGRAM: Inset = Inset::new(20.0, false, 0.2, 1.0);
    pub(super) const TRAPEZOID: Inset = Inset::new(20.0, true, 0.2, 0.5);
    pub(super) const STEP: Inset = Inset::new(20.0, false, 0.2, 1.0);
    pub(super) const HEXAGON: Inset = Inset::new(20.0, true, 0.25, 1.0);
    pub(super) const DATA_STORAGE: Inset = Inset::new(20.0, false, 0.1, 1.0);

    const fn new(fixed: f64, half: bool, share: f64, limit: f64) -> Inset {
        Inset {
            fixed,
            half,
            share,
            limit,
        }
    }

    pub fn of(shape: &str) -> Option<Inset> {
        Some(match shape {
            "process" | "process2" => Inset::PROCESS,
            "parallelogram" => Inset::PARALLELOGRAM,
            "trapezoid" => Inset::TRAPEZOID,
            "step" => Inset::STEP,
            "hexagon" => Inset::HEXAGON,
            "dataStorage" => Inset::DATA_STORAGE,
            _ => return None,
        })
    }

    /// How far in `size` reaches in a shape `w` wide.
    pub(super) fn length(self, w: f64, style: &Resolved) -> f64 {
        let max = if self.half { w * 0.5 } else { w };
        match style.flag("fixedSize", false) {
            true => style.num("size", self.fixed).min(max).max(0.0),
            false => w * style.num("size", self.share).min(self.limit).max(0.0),
        }
    }

    /// The `size` that reaches as far into `b` as `style` has it now once `fixedSize` is
    /// `fixed`: the length, to a hundredth, or its share of the width, to as many places as keep
    /// it within a hundredth of a unit; `None` for a shape with no width to have a share of.
    pub fn kept(self, b: Rect, style: &Resolved, fixed: bool) -> Option<f64> {
        let length = self.length(b.w, style);
        if fixed {
            return Some((length * 100.0).round() / 100.0);
        }
        (b.w > 0.0).then(|| {
            let places = 10f64.powf((100.0 * b.w).log10().ceil());
            (length / b.w * places).round() / places
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shapes::tests::style;

    #[test]
    fn a_size_reaches_as_far_either_way() {
        let b = Rect::new(0.0, 0.0, 120.0, 60.0);
        // A fifth of 120 is 24, which a fixed size gives as it is.
        let share = style("shape=parallelogram;", false);
        assert_eq!(Inset::PARALLELOGRAM.kept(b, &share, true), Some(24.0));
        let fixed = style("shape=parallelogram;fixedSize=1;size=30;", false);
        assert_eq!(Inset::PARALLELOGRAM.kept(b, &fixed, false), Some(0.25));
        // A third of the width, to as many places as keep it within a hundredth of a unit.
        let odd = style("shape=hexagon;fixedSize=1;size=10;", false);
        assert_eq!(
            Inset::HEXAGON.kept(Rect::new(0.0, 0.0, 30.0, 10.0), &odd, false),
            Some(0.3333)
        );
        // Within its bounds: a trapezoid's half of the width, and no width to share.
        let wide = style("shape=trapezoid;size=0.9;", false);
        assert_eq!(Inset::TRAPEZOID.kept(b, &wide, true), Some(60.0));
        let flat = Rect::new(0.0, 0.0, 0.0, 60.0);
        assert_eq!(Inset::STEP.kept(flat, &fixed, false), None);
    }

    #[test]
    fn only_shapes_draw_io_gives_a_fixed_size_have_one() {
        assert_eq!(Inset::of("process2"), Some(Inset::PROCESS));
        assert_eq!(Inset::of("dataStorage"), Some(Inset::DATA_STORAGE));
        for shape in ["rectangle", "document", "tape", "cylinder3", "callout"] {
            assert_eq!(Inset::of(shape), None, "{shape}");
        }
    }
}
