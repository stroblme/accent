// The label placement and `point_along` are derived from draw.io mxgraph/src/view/mxCellRenderer.js,
// mxgraph/src/view/mxGraphView.js, mxgraph/src/shape/mxText.js, the text setting from
// mxgraph/src/util/mxSvgCanvas2D.js and [`Snap::offset`] from mxgraph/src/shape/mxShape.js
// (Apache-2.0, Copyright (c) 2006-2026 JGraph Holdings Ltd / draw.io AG), ported to Rust and
// modified for accent; see crates/drawio/NOTICE.
//! Toolkit-independent display lists in paint order and page coordinates.
//! The toolkit paints each [`Prim`] and measures text.

use std::collections::{HashMap, HashSet};

use crate::comic::{self, Jiggle};
use crate::geom::{self, PathCmd, Point, Rect};
use crate::label::{self, Run};
use crate::marker;
use crate::model::{Cell, CellId, Geometry, Page};
use crate::placeholders::{self, Context};
use crate::route::{self, EdgeInput, Terminal};
use crate::shapes::{self, Fill, Piece, Placement, Stencil};
use crate::style::{Color, Resolved};

/// A page ready to paint.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Scene {
    pub prims: Vec<Prim>,
    pub page_size: (f64, f64),
    /// The page colour; `None` is draw.io's default white.
    pub background: Option<Color>,
    /// Each drawn edge's route, by id; see [`Scene::route`].
    pub(crate) routes: HashMap<CellId, Vec<Point>>,
    /// The cells a hand would draw otherwise; see [`Scene::wobbles`].
    pub(crate) hand: HashSet<CellId>,
}

impl Scene {
    /// Absolute route points from source to target before arrow heads shorten the line
    /// (mxGraph's `absolutePoints`); `None` when the edge is not drawn.
    pub fn route(&self, id: &str) -> Option<&[Point]> {
        self.routes.get(id).map(Vec::as_slice)
    }

    /// Whether drawing cell `id` by hand ([`crate::is_comic`]) changes it, drawn so or not:
    /// something it paints has a straight piece long enough to wobble.
    pub fn wobbles(&self, id: &str) -> bool {
        self.hand.contains(id)
    }
}

/// How an outline is filled.
#[derive(Debug, Clone, PartialEq)]
pub enum Paint {
    Solid(Color),
    /// A two-stop gradient running from `start` to `end`.
    Linear {
        from: Color,
        to: Color,
        start: Point,
        end: Point,
    },
    /// A two-stop gradient from `centre` out to the ellipse of `radii`, turned `rotation`
    /// degrees about the centre; `to` beyond it.
    Radial {
        from: Color,
        to: Color,
        centre: Point,
        radii: (f64, f64),
        rotation: f64,
    },
}

/// How an outline is stroked.
#[derive(Debug, Clone, PartialEq)]
pub struct Stroke {
    pub color: Color,
    pub width: f64,
    /// Alternating on/off lengths in page units, already scaled by the width.
    pub dash: Option<Vec<f64>>,
    pub cap: Cap,
    pub join: Join,
}

/// How a stroke's open ends are capped: butt unless a shape asks for another
/// (`mxAbstractCanvas2D.setLineCap`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Cap {
    #[default]
    Butt,
    Square,
    Round,
}

/// How a stroke's corners are joined (`setLineJoin`, `setMiterLimit`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Join {
    /// Mitred up to this many times the width, bevelled beyond.
    Miter(f64),
    Round,
    Bevel,
}

impl Default for Join {
    /// draw.io's canvas: mitred, limit 10 (`mxAbstractCanvas2D` state, `stroke-miterlimit:10`).
    fn default() -> Join {
        Join::Miter(10.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Align {
    Left,
    #[default]
    Center,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VAlign {
    Top,
    #[default]
    Middle,
    Bottom,
}

impl Align {
    /// The side it is on in a mirror.
    fn mirrored(self) -> Align {
        match self {
            Align::Left => Align::Right,
            Align::Center => Align::Center,
            Align::Right => Align::Left,
        }
    }
}

impl VAlign {
    /// The side it is on in a mirror.
    fn mirrored(self) -> VAlign {
        match self {
            VAlign::Top => VAlign::Bottom,
            VAlign::Middle => VAlign::Middle,
            VAlign::Bottom => VAlign::Top,
        }
    }
}

/// A label's font where its runs do not say otherwise.
#[derive(Debug, Clone, PartialEq)]
pub struct Font {
    /// In page units (CSS pixels at zoom 1).
    pub size: f64,
    pub family: String,
    pub color: Color,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ImageSource {
    /// `data:` URI as written in the style; see [`crate::decode_data_uri`].
    DataUri(String),
    /// A picture on the web, which accent does not fetch.
    Url(String),
}

/// draw.io's shadow: the shape again, offset and translucent black (`Graph.js` overrides
/// mxGraph's grey to black at a quarter).
pub const SHADOW_OFFSET: Point = Point::new(2.0, 3.0);
pub const SHADOW_COLOR: Color = Color {
    r: 0,
    g: 0,
    b: 0,
    a: 64,
};

/// A drawing primitive. `cell` identifies it for selection and hit testing; `locked` excludes
/// it from hit testing.
#[derive(Debug, Clone, PartialEq)]
pub enum Prim {
    /// An outline, rotation already applied to its points.
    Path {
        cell: CellId,
        locked: bool,
        path: Vec<PathCmd>,
        fill: Option<Paint>,
        stroke: Option<Stroke>,
        /// 0–1, applied to fill and stroke together.
        opacity: f64,
        /// Paint [`SHADOW_COLOR`] under it at [`SHADOW_OFFSET`] first.
        shadow: bool,
        /// What puts its whole shape on whole pixels ([`Snap`]): its cell's line, stroked or not,
        /// or its stencil's own; `None` for a picture's background and border, which stay where
        /// they are set (`mxShape.getSvgScreenOffset`, `mxImageShape`).
        snap: Option<Snap>,
    },
    /// A label. With `wrap` the runs are wrapped to [`wrap_width`] of `rect.w`, else laid out
    /// unbounded; [`Prim::text_origin`] places the text by `align`/`valign` on `anchor`, and it is
    /// turned `rotation` degrees about `anchor`.
    Text {
        cell: CellId,
        locked: bool,
        rect: Rect,
        anchor: Point,
        align: Align,
        valign: VAlign,
        wrap: bool,
        /// Set as draw.io sets an HTML label, as CSS (`html=1`, or `whiteSpace=wrap`, which
        /// draw.io's `Graph.isHtmlLabel` counts as one), rather than as SVG text.
        html: bool,
        rotation: f64,
        font: Font,
        runs: Vec<Run>,
        background: Option<Color>,
        border: Option<Color>,
        opacity: f64,
        /// A stencil's text moves with its lines ([`Snap`]); `None` for a label, which stays
        /// where it is set (`mxText`).
        snap: Option<Snap>,
    },
    /// A picture filling `rect` (keeping its aspect within it when `keep_aspect`), mirrored
    /// across its middle by `flip_h` and `flip_v`, then turned `rotation` degrees about the
    /// centre of `rect`.
    Image {
        cell: CellId,
        locked: bool,
        rect: Rect,
        source: ImageSource,
        keep_aspect: bool,
        rotation: f64,
        flip_h: bool,
        flip_v: bool,
        opacity: f64,
        /// A stencil's picture moves with its lines ([`Snap`]); `None` for a picture cell's,
        /// which stays where it is set (`mxImageShape`).
        snap: Option<Snap>,
    },
}

impl Prim {
    pub fn cell(&self) -> &str {
        match self {
            Prim::Path { cell, .. } | Prim::Text { cell, .. } | Prim::Image { cell, .. } => cell,
        }
    }

    pub fn locked(&self) -> bool {
        match self {
            Prim::Path { locked, .. } | Prim::Text { locked, .. } | Prim::Image { locked, .. } => {
                *locked
            }
        }
    }

    /// The box it paints into, half the stroke included, for culling and selection.
    pub fn bounds(&self) -> Rect {
        match self {
            Prim::Path { path, stroke, .. } => {
                let r = geom::path_bounds(path).unwrap_or_default();
                r.grow(stroke.as_ref().map_or(0.0, |s| s.width / 2.0))
            }
            Prim::Text {
                rect,
                anchor,
                rotation,
                ..
            } => geom::bounds_of(geom::corners(rect, *anchor, *rotation)).unwrap_or(*rect),
            Prim::Image { rect, rotation, .. } => geom::bounding_box(rect, *rotation),
        }
    }

    /// Where a label's laid-out text goes, as draw.io's canvas sets it (`mxSvgCanvas2D`), in page
    /// units: the top of its block, and the left of the box a wrapped label's lines are aligned
    /// across ([`wrap_width`]) or else of its widest line, and how far below where the toolkit
    /// set them its lines are drawn, each line's own or one for all. `block` is the text's widest
    /// line across and its lines down, `baseline` its first baseline and `tops` each line's top
    /// below its top, and `metrics` its font's ascent and descent. `None` for anything but a
    /// label.
    ///
    /// An HTML label is a block aligned, overflowing as it must, in a flex box a unit square at
    /// the anchor rounded to whole units, or as wide as it wraps (`updateTextNodes`, `createCss`).
    /// Chromium draws each of its lines' tops on a whole unit (`TextFragmentPainter`), and the
    /// baseline below it by the font's ascent and the leading above that, each in whole units
    /// (`FontMetrics::AscentDescentWithHacks`, `CalculateLeadingSpace`), on boxes measured in
    /// 64ths of a unit (`LayoutUnit`). The toolkit sets its lines evenly, so each is drawn down
    /// onto Chromium's on its own.
    /// SVG text has its first baseline a font size less one below the anchor, less half the
    /// text's height middle-aligned and all of it and one more bottom-aligned, the text a font
    /// size tall and a line more for every line after the first (`plainText`).
    pub fn text_origin(
        &self,
        block: (f64, f64),
        baseline: f64,
        tops: &[f64],
        metrics: (f64, f64),
    ) -> Option<(Point, Vec<f64>)> {
        let Prim::Text {
            rect,
            anchor,
            align,
            valign,
            wrap,
            html,
            font,
            ..
        } = self
        else {
            return None;
        };
        let mx = match align {
            Align::Left => 0.0,
            Align::Center => 0.5,
            Align::Right => 1.0,
        };
        let my = match valign {
            VAlign::Top => 0.0,
            VAlign::Middle => 0.5,
            VAlign::Bottom => 1.0,
        };
        let (w, h) = block;
        if !html {
            let lines = tops.len().saturating_sub(1) as f64;
            let text = font.size + lines * line_height(font.size, false);
            let above = match valign {
                VAlign::Top => 0.0,
                VAlign::Middle => text / 2.0,
                VAlign::Bottom => text + 1.0,
            };
            let first = anchor.y + font.size - 1.0 - above;
            return Some((Point::new(anchor.x - mx * w, first - baseline), vec![0.0]));
        }
        // JavaScript's `Math.round`, halves up.
        let round = |v: f64| (v + 0.5).floor();
        let left = match wrap {
            true => round(anchor.x - mx * (rect.w + FOREIGN_OBJECT_PADDING)),
            false => round(anchor.x) + mx * (1.0 - w),
        };
        // A `LayoutUnit` cut toward zero, as Chromium's are from a float or halved, and the
        // toolkit's lengths snapped to one.
        let fixed = |v: f64| (v * 64.0).trunc() / 64.0;
        let snap = |v: f64| (v * 64.0).round() / 64.0;
        // The baseline below a line's top: Chromium's, and the toolkit's half the leading above
        // the ascent.
        let line = line_height(font.size, true);
        let (ascent, descent) = (metrics.0.round(), metrics.1.round());
        let chromium = fixed((line - ascent - descent) / 2.0).floor() + ascent;
        let ours = (line - metrics.0 - metrics.1) / 2.0 + metrics.0;
        let top = round(anchor.y) + fixed(my * (1.0 - snap(h)));
        let down = |&t: &f64| round(top + snap(t)) - (top + t) + chromium - ours;
        Some((Point::new(left, top), tops.iter().map(down).collect()))
    }
}

/// What puts a shape on whole pixels, its text and pictures with it: its line's width, its
/// cell's or its stencil's own, and whether its cell has a line at all.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Snap {
    pub width: f64,
    pub stroked: bool,
}

impl Snap {
    /// How far right and down draw.io draws the shape at `pixels` output pixels a unit, in
    /// output pixels: half a pixel when its line is an odd number of whole pixels wide (one at
    /// the least), which puts it on whole pixels, else none (`mxShape.getSvgScreenOffset`).
    /// draw.io's SVG export, `svg`, moves only a shape with a line
    /// (`mxImageExport.getSvgShapeOffset`). Labels and pictures stay where they are set.
    pub fn offset(self, pixels: f64, svg: bool) -> f64 {
        let lined = self.stroked && self.width > 0.0;
        let odd = (self.width * pixels).round().max(1.0) % 2.0 == 1.0;
        match odd && (lined || !svg) {
            true => 0.5,
            false => 0.0,
        }
    }
}

/// The thinnest line draw.io draws, in output pixels, a line 0 wide included (`Graph`'s
/// `minSvgStrokeWidth`, `mxSvgCanvas2D.getCurrentStrokeWidth`).
const THINNEST: f64 = 0.1;

/// How wide draw.io draws a line `width` units wide at `pixels` output pixels a unit, in units.
pub fn line_width(width: f64, pixels: f64) -> f64 {
    width.max(THINNEST / pixels)
}

/// draw.io sets a label's lines 1.2 font sizes apart (`mxConstants.LINE_HEIGHT`).
pub const LINE_HEIGHT: f64 = 1.2;

/// How much wider than its room draw.io lays an HTML label out (`foreignObjectPadding`).
const FOREIGN_OBJECT_PADDING: f64 = 2.0;

/// How far apart a label's lines at font `size` are set: an HTML label's 1.2 sizes in 64ths of a
/// unit, cut as Chromium cuts them (`ComputedStyle::ComputedLineHeightAsFixed`), SVG text's
/// rounded to whole units (`mxSvgCanvas2D.plainText`).
pub fn line_height(size: f64, html: bool) -> f64 {
    match html {
        // As a percentage of the size, which keeps a whole 64th exact.
        true => (size * 120.0 / 100.0 * 64.0).trunc() / 64.0,
        false => (LINE_HEIGHT * size).round(),
    }
}

/// The width a wrapped label with room `w` across breaks its lines at: its room and draw.io's
/// padding, in whole units as its CSS box is (`mxSvgCanvas2D.createCss`).
pub fn wrap_width(w: f64) -> f64 {
    (w + FOREIGN_OBJECT_PADDING).round()
}

/// Build a display list in paint order: layers bottom first, then each cell's shape,
/// picture, label and children. Uses single-page context without a clock; see [`scene_with`].
pub fn scene(page: &Page) -> Scene {
    scene_with(page, &Context::default())
}

/// Build [`scene`] with `ctx` for page and time placeholder expansion.
pub fn scene_with(page: &Page, ctx: &Context) -> Scene {
    let b = Builder::run(page, ctx);
    Scene {
        prims: b.prims,
        page_size: page.size(),
        background: page.background(),
        routes: b
            .edge_points
            .into_iter()
            .map(|(id, points)| (id.to_string(), points))
            .collect(),
        hand: b.hand,
    }
}

impl<'a> Builder<'a> {
    /// Every visible layer of `page` walked, bottom first.
    fn run(page: &'a Page, ctx: &'a Context) -> Builder<'a> {
        let mut b = Builder {
            page,
            ctx,
            math: page.model_attr("math") == Some("1"),
            prims: Vec::new(),
            edge_points: HashMap::new(),
            hand: HashSet::new(),
            index: page
                .cells
                .iter()
                .enumerate()
                .map(|(i, c)| (c.id.as_str(), i))
                .collect(),
            children: HashMap::new(),
        };
        for (i, cell) in page.cells.iter().enumerate() {
            if let Some(parent) = cell.parent.as_deref() {
                b.children.entry(parent).or_default().push(i);
            }
        }
        if let Some(root) = page.root() {
            for &layer in b
                .children
                .get(root.id.as_str())
                .cloned()
                .unwrap_or_default()
                .iter()
            {
                let layer = &page.cells[layer];
                if !layer.is_visible() {
                    continue;
                }
                let locked = layer.style.get("locked") == Some("1");
                b.walk(&layer.id, Point::default(), locked);
            }
        }
        b
    }
}

/// draw.io's extra room above a top-aligned label and below a bottom-aligned one
/// (`mxText.prototype.baseSpacingTop`/`Bottom`, overridden in Graph.js).
pub const BASE_SPACING_TOP: f64 = 5.0;
pub const BASE_SPACING_BOTTOM: f64 = 1.0;

/// Fallback font size (`mxConstants.DEFAULT_FONTSIZE`) for styles that skip the
/// stylesheet with a leading `;`. Normal vertex and edge defaults are 12 and 11.
pub(crate) const DEFAULT_FONTSIZE: f64 = 11.0;

/// Fallback font family (`mxConstants.DEFAULT_FONTFAMILY`) for styles that skip
/// the stylesheet's Helvetica. Pango resolves the CSS list as Arial, then Helvetica.
pub(crate) const DEFAULT_FONTFAMILY: &str = "Arial,Helvetica";

struct Builder<'a> {
    page: &'a Page,
    /// Where and when the page is shown, for labels with placeholders.
    ctx: &'a Context,
    /// The page has `math="1"`: `\(…\)` in labels is a formula.
    math: bool,
    prims: Vec<Prim>,
    /// Each edge's routed points, for the labels that sit on it.
    edge_points: HashMap<&'a str, Vec<Point>>,
    /// The cells a hand would draw otherwise ([`Scene::wobbles`]).
    hand: HashSet<CellId>,
    index: HashMap<&'a str, usize>,
    /// Each cell's children, by index into `page.cells`, in document order.
    children: HashMap<&'a str, Vec<usize>>,
}

/// A cell still to paint in [`Builder::walk`], by index into `page.cells`, with the origin and
/// lock it takes from its parent.
type Next = (usize, Point, bool);

impl<'a> Builder<'a> {
    fn cell(&self, id: &str) -> Option<&'a Cell> {
        self.index.get(id).map(|&i| &self.page.cells[i])
    }

    /// Paint the children of `parent`, whose own top-left is `origin`, each before what it holds.
    /// It keeps a stack of the cells still to paint instead of recursing, since a file holds its
    /// cells flat and nothing bounds how deep their parents go.
    fn walk(&mut self, parent: &str, origin: Point, locked: bool) {
        let mut next: Vec<Next> = self.held(parent, origin, locked).collect();
        while let Some((i, origin, locked)) = next.pop() {
            let cell = &self.page.cells[i];
            if !cell.is_visible() {
                continue;
            }
            let locked = locked || cell.style.get("locked") == Some("1");
            if cell.edge {
                self.edge(cell, origin, locked);
                next.extend(self.held(&cell.id, origin, locked));
            } else if cell.vertex {
                let Some(rect) = self.vertex_rect(cell, origin) else {
                    continue;
                };
                self.vertex(cell, rect, locked);
                next.extend(self.held(&cell.id, Point::new(rect.x, rect.y), locked));
            }
        }
    }

    /// The children of `parent` for [`Builder::walk`]'s stack, the first in document order last.
    fn held(&self, parent: &str, origin: Point, locked: bool) -> impl Iterator<Item = Next> {
        let children = self.children.get(parent).into_iter().flatten().rev();
        children.map(move |&i| (i, origin, locked))
    }

    /// A vertex's absolute rectangle: its geometry from its parent's top-left, or for a
    /// relative child a position on its parent (`mxGraphView.updateCellState`).
    fn vertex_rect(&self, cell: &Cell, origin: Point) -> Option<Rect> {
        let g = cell.geometry.as_ref()?;
        if !g.relative {
            return Some(Rect::new(origin.x + g.x, origin.y + g.y, g.width, g.height));
        }
        let offset = g.offset.unwrap_or_default();
        let parent = self.cell(cell.parent.as_deref()?)?;
        if parent.edge {
            let points = self.edge_points.get(parent.id.as_str())?;
            let at = point_along(points, g.x, g.y, offset);
            return Some(Rect::new(at.x, at.y, g.width, g.height));
        }
        let pg = parent.geometry.as_ref()?;
        let parent_origin = Point::new(origin.x, origin.y);
        Some(Rect::new(
            parent_origin.x + g.x * pg.width + offset.x,
            parent_origin.y + g.y * pg.height + offset.y,
            g.width,
            g.height,
        ))
    }

    fn vertex(&mut self, cell: &Cell, rect: Rect, locked: bool) {
        let style = cell.style.resolve(false);
        let rotation = style.num("rotation", 0.0);
        let opacity = style.num("opacity", 100.0) / 100.0;
        let shape = style.shape();
        let known = shapes::is_known(shape);
        let stencil = match known {
            true => None,
            false => Stencil::of(shape, self.page),
        };
        let mut stroke = stroke(&style);
        if !known
            && stencil.is_none()
            && let Some(s) = stroke.as_mut()
        {
            // ponytail: a shape draw.io draws in code and nobody has ported, or a library
            // stencil whose pack is not installed, is drawn as its box, dashed so it reads as a
            // stand-in.
            s.dash = Some(vec![3.0 * s.width, 3.0 * s.width]);
        }
        let shadow = style.flag("shadow", false);
        // An unfilled, unstroked shape still takes a click inside (draw.io's `pointerEvents`),
        // except a group's, which leaves the clicks to what is in it.
        let hittable = style.flag("pointerEvents", true);
        let place = Placement::of(rect, &style);
        if let Some(stencil) = &stencil {
            self.stencil(cell, stencil, &place, &style, shadow, locked);
        }
        let drawn = if known { shape } else { "label" };
        let parts = match stencil {
            Some(_) => Vec::new(),
            None => shapes::vertex(drawn, place.bounds, &style),
        };
        let snap = Some(Snap {
            width: style.num("strokeWidth", 1.0),
            stroked: stroke.is_some(),
        });
        let mut jiggle = Jiggle::of(&cell.id, &style);
        for (i, mut part) in parts.into_iter().enumerate() {
            let stroke = part_stroke(&style, stroke.as_ref(), &part);
            let wobbles = comic::wobbles(&part.path);
            let stroke = comic::draw(&mut jiggle, &mut part.path, stroke);
            let fill = fill(&style, part.fill, &part.path, &place);
            let painted = fill.is_some() || stroke.is_some();
            if !painted && !hittable {
                continue;
            }
            self.note(&cell.id, painted && wobbles);
            geom::map_path(&mut part.path, |p| place.apply(p));
            self.prims.push(Prim::Path {
                cell: cell.id.clone(),
                locked,
                path: part.path,
                fill,
                stroke,
                opacity,
                shadow: shadow && i == 0,
                snap,
            });
        }
        if shape == "image" {
            self.image(cell, &place, &style, opacity, locked);
        }
        let offset = cell
            .geometry
            .as_ref()
            .filter(|g| !g.relative)
            .and_then(|g| g.offset)
            .unwrap_or_default();
        self.label(
            cell,
            &style,
            LabelAt::Vertex { rect, offset },
            rotation,
            locked,
        );
    }

    /// What a stencil draws for `cell`, put on the page as any shape is. A stencil takes clicks
    /// on what it paints, and across its box only where the style says `pointerEvents=1`
    /// (`mxStencil.drawShape`).
    // ponytail: text in a mirrored stencil is set the right way round, where draw.io mirrors its
    // letters too; and the legacy `stencilFlipH`/`stencilFlipV` keys are not read.
    fn stencil(
        &mut self,
        cell: &Cell,
        stencil: &Stencil,
        place: &Placement,
        style: &Resolved,
        shadow: bool,
        locked: bool,
    ) {
        let id = || cell.id.clone();
        let opacity = style.num("opacity", 100.0) / 100.0;
        let snap = Some(Snap {
            width: stencil.snap_width(style),
            stroked: style.color("strokeColor").is_some(),
        });
        if style.get("pointerEvents") == Some("1") {
            let mut path = shapes::rect(place.bounds);
            geom::map_path(&mut path, |p| place.apply(p));
            self.prims.push(Prim::Path {
                cell: id(),
                locked,
                path,
                fill: None,
                stroke: None,
                opacity,
                shadow: false,
                snap,
            });
        }
        let mirrored = place.flip_h != place.flip_v;
        let mut shadow = shadow;
        let mut jiggle = Jiggle::of(&cell.id, style);
        for piece in stencil.draw(place.bounds, style) {
            match piece {
                Piece::Path {
                    mut path,
                    fill: ink,
                    stroke,
                    opacity,
                } => {
                    let wobbles = comic::wobbles(&path);
                    let stroke = comic::draw(&mut jiggle, &mut path, stroke);
                    let fill = fill(style, ink, &path, place);
                    if fill.is_none() && stroke.is_none() {
                        continue;
                    }
                    self.note(&cell.id, wobbles);
                    geom::map_path(&mut path, |p| place.apply(p));
                    self.prims.push(Prim::Path {
                        cell: id(),
                        locked,
                        path,
                        fill,
                        stroke,
                        opacity,
                        // Under the first thing painted, as under any shape's body.
                        shadow: std::mem::take(&mut shadow),
                        snap,
                    });
                }
                Piece::Text {
                    at,
                    text,
                    align,
                    valign,
                    rotation,
                    font,
                    opacity,
                } => {
                    let anchor = place.apply(at);
                    // A mirror turns the text the other way and swaps the side it hangs from.
                    let (align, valign) = match (place.flip_h, place.flip_v, mirrored) {
                        (true, _, true) => (align.mirrored(), valign),
                        (_, true, true) => (align, valign.mirrored()),
                        _ => (align, valign),
                    };
                    let rotation = match (mirrored, place.flip_h && place.flip_v) {
                        (true, _) => place.degrees - rotation,
                        (false, true) => place.degrees + 180.0 + rotation,
                        (false, false) => place.degrees + rotation,
                    };
                    self.prims.push(Prim::Text {
                        cell: id(),
                        locked,
                        rect: Rect::new(anchor.x, anchor.y, 0.0, 0.0),
                        anchor,
                        align,
                        valign,
                        wrap: false,
                        html: false,
                        rotation,
                        font,
                        runs: label::plain_to_runs(&text, false),
                        background: None,
                        border: None,
                        opacity,
                        snap,
                    });
                }
                Piece::Image {
                    rect,
                    src,
                    flip_h,
                    flip_v,
                    opacity,
                } => {
                    let centre = place.apply(rect.centre());
                    self.prims.push(Prim::Image {
                        cell: id(),
                        locked,
                        rect: Rect::new(
                            centre.x - rect.w / 2.0,
                            centre.y - rect.h / 2.0,
                            rect.w,
                            rect.h,
                        ),
                        source: match src.starts_with("data:") {
                            true => ImageSource::DataUri(src),
                            false => ImageSource::Url(src),
                        },
                        keep_aspect: false,
                        rotation: place.degrees,
                        flip_h: flip_h != place.flip_h,
                        flip_v: flip_v != place.flip_v,
                        // At the fill's opacity too, as `mxSvgCanvas2D.image` draws it.
                        opacity: opacity * style.num("fillOpacity", 100.0) / 100.0,
                        snap,
                    });
                }
            }
        }
    }

    /// A picture cell's background, the picture, and its border (`mxImageShape`), placed as its
    /// shape is: facing its direction, mirrored by its flips and turned. The background and the
    /// picture take the fill's opacity and the border the line's, as `mxSvgCanvas2D` draws them.
    fn image(
        &mut self,
        cell: &Cell,
        place: &Placement,
        style: &Resolved,
        opacity: f64,
        locked: bool,
    ) {
        let (rect, rotation) = (place.bounds, place.degrees);
        let fill_alpha = style.num("fillOpacity", 100.0) / 100.0;
        let mut outline = shapes::rect(rect);
        geom::rotate_path(&mut outline, rect.centre(), rotation);
        if let Some(bg) = style.color("imageBackground") {
            self.prims.push(Prim::Path {
                cell: cell.id.clone(),
                locked,
                path: outline.clone(),
                fill: Some(Paint::Solid(bg.fade(fill_alpha))),
                stroke: None,
                opacity,
                shadow: false,
                snap: None,
            });
        }
        if let Some(src) = style.get("image").filter(|s| !s.is_empty()) {
            let source = match src.starts_with("data:") {
                true => ImageSource::DataUri(src.to_string()),
                false => ImageSource::Url(src.to_string()),
            };
            self.prims.push(Prim::Image {
                cell: cell.id.clone(),
                locked,
                rect,
                source,
                keep_aspect: style.flag("imageAspect", true),
                rotation,
                flip_h: place.flip_h,
                flip_v: place.flip_v,
                opacity: opacity * fill_alpha,
                snap: None,
            });
        }
        if let Some(border) = style.color("imageBorder") {
            self.prims.push(Prim::Path {
                cell: cell.id.clone(),
                locked,
                path: outline,
                fill: None,
                stroke: Some(stroke_in(style, border, style.flag("dashed", false))),
                opacity,
                shadow: false,
                snap: None,
            });
        }
    }

    fn edge(&mut self, cell: &'a Cell, origin: Point, locked: bool) {
        let style = cell.style.resolve(true);
        let g = cell.geometry.clone().unwrap_or_default();
        let at = |p: Point| Point::new(origin.x + p.x, origin.y + p.y);
        let waypoints: Vec<Point> = g
            .points
            .as_deref()
            .unwrap_or_default()
            .iter()
            .map(|&p| at(p))
            .collect();
        let input = EdgeInput {
            style: &style,
            source: self.terminal(cell.source.as_deref()),
            target: self.terminal(cell.target.as_deref()),
            source_port: self.terminal(style.get("sourcePort")),
            target_port: self.terminal(style.get("targetPort")),
            source_point: g.source_point.map(at),
            target_point: g.target_point.map(at),
            waypoints: &waypoints,
            is_loop: cell.source.is_some() && cell.source == cell.target,
            grid_size: self.page.grid_size(),
        };
        let mut points = route::route(&input);
        if points.len() < 2 {
            return;
        }
        self.edge_points.insert(cell.id.as_str(), points.clone());
        let opacity = style.num("opacity", 100.0) / 100.0;
        let shadow = style.flag("shadow", false);
        let line = stroke(&style);
        let snap = Some(Snap {
            width: style.num("strokeWidth", 1.0),
            stroked: line.is_some(),
        });
        let mut jiggle = Jiggle::of(&cell.id, &style);
        if style.shape() == "flexArrow" {
            // A band, filled; its outline is stroked only if it has a stroke colour.
            let width = style.num("strokeWidth", 1.0);
            for (i, mut part) in shapes::flex_arrow(&points, &style, width)
                .into_iter()
                .enumerate()
            {
                let stroke = part_stroke(&style, line.as_ref(), &part);
                let wobbles = comic::wobbles(&part.path);
                let stroke = comic::draw(&mut jiggle, &mut part.path, stroke);
                let fill = fill(&style, part.fill, &part.path, &Placement::default());
                self.note(&cell.id, wobbles && (fill.is_some() || stroke.is_some()));
                self.prims.push(Prim::Path {
                    cell: cell.id.clone(),
                    locked,
                    fill,
                    stroke,
                    path: part.path,
                    opacity,
                    shadow: shadow && i == 0,
                    snap,
                });
            }
        } else if let Some(line) = line {
            // The heads first, because they shorten the line; painted after it
            // (`mxConnector.paintEdgeShape`).
            let heads = [
                self.head(&style, &mut points, false, &line),
                self.head(&style, &mut points, true, &line),
            ];
            let mut path = shapes::edge_line(&points, &style);
            self.note(&cell.id, comic::wobbles(&path));
            let stroke = comic::draw(&mut jiggle, &mut path, Some(line.clone()));
            self.prims.push(Prim::Path {
                cell: cell.id.clone(),
                locked,
                path,
                fill: None,
                stroke,
                opacity,
                shadow,
                snap,
            });
            for mut head in heads.into_iter().flatten().flatten() {
                let stroke = Stroke {
                    dash: None,
                    ..line.clone()
                };
                self.note(&cell.id, comic::wobbles(&head.path));
                let stroke = comic::draw(&mut jiggle, &mut head.path, Some(stroke));
                self.prims.push(Prim::Path {
                    cell: cell.id.clone(),
                    locked,
                    path: head.path,
                    fill: head.fill.map(Paint::Solid),
                    stroke,
                    opacity,
                    shadow: false,
                    snap,
                });
            }
        }
        let routed = self.edge_points[cell.id.as_str()].clone();
        self.label(cell, &style, LabelAt::Edge(&routed, &g), 0.0, locked);
    }

    /// Note cell `id` as one a hand would draw otherwise ([`Scene::wobbles`]) when `wobbles`.
    fn note(&mut self, id: &str, wobbles: bool) {
        if wobbles {
            self.hand.insert(id.to_string());
        }
    }

    /// The arrow head at one end of `points`, which it shortens: its outlines, each with its fill.
    fn head(
        &self,
        style: &Resolved,
        points: &mut [Point],
        end: bool,
        line: &Stroke,
    ) -> Option<Vec<marker::Marker>> {
        let (kind, size, fill, colour) = match end {
            true => ("endArrow", "endSize", "endFill", "endFillColor"),
            false => ("startArrow", "startSize", "startFill", "startFillColor"),
        };
        let kind = style.get(kind).filter(|k| *k != "none")?;
        let n = points.len();
        let (tip, from) = match end {
            true => (n - 1, n - 2),
            false => (0, 1),
        };
        let (dx, dy) = (
            points[tip].x - points[from].x,
            points[tip].y - points[from].y,
        );
        let length = dx.hypot(dy);
        if length == 0.0 {
            return None;
        }
        let unit = Point::new(dx / length, dy / length);
        let fill = style.flag(fill, true);
        marker::marker(
            kind,
            &mut points[tip],
            unit,
            style.num(size, marker::DEFAULT_MARKERSIZE),
            line.width,
            !end,
            fill.then(|| style.color(colour).unwrap_or(line.color)),
        )
    }

    /// The vertex an edge end is attached to, as routing needs it.
    fn terminal(&self, id: Option<&str>) -> Option<Terminal> {
        Terminal::of(self.page, self.cell(id?)?)
    }

    /// A cell's label, placed as `mxCellRenderer.getLabelBounds` and `rotateLabelBounds` place
    /// it: the alignment point (`anchor`) and the box the text is laid out in.
    fn label(&mut self, cell: &Cell, style: &Resolved, at: LabelAt, rotation: f64, locked: bool) {
        let text = placeholders::label(self.page, cell, self.ctx);
        if text.is_empty() || style.flag("noLabel", false) {
            return;
        }
        let runs = match cell.is_html() {
            true => label::html_to_runs(&text, self.math),
            false => label::plain_to_runs(&text, self.math),
        };
        if !runs
            .iter()
            .any(|r| matches!(r, Run::Text { .. } | Run::Math { .. }))
        {
            return;
        }
        let align = match style.get("align") {
            Some("left") => Align::Left,
            Some("right") => Align::Right,
            _ => Align::Center,
        };
        let valign = match style.get("verticalAlign") {
            Some("top") => VAlign::Top,
            Some("bottom") => VAlign::Bottom,
            _ => VAlign::Middle,
        };
        let m = (
            match align {
                Align::Left => 0.0,
                Align::Center => -0.5,
                Align::Right => -1.0,
            },
            match valign {
                VAlign::Top => 0.0,
                VAlign::Middle => -0.5,
                VAlign::Bottom => -1.0,
            },
        );
        // `mxText`: every side's spacing is `spacing` (2 by default) plus its own.
        let spacing = style.num("spacing", 2.0).trunc();
        let side = |key: &str| spacing + style.num(key, 0.0).trunc();
        let (sl, sr, st, sb) = (
            side("spacingLeft"),
            side("spacingRight"),
            side("spacingTop"),
            side("spacingBottom"),
        );
        // `mxText.getSpacing`.
        let shift = Point::new(
            match align {
                Align::Center => (sl - sr) / 2.0,
                Align::Right => -sr,
                Align::Left => sl,
            },
            match valign {
                VAlign::Middle => (st - sb) / 2.0,
                VAlign::Bottom => -sb - BASE_SPACING_BOTTOM,
                VAlign::Top => st + BASE_SPACING_TOP,
            },
        );
        let (anchor, size, rotation) = match at {
            LabelAt::Vertex { rect, offset } => {
                let hpos = style.get("labelPosition").unwrap_or("center");
                let vpos = style.get("verticalLabelPosition").unwrap_or("middle");
                // A label beside its shape is a shape's width or height off
                // (`mxGraphView.updateVertexLabelOffset`).
                let mut off = offset;
                match hpos {
                    "left" => off.x -= rect.w,
                    "right" => off.x += rect.w,
                    _ => {}
                }
                match vpos {
                    "top" => off.y -= rect.h,
                    "bottom" => off.y += rect.h,
                    _ => {}
                }
                // A label across the height (`horizontal=0`) is laid out in the box turned a
                // quarter, and turned back with its text (`mxCellRenderer.getLabelBounds`).
                let inverted = !style.flag("horizontal", true);
                if inverted {
                    off = Point::new(off.y, off.x);
                }
                let mut base = Rect::new(
                    rect.x + off.x,
                    rect.y + off.y,
                    rect.w.max(1.0),
                    rect.h.max(1.0),
                );
                if inverted {
                    base = shapes::rotate90(base);
                }
                if hpos == "center" && vpos == "middle" {
                    base = shapes::label_bounds(style.shape(), base, style, inverted);
                }
                let anchor = Point::new(
                    base.x - m.0 * base.w + shift.x,
                    base.y - m.1 * base.h + shift.y,
                );
                let w = base.w - if hpos == "center" { sl + sr } else { 0.0 };
                let h = base.h - if vpos == "middle" { st + sb } else { 0.0 };
                let turn = rotation + if inverted { -90.0 } else { 0.0 };
                (
                    geom::rotate(anchor, rect.centre(), turn),
                    (w.max(0.0), h.max(0.0)),
                    turn,
                )
            }
            LabelAt::Edge(points, g) => {
                let on = edge_label_at(points, g);
                let anchor = Point::new(on.x + shift.x, on.y + shift.y);
                (anchor, (g.width.max(0.0), g.height.max(0.0)), rotation)
            }
        };
        let rect = Rect::new(
            anchor.x + m.0 * size.0,
            anchor.y + m.1 * size.1,
            size.0,
            size.1,
        );
        let bits = style.num("fontStyle", 0.0) as u32;
        // `textOpacity` alone, as draw.io's label leaves the cell's `opacity` to its shape
        // (`mxText.apply`).
        let opacity = style.num("textOpacity", 100.0) / 100.0;
        self.prims.push(Prim::Text {
            cell: cell.id.clone(),
            locked,
            rect,
            anchor,
            align,
            valign,
            wrap: style.get("whiteSpace") == Some("wrap") && size.0 > 0.0,
            html: cell.is_html() || style.get("whiteSpace") == Some("wrap"),
            rotation,
            font: Font {
                size: style.num("fontSize", DEFAULT_FONTSIZE),
                family: style
                    .get("fontFamily")
                    .unwrap_or(DEFAULT_FONTFAMILY)
                    .to_string(),
                color: style.color("fontColor").unwrap_or(Color::BLACK),
                bold: bits & 1 != 0,
                italic: bits & 2 != 0,
                underline: bits & 4 != 0,
            },
            runs,
            background: style.color("labelBackgroundColor"),
            border: style.color("labelBorderColor"),
            opacity,
            snap: None,
        });
    }
}

/// Where a label hangs: on a vertex's rectangle (shifted by its geometry's offset), or on an
/// edge's route.
enum LabelAt<'p> {
    Vertex { rect: Rect, offset: Point },
    Edge(&'p [Point], &'p Geometry),
}

/// Position an edge label (`mxGraphView.getPoint`, `absoluteOffset`). Relative geometry
/// uses `x` along the route (-1 start, 0 middle, 1 end) and `y` to its left; other
/// geometry uses the midpoint of the endpoints. Add `offset` in either case.
// mxGraphView.updateEdgeLabelOffset, mxGraphView.js 2213-2262
pub fn edge_label_at(points: &[Point], g: &Geometry) -> Point {
    match g.relative {
        true => point_along(points, g.x, g.y, g.offset.unwrap_or_default()),
        false => {
            let (a, b) = (points[0], points[points.len() - 1]);
            let off = g.offset.unwrap_or_default();
            Point::new((a.x + b.x) / 2.0 + off.x, (a.y + b.y) / 2.0 + off.y)
        }
    }
}

fn point_along(points: &[Point], x: f64, y: f64, offset: Point) -> Point {
    let segments: Vec<f64> = points.windows(2).map(|w| w[0].distance(w[1])).collect();
    let length: f64 = segments.iter().sum();
    if segments.is_empty() {
        return points.first().copied().unwrap_or_default();
    }
    let dist = ((x / 2.0 + 0.5) * length).round();
    let (mut walked, mut index) = (0.0, 0);
    while index + 1 < segments.len() && dist >= (walked + segments[index]).round() {
        walked += segments[index];
        index += 1;
    }
    let segment = segments[index];
    let factor = if segment == 0.0 {
        0.0
    } else {
        (dist - walked) / segment
    };
    let (p0, pe) = (points[index], points[index + 1]);
    let (dx, dy) = (pe.x - p0.x, pe.y - p0.y);
    let (nx, ny) = match segment {
        0.0 => (0.0, 0.0),
        s => (dy / s, dx / s),
    };
    Point::new(
        p0.x + dx * factor + nx * y + offset.x,
        p0.y + dy * factor - ny * y + offset.y,
    )
}

/// What a shape's part `outline` is filled with, before `place` puts it on the page.
fn fill(style: &Resolved, fill: Fill, outline: &[PathCmd], place: &Placement) -> Option<Paint> {
    match fill {
        Fill::None => None,
        Fill::Cell => paint(style, outline, place),
        Fill::Own(colour) => Some(Paint::Solid(
            colour.fade(style.num("fillOpacity", 100.0) / 100.0),
        )),
        Fill::Shade(opacity) => {
            let ink = if opacity < 0.0 {
                Color::WHITE
            } else {
                Color::BLACK
            };
            Some(Paint::Solid(ink.fade(opacity.abs())))
        }
    }
}

/// Resolve fill colour, opacity and gradient within the outline's box before
/// `place` mirrors and rotates it. Gradients follow `gradientDirection` (south by
/// default, or radial from the centre). Per-path bounds match draw.io's SVG
/// `objectBoundingBox` gradients (`mxSvgCanvas2D.createSvgGradient`).
fn paint(style: &Resolved, outline: &[PathCmd], place: &Placement) -> Option<Paint> {
    let alpha = style.num("fillOpacity", 100.0) / 100.0;
    let from = style.color("fillColor")?.fade(alpha);
    let gradient = style.color("gradientColor");
    let Some((to, bounds)) = gradient.and_then(|to| Some((to, geom::path_bounds(outline)?))) else {
        return Some(Paint::Solid(from));
    };
    let to = to.fade(alpha);
    let (l, t, r, b) = (bounds.x, bounds.y, bounds.right(), bounds.bottom());
    let mid = bounds.centre();
    let (start, end) = match style.get("gradientDirection").unwrap_or("south") {
        "radial" => {
            return Some(Paint::Radial {
                from,
                to,
                centre: place.apply(mid),
                radii: (bounds.w / 2.0, bounds.h / 2.0),
                rotation: place.degrees,
            });
        }
        "north" => (Point::new(mid.x, b), Point::new(mid.x, t)),
        "east" => (Point::new(l, mid.y), Point::new(r, mid.y)),
        "west" => (Point::new(r, mid.y), Point::new(l, mid.y)),
        _ => (Point::new(mid.x, t), Point::new(mid.x, b)),
    };
    Some(Paint::Linear {
        from,
        to,
        start: place.apply(start),
        end: place.apply(end),
    })
}

/// How `part` of a shape is stroked: with the cell's stroke `cell`, or a dashed colour of its
/// own, in the pen the shape gives it; `None` when it is not stroked.
fn part_stroke(style: &Resolved, cell: Option<&Stroke>, part: &shapes::Part) -> Option<Stroke> {
    let stroke = match part.pen.dashed {
        Some(colour) => Some(stroke_in(style, colour, true)),
        None => cell.filter(|_| part.stroke).cloned(),
    };
    stroke.map(|s| Stroke {
        cap: part.pen.cap,
        join: part.pen.join,
        ..s
    })
}

/// A cell's stroke, or `None` for `strokeColor=none`.
fn stroke(style: &Resolved) -> Option<Stroke> {
    let color = style.color("strokeColor")?;
    Some(stroke_in(style, color, style.flag("dashed", false)))
}

/// The cell's stroke in `color`, at its `strokeOpacity` and width. A dashed line's pattern is in
/// multiples of the width, a line thinner than a unit's too, unless `fixDash=1`
/// (`mxSvgCanvas2D.updateStroke`, `createDashPattern`).
fn stroke_in(style: &Resolved, color: Color, dashed: bool) -> Stroke {
    let color = color.fade(style.num("strokeOpacity", 100.0) / 100.0);
    let width = style.num("strokeWidth", 1.0).max(0.0);
    let dash = dashed.then(|| {
        let scale = if style.flag("fixDash", false) {
            1.0
        } else {
            width
        };
        let pattern: Vec<f64> = style
            .get("dashPattern")
            .unwrap_or("3 3")
            .split_whitespace()
            .filter_map(crate::style::parse_num)
            .filter(|v| *v >= 0.0)
            .map(|v| v * scale)
            .collect();
        match pattern.is_empty() {
            true => vec![3.0 * scale, 3.0 * scale],
            false => pattern,
        }
    });
    Stroke {
        color,
        width,
        dash,
        cap: Cap::Butt,
        join: Join::default(),
    }
}

#[cfg(test)]
mod tests;
