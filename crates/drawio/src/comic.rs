// Derived from draw.io js/grapheditor/Shapes.js and js/diagramly/Editor.js (Apache-2.0, Copyright (c) 2006-2026 JGraph Holdings Ltd / draw.io AG), ported to Rust and modified for accent; see crates/drawio/NOTICE.
//! draw.io's comic drawing, a line drawn as by hand (`HandJiggle`, Shapes.js 1684-1821): each
//! straight piece of an outline wobbles a little off its course, while curves stay as they are.
//! draw.io draws it afresh at random on every paint; here each cell's wobble is drawn from its
//! id, so it holds still while the page is redrawn, zoomed, moved or exported.

use crate::geom::{PathCmd, Point};
use crate::scene::{Cap, Join, Stroke};
use crate::style::Resolved;

/// `Editor.sketchDefaultJiggle`, how far a line wobbles where the style gives no `jiggle`.
const DEFAULT_JIGGLE: f64 = 2.0;

/// A straight piece shorter than this stays straight (`HandJiggle.lineTo`).
const STEADY: f64 = 2.0;

/// Whether a cell is drawn by hand: `comic=1`, or `sketch=1` in either `sketchStyle`, rough.js's
/// rough drawing being drawn as comic (`mxShape.createHandJiggle` and its override in Editor.js
/// 2093-2111).
pub fn is_comic(style: &Resolved) -> bool {
    style.flag("sketch", false) || style.flag("comic", false)
}

/// Whether drawing `path` by hand changes it: some straight piece of it, the closing one
/// included, is long enough to wobble.
pub(crate) fn wobbles(path: &[PathCmd]) -> bool {
    let (mut first, mut last) = (None, None);
    path.iter().any(|&cmd| {
        let to = match cmd {
            PathCmd::MoveTo(p) => {
                (first, last) = (Some(p), Some(p));
                return false;
            }
            PathCmd::QuadTo(_, p) | PathCmd::CurveTo(_, _, p) => {
                last = Some(p);
                return false;
            }
            PathCmd::LineTo(p) => p,
            PathCmd::Close => match first {
                Some(p) => p,
                None => return false,
            },
        };
        last.replace(to)
            .is_some_and(|from| from.distance(to) >= STEADY)
    })
}

/// `path` and its `stroke` as `jiggle` draws them by hand, round at the ends and corners where
/// the shape sets neither (`HandJiggle` sets both before the shape paints), or as they are
/// without one.
pub(crate) fn draw(
    jiggle: &mut Option<Jiggle>,
    path: &mut Vec<PathCmd>,
    stroke: Option<Stroke>,
) -> Option<Stroke> {
    let Some(jiggle) = jiggle else {
        return stroke;
    };
    *path = jiggle.path(path);
    stroke.map(|s| Stroke {
        cap: if s.cap == Cap::Butt {
            Cap::Round
        } else {
            s.cap
        },
        join: if s.join == Join::default() {
            Join::Round
        } else {
            s.join
        },
        ..s
    })
}

/// The wobble of one cell's lines.
// ponytail: draw.io's canvas draws a stencil's `rect` and `roundrect` and a partial rectangle's
// fill whole, unwobbled, where their outlines here are lines like any other and wobble too.
pub(crate) struct Jiggle {
    /// How far a line wobbles across its course, all told.
    variation: f64,
    /// A splitmix64 state, seeded from the cell's id.
    state: u64,
}

impl Jiggle {
    /// The wobble of cell `id` drawn with `style`, or `None` where it is not drawn by hand.
    pub(crate) fn of(id: &str, style: &Resolved) -> Option<Jiggle> {
        // FNV-1a: a hash that stays the same across builds and platforms.
        let seed = id.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
            (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
        });
        is_comic(style).then(|| Jiggle {
            variation: style.num("jiggle", DEFAULT_JIGGLE),
            state: seed,
        })
    }

    /// The next number in `[0, 1)`, as `Math.random()` gives one.
    fn random(&mut self) -> f64 {
        self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        ((z ^ (z >> 31)) >> 11) as f64 / (1u64 << 53) as f64
    }

    /// `path` with every straight piece drawn by hand, closing pieces included.
    pub(crate) fn path(&mut self, path: &[PathCmd]) -> Vec<PathCmd> {
        let mut out = Vec::with_capacity(path.len());
        let (mut first, mut last) = (None, None);
        for &cmd in path {
            match cmd {
                PathCmd::MoveTo(p) => (first, last) = (Some(p), Some(p)),
                PathCmd::LineTo(p) => {
                    self.line(&mut out, last, p);
                    last = Some(p);
                    continue;
                }
                PathCmd::QuadTo(_, p) | PathCmd::CurveTo(_, _, p) => last = Some(p),
                PathCmd::Close => {
                    if let Some(p) = first {
                        self.line(&mut out, last, p);
                        last = Some(p);
                    }
                }
            }
            out.push(cmd);
        }
        out
    }

    /// A line from `from` to `to`: pieces about ten units long, five at least for a third of
    /// the wobble, each begun off the line by up to half the variation; under two units, or with
    /// nowhere to start from, straight (`HandJiggle.lineTo`).
    fn line(&mut self, out: &mut Vec<PathCmd>, from: Option<Point>, to: Point) {
        let Some(from) = from else {
            out.push(PathCmd::LineTo(to));
            return;
        };
        let (dx, dy) = ((to.x - from.x).abs(), (to.y - from.y).abs());
        let dist = dx.hypot(dy);
        if dist < STEADY {
            out.push(PathCmd::LineTo(to));
            return;
        }
        let (mut segs, mut variation) = ((dist / 10.0).round(), self.variation);
        if segs < 5.0 {
            segs = 5.0;
            variation /= 3.0;
        }
        let step = Point::new((to.x - from.x) / segs, (to.y - from.y) / segs);
        // draw.io's offset leans on the line's slope by its size alone, whichever way it runs.
        let (fx, fy) = (dx / dist, dy / dist);
        for s in 0..segs as usize {
            let (x, y) = (from.x + step.x * s as f64, from.y + step.y * s as f64);
            let offset = (self.random() - 0.5) * variation;
            out.push(PathCmd::LineTo(Point::new(
                x - offset * fy,
                y - offset * fx,
            )));
        }
        out.push(PathCmd::LineTo(to));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::Style;

    fn comic(id: &str, style: &str) -> Option<Jiggle> {
        Jiggle::of(id, &Style::parse(style).resolve(false))
    }

    fn square() -> Vec<PathCmd> {
        let p = Point::new;
        vec![
            PathCmd::MoveTo(p(0.0, 0.0)),
            PathCmd::LineTo(p(100.0, 0.0)),
            PathCmd::LineTo(p(100.0, 100.0)),
            PathCmd::QuadTo(p(50.0, 150.0), p(0.0, 100.0)),
            PathCmd::Close,
        ]
    }

    #[test]
    fn comic_and_either_sketch_are_drawn_by_hand() {
        for style in ["comic=1;", "sketch=1;sketchStyle=comic;", "sketch=1;"] {
            assert!(comic("a", style).is_some(), "{style}");
        }
        for style in ["", "comic=0;", "sketch=0;sketchStyle=comic;"] {
            assert!(comic("a", style).is_none(), "{style}");
        }
    }

    #[test]
    fn a_cell_wobbles_the_same_every_time_and_another_its_own_way() {
        let drawn = |id: &str| comic(id, "comic=1;").unwrap().path(&square());
        assert_eq!(drawn("a"), drawn("a"));
        assert_ne!(drawn("a"), drawn("b"));
    }

    #[test]
    fn straight_pieces_wobble_within_half_the_jiggle_and_curves_stay() {
        let path = comic("a", "comic=1;jiggle=4;").unwrap().path(&square());
        // 100 long: ten pieces each, the closing side's too, round the untouched curve.
        let quad = PathCmd::QuadTo(Point::new(50.0, 150.0), Point::new(0.0, 100.0));
        let at = path.iter().position(|c| *c == quad).unwrap();
        assert_eq!(at, 1 + 2 * 11);
        assert_eq!(path.len(), at + 1 + 11 + 1);
        assert_eq!(path[0], PathCmd::MoveTo(Point::new(0.0, 0.0)));
        assert_eq!(path[at - 1], PathCmd::LineTo(Point::new(100.0, 100.0)));
        assert_eq!(path[path.len() - 2], PathCmd::LineTo(Point::new(0.0, 0.0)));
        assert_eq!(path.last(), Some(&PathCmd::Close));
        // Along the top only across it, by up to 2, and the pieces evenly spaced.
        for (s, cmd) in path[1..11].iter().enumerate() {
            let PathCmd::LineTo(p) = cmd else { panic!() };
            assert_eq!(p.x, 10.0 * s as f64);
            assert!(p.y.abs() <= 2.0 && p.y != 0.0, "{p:?}");
        }
    }

    #[test]
    fn a_path_wobbles_where_it_has_a_straight_piece_of_two_units() {
        let p = Point::new;
        assert!(wobbles(&square()));
        let tiny = [
            PathCmd::MoveTo(p(0.0, 0.0)),
            PathCmd::LineTo(p(1.0, 1.0)),
            PathCmd::QuadTo(p(50.0, 50.0), p(0.0, 0.0)),
            PathCmd::Close,
        ];
        assert!(!wobbles(&tiny), "curves and short lines stay");
        let closing = [
            PathCmd::MoveTo(p(0.0, 0.0)),
            PathCmd::QuadTo(p(50.0, 50.0), p(0.0, 10.0)),
            PathCmd::Close,
        ];
        assert!(wobbles(&closing), "the closing piece is a line");
    }

    #[test]
    fn short_lines_take_five_pieces_and_a_third_of_the_wobble() {
        let line = [
            PathCmd::MoveTo(Point::new(0.0, 0.0)),
            PathCmd::LineTo(Point::new(0.0, 30.0)),
            PathCmd::LineTo(Point::new(1.0, 30.0)),
        ];
        let path = comic("a", "sketch=1;jiggle=6;").unwrap().path(&line);
        assert_eq!(path.len(), 1 + 5 + 1 + 1, "under two units is straight");
        for cmd in &path[1..6] {
            let PathCmd::LineTo(p) = cmd else { panic!() };
            assert!(p.x.abs() <= 1.0, "{p:?}");
        }
        assert_eq!(path[7], PathCmd::LineTo(Point::new(1.0, 30.0)));
    }
}
