//! The graph's own drawer (user decision 2026-11-21): the links and nodes antialiased into
//! premultiplied RGBA pixels on the layout's thread, for the canvas to show as one picture.
//! A line costs the pixels it covers. Cairo's stroker, whose cost grows with how many edges
//! each row of pixels crosses, took about 100 ms for the 1889 links round the generated vault's
//! best-linked note and over 20 s for the whole vault's 49k (AMCOMP, 2026-11-21).
//!
//! The links are the costly part, and a still layout's pictures share them ([`Links`]): a picture
//! that only lights another node draws that node's links and every node again over the links'
//! coverage, and one that only rings or accents another node draws every node again.
//!
//! Coordinates are device pixels, `y` down, with a pixel's centre at `+0.5`. Colours are
//! straight RGBA in `0..=1`, as `gdk::RGBA` holds them.

use std::ops::Range;

use super::camera::{self, Camera};
use super::model::{self, Paint};
use crate::theme;

/// Straight red, green, blue and alpha.
pub type Rgba = [f32; 4];

/// The side of the cells, in pixels, in which the other links are drawn again where only the lit
/// node's links cover a pixel as fully as every link does.
const CELL: usize = 16;

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

/// What is drawn: the nodes' places and paints, their neighbours, and the links between them,
/// of the nodes shown: one the find bar's query hides is drawn not at all, nor are its links.
pub struct Scene<'a> {
    pub at: &'a [[f32; 2]],
    pub paints: &'a [Paint],
    pub near: &'a [Vec<usize>],
    pub links: &'a [(usize, usize)],
    pub shown: &'a [bool],
}

pub struct Pixels {
    pub width: usize,
    pub height: usize,
    /// Premultiplied blue, green, red and alpha, `width * 4` bytes a row: cairo's ARGB32 on a
    /// little-endian machine (`gdk::MemoryFormat::B8g8r8a8Premultiplied`), which GSK's cairo
    /// renderer takes as it is.
    pub data: Vec<u8>,
}

/// How much of each pixel some lines cover, the most any one of them does, so that lines are no
/// darker where they cross, as a stroked path is not, and the rows they reach.
#[derive(Default)]
struct Coverage {
    data: Vec<u8>,
    width: usize,
    height: usize,
    rows: Range<usize>,
}

/// The coverage of every link drawn at one state of the layout and one camera, which the
/// drawing thread keeps for the next picture, and once the layout has settled how many lines
/// cover each pixel that fully, `counted`. With it the lit node's links (`lit`) and the other
/// links (`rest`), of the node `lit_for` and kept for the next picture lighting it too: drawn
/// afresh, `rest` holds every other link, and drawn over the links kept, only what every link's
/// coverage leaves unknown.
#[derive(Default)]
pub struct Links {
    all: Coverage,
    count: Vec<u8>,
    counted: bool,
    lit: Coverage,
    rest: Coverage,
    lit_for: Option<usize>,
    /// The cells `rest` is drawn in, a row of `width / CELL` rounded up at a time.
    cells: Vec<bool>,
}

/// The graph in `scene` drawn as `view` asks, at `camera`, which is the view's own unless it is
/// fitting. `links` holds every link's coverage, drawn afresh unless `still` says it was drawn
/// for these places, links, camera and canvas, and counted for the pictures to come if the
/// layout has `settled`.
pub fn draw(
    scene: &Scene,
    view: &View,
    camera: Camera,
    links: &mut Links,
    [still, settled]: [bool; 2],
) -> Pixels {
    let drawing = Drawing::new(scene, view, camera);
    let mut px = Pixels::new(drawing.width, drawing.height);
    drawing.links(&mut px, links, [still, settled]);
    drawing.nodes(&mut px);
    px
}

/// A picture under way: where each node is on it and how large, and which are lit.
struct Drawing<'a> {
    scene: &'a Scene<'a>,
    view: &'a View,
    width: usize,
    height: usize,
    n: usize,
    screen: Vec<[f32; 2]>,
    radii: Vec<f32>,
    light: Option<Light>,
    /// Whether each node is the lit one or its neighbour, every node with none lit.
    near: Vec<bool>,
}

impl<'a> Drawing<'a> {
    fn new(scene: &'a Scene<'a>, view: &'a View, camera: Camera) -> Drawing<'a> {
        let scale = view.scale;
        let n = (scene.at.len().min(scene.paints.len()))
            .min(scene.near.len())
            .min(scene.shown.len());
        let screen = (scene.at[..n].iter())
            .map(|&p| {
                let s = camera.to_screen(p);
                [(s[0] * scale) as f32, (s[1] * scale) as f32]
            })
            .collect();
        let radii = (0..n)
            .map(|i| {
                let r = f64::from(model::radius(scene.near[i].len())) * camera.zoom;
                (r.max(camera::MIN_RADIUS) * scale) as f32
            })
            .collect();
        let light = view.lit.filter(|l| l.node < n);
        let mut near = vec![light.is_none(); n];
        if let Some(l) = light {
            near[l.node] = true;
            for &j in &scene.near[l.node] {
                near[j] = true;
            }
        }
        Drawing {
            scene,
            view,
            width: (view.size[0] * scale).ceil() as usize,
            height: (view.size[1] * scale).ceil() as usize,
            n,
            screen,
            radii,
            light,
            near,
        }
    }

    /// Whether a link is drawn, both its nodes shown.
    fn drawn(&self, &(a, b): &(usize, usize)) -> bool {
        a < self.n && b < self.n && self.scene.shown[a] && self.scene.shown[b]
    }

    /// Whether a link is the lit node's.
    fn touches(&self, &(a, b): &(usize, usize)) -> bool {
        self.light.is_some_and(|l| a == l.node || b == l.node)
    }

    /// The links that are drawn, by index, and of them the lit node's or the others.
    fn drawn_links(&self, lit: Option<bool>) -> impl Iterator<Item = usize> {
        let links = self.scene.links;
        (0..links.len()).filter(move |&i| {
            self.drawn(&links[i]) && lit.is_none_or(|lit| self.touches(&links[i]) == lit)
        })
    }

    /// Link `i`'s pixels given to `plot` with how much it covers each, but for the steps of its
    /// line `skip` passes over, and the rows it reaches.
    fn stroke(
        &self,
        i: usize,
        skip: impl Fn([usize; 2], [usize; 2]) -> bool,
        plot: impl FnMut(usize, u8),
    ) -> Range<usize> {
        let (a, b) = self.scene.links[i];
        let size = [self.width, self.height];
        line(
            size,
            self.screen[a],
            self.screen[b],
            self.view.scale as f32,
            skip,
            plot,
        )
    }

    /// The links over the pixels, the other links in the edges' alpha and the lit node's over
    /// them in the faint one, as the canvas's two paths of them were drawn. Drawn afresh, each
    /// link goes into the lit node's links or the others', and every link's coverage is theirs
    /// together, counted too once the layout has `settled`. Over counted links kept from a `still`
    /// picture lighting another node, that node's links alone are drawn. Where they cover a pixel
    /// less than every link does, or another line covers it as fully, the other links cover it as
    /// every link does, and elsewhere on them the other links' coverage is not known, so they are
    /// drawn again in the cells holding such pixels.
    fn links(&self, px: &mut Pixels, links: &mut Links, [still, settled]: [bool; 2]) {
        let (width, height) = (self.width, self.height);
        let never = |_: [usize; 2], _: [usize; 2]| false;
        let Links {
            all,
            count,
            counted,
            lit,
            rest,
            lit_for,
            cells,
        } = links;
        let node = self.light.map(|l| l.node);
        if !still || (*lit_for != node && !*counted) {
            for cover in [&mut *all, &mut *lit, &mut *rest] {
                cover.reset(width, height);
            }
            count.clear();
            count.resize(width * height, 0);
            *counted = settled;
            for i in self.drawn_links(None) {
                let touches = self.touches(&self.scene.links[i]);
                let side = if touches { &mut *lit } else { &mut *rest };
                let plot = |at: usize, c: u8| {
                    side.data[at] = side.data[at].max(c);
                    if !settled {
                        return;
                    }
                    let (k, n) = (&mut all.data[at], &mut count[at]);
                    if c > *k {
                        (*k, *n) = (c, 1);
                    } else if c == *k && c > 0 {
                        *n = n.saturating_add(1);
                    }
                };
                let rows = self.stroke(i, never, plot);
                side.rows = union(&side.rows, &rows);
            }
            all.rows = union(&lit.rows, &rest.rows);
            if !settled {
                let span = all.rows.start * width..all.rows.end * width;
                let sides = lit.data[span.clone()].iter().zip(&rest.data[span.clone()]);
                for (k, (t, r)) in all.data[span].iter_mut().zip(sides) {
                    *k = (*t).max(*r);
                }
            }
            *lit_for = node;
        } else if *lit_for != node {
            // `rest` counts, for now, the lit lines covering a pixel as fully as every link does.
            lit.reset(width, height);
            rest.reset(width, height);
            for i in self.drawn_links(Some(true)) {
                let plot = |at: usize, c: u8| {
                    lit.data[at] = lit.data[at].max(c);
                    if c == all.data[at] && c > 0 {
                        rest.data[at] = rest.data[at].saturating_add(1);
                    }
                };
                let rows = self.stroke(i, never, plot);
                lit.rows = union(&lit.rows, &rows);
            }
            rest.rows = lit.rows.clone();
            let cols = width.div_ceil(CELL);
            cells.clear();
            cells.resize(cols * height.div_ceil(CELL), false);
            // The pixels' box round the cells marked: left, top, right and bottom.
            let mut marked = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
            for y in lit.rows.clone() {
                for x in 0..width {
                    let at = y * width + x;
                    let (t, k) = (lit.data[at], all.data[at]);
                    if t == 0 || t < k {
                        continue;
                    }
                    let another = count[at] > rest.data[at];
                    rest.data[at] = if another { k } else { 0 };
                    if !another {
                        cells[(y / CELL) * cols + x / CELL] = true;
                        let (x, y) = (x as f32, y as f32);
                        marked = [
                            marked[0].min(x),
                            marked[1].min(y),
                            marked[2].max(x + 1.0),
                            marked[3].max(y + 1.0),
                        ];
                    }
                }
            }
            if marked[0] <= marked[2] {
                let cells = &*cells;
                let cell = |[x, y]: [usize; 2]| cells[(y / CELL) * cols + x / CELL];
                let skip = |a: [usize; 2], b: [usize; 2]| !cell(a) && !cell(b);
                let pad = self.view.scale as f32 + 2.0;
                // Pixels of a marked cell are at most a cell from the marked pixels' box.
                let reach = pad + CELL as f32;
                for i in self.drawn_links(Some(false)) {
                    let (a, b) = self.scene.links[i];
                    let (a, b) = (self.screen[a], self.screen[b]);
                    let apart = a[0].max(b[0]) + reach < marked[0]
                        || a[1].max(b[1]) + reach < marked[1]
                        || a[0].min(b[0]) - reach > marked[2]
                        || a[1].min(b[1]) - reach > marked[3];
                    if !apart && crosses([width, height], a, b, pad, cells) {
                        let plot = |at: usize, c: u8| rest.data[at] = rest.data[at].max(c);
                        let rows = self.stroke(i, skip, plot);
                        rest.rows = union(&rest.rows, &rows);
                    }
                }
            }
            *lit_for = node;
        }
        // Over nothing, a coat of the other links depends on their coverage alone.
        let ink = self.view.colours.ink;
        let edge = premultiplied(alpha(ink, theme::GRAPH_EDGE_ALPHA));
        let faint = premultiplied(alpha(ink, theme::GRAPH_FAINT_ALPHA));
        let coat: [[u8; 4]; 256] = std::array::from_fn(|k| {
            let mut px = [0; 4];
            blend(&mut px, edge, k as f32 / 255.0);
            px
        });
        let (from, to) = (all.rows.start * width, all.rows.end * width);
        let (pixels, _) = px.data[from * 4..to * 4].as_chunks_mut::<4>();
        for (at, px) in (from..to).zip(pixels) {
            let (k, t) = (all.data[at], lit.data[at]);
            match t {
                0 if k > 0 => *px = coat[usize::from(k)],
                0 => {}
                t => {
                    let other = if k > t { k } else { rest.data[at] };
                    *px = coat[usize::from(other)];
                    blend(px, faint, f32::from(t) / 255.0);
                }
            }
        }
    }

    /// The nodes over the links, in the order the canvas's paths were drawn: a note's, the five
    /// hues', a faint file's, a tag's square, a ghost's ring, then the dimmed discs and rings
    /// outside the lit node's neighbourhood, then the file in front and the keyboard's ring.
    fn nodes(&self, px: &mut Pixels) {
        let (scene, view) = (self.scene, self.view);
        let scale = view.scale as f32;
        let colours = view.colours;
        let ink = colours.ink;
        let front = view.front.filter(|&f| f < self.n);
        let slot = |i: usize| match (scene.paints[i], self.near[i]) {
            (Paint::Ghost, false) => 10,
            (_, false) => 9,
            (Paint::Ink, _) => 0,
            (Paint::Lane(k), _) => k,
            (Paint::Faint, _) => 6,
            (Paint::Tag, _) => 7,
            (Paint::Ghost, _) => 8,
        };
        let mut order: Vec<usize> = (0..self.n)
            .filter(|&i| scene.shown[i] && Some(i) != front)
            .collect();
        order.sort_by_key(|&i| slot(i));
        let ring = 1.5 * scale;
        for i in order {
            let (p, r) = (self.screen[i], self.radii[i]);
            if !px.reaches(p, r + ring) {
                continue;
            }
            let colour = match slot(i) {
                0 => ink,
                k @ 1..=5 => colours.lanes[k - 1],
                6 | 7 => alpha(ink, theme::GRAPH_FAINT_ALPHA),
                8 => alpha(ink, theme::GRAPH_RING_ALPHA),
                _ => alpha(ink, self.light.map_or(1.0, |l| l.dim)),
            };
            match scene.paints[i] {
                Paint::Tag => px.square(p, r, colour),
                Paint::Ghost => px.ring(p, r, ring, colour),
                _ => px.disc(p, r, colour),
            }
        }
        if let Some(i) = front {
            let (p, r) = (self.screen[i], self.radii[i]);
            px.disc(p, r, colours.accent);
            px.ring(p, r + 3.0 * scale, ring, colours.accent);
        }
        if let Some(i) = view.current.filter(|&c| c < self.n) {
            let gap = if Some(i) == front { 6.0 } else { 3.0 };
            let focus = alpha(colours.accent, theme::FOCUS_ALPHA);
            px.ring(
                self.screen[i],
                self.radii[i] + gap * scale,
                2.0 * scale,
                focus,
            );
        }
    }
}

/// `colour` at `alpha`, as `theme::at` sets it.
fn alpha(colour: Rgba, alpha: f32) -> Rgba {
    [colour[0], colour[1], colour[2], alpha]
}

impl Coverage {
    /// Nothing covered, `width` by `height`.
    fn reset(&mut self, width: usize, height: usize) {
        let rows = std::mem::replace(&mut self.rows, 0..0);
        if (self.width, self.height) == (width, height) {
            self.data[rows.start * width..rows.end * width].fill(0);
            return;
        }
        (self.width, self.height) = (width, height);
        self.data.clear();
        self.data.resize(width * height, 0);
    }
}

/// The rows either range reaches, and those between.
fn union(a: &Range<usize>, b: &Range<usize>) -> Range<usize> {
    match (a.is_empty(), b.is_empty()) {
        (true, _) => b.clone(),
        (_, true) => a.clone(),
        _ => a.start.min(b.start)..a.end.max(b.end),
    }
}

/// A line from `a` to `b`, `width` pixels across, on a canvas of `size` pixels: along its longer
/// axis a pixel at a time, each pixel across it given to `plot` by index with how much of it the
/// line covers there, unless `skip` passes over that step, which it is asked of by the step's two
/// end pixels. The rows it reaches.
fn line(
    size: [usize; 2],
    a: [f32; 2],
    b: [f32; 2],
    width: f32,
    skip: impl Fn([usize; 2], [usize; 2]) -> bool,
    mut plot: impl FnMut(usize, u8),
) -> Range<usize> {
    let pad = width + 1.0;
    let far = [size[0] as f32 + pad, size[1] as f32 + pad];
    let Some((a, b)) = clip(a, b, [-pad, -pad], far) else {
        return 0..0;
    };
    let high = (a[1].min(b[1]) - pad).max(0.0) as usize;
    let low = ((a[1].max(b[1]) + pad).max(0.0) as usize).min(size[1]);
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
        true => (size[1], size[0], size[0], 1),
        false => (size[0], size[1], 1, size[0]),
    };
    let xy = |u: usize, v: usize| if steep { [v, u] } else { [u, v] };
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
        if to <= from || skip(xy(u, from), xy(u, to - 1)) {
            continue;
        }
        let mut i = u * du + from * dv;
        for v in from..to {
            let cover = (bottom.min((v + 1) as f32) - top.max(v as f32)) * share;
            plot(i, (cover.min(1.0) * 255.0 + 0.5) as u8);
            i += dv;
        }
    }
    high..low
}

/// Whether the line from `a` to `b`, on a canvas of `size` pixels, may cover a pixel of a cell
/// `cells` marks: whether a cell within `pad` of it is marked, looked at a cell's length of the
/// line at a time.
fn crosses(size: [usize; 2], a: [f32; 2], b: [f32; 2], pad: f32, cells: &[bool]) -> bool {
    let far = [size[0] as f32 + pad, size[1] as f32 + pad];
    let Some((a, b)) = clip(a, b, [-pad, -pad], far) else {
        return false;
    };
    let cols = size[0].div_ceil(CELL);
    let rows = size[1].div_ceil(CELL);
    let cell = |v: f32, n: usize| ((v.max(0.0) as usize) / CELL).min(n - 1);
    let steps = ((b[0] - a[0]).abs().max((b[1] - a[1]).abs()) / CELL as f32).ceil();
    let steps = steps.max(1.0) as usize;
    let at = |s: usize| {
        let t = s as f32 / steps as f32;
        [a[0] + t * (b[0] - a[0]), a[1] + t * (b[1] - a[1])]
    };
    (0..steps).any(|s| {
        let (p, q) = (at(s), at(s + 1));
        let xs = cell(p[0].min(q[0]) - pad, cols)..=cell(p[0].max(q[0]) + pad, cols);
        let ys = cell(p[1].min(q[1]) - pad, rows)..=cell(p[1].max(q[1]) + pad, rows);
        ys.into_iter()
            .any(|y| xs.clone().any(|x| cells[y * cols + x]))
    })
}

impl Pixels {
    pub fn new(width: usize, height: usize) -> Pixels {
        Pixels {
            width,
            height,
            data: vec![0; width * height * 4],
        }
    }

    /// Whether a shape `reach` round `p` touches the pixels.
    fn reaches(&self, p: [f32; 2], reach: f32) -> bool {
        p[0] + reach >= 0.0
            && p[1] + reach >= 0.0
            && p[0] - reach <= self.width as f32
            && p[1] - reach <= self.height as f32
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

    /// `cover` laid over `px` in `colour`, as the canvas's path of those lines was.
    fn paint(px: &mut Pixels, cover: &Coverage, colour: Rgba) {
        let src = premultiplied(colour);
        let (pixels, _) = px.data.as_chunks_mut::<4>();
        for (px, &k) in pixels.iter_mut().zip(&cover.data) {
            if k > 0 {
                blend(px, src, f32::from(k) / 255.0);
            }
        }
    }

    fn lines(width: usize, height: usize, lines: &[([f32; 2], [f32; 2])]) -> Pixels {
        let mut cover = Coverage::default();
        cover.reset(width, height);
        for &(a, b) in lines {
            let plot = |i: usize, c: u8| cover.data[i] = cover.data[i].max(c);
            line([width, height], a, b, 1.0, |_, _| false, plot);
        }
        let mut px = Pixels::new(width, height);
        paint(&mut px, &cover, [1.0, 1.0, 1.0, 0.5]);
        px
    }

    /// A line a pixel wide along a row of centres covers that row and no other, and a line far
    /// off the pixels draws nothing.
    #[test]
    fn a_line_covers_its_pixels() {
        let px = lines(
            10,
            5,
            &[([0.0, 2.5], [10.0, 2.5]), ([-500.0, -40.0], [-20.0, 900.0])],
        );
        for x in 0..10 {
            assert_eq!(
                (
                    alpha_at(&px, x, 1),
                    alpha_at(&px, x, 2),
                    alpha_at(&px, x, 3)
                ),
                (0, 128, 0)
            );
        }
        assert!((0..5).all(|y| alpha_at(&px, 0, y) == 0 || y == 2));
    }

    /// A steep line covers each row once, and two lines crossing are no darker where they cross.
    #[test]
    fn lines_cross_without_darkening() {
        let px = lines(9, 9, &[([4.5, 0.0], [4.5, 9.0]), ([0.0, 4.5], [9.0, 4.5])]);
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

    /// A picture drawn over the links kept from the picture before is the one drawn with the
    /// links as the canvas's two paths, the lit node's over the others, whichever node is lit:
    /// one whose links cross others, none, and a hub linked to half the nodes. A crowded graph on
    /// a small canvas, so that lit and other links meet as fully at many pixels.
    #[test]
    fn a_still_layout_draws_as_it_would_afresh() {
        let mut seed = 7u32;
        let mut next = move || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) as f32 / (1 << 24) as f32
        };
        let n = 60;
        let at: Vec<[f32; 2]> = (0..n)
            .map(|_| [next() * 160.0 - 80.0, next() * 120.0 - 60.0])
            .collect();
        let mut links: Vec<(usize, usize)> =
            (1..n).filter(|_| next() < 0.5).map(|i| (0, i)).collect();
        let pick = |r: f32| (r * n as f32) as usize % n;
        links.extend((0..200).map(|_| (pick(next()), pick(next()))));
        links.retain(|&(a, b)| a != b);
        let mut near = vec![Vec::new(); n];
        for &(a, b) in &links {
            near[a].push(b);
            near[b].push(a);
        }
        let paints: Vec<Paint> = (0..n)
            .map(|i| [Paint::Ink, Paint::Lane(2), Paint::Ghost][i % 3])
            .collect();
        let shown: Vec<bool> = (0..n).map(|i| i % 11 != 5).collect();
        let scene = Scene {
            at: &at,
            paints: &paints,
            near: &near,
            links: &links,
            shown: &shown,
        };
        let view = |lit: Option<usize>| View {
            camera: Camera::default(),
            size: [90.0, 70.0],
            scale: 2.0,
            lit: lit.map(|node| Light { node, dim: 0.35 }),
            front: Some(3),
            current: lit,
            colours: Colours {
                ink: [0.1, 0.2, 0.3, 1.0],
                lanes: [[0.8, 0.1, 0.1, 1.0]; 5],
                accent: [0.0, 0.0, 1.0, 1.0],
            },
        };
        let camera = Camera::fit(camera::shown(&at, &shown), [90.0, 70.0]);
        let as_paths = |view: &View| {
            let drawing = Drawing::new(&scene, view, camera);
            let mut px = Pixels::new(drawing.width, drawing.height);
            for (lit, a) in [
                (false, theme::GRAPH_EDGE_ALPHA),
                (true, theme::GRAPH_FAINT_ALPHA),
            ] {
                let mut cover = Coverage::default();
                cover.reset(px.width, px.height);
                for i in drawing.drawn_links(Some(lit)) {
                    let plot = |at: usize, c: u8| cover.data[at] = cover.data[at].max(c);
                    drawing.stroke(i, |_, _| false, plot);
                }
                paint(&mut px, &cover, alpha(view.colours.ink, a));
            }
            drawing.nodes(&mut px);
            px.data
        };
        // Over links kept from a settled layout, counted, and from a moving one, not.
        for settled in [true, false] {
            let mut kept = Links::default();
            draw(&scene, &view(Some(9)), camera, &mut kept, [false, settled]);
            // The hub twice, the second time over its own links kept.
            for lit in [Some(17), None, Some(0), Some(0), Some(17)] {
                let view = view(lit);
                let still = draw(&scene, &view, camera, &mut kept, [true, settled]).data;
                assert!(still == as_paths(&view), "lit {lit:?}, settled {settled}");
            }
        }
    }
}
