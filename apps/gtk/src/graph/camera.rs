//! Where the graph's world is on the canvas: the layout's coordinates scaled by the zoom and
//! moved by the offset, a screen point being `world · zoom + offset`, and which parts of the
//! canvas its names take. Pure, so it is tested without a window.

/// The space kept round what a fit shows, and round the node the keyboard is on, in pixels.
pub(super) const MARGIN: f64 = 32.0;

/// How far past a node's disc a press or the pointer still finds it, in pixels.
const SLOP: f64 = 4.0;

/// The smallest a node is drawn, in pixels, however far out the graph is zoomed.
pub(super) const MIN_RADIUS: f64 = 1.5;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    /// Where the world's origin is on the canvas.
    pub offset: [f64; 2],
    pub zoom: f64,
    /// Kept fitted to the nodes as they settle, until the reader pans or zooms.
    pub fitting: bool,
}

impl Default for Camera {
    fn default() -> Self {
        Camera {
            offset: [0.0; 2],
            zoom: 1.0,
            fitting: true,
        }
    }
}

/// An arrow key's way across the canvas, `y` growing downwards as the canvas's does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Arrow {
    Left,
    Right,
    Up,
    Down,
}

impl Arrow {
    fn vector(self) -> [f32; 2] {
        match self {
            Arrow::Left => [-1.0, 0.0],
            Arrow::Right => [1.0, 0.0],
            Arrow::Up => [0.0, -1.0],
            Arrow::Down => [0.0, 1.0],
        }
    }
}

impl Camera {
    pub fn to_screen(self, p: [f32; 2]) -> [f64; 2] {
        [
            f64::from(p[0]) * self.zoom + self.offset[0],
            f64::from(p[1]) * self.zoom + self.offset[1],
        ]
    }

    pub fn to_world(self, s: [f64; 2]) -> [f32; 2] {
        [
            ((s[0] - self.offset[0]) / self.zoom) as f32,
            ((s[1] - self.offset[1]) / self.zoom) as f32,
        ]
    }

    /// At `zoom`, the world point under the canvas point `at` staying there.
    pub fn zoom_at(&mut self, zoom: f64, at: [f64; 2]) {
        let world = self.to_world(at);
        self.zoom = zoom;
        self.offset = [
            at[0] - f64::from(world[0]) * zoom,
            at[1] - f64::from(world[1]) * zoom,
        ];
        self.fitting = false;
    }

    /// Moved by `by` canvas pixels, as a drag or a scroll moves it.
    pub fn pan(&mut self, by: [f64; 2]) {
        self.offset = [self.offset[0] + by[0], self.offset[1] + by[1]];
        self.fitting = false;
    }

    /// World point `p` in the middle of a canvas of `size`, at the zoom it is at.
    pub fn centre(&mut self, p: [f32; 2], size: [f64; 2]) {
        self.offset = [
            size[0] / 2.0 - f64::from(p[0]) * self.zoom,
            size[1] / 2.0 - f64::from(p[1]) * self.zoom,
        ];
    }

    /// Showing every point of `points` on a canvas of `size` with [`MARGIN`] round them, never
    /// above 100 %, and still fitting: a single node or none at 100 % in the middle.
    pub fn fit(points: &[[f32; 2]], size: [f64; 2]) -> Camera {
        let mut camera = Camera::default();
        let Some(&first) = points.first() else {
            camera.centre([0.0; 2], size);
            return camera;
        };
        let (mut min, mut max) = (first, first);
        for p in points {
            min = [min[0].min(p[0]), min[1].min(p[1])];
            max = [max[0].max(p[0]), max[1].max(p[1])];
        }
        let room = |axis: usize| (size[axis] - 2.0 * MARGIN).max(1.0);
        let span = |axis: usize| f64::from(max[axis] - min[axis]).max(1.0);
        camera.zoom = (room(0) / span(0)).min(room(1) / span(1)).min(1.0);
        camera.centre([(min[0] + max[0]) / 2.0, (min[1] + max[1]) / 2.0], size);
        camera
    }

    /// The camera once a picture drawn at `drawn` lands: a fitting canvas takes the fit the
    /// picture was drawn at, and any other camera stays, so neither a picture fitted before a pan
    /// nor one drawn before a Fit moves the canvas.
    pub fn landed(self, drawn: Camera) -> Camera {
        match self.fitting && drawn.fitting {
            true => drawn,
            false => self,
        }
    }

    /// The node whose disc, `radius(i)` world units or [`MIN_RADIUS`] pixels, holds canvas point
    /// `at` give or take [`SLOP`], the nearest where several do.
    pub fn hit(
        &self,
        at: [f64; 2],
        points: &[[f32; 2]],
        radius: impl Fn(usize) -> f32,
    ) -> Option<usize> {
        let mut best = None::<(usize, f64)>;
        for (i, &p) in points.iter().enumerate() {
            let s = self.to_screen(p);
            let d2 = (s[0] - at[0]).powi(2) + (s[1] - at[1]).powi(2);
            let reach = (f64::from(radius(i)) * self.zoom).max(MIN_RADIUS) + SLOP;
            if d2 <= reach * reach && best.is_none_or(|(_, b)| d2 < b) {
                best = Some((i, d2));
            }
        }
        best.map(|(i, _)| i)
    }
}

/// Of `candidates`, the nearest to `from` within 45° either side of `arrow`, the lower index
/// where two are as near.
pub(super) fn step(
    from: [f32; 2],
    arrow: Arrow,
    candidates: impl IntoIterator<Item = (usize, [f32; 2])>,
) -> Option<usize> {
    let way = arrow.vector();
    let mut best = None::<(usize, f32)>;
    for (i, p) in candidates {
        let d = [p[0] - from[0], p[1] - from[1]];
        let along = d[0] * way[0] + d[1] * way[1];
        let across = (d[0] * way[1] - d[1] * way[0]).abs();
        let d2 = d[0] * d[0] + d[1] * d[1];
        let nearer = best.is_none_or(|(b, bd)| d2 < bd || (d2 == bd && i < b));
        if along > 0.0 && across <= along && nearer {
            best = Some((i, d2));
        }
    }
    best.map(|(i, _)| i)
}

/// The side of an [`Occupied`] cell, in pixels.
const CELL: f64 = 8.0;

/// Which parts of a canvas the names placed so far cover, in cells of [`CELL`] pixels, so a
/// name's box is checked against a few cells however many names were placed. A box takes every
/// cell it touches, which keeps the names a few pixels apart.
pub(super) struct Occupied {
    /// Where the cells start, a cell or less before the canvas's corner.
    corner: [f64; 2],
    columns: usize,
    rows: usize,
    cells: Vec<bool>,
}

impl Occupied {
    /// A canvas of `size` with nothing on it, its cells laid from `origin`, where the world's
    /// origin is, so they move with a pan and the same names meet before and after it.
    pub fn new(size: [f64; 2], origin: [f64; 2]) -> Occupied {
        let corner = origin.map(|o| o.rem_euclid(CELL) - CELL);
        let [columns, rows] = size.map(|side| (side / CELL).ceil() as usize + 1);
        Occupied {
            corner,
            columns,
            rows,
            cells: vec![false; columns * rows],
        }
    }

    /// The cells box `[left, top, right, bottom]` touches, none for a box off the canvas.
    fn cells(&self, b: [f64; 4]) -> Option<(std::ops::Range<usize>, std::ops::Range<usize>)> {
        let span = |from: f64, to: f64, axis: usize, n: usize| {
            let first = ((from - self.corner[axis]) / CELL).floor().max(0.0) as usize;
            let last = (((to - self.corner[axis]) / CELL).ceil().max(0.0) as usize).min(n);
            (first < last).then_some(first..last)
        };
        let xs = span(b[0], b[2], 0, self.columns)?;
        Some((xs, span(b[1], b[3], 1, self.rows)?))
    }

    /// Whether box `b` reaches the canvas.
    pub fn shows(&self, b: [f64; 4]) -> bool {
        self.cells(b).is_some()
    }

    /// Whether box `b` is on the canvas and meets no box taken.
    pub fn fits(&self, b: [f64; 4]) -> bool {
        self.cells(b).is_some_and(|(xs, ys)| {
            ys.into_iter()
                .all(|y| !self.cells[y * self.columns..][xs.clone()].contains(&true))
        })
    }

    pub fn take(&mut self, b: [f64; 4]) {
        if let Some((xs, ys)) = self.cells(b) {
            for y in ys {
                self.cells[y * self.columns..][xs.clone()].fill(true);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_point_goes_to_the_canvas_and_back() {
        let camera = Camera {
            offset: [100.0, 50.0],
            zoom: 2.0,
            fitting: false,
        };
        assert_eq!(camera.to_screen([10.0, -5.0]), [120.0, 40.0]);
        assert_eq!(camera.to_world([120.0, 40.0]), [10.0, -5.0]);
    }

    /// The world point under the pointer stays under it, and the reader has taken over.
    #[test]
    fn a_zoom_keeps_the_point_under_the_pointer() {
        let mut camera = Camera::default();
        let at = [300.0, 200.0];
        let under = camera.to_world(at);
        camera.zoom_at(2.5, at);
        assert_eq!(camera.to_world(at), under);
        assert_eq!(camera.zoom, 2.5);
        assert!(!camera.fitting);
    }

    #[test]
    fn a_fit_shows_every_point_and_never_enlarges() {
        let size = [800.0, 600.0];
        let points = [[-1000.0, -10.0], [1000.0, 10.0]];
        let camera = Camera::fit(&points, size);
        assert!(camera.fitting);
        for p in points {
            let s = camera.to_screen(p);
            assert!((MARGIN - 1e-6..=size[0] - MARGIN + 1e-6).contains(&s[0]));
            assert!((0.0..=size[1]).contains(&s[1]));
        }
        // Two nodes a link apart are shown at 100 %, in the middle.
        let near = Camera::fit(&[[0.0, 0.0], [30.0, 0.0]], size);
        assert_eq!(near.zoom, 1.0);
        assert_eq!(near.to_screen([15.0, 0.0]), [400.0, 300.0]);
        assert_eq!(Camera::fit(&[], size).to_screen([0.0; 2]), [400.0, 300.0]);
    }

    /// A Fit stays put whatever picture lands next: a fitting canvas takes a fitted picture's
    /// camera, keeps its own fit under one drawn before the Fit, and a camera the reader moved
    /// takes neither.
    #[test]
    fn a_landed_picture_moves_only_a_fitting_canvas_to_its_fit() {
        let size = [800.0, 600.0];
        let fit = Camera::fit(&[[-100.0, 0.0], [300.0, 50.0]], size);
        let fitted = Camera::fit(&[[-120.0, 0.0], [320.0, 60.0]], size);
        let mut panned = fit;
        panned.pan([40.0, 0.0]);
        assert_eq!(fit.landed(fitted), fitted);
        assert_eq!(fit.landed(panned), fit);
        assert_eq!(panned.landed(fitted), panned);
    }

    /// A box meeting one taken is refused, one beside it is not, and one off the canvas never
    /// fits. Two boxes a few pixels apart meet or not alike wherever a pan puts them.
    #[test]
    fn a_name_is_placed_only_where_none_is() {
        let mut taken = Occupied::new([200.0, 100.0], [0.0; 2]);
        assert!(taken.fits([10.0, 10.0, 90.0, 26.0]));
        taken.take([10.0, 10.0, 90.0, 26.0]);
        assert!(!taken.fits([80.0, 20.0, 150.0, 36.0]));
        assert!(taken.fits([100.0, 10.0, 180.0, 26.0]));
        assert!(taken.fits([10.0, 40.0, 90.0, 56.0]));
        assert!(!taken.fits([210.0, 10.0, 290.0, 26.0]));
        assert!(!taken.fits([-90.0, 10.0, -10.0, 26.0]));
        let meets = |pan: f64| {
            let mut taken = Occupied::new([200.0, 100.0], [pan, 0.0]);
            taken.take([10.0 + pan, 10.0, 50.0 + pan, 26.0]);
            taken.fits([53.0 + pan, 10.0, 90.0 + pan, 26.0])
        };
        assert!((1..16).all(|pan| meets(f64::from(pan)) == meets(0.0)));
    }

    /// A press finds the nearest node whose disc it is on, a few pixels' slop included, and a
    /// node zoomed far out is still found at its smallest size.
    #[test]
    fn a_press_hits_the_nearest_disc() {
        let camera = Camera::default();
        let points = [[0.0, 0.0], [20.0, 0.0], [100.0, 0.0]];
        let radius = |_| 6.0;
        assert_eq!(camera.hit([2.0, 0.0], &points, radius), Some(0));
        assert_eq!(camera.hit([12.0, 0.0], &points, radius), Some(1));
        assert_eq!(camera.hit([60.0, 0.0], &points, radius), None);
        let far = Camera {
            zoom: 0.01,
            ..camera
        };
        assert_eq!(far.hit([4.0, 0.0], &[[0.0, 0.0]], radius), Some(0));
    }

    /// An arrow goes to the nearest node in its quarter, never to one beside or behind.
    #[test]
    fn an_arrow_steps_to_the_nearest_node_that_way() {
        let nodes = [
            (0, [10.0, 0.0]),
            (1, [30.0, 5.0]),
            (2, [5.0, 20.0]),
            (3, [-10.0, 0.0]),
        ];
        assert_eq!(step([0.0; 2], Arrow::Right, nodes), Some(0));
        assert_eq!(step([0.0; 2], Arrow::Left, nodes), Some(3));
        assert_eq!(step([0.0; 2], Arrow::Down, nodes), Some(2));
        assert_eq!(step([0.0; 2], Arrow::Up, nodes), None);
    }
}
