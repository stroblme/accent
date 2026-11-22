//! The graph's own drawer (user decision 2026-11-21): the links and nodes antialiased into
//! premultiplied RGBA pixels on the layout's thread, for the canvas to show as one picture.
//! A line costs the pixels it covers. Cairo's stroker, whose cost grows with how many edges
//! each row of pixels crosses, took about 100 ms for the 1889 links round the generated vault's
//! best-linked note and over 20 s for the whole vault's 49k (AMCOMP, 2026-11-21).
//!
//! Coordinates are device pixels, `y` down, with a pixel's centre at `+0.5`. Colours are
//! straight RGBA in `0..=1`, as `gdk::RGBA` holds them.

use super::camera::{self, Camera};
use super::model::{self, Paint};
use crate::theme;

/// Straight red, green, blue and alpha.
pub type Rgba = [f32; 4];

/// The theme's colours the graph is drawn in, read on the main thread as each frame is drawn.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Colours {
    /// The canvas's foreground.
    pub ink: Rgba,
    /// The five hues after the accent (`highlight::lane_colour` 1 to 5).
    pub lanes: [Rgba; 5],
    pub accent: Rgba,
}

/// A node lit, and how far everything outside its neighbourhood is dimmed: its alpha.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Light {
    pub node: usize,
    pub dim: f32,
}

/// How the canvas wants the graph drawn.
#[derive(Clone, Debug, PartialEq)]
pub struct View {
    /// Where the world is on the canvas. While it is fitting, the drawer fits it to the places
    /// it draws.
    pub camera: Camera,
    /// The canvas in pixels, and its display's pixels to one of them.
    pub size: [f64; 2],
    pub scale: f64,
    /// The node lit, its links and neighbours over the rest: the one under the pointer, or else,
    /// more mildly, the file in front.
    pub lit: Option<Light>,
    /// The file in front, in the accent with a ring.
    pub front: Option<usize>,
    /// The node the keyboard is on while the canvas has the keyboard, ringed.
    pub current: Option<usize>,
    pub colours: Colours,
}

/// What is drawn: the nodes' places and paints, their neighbours, and the links between them.
pub struct Scene<'a> {
    pub at: &'a [[f32; 2]],
    pub paints: &'a [Paint],
    pub near: &'a [Vec<usize>],
    pub links: &'a [(usize, usize)],
}

pub struct Pixels {
    pub width: usize,
    pub height: usize,
    /// Premultiplied blue, green, red and alpha, `width * 4` bytes a row: cairo's ARGB32 on a
    /// little-endian machine (`gdk::MemoryFormat::B8g8r8a8Premultiplied`), which GSK's cairo
    /// renderer takes as it is.
    pub data: Vec<u8>,
    /// How much of each pixel the lines drawn since the last [`Pixels::paint_lines`] cover, kept
    /// apart so that lines are no darker where they cross, as a stroked path is not, and the
    /// rows they reach.
    lines: Vec<u8>,
    rows: std::ops::Range<usize>,
}

/// The graph in `scene` drawn as `view` asks, at `camera`, which is the view's own unless it is
/// fitting.
pub fn draw(scene: &Scene, view: &View, camera: Camera) -> Pixels {
    let scale = view.scale;
    let mut px = Pixels::new(
        (view.size[0] * scale).ceil() as usize,
        (view.size[1] * scale).ceil() as usize,
    );
    let n = scene.at.len().min(scene.paints.len()).min(scene.near.len());
    let device = |p: [f32; 2]| {
        let s = camera.to_screen(p);
        [(s[0] * scale) as f32, (s[1] * scale) as f32]
    };
    let screen: Vec<[f32; 2]> = scene.at[..n].iter().map(|&p| device(p)).collect();
    let radius = |i: usize| {
        ((f64::from(model::radius(scene.near[i].len())) * camera.zoom).max(camera::MIN_RADIUS)
            * scale) as f32
    };
    let light = view.lit.filter(|l| l.node < n);
    let lit = light.map(|l| l.node);
    let mut near = vec![lit.is_none(); n];
    if let Some(l) = lit {
        near[l] = true;
        for &j in &scene.near[l] {
            near[j] = true;
        }
    }
    let colours = view.colours;
    let ink = colours.ink;
    let hairline = scale as f32;

    let touches = |&(a, b): &(usize, usize)| lit.is_some_and(|l| a == l || b == l);
    for &(a, b) in scene.links.iter().filter(|l| !touches(l)) {
        if a < n && b < n {
            px.line(screen[a], screen[b], hairline);
        }
    }
    px.paint_lines(alpha(ink, theme::GRAPH_EDGE_ALPHA));
    for &(a, b) in scene.links.iter().filter(|l| touches(l)) {
        if a < n && b < n {
            px.line(screen[a], screen[b], hairline);
        }
    }
    px.paint_lines(alpha(ink, theme::GRAPH_FAINT_ALPHA));

    // In the order the canvas's paths were drawn: a note's, the five hues', a faint file's, a
    // tag's square, a ghost's ring, then the dimmed discs and rings outside the lit node's
    // neighbourhood.
    let front = view.front.filter(|&f| f < n);
    let slot = |i: usize| match (scene.paints[i], near[i]) {
        (Paint::Ghost, false) => 10,
        (_, false) => 9,
        (Paint::Ink, _) => 0,
        (Paint::Lane(k), _) => k,
        (Paint::Faint, _) => 6,
        (Paint::Tag, _) => 7,
        (Paint::Ghost, _) => 8,
    };
    let mut order: Vec<usize> = (0..n).filter(|&i| Some(i) != front).collect();
    order.sort_by_key(|&i| slot(i));
    let ring = 1.5 * scale as f32;
    for i in order {
        let (p, r) = (screen[i], radius(i));
        if !px.reaches(p, r + ring) {
            continue;
        }
        let colour = match slot(i) {
            0 => ink,
            k @ 1..=5 => colours.lanes[k - 1],
            6 | 7 => alpha(ink, theme::GRAPH_FAINT_ALPHA),
            8 => alpha(ink, theme::GRAPH_RING_ALPHA),
            _ => alpha(ink, light.map_or(1.0, |l| l.dim)),
        };
        match scene.paints[i] {
            Paint::Tag => px.square(p, r, colour),
            Paint::Ghost => px.ring(p, r, ring, colour),
            _ => px.disc(p, r, colour),
        }
    }
    if let Some(i) = front {
        let (p, r) = (screen[i], radius(i));
        px.disc(p, r, colours.accent);
        px.ring(p, r + 3.0 * scale as f32, ring, colours.accent);
    }
    if let Some(i) = view.current.filter(|&c| c < n) {
        let gap = if Some(i) == front { 6.0 } else { 3.0 };
        let focus = alpha(colours.accent, theme::FOCUS_ALPHA);
        px.ring(
            screen[i],
            radius(i) + gap * scale as f32,
            2.0 * scale as f32,
            focus,
        );
    }
    px
}

/// `colour` at `alpha`, as `theme::at` sets it.
fn alpha(colour: Rgba, alpha: f32) -> Rgba {
    [colour[0], colour[1], colour[2], alpha]
}

impl Pixels {
    pub fn new(width: usize, height: usize) -> Pixels {
        Pixels {
            width,
            height,
            data: vec![0; width * height * 4],
            lines: vec![0; width * height],
            rows: 0..0,
        }
    }

    /// Whether a shape `reach` round `p` touches the pixels.
    fn reaches(&self, p: [f32; 2], reach: f32) -> bool {
        p[0] + reach >= 0.0
            && p[1] + reach >= 0.0
            && p[0] - reach <= self.width as f32
            && p[1] - reach <= self.height as f32
    }

    /// A line from `a` to `b`, `width` pixels across, into the lines' coverage: along its longer
    /// axis a pixel at a time, covering across it the line's extent there.
    pub fn line(&mut self, a: [f32; 2], b: [f32; 2], width: f32) {
        let pad = width + 1.0;
        let far = [self.width as f32 + pad, self.height as f32 + pad];
        let Some((a, b)) = clip(a, b, [-pad, -pad], far) else {
            return;
        };
        let high = (a[1].min(b[1]) - pad).max(0.0) as usize;
        let low = ((a[1].max(b[1]) + pad).max(0.0) as usize).min(self.height);
        self.rows = match self.rows.is_empty() {
            true => high..low,
            false => self.rows.start.min(high)..self.rows.end.max(low),
        };
        let steep = (b[1] - a[1]).abs() > (b[0] - a[0]).abs();
        let (mut a, mut b) = match steep {
            true => ([a[1], a[0]], [b[1], b[0]]),
            false => (a, b),
        };
        if a[0] > b[0] {
            std::mem::swap(&mut a, &mut b);
        }
        let slope = (b[1] - a[1]) / (b[0] - a[0]).max(f32::EPSILON);
        let half = width * (1.0 + slope * slope).sqrt() / 2.0;
        // `u` counts along the line and `v` across it, a step of each `du` and `dv` cells.
        let (along, across, du, dv) = match steep {
            true => (self.height, self.width, self.width, 1),
            false => (self.width, self.height, 1, self.width),
        };
        let first = a[0].max(0.0) as usize;
        let last = (b[0].ceil().min(along as f32)).max(0.0) as usize;
        let mut centre = a[1] + slope * (first as f32 + 0.5 - a[0]);
        for u in first..last {
            let share = ((u + 1) as f32).min(b[0]) - (u as f32).max(a[0]);
            let (top, bottom) = (centre - half, centre + half);
            centre += slope;
            if share <= 0.0 {
                continue;
            }
            let from = top.max(0.0) as usize;
            let to = (bottom.ceil() as usize).min(across);
            let mut i = u * du + from * dv;
            for v in from..to {
                let cover = (bottom.min((v + 1) as f32) - top.max(v as f32)) * share;
                let cell = &mut self.lines[i];
                *cell = (*cell).max((cover.min(1.0) * 255.0 + 0.5) as u8);
                i += dv;
            }
        }
    }

    /// The lines drawn since the last call laid over the pixels in `colour`, and forgotten.
    pub fn paint_lines(&mut self, colour: Rgba) {
        let src = premultiplied(colour);
        let rows = std::mem::replace(&mut self.rows, 0..0);
        let (from, to) = (rows.start * self.width, rows.end * self.width);
        let (pixels, _) = self.data[from * 4..to * 4].as_chunks_mut::<4>();
        for (px, cover) in pixels.iter_mut().zip(&mut self.lines[from..to]) {
            if *cover > 0 {
                blend(px, src, f32::from(*cover) / 255.0);
                *cover = 0;
            }
        }
    }

    /// A disc of radius `r` round `c`.
    pub fn disc(&mut self, c: [f32; 2], r: f32, colour: Rgba) {
        self.shade(c, r + 1.0, colour, |dx, dy| r + 0.5 - dx.hypot(dy));
    }

    /// A circle of radius `r` round `c`, `width` pixels across.
    pub fn ring(&mut self, c: [f32; 2], r: f32, width: f32, colour: Rgba) {
        let half = width / 2.0;
        self.shade(c, r + half + 1.0, colour, |dx, dy| {
            half + 0.5 - (dx.hypot(dy) - r).abs()
        });
    }

    /// A square `2 · half` pixels across round `c`.
    pub fn square(&mut self, c: [f32; 2], half: f32, colour: Rgba) {
        let side = |d: f32| (half + 0.5 - d.abs()).clamp(0.0, 1.0);
        self.shade(c, half + 1.0, colour, |dx, dy| side(dx) * side(dy));
    }

    /// Every pixel within `reach` of `c` covered as `cover` says of its centre's offset from `c`.
    fn shade(&mut self, c: [f32; 2], reach: f32, colour: Rgba, cover: impl Fn(f32, f32) -> f32) {
        let src = premultiplied(colour);
        let span = |c: f32, n: usize| {
            let from = (c - reach).floor().max(0.0) as usize;
            (from, ((c + reach).ceil().max(0.0) as usize).min(n))
        };
        let (x0, x1) = span(c[0], self.width);
        let (y0, y1) = span(c[1], self.height);
        for y in y0..y1 {
            let dy = y as f32 + 0.5 - c[1];
            for x in x0..x1 {
                let k = cover(x as f32 + 0.5 - c[0], dy).min(1.0);
                if k > 0.0 {
                    let i = (y * self.width + x) * 4;
                    blend(&mut self.data[i..i + 4], src, k);
                }
            }
        }
    }
}

/// `colour` premultiplied, in `0..=255`, in the pixels' order.
fn premultiplied([r, g, b, a]: Rgba) -> [f32; 4] {
    [b * a * 255.0, g * a * 255.0, r * a * 255.0, a * 255.0]
}

/// `src`, premultiplied, laid over the pixel `px` at coverage `k`.
fn blend(px: &mut [u8], src: [f32; 4], k: f32) {
    let keep = 1.0 - src[3] * k / 255.0;
    for (d, s) in px.iter_mut().zip(src) {
        *d = (s * k + f32::from(*d) * keep + 0.5) as u8;
    }
}

/// The part of the segment from `a` to `b` inside the box from `min` to `max` (Liang–Barsky).
fn clip(a: [f32; 2], b: [f32; 2], min: [f32; 2], max: [f32; 2]) -> Option<([f32; 2], [f32; 2])> {
    let d = [b[0] - a[0], b[1] - a[1]];
    let (mut t0, mut t1) = (0.0f32, 1.0f32);
    for axis in 0..2 {
        for (p, q) in [
            (-d[axis], a[axis] - min[axis]),
            (d[axis], max[axis] - a[axis]),
        ] {
            if p == 0.0 {
                if q < 0.0 {
                    return None;
                }
                continue;
            }
            let t = q / p;
            match p < 0.0 {
                true => t0 = t0.max(t),
                false => t1 = t1.min(t),
            }
            if t0 > t1 {
                return None;
            }
        }
    }
    let at = |t: f32| [a[0] + t * d[0], a[1] + t * d[1]];
    Some((at(t0), at(t1)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alpha_at(px: &Pixels, x: usize, y: usize) -> u8 {
        px.data[(y * px.width + x) * 4 + 3]
    }

    /// A line a pixel wide along a row of centres covers that row and no other, and a line far
    /// off the pixels draws nothing.
    #[test]
    fn a_line_covers_its_pixels() {
        let mut px = Pixels::new(10, 5);
        px.line([0.0, 2.5], [10.0, 2.5], 1.0);
        px.line([-500.0, -40.0], [-20.0, 900.0], 1.0);
        px.paint_lines([1.0; 4]);
        for x in 0..10 {
            assert_eq!(
                (
                    alpha_at(&px, x, 1),
                    alpha_at(&px, x, 2),
                    alpha_at(&px, x, 3)
                ),
                (0, 255, 0)
            );
        }
        assert!((0..5).all(|y| alpha_at(&px, 0, y) == 0 || y == 2));
    }

    /// A steep line covers each row once, and two lines crossing are no darker where they cross.
    #[test]
    fn lines_cross_without_darkening() {
        let mut px = Pixels::new(9, 9);
        px.line([4.5, 0.0], [4.5, 9.0], 1.0);
        px.line([0.0, 4.5], [9.0, 4.5], 1.0);
        px.paint_lines([1.0, 1.0, 1.0, 0.5]);
        let half = alpha_at(&px, 4, 0);
        assert_eq!(half, 128);
        assert_eq!(alpha_at(&px, 4, 4), half);
        assert_eq!(alpha_at(&px, 0, 4), half);
        assert_eq!(alpha_at(&px, 3, 0), 0);
    }

    /// A disc is solid inside and clear outside, and a ring is clear in its middle.
    #[test]
    fn discs_and_rings() {
        let mut px = Pixels::new(20, 20);
        px.disc([5.0, 5.0], 3.0, [1.0; 4]);
        px.ring([15.0, 15.0], 3.0, 1.5, [1.0; 4]);
        assert_eq!(alpha_at(&px, 4, 4), 255);
        assert_eq!(alpha_at(&px, 9, 5), 0);
        assert_eq!(alpha_at(&px, 15, 15), 0);
        assert!(alpha_at(&px, 18, 15) > 128);
    }
}
