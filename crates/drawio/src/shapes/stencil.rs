// Derived from draw.io mxgraph/src/shape/mxStencil.js, mxgraph/src/util/mxUtils.js, mxgraph/src/util/mxAbstractCanvas2D.js, mxgraph/src/util/mxSvgCanvas2D.js, mxgraph/src/view/mxCellState.js and js/grapheditor/Graph.js (Apache-2.0, Copyright (c) 2006-2026 JGraph Holdings Ltd / draw.io AG), ported to Rust and modified for accent; see crates/drawio/NOTICE.
//! Stencils: shapes a small XML program draws (`mxStencil`), which is how draw.io keeps a custom
//! shape. A cell carries one in its style, deflated, as `shape=stencil(…)` (a freehand drawing,
//! an edited shape, an imported one), or names one of draw.io's library shapes
//! (`mxgraph.aws4.lambda`), which [`library`](super::library) finds where the packs are installed.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, PoisonError};

use super::{Fill, RECTANGLE_ROUNDING_FACTOR, add_points, ellipse, library, mxgraph, rect};
use crate::geom::{PathCmd, Point, Rect};
use crate::model::Element;
use crate::route::Constraint;
use crate::scene::{Align, Cap, DEFAULT_FONTFAMILY, DEFAULT_FONTSIZE, Font, Join, Stroke, VAlign};
use crate::style::{Color, Resolved, parse_num};

/// A shape as its XML describes it, ready to draw at any size.
#[derive(Debug, Clone, PartialEq)]
pub struct Stencil {
    /// The size its coordinates are measured in.
    w0: f64,
    h0: f64,
    /// `aspect="fixed"`: drawn at its own proportions in the middle of the cell, where any other
    /// is stretched over it.
    fixed: bool,
    /// Its line width at its own size, `None` to take the cell's (`strokewidth="inherit"`).
    stroke_width: Option<f64>,
    constraints: Vec<Constraint>,
    background: Vec<Op>,
    foreground: Vec<Op>,
}

/// One thing a stencil draws, facing east in the bounds it was drawn into; the scene mirrors and
/// turns it into place.
#[derive(Debug, Clone, PartialEq)]
pub enum Piece {
    Path {
        path: Vec<PathCmd>,
        fill: Fill,
        stroke: Option<Stroke>,
        opacity: f64,
    },
    /// Plain text set at a point and turned `rotation` degrees about it.
    Text {
        at: Point,
        text: String,
        align: Align,
        valign: VAlign,
        rotation: f64,
        font: Font,
        opacity: f64,
    },
    /// A picture stretched over `rect`, mirrored across its own middle.
    Image {
        rect: Rect,
        src: String,
        flip_h: bool,
        flip_v: bool,
        opacity: f64,
    },
}

/// A step of a stencil's program (`mxStencil.drawNode`).
#[derive(Debug, Clone, PartialEq)]
enum Op {
    Save,
    Restore,
    /// An outline, and the corner size of one of straight lines with `rounded="1"`.
    Path(Vec<Seg>, Option<f64>),
    Rect(Rect),
    /// A rectangle whose corners are this percentage of its shorter side.
    RoundRect(Rect, f64),
    Ellipse(Rect),
    Image {
        rect: Rect,
        src: String,
        flip_h: bool,
        flip_v: bool,
    },
    Text {
        at: Point,
        text: String,
        align: Align,
        valign: VAlign,
        rotation: f64,
        /// `align-shape="0"`: it keeps its own angle when the cell is turned.
        upright: bool,
    },
    /// Another library shape drawn into `rect`.
    Include(String, Rect),
    /// Paint the outline begun last.
    Paint {
        fill: bool,
        stroke: bool,
    },
    StrokeWidth {
        width: f64,
        fixed: bool,
    },
    Dashed(bool),
    DashPattern(Vec<f64>),
    StrokeColor(Hue),
    FillColor(Hue),
    FontColor(Hue),
    Cap(Cap),
    Join(Corner),
    MiterLimit(f64),
    Alpha(f64),
    FontStyle(u32),
    FontFamily(String),
    FontSize(f64),
}

/// A piece of an outline, in the stencil's own coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Seg {
    Move(Point),
    Line(Point),
    Quad(Point, Point),
    Curve(Point, Point, Point),
    /// An SVG elliptical arc to `to`.
    Arc {
        radii: Point,
        angle: f64,
        large: bool,
        sweep: bool,
        to: Point,
    },
    Close,
}

/// How a stroke's corners are joined, before the miter limit is known.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Corner {
    Miter,
    Round,
    Bevel,
}

/// A colour as a stencil names it: a CSS colour, the cell's `stroke`, `fill` or `font` colour,
/// `default`, or a key of the cell's style with the colour to use where the style has none.
#[derive(Debug, Clone, PartialEq)]
struct Hue {
    name: String,
    default: Option<String>,
}

/// Includes nested deeper than this are not drawn, which ends a shape that includes itself.
const MAX_INCLUDES: usize = 8;

impl Stencil {
    /// The stencil a cell's `shape` is: one written into the style, or a library shape whose pack
    /// is installed. `None` for every other shape and for a stencil that cannot be read.
    pub fn of(shape: &str) -> Option<Arc<Stencil>> {
        let packed = shape.strip_prefix("stencil(");
        if packed.is_none() && !shape.starts_with("mxgraph.") {
            return None;
        }
        // Read once, a miss too: a scene asks again for every cell at every paint.
        // ponytail: emptied when full rather than evicting the oldest; an LRU if a session ever
        // holds this many shapes at once.
        static CACHE: LazyLock<Mutex<HashMap<String, Option<Arc<Stencil>>>>> =
            LazyLock::new(Mutex::default);
        const ROOM: usize = 4096;
        let mut cache = CACHE.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(known) = cache.get(shape) {
            return known.clone();
        }
        let xml = match packed {
            Some(packed) => {
                // `Graph.js` cuts the last character off, whatever it is.
                let mut packed = packed.chars();
                packed.next_back();
                crate::xml::decompress(packed.as_str()).ok()
            }
            None => library::source(shape),
        };
        let stencil = xml.and_then(|xml| Stencil::parse(&xml)).map(Arc::new);
        if cache.len() >= ROOM {
            cache.clear();
        }
        cache.insert(shape.to_string(), stencil.clone());
        stencil
    }

    /// Read a `<shape>` element (`mxStencil.parseDescription`, `parseConstraints`). `None` for
    /// XML that is broken or is some other element.
    pub fn parse(xml: &str) -> Option<Stencil> {
        let shape = crate::xml::tree(xml).ok()?;
        if shape.name != "shape" {
            return None;
        }
        let child = |name: &str| shape.children.iter().find(|c| c.name == name);
        let ops = |name: &str| -> Vec<Op> {
            child(name)
                .map(|node| node.children.iter().filter_map(op).collect())
                .unwrap_or_default()
        };
        // Sizes default to 100, and a size that is none would scale the shape to nothing.
        let size = |name: &str| {
            let n = shape.attr(name).and_then(parse_num).unwrap_or(100.0);
            if n > 0.0 { n } else { 100.0 }
        };
        Some(Stencil {
            w0: size("w"),
            h0: size("h"),
            fixed: shape.attr("aspect") == Some("fixed"),
            stroke_width: match shape.attr("strokewidth") {
                Some("inherit") => None,
                width => Some(width.and_then(parse_num).unwrap_or(1.0)),
            },
            constraints: child("connections")
                .map(|node| node.children.iter().map(constraint).collect())
                .unwrap_or_default(),
            background: ops("background"),
            foreground: ops("foreground"),
        })
    }

    /// Its own connection points (`<connections>`), none where it names none.
    pub(crate) fn constraints(&self) -> &[Constraint] {
        &self.constraints
    }

    /// The box of `bounds` edges meet and route around: what a fixed-aspect stencil fills of it,
    /// `vertical` for a cell facing north or south, and all of it for one that is stretched
    /// (`mxCellState.getPerimeterBounds`, `mxGraphView.updateBoundsFromStencil`).
    pub(crate) fn perimeter_bounds(&self, bounds: Rect, vertical: bool) -> Rect {
        if !self.fixed {
            return bounds;
        }
        let (w, h) = match vertical {
            true => (self.h0, self.w0),
            false => (self.w0, self.h0),
        };
        let s = (bounds.w / w).min(bounds.h / h);
        Rect::new(
            bounds.x + (bounds.w - w * s) / 2.0,
            bounds.y + (bounds.h - h * s) / 2.0,
            w * s,
            h * s,
        )
    }

    /// What it draws into `bounds` for a cell styled `style`, background first
    /// (`mxStencil.drawShape` after `mxShape.configureCanvas`).
    pub fn draw(&self, bounds: Rect, style: &Resolved) -> Vec<Piece> {
        let mut canvas = Canvas::new(style);
        self.draw_into(&mut canvas, bounds, 0);
        canvas.pieces
    }

    fn draw_into(&self, canvas: &mut Canvas, bounds: Rect, depth: usize) {
        let frame = self.frame(bounds);
        canvas.state.width = match self.stroke_width {
            Some(width) => width * frame.min,
            None => canvas.style.num("strokeWidth", 1.0),
        };
        if bounds.w > 0.0 && bounds.h > 0.0 {
            let saved = canvas.saved.len();
            for op in self.background.iter().chain(&self.foreground) {
                canvas.run(op, &frame, depth);
            }
            // A stencil that saves more than it restores does not leak into the next.
            canvas.saved.truncate(saved);
        }
    }

    /// Where its coordinates land in `bounds` (`mxStencil.computeAspect`; the scene has already
    /// turned the bounds of a cell facing north or south).
    fn frame(&self, bounds: Rect) -> Frame {
        let (mut sx, mut sy) = (bounds.w / self.w0, bounds.h / self.h0);
        let (mut x0, mut y0) = (bounds.x, bounds.y);
        if self.fixed {
            let s = sx.min(sy);
            (sx, sy) = (s, s);
            x0 += (bounds.w - self.w0 * s) / 2.0;
            y0 += (bounds.h - self.h0 * s) / 2.0;
        }
        Frame {
            x0,
            y0,
            sx,
            sy,
            min: sx.min(sy),
        }
    }
}

/// The stencil's coordinates on the page: an offset and a scale each way.
#[derive(Debug, Clone, Copy)]
struct Frame {
    x0: f64,
    y0: f64,
    sx: f64,
    sy: f64,
    /// The smaller scale, which line widths, dashes and font sizes follow.
    min: f64,
}

impl Frame {
    fn at(&self, p: Point) -> Point {
        Point::new(self.x0 + p.x * self.sx, self.y0 + p.y * self.sy)
    }

    fn rect(&self, r: Rect) -> Rect {
        Rect::new(
            self.x0 + r.x * self.sx,
            self.y0 + r.y * self.sy,
            r.w * self.sx,
            r.h * self.sy,
        )
    }
}

/// What the canvas paints with (`mxAbstractCanvas2D.createState`, the part a stencil can set).
#[derive(Debug, Clone)]
struct State {
    fill: Fill,
    stroke: Option<Color>,
    width: f64,
    dashed: bool,
    /// Dashes in page units rather than multiples of the width.
    fix_dash: bool,
    pattern: Vec<f64>,
    cap: Cap,
    corner: Corner,
    miter_limit: f64,
    alpha: f64,
    font: Font,
}

struct Canvas<'a> {
    style: &'a Resolved,
    state: State,
    saved: Vec<State>,
    /// The outline begun last and the piece it was painted as. Painting it again repaints that
    /// piece, as the SVG canvas rewrites the one node it made for the outline.
    node: Vec<PathCmd>,
    painted: Option<usize>,
    pieces: Vec<Piece>,
}

impl<'a> Canvas<'a> {
    /// A canvas as `mxShape.configureCanvas` leaves it for the cell: its fill, stroke, dashes and
    /// opacity, and the canvas's own font.
    fn new(style: &'a Resolved) -> Canvas<'a> {
        let pattern = style.get("dashPattern").unwrap_or("3 3");
        Canvas {
            style,
            state: State {
                fill: Fill::Cell,
                stroke: style.color("strokeColor"),
                width: 1.0,
                dashed: style.flag("dashed", false),
                fix_dash: style.flag("fixDash", false),
                pattern: numbers(pattern),
                cap: Cap::Butt,
                corner: Corner::Miter,
                miter_limit: 10.0,
                alpha: style.num("opacity", 100.0) / 100.0,
                font: Font {
                    size: DEFAULT_FONTSIZE,
                    family: DEFAULT_FONTFAMILY.to_string(),
                    color: Color::BLACK,
                    bold: false,
                    italic: false,
                    underline: false,
                },
            },
            saved: Vec::new(),
            node: Vec::new(),
            painted: None,
            pieces: Vec::new(),
        }
    }

    fn run(&mut self, op: &Op, frame: &Frame, depth: usize) {
        match op {
            Op::Save => self.saved.push(self.state.clone()),
            Op::Restore => {
                if let Some(state) = self.saved.pop() {
                    self.state = state;
                }
            }
            Op::Path(segs, arc) => {
                let rounded = arc.and_then(|arc| rounded(segs, arc, frame));
                self.begin(rounded.unwrap_or_else(|| outline(segs, frame)));
            }
            Op::Rect(r) => self.begin(rect(frame.rect(*r))),
            Op::RoundRect(r, arc) => {
                let r = frame.rect(*r);
                let factor = match *arc {
                    0.0 => RECTANGLE_ROUNDING_FACTOR,
                    arc => arc / 100.0,
                };
                self.begin(mxgraph::round_rect(r, (r.w * factor).min(r.h * factor)));
            }
            Op::Ellipse(r) => self.begin(ellipse(frame.rect(*r))),
            Op::Image {
                rect,
                src,
                flip_h,
                flip_v,
            } => self.pieces.push(Piece::Image {
                rect: frame.rect(*rect),
                src: src.clone(),
                flip_h: *flip_h,
                flip_v: *flip_v,
                opacity: self.state.alpha,
            }),
            Op::Text {
                at,
                text,
                align,
                valign,
                rotation,
                upright,
            } => {
                // Against the cell's own turn, which a single flip reverses.
                let turn = self.style.num("rotation", 0.0);
                let flipped = self.style.flag("flipH", false) != self.style.flag("flipV", false);
                let against = match (upright, flipped) {
                    (false, _) => 0.0,
                    (true, false) => -turn,
                    (true, true) => turn,
                };
                self.pieces.push(Piece::Text {
                    at: frame.at(*at),
                    text: text.clone(),
                    align: *align,
                    valign: *valign,
                    rotation: rotation + against,
                    font: self.state.font.clone(),
                    opacity: self.state.alpha,
                });
            }
            Op::Include(name, r) => {
                if depth < MAX_INCLUDES
                    && let Some(stencil) = Stencil::of(name)
                {
                    stencil.draw_into(self, frame.rect(*r), depth + 1);
                }
            }
            Op::Paint { fill, stroke } => self.paint(*fill, *stroke),
            Op::StrokeWidth { width, fixed } => {
                self.state.width = width * if *fixed { 1.0 } else { frame.min };
            }
            Op::Dashed(dashed) => {
                // `setDashed` is called without `fixDash`, which turns it off.
                self.state.dashed = *dashed;
                self.state.fix_dash = false;
            }
            Op::DashPattern(pattern) => {
                self.state.pattern = pattern.iter().map(|v| v * frame.min).collect();
            }
            Op::StrokeColor(hue) => self.state.stroke = hue.colour(self.style, Color::BLACK),
            Op::FillColor(hue) => {
                self.state.fill = hue
                    .colour(self.style, Color::WHITE)
                    .map_or(Fill::None, Fill::Own);
            }
            Op::FontColor(hue) => {
                self.state.font.color =
                    hue.colour(self.style, Color::BLACK).unwrap_or(Color::BLACK);
            }
            Op::Cap(cap) => self.state.cap = *cap,
            Op::Join(corner) => self.state.corner = *corner,
            Op::MiterLimit(limit) => self.state.miter_limit = *limit,
            Op::Alpha(alpha) => self.state.alpha = *alpha,
            Op::FontStyle(bits) => {
                self.state.font.bold = bits & 1 != 0;
                self.state.font.italic = bits & 2 != 0;
                self.state.font.underline = bits & 4 != 0;
            }
            Op::FontFamily(family) => self.state.font.family = family.clone(),
            Op::FontSize(size) => self.state.font.size = size * frame.min,
        }
    }

    fn begin(&mut self, node: Vec<PathCmd>) {
        self.node = node;
        self.painted = None;
    }

    /// Paint the outline begun last (`mxSvgCanvas2D.addNode`).
    fn paint(&mut self, fill: bool, stroke: bool) {
        if self.node.is_empty() {
            return;
        }
        let piece = Piece::Path {
            path: self.node.clone(),
            fill: if fill { self.state.fill } else { Fill::None },
            stroke: if stroke { self.stroke() } else { None },
            opacity: self.state.alpha,
        };
        match self.painted {
            Some(i) => self.pieces[i] = piece,
            None => {
                self.painted = Some(self.pieces.len());
                self.pieces.push(piece);
            }
        }
    }

    /// The stroke as the state has it (`mxSvgCanvas2D.updateStroke`): dashes in multiples of the
    /// width unless they are fixed.
    fn stroke(&self) -> Option<Stroke> {
        let s = &self.state;
        let width = s.width.max(0.0);
        let scale = if s.fix_dash { 1.0 } else { width };
        Some(Stroke {
            color: s
                .stroke?
                .fade(self.style.num("strokeOpacity", 100.0) / 100.0),
            width,
            dash: s
                .dashed
                .then(|| s.pattern.iter().map(|v| v * scale).collect()),
            cap: s.cap,
            join: match s.corner {
                Corner::Miter => Join::Miter(s.miter_limit),
                Corner::Round => Join::Round,
                Corner::Bevel => Join::Bevel,
            },
        })
    }
}

impl Hue {
    /// The colour it names for a cell styled `style`, `None` for no paint
    /// (`mxStencil.parseColor`, and `Graph.js`'s override of it). `theme` is what `default`
    /// stands for: white for a fill, black for a line or text.
    // ponytail: a key the style sets to `none` reads as one it does not set, so the node's
    // `default` colour is used where draw.io paints nothing; `Resolved` drops `none`.
    fn colour(&self, style: &Resolved, theme: Color) -> Option<Color> {
        match self.name.as_str() {
            "stroke" => style.color("strokeColor"),
            "fill" => style.color("fillColor"),
            "font" => Some(style.color("fontColor").unwrap_or(Color::BLACK)),
            "default" => Some(theme),
            name => Color::parse(name).or_else(|| {
                let fallback = self.default.as_deref().filter(|d| *d != "none");
                match style.get(name) {
                    Some("default") => fallback.and_then(Color::parse).or(Some(theme)),
                    Some(value) => Color::parse(value),
                    None => fallback.and_then(Color::parse),
                }
            }),
        }
    }
}

/// An outline's segments on the page (`mxStencil.drawNode`'s path elements).
fn outline(segs: &[Seg], frame: &Frame) -> Vec<PathCmd> {
    let mut path = Vec::with_capacity(segs.len());
    // Where the path is, which an arc starts from (`mxAbstractCanvas2D.lastX`).
    let mut last = Point::default();
    for seg in segs {
        match *seg {
            Seg::Move(p) => {
                last = frame.at(p);
                path.push(PathCmd::MoveTo(last));
            }
            Seg::Line(p) => {
                last = frame.at(p);
                path.push(PathCmd::LineTo(last));
            }
            Seg::Quad(c, p) => {
                last = frame.at(p);
                path.push(PathCmd::QuadTo(frame.at(c), last));
            }
            Seg::Curve(a, b, p) => {
                last = frame.at(p);
                path.push(PathCmd::CurveTo(frame.at(a), frame.at(b), last));
            }
            Seg::Arc {
                radii,
                angle,
                large,
                sweep,
                to,
            } => {
                let to = frame.at(to);
                let (rx, ry) = (radii.x * frame.sx, radii.y * frame.sy);
                let curves = arc_to_curves(last, rx, ry, angle, large, sweep, to);
                if !curves.is_empty() {
                    last = to;
                }
                path.extend(curves);
            }
            Seg::Close => path.push(PathCmd::Close),
        }
    }
    path
}

/// A `rounded="1"` path: each run of straight lines with its corners rounded by `arc`, closed
/// where it ends on its start. `None` for a path with anything but moves and lines, which is
/// drawn as it is.
fn rounded(segs: &[Seg], arc: f64, frame: &Frame) -> Option<Vec<PathCmd>> {
    let mut runs: Vec<Vec<Point>> = Vec::new();
    for seg in segs {
        match *seg {
            Seg::Move(p) => runs.push(vec![frame.at(p)]),
            Seg::Line(p) => match runs.last_mut() {
                Some(run) => run.push(frame.at(p)),
                None => runs.push(vec![frame.at(p)]),
            },
            _ => return None,
        }
    }
    if runs.is_empty() {
        return None;
    }
    let mut path = Vec::new();
    for mut run in runs {
        let close = run.first() == run.last();
        if close {
            run.pop();
        }
        path.extend(add_points(&run, true, arc, close));
    }
    Some(path)
}

/// An SVG elliptical arc from `from` to `to` as cubics, a quarter turn each at most
/// (`mxUtils.arcToCurves`). Empty for a radius of zero, which draw.io draws nothing for.
fn arc_to_curves(
    from: Point,
    r1: f64,
    r2: f64,
    angle: f64,
    large: bool,
    sweep: bool,
    to: Point,
) -> Vec<PathCmd> {
    use std::f64::consts::{PI, TAU};
    if r1 == 0.0 || r2 == 0.0 {
        return Vec::new();
    }
    let (x, y) = (to.x - from.x, to.y - from.y);
    let (mut r1, mut r2) = (r1.abs(), r2.abs());
    let (ctx, cty) = (-x / 2.0, -y / 2.0);
    let (spsi, cpsi) = angle.to_radians().sin_cos();
    let rxd = cpsi * ctx + spsi * cty;
    let ryd = -spsi * ctx + cpsi * cty;
    let (rxdd, rydd) = (rxd * rxd, ryd * ryd);
    let (r1x, r2y) = (r1 * r1, r2 * r2);
    let lamda = rxdd / r1x + rydd / r2y;
    let sds = if lamda > 1.0 {
        // Radii too short to span the chord grow until they do.
        r1 *= lamda.sqrt();
        r2 *= lamda.sqrt();
        0.0
    } else {
        let sign = if large == sweep { -1.0 } else { 1.0 };
        let radicand = (r1x * r2y - r1x * rydd - r2y * rxdd) / (r1x * rydd + r2y * rxdd);
        sign * radicand.max(0.0).sqrt()
    };
    let txd = sds * r1 * ryd / r2;
    let tyd = -sds * r2 * rxd / r1;
    let tx = cpsi * txd - spsi * tyd + x / 2.0;
    let ty = spsi * txd + cpsi * tyd + y / 2.0;
    let start = ((ryd - tyd) / r2).atan2((rxd - txd) / r1);
    let mut s1 = if start >= 0.0 { start } else { TAU + start };
    let rad = ((-ryd - tyd) / r2).atan2((-rxd - txd) / r1) - start;
    let mut dr = if rad >= 0.0 { rad } else { TAU + rad };
    if !sweep && dr > 0.0 {
        dr -= TAU;
    } else if sweep && dr < 0.0 {
        dr += TAU;
    }
    let seg = (dr * 2.0 / PI).abs().ceil();
    let segr = dr / seg;
    let t = 8.0 / 3.0 * (segr / 4.0).sin().powi(2) / (segr / 2.0).sin();
    let (cpsir1, cpsir2, spsir1, spsir2) = (cpsi * r1, cpsi * r2, spsi * r1, spsi * r2);
    let (mut ms, mut mc) = s1.sin_cos();
    let mut x2 = -t * (cpsir1 * ms + spsir2 * mc);
    let mut y2 = -t * (spsir1 * ms - cpsir2 * mc);
    let at = |x: f64, y: f64| Point::new(x + from.x, y + from.y);
    let mut curves = Vec::new();
    // A full turn is four segments; a count that is not a number draws none.
    for _ in 0..seg.min(4.0) as usize {
        s1 += segr;
        (ms, mc) = s1.sin_cos();
        let x3 = cpsir1 * mc - spsir2 * ms + tx;
        let y3 = spsir1 * mc + cpsir2 * ms + ty;
        let dx = -t * (cpsir1 * ms + spsir2 * mc);
        let dy = -t * (spsir1 * ms - cpsir2 * mc);
        curves.push(PathCmd::CurveTo(
            at(x2, y2),
            at(x3 - dx, y3 - dy),
            at(x3, y3),
        ));
        (x2, y2) = (x3 + dx, y3 + dy);
    }
    let finite = |p: &Point| p.x.is_finite() && p.y.is_finite();
    let sound = curves.iter().all(|c| match c {
        PathCmd::CurveTo(a, b, p) => finite(a) && finite(b) && finite(p),
        _ => true,
    });
    if sound { curves } else { Vec::new() }
}

/// A number attribute, 0 where it is missing or not a number.
fn num(el: &Element, name: &str) -> f64 {
    el.attr(name).and_then(parse_num).unwrap_or(0.0)
}

/// The numbers of a space-separated list, as a dash pattern is written.
fn numbers(list: &str) -> Vec<f64> {
    list.split_whitespace()
        .filter_map(parse_num)
        .filter(|v| *v >= 0.0)
        .collect()
}

/// A `<constraint>` of `<connections>` (`mxStencil.parseConstraint`).
fn constraint(el: &Element) -> Constraint {
    Constraint {
        point: Point::new(num(el, "x"), num(el, "y")),
        dx: 0.0,
        dy: 0.0,
        perimeter: el.attr("perimeter") == Some("1"),
    }
}

/// A child of `<background>` or `<foreground>` as a step, `None` for an element the language
/// does not have.
// ponytail: text and picture sources a stencil computes in JavaScript (`evaluate`) are not run,
// which draw.io leaves off too (`mxStencil.allowEval`); a `localized` text is drawn as its key.
fn op(el: &Element) -> Option<Op> {
    let n = |name: &str| num(el, name);
    let bounds = || Rect::new(n("x"), n("y"), n("w"), n("h"));
    let on = |name: &str| el.attr(name) == Some("1");
    let hue = || Hue {
        name: el.attr("color").unwrap_or_default().to_string(),
        default: el.attr("default").map(str::to_string),
    };
    Some(match el.name.as_str() {
        "save" => Op::Save,
        "restore" => Op::Restore,
        "path" => Op::Path(
            el.children.iter().filter_map(seg).collect(),
            on("rounded").then(|| n("arcSize")),
        ),
        "rect" => Op::Rect(bounds()),
        "roundrect" => Op::RoundRect(bounds(), n("arcsize")),
        "ellipse" => Op::Ellipse(bounds()),
        "image" => Op::Image {
            rect: bounds(),
            src: el.attr("src")?.to_string(),
            flip_h: on("flipH"),
            flip_v: on("flipV"),
        },
        "text" => Op::Text {
            at: Point::new(n("x"), n("y")),
            text: el.attr("str")?.to_string(),
            align: match el.attr("align") {
                Some("center") => Align::Center,
                Some("right") => Align::Right,
                _ => Align::Left,
            },
            valign: match el.attr("valign") {
                Some("middle") => VAlign::Middle,
                Some("bottom") => VAlign::Bottom,
                _ => VAlign::Top,
            },
            rotation: if on("vertical") { -90.0 } else { 0.0 } - n("rotation"),
            upright: el.attr("align-shape") == Some("0"),
        },
        "include-shape" => Op::Include(el.attr("name")?.to_string(), bounds()),
        "fillstroke" => Op::Paint {
            fill: true,
            stroke: true,
        },
        "fill" => Op::Paint {
            fill: true,
            stroke: false,
        },
        "stroke" => Op::Paint {
            fill: false,
            stroke: true,
        },
        "strokewidth" => Op::StrokeWidth {
            width: n("width"),
            fixed: on("fixed"),
        },
        "dashed" => Op::Dashed(on("dashed")),
        "dashpattern" => Op::DashPattern(numbers(el.attr("pattern")?)),
        "strokecolor" => Op::StrokeColor(hue()),
        "fillcolor" => Op::FillColor(hue()),
        "fontcolor" => Op::FontColor(hue()),
        "linecap" => Op::Cap(match el.attr("cap") {
            Some("round") => Cap::Round,
            Some("square") => Cap::Square,
            _ => Cap::Butt,
        }),
        "linejoin" => Op::Join(match el.attr("join") {
            Some("round") => Corner::Round,
            Some("bevel") => Corner::Bevel,
            _ => Corner::Miter,
        }),
        "miterlimit" => Op::MiterLimit(n("limit")),
        // All three set the canvas's one alpha, as `mxStencil.drawNode` has them.
        "alpha" | "fillalpha" | "strokealpha" => Op::Alpha(n("alpha")),
        "fontstyle" => Op::FontStyle(n("style") as u32),
        "fontfamily" => Op::FontFamily(el.attr("family")?.to_string()),
        "fontsize" => Op::FontSize(n("size")),
        _ => return None,
    })
}

/// A child of `<path>` as a segment.
fn seg(el: &Element) -> Option<Seg> {
    let at = |x: &str, y: &str| Point::new(num(el, x), num(el, y));
    Some(match el.name.as_str() {
        "move" => Seg::Move(at("x", "y")),
        "line" => Seg::Line(at("x", "y")),
        "quad" => Seg::Quad(at("x1", "y1"), at("x2", "y2")),
        "curve" => Seg::Curve(at("x1", "y1"), at("x2", "y2"), at("x3", "y3")),
        "arc" => Seg::Arc {
            radii: at("rx", "ry"),
            angle: num(el, "x-axis-rotation"),
            large: num(el, "large-arc-flag") != 0.0,
            sweep: num(el, "sweep-flag") != 0.0,
            to: at("x", "y"),
        },
        "close" => Seg::Close,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::Style;

    fn style(s: &str) -> Resolved {
        Style::parse(s).resolve(false)
    }

    /// A box with a line across it in a colour and width of its own, twice as wide as high.
    const BOXED: &str = r##"<shape w="20" h="10" aspect="variable" strokewidth="inherit">
        <connections><constraint x="0.5" y="0" perimeter="1" name="N"/></connections>
        <background><rect x="0" y="0" w="20" h="10"/><fillstroke/></background>
        <foreground>
            <save/>
            <strokecolor color="#ff0000"/><strokewidth width="2" fixed="1"/><alpha alpha="0.5"/>
            <path><move x="0" y="5"/><line x="20" y="5"/></path><fill/><stroke/>
            <restore/>
            <fillcolor color="accent" default="#00ff00"/>
            <ellipse x="5" y="0" w="10" h="10"/><fill/>
        </foreground>
    </shape>"##;

    #[test]
    fn a_stencil_draws_its_program_scaled_into_the_bounds() {
        let stencil = Stencil::parse(BOXED).unwrap();
        let cell = style("strokeColor=#0000ff;strokeWidth=3;opacity=80;");
        let pieces = stencil.draw(Rect::new(100.0, 50.0, 40.0, 40.0), &cell);
        let [body, line, dot] = pieces.as_slice() else {
            panic!("three pieces, the line painted twice being one: {pieces:?}");
        };
        // The background takes the cell's paint, its coordinates stretched 2 by 4.
        let Piece::Path {
            path,
            fill,
            stroke,
            opacity,
        } = body
        else {
            panic!("{body:?}")
        };
        assert_eq!(path, &rect(Rect::new(100.0, 50.0, 40.0, 40.0)));
        assert_eq!((*fill, *opacity), (Fill::Cell, 0.8));
        let stroke = stroke.as_ref().unwrap();
        assert_eq!((stroke.color, stroke.width), (Color::rgb(0, 0, 255), 3.0));
        // The line's own colour, fixed width and alpha; stroking it replaced its fill.
        let Piece::Path {
            path,
            fill,
            stroke,
            opacity,
        } = line
        else {
            panic!("{line:?}")
        };
        let p = Point::new;
        assert_eq!(
            path,
            &[
                PathCmd::MoveTo(p(100.0, 70.0)),
                PathCmd::LineTo(p(140.0, 70.0))
            ]
        );
        assert_eq!((*fill, *opacity), (Fill::None, 0.5));
        let stroke = stroke.as_ref().unwrap();
        assert_eq!((stroke.color, stroke.width), (Color::rgb(255, 0, 0), 2.0));
        // Restored, then filled in the style key's fallback colour and nothing stroked.
        let Piece::Path {
            fill,
            stroke,
            opacity,
            ..
        } = dot
        else {
            panic!("{dot:?}")
        };
        assert_eq!(
            (*fill, stroke, *opacity),
            (Fill::Own(Color::rgb(0, 255, 0)), &None, 0.8)
        );
        assert_eq!(stencil.constraints().len(), 1);
        assert!(stencil.constraints()[0].perimeter);
    }

    #[test]
    fn a_fixed_aspect_stencil_keeps_its_proportions_in_the_middle() {
        let fixed = BOXED.replace("variable", "fixed");
        let stencil = Stencil::parse(&fixed).unwrap();
        let bounds = Rect::new(0.0, 0.0, 40.0, 40.0);
        assert_eq!(
            stencil.perimeter_bounds(bounds, false),
            Rect::new(0.0, 10.0, 40.0, 20.0)
        );
        assert_eq!(
            stencil.perimeter_bounds(bounds, true),
            Rect::new(10.0, 0.0, 20.0, 40.0)
        );
        let pieces = stencil.draw(bounds, &style(""));
        let Piece::Path { path, .. } = &pieces[0] else {
            panic!("{pieces:?}")
        };
        assert_eq!(path, &rect(Rect::new(0.0, 10.0, 40.0, 20.0)));
    }

    #[test]
    fn a_style_carries_a_stencil_deflated() {
        // `<shape w="10" h="10"><foreground><rect x="0" y="0" w="10" h="5"/><stroke/></foreground></shape>`
        // as `Graph.compress` writes it.
        let shape = "stencil(UzV2Ls5ILEhVNTIoVzV2UTUyMjQAEkBuBjLX2FXV2Dktvyg1vSi/NC8Fwi9KTS4BKqyAKIRqq0ThYTfTFMxzg5hSXFKUn50K5wIZ6PYAhSBuNHYFAA==)";
        let stencil = Stencil::of(shape).unwrap();
        let pieces = stencil.draw(Rect::new(0.0, 0.0, 20.0, 20.0), &style(""));
        let [Piece::Path { path, fill, .. }] = pieces.as_slice() else {
            panic!("{pieces:?}")
        };
        assert_eq!(path, &rect(Rect::new(0.0, 0.0, 20.0, 10.0)));
        assert_eq!(*fill, Fill::None);
        assert!(Stencil::of("stencil(not deflate)").is_none());
        assert!(Stencil::of("rhombus").is_none());
    }

    #[test]
    fn an_arc_is_drawn_as_cubics_on_its_ellipse() {
        // A quarter of the unit circle about the origin, clockwise from the right to the bottom.
        let (from, to) = (Point::new(1.0, 0.0), Point::new(0.0, 1.0));
        let curves = arc_to_curves(from, 1.0, 1.0, 0.0, false, true, to);
        let [PathCmd::CurveTo(a, b, end)] = curves.as_slice() else {
            panic!("one quarter: {curves:?}")
        };
        let close = |a: f64, b: f64| (a - b).abs() < 1e-9;
        assert!(close(end.x, to.x) && close(end.y, to.y));
        // The cubic's middle is on the circle, an eighth of a turn round.
        let mid = |p0: f64, p1: f64, p2: f64, p3: f64| (p0 + 3.0 * p1 + 3.0 * p2 + p3) / 8.0;
        let (x, y) = (mid(from.x, a.x, b.x, end.x), mid(from.y, a.y, b.y, end.y));
        assert!((x.hypot(y) - 1.0).abs() < 1e-3 && close(x, y), "({x}, {y})");
        // The long way round is three quarters, and no radius is no arc.
        assert_eq!(arc_to_curves(from, 1.0, 1.0, 0.0, true, false, to).len(), 3);
        assert!(arc_to_curves(from, 0.0, 1.0, 0.0, false, true, to).is_empty());
    }
}
