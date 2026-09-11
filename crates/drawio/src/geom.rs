//! Points, rectangles and paths in page units: pixels at zoom 1, y growing downwards, angles in
//! degrees clockwise, which is how draw.io stores all three.

/// A point on the page.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    pub const fn new(x: f64, y: f64) -> Point {
        Point { x, y }
    }

    pub fn distance(self, other: Point) -> f64 {
        (other.x - self.x).hypot(other.y - self.y)
    }
}

/// An axis-aligned rectangle, its origin top-left.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub const fn new(x: f64, y: f64, w: f64, h: f64) -> Rect {
        Rect { x, y, w, h }
    }

    /// The rectangle two corners span, whichever way round they are given.
    pub fn from_corners(a: Point, b: Point) -> Rect {
        Rect::new(
            a.x.min(b.x),
            a.y.min(b.y),
            (a.x - b.x).abs(),
            (a.y - b.y).abs(),
        )
    }

    pub fn right(&self) -> f64 {
        self.x + self.w
    }

    pub fn bottom(&self) -> f64 {
        self.y + self.h
    }

    pub fn centre(&self) -> Point {
        Point::new(self.x + self.w / 2.0, self.y + self.h / 2.0)
    }

    /// Edges included, so a zero-sized rectangle still contains its own corner.
    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.x && p.x <= self.right() && p.y >= self.y && p.y <= self.bottom()
    }

    pub fn contains_rect(&self, other: &Rect) -> bool {
        other.x >= self.x
            && other.y >= self.y
            && other.right() <= self.right()
            && other.bottom() <= self.bottom()
    }

    pub fn intersects(&self, other: &Rect) -> bool {
        self.x <= other.right()
            && other.x <= self.right()
            && self.y <= other.bottom()
            && other.y <= self.bottom()
    }

    pub fn union(&self, other: &Rect) -> Rect {
        let (x, y) = (self.x.min(other.x), self.y.min(other.y));
        Rect::new(
            x,
            y,
            self.right().max(other.right()) - x,
            self.bottom().max(other.bottom()) - y,
        )
    }

    /// Grown by `d` on every side (shrunk for a negative `d`).
    pub fn grow(&self, d: f64) -> Rect {
        Rect::new(self.x - d, self.y - d, self.w + 2.0 * d, self.h + 2.0 * d)
    }

    pub fn translate(&self, dx: f64, dy: f64) -> Rect {
        Rect::new(self.x + dx, self.y + dy, self.w, self.h)
    }
}

/// `p` turned `degrees` clockwise about `centre`, in `mxUtils.getRotatedPoint`'s order of
/// operations, so a rotated terminal's routes agree with draw.io's to the last bit.
pub fn rotate(p: Point, centre: Point, degrees: f64) -> Point {
    if degrees == 0.0 {
        return p;
    }
    let (sin, cos) = (std::f64::consts::PI * degrees / 180.0).sin_cos();
    let (dx, dy) = (p.x - centre.x, p.y - centre.y);
    Point::new(
        dx * cos - dy * sin + centre.x,
        dy * cos + dx * sin + centre.y,
    )
}

/// The axis-aligned box around `r` turned `degrees` about its centre (`mxUtils.getBoundingBox`).
pub fn bounding_box(r: &Rect, degrees: f64) -> Rect {
    if degrees == 0.0 {
        return *r;
    }
    let c = r.centre();
    let corners = [
        Point::new(r.x, r.y),
        Point::new(r.right(), r.y),
        Point::new(r.right(), r.bottom()),
        Point::new(r.x, r.bottom()),
    ];
    bounds_of(corners.map(|p| rotate(p, c, degrees))).unwrap_or(*r)
}

/// The box around a set of points, or `None` for no points.
pub fn bounds_of(points: impl IntoIterator<Item = Point>) -> Option<Rect> {
    let mut it = points.into_iter();
    let first = it.next()?;
    let (mut lo, mut hi) = (first, first);
    for p in it {
        lo = Point::new(lo.x.min(p.x), lo.y.min(p.y));
        hi = Point::new(hi.x.max(p.x), hi.y.max(p.y));
    }
    Some(Rect::from_corners(lo, hi))
}

/// How far `p` is from the segment `a`–`b`.
pub fn distance_to_segment(p: Point, a: Point, b: Point) -> f64 {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len2 = dx * dx + dy * dy;
    if len2 == 0.0 {
        return p.distance(a);
    }
    let t = (((p.x - a.x) * dx + (p.y - a.y) * dy) / len2).clamp(0.0, 1.0);
    p.distance(Point::new(a.x + t * dx, a.y + t * dy))
}

/// One step of an outline. Every coordinate is absolute.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PathCmd {
    MoveTo(Point),
    LineTo(Point),
    /// Control point, end point.
    QuadTo(Point, Point),
    /// Two control points, end point.
    CurveTo(Point, Point, Point),
    Close,
}

/// Every point of `path` passed through `f`: how a shape is rotated or moved.
pub fn map_path(path: &mut [PathCmd], f: impl Fn(Point) -> Point) {
    for cmd in path {
        *cmd = match *cmd {
            PathCmd::MoveTo(p) => PathCmd::MoveTo(f(p)),
            PathCmd::LineTo(p) => PathCmd::LineTo(f(p)),
            PathCmd::QuadTo(c, p) => PathCmd::QuadTo(f(c), f(p)),
            PathCmd::CurveTo(c1, c2, p) => PathCmd::CurveTo(f(c1), f(c2), f(p)),
            PathCmd::Close => PathCmd::Close,
        };
    }
}

/// Segments a curve is cut into by [`flatten`]. Enough for hit testing and bounds at any zoom
/// the canvas allows; painting uses the real curves.
const CURVE_STEPS: usize = 8;

/// `path` as polylines, one per subpath; a closed subpath ends on its first point again.
pub fn flatten(path: &[PathCmd]) -> Vec<Vec<Point>> {
    let mut out: Vec<Vec<Point>> = Vec::new();
    let mut at = Point::default();
    for cmd in path {
        match *cmd {
            PathCmd::MoveTo(p) => {
                out.push(vec![p]);
                at = p;
            }
            PathCmd::LineTo(p) => {
                current(&mut out, at).push(p);
                at = p;
            }
            PathCmd::QuadTo(c, p) => {
                let line = current(&mut out, at);
                for i in 1..=CURVE_STEPS {
                    let t = i as f64 / CURVE_STEPS as f64;
                    let u = 1.0 - t;
                    line.push(Point::new(
                        u * u * at.x + 2.0 * u * t * c.x + t * t * p.x,
                        u * u * at.y + 2.0 * u * t * c.y + t * t * p.y,
                    ));
                }
                at = p;
            }
            PathCmd::CurveTo(c1, c2, p) => {
                let line = current(&mut out, at);
                for i in 1..=CURVE_STEPS {
                    let t = i as f64 / CURVE_STEPS as f64;
                    let u = 1.0 - t;
                    let (a, b, c, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
                    line.push(Point::new(
                        a * at.x + b * c1.x + c * c2.x + d * p.x,
                        a * at.y + b * c1.y + c * c2.y + d * p.y,
                    ));
                }
                at = p;
            }
            PathCmd::Close => {
                if let Some(line) = out.last_mut()
                    && let Some(&first) = line.first()
                {
                    line.push(first);
                    at = first;
                }
            }
        }
    }
    out
}

/// The polyline being drawn, starting one at `at` if a path begins without a move.
fn current(out: &mut Vec<Vec<Point>>, at: Point) -> &mut Vec<Point> {
    if out.is_empty() {
        out.push(vec![at]);
    }
    out.last_mut().expect("just pushed")
}

/// The box around every point `path` passes through (curves included, control points not).
pub fn path_bounds(path: &[PathCmd]) -> Option<Rect> {
    bounds_of(flatten(path).into_iter().flatten())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(a: Point, b: Point) -> bool {
        a.distance(b) < 1e-9
    }

    #[test]
    fn a_quarter_turn_is_clockwise_with_y_down() {
        let p = rotate(Point::new(10.0, 0.0), Point::new(0.0, 0.0), 90.0);
        assert!(near(p, Point::new(0.0, 10.0)), "{p:?}");
    }

    #[test]
    fn a_rotated_square_grows_its_box() {
        let b = bounding_box(&Rect::new(0.0, 0.0, 10.0, 10.0), 45.0);
        let d = 10.0 * std::f64::consts::SQRT_2;
        assert!((b.w - d).abs() < 1e-9 && (b.h - d).abs() < 1e-9, "{b:?}");
        assert!(near(b.centre(), Point::new(5.0, 5.0)));
    }

    #[test]
    fn flattening_closes_and_keeps_curve_ends() {
        let path = [
            PathCmd::MoveTo(Point::new(0.0, 0.0)),
            PathCmd::QuadTo(Point::new(5.0, 10.0), Point::new(10.0, 0.0)),
            PathCmd::Close,
        ];
        let lines = flatten(&path);
        assert_eq!(lines.len(), 1);
        let line = &lines[0];
        assert_eq!(line.len(), 1 + CURVE_STEPS + 1);
        assert!(near(line[CURVE_STEPS], Point::new(10.0, 0.0)));
        assert!(near(*line.last().unwrap(), Point::new(0.0, 0.0)));
        let b = path_bounds(&path).unwrap();
        assert!(
            (b.h - 5.0).abs() < 1e-9,
            "the curve peaks at half its control point: {b:?}"
        );
    }

    #[test]
    fn segment_distance_clamps_to_the_ends() {
        let (a, b) = (Point::new(0.0, 0.0), Point::new(10.0, 0.0));
        assert_eq!(distance_to_segment(Point::new(5.0, 3.0), a, b), 3.0);
        assert_eq!(distance_to_segment(Point::new(13.0, 4.0), a, b), 5.0);
    }
}
