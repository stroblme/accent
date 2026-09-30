// The label placement and `point_along` are derived from draw.io mxgraph/src/view/mxCellRenderer.js,
// mxgraph/src/view/mxGraphView.js and mxgraph/src/shape/mxText.js (Apache-2.0, Copyright (c)
// 2006-2026 JGraph Holdings Ltd / draw.io AG), ported to Rust and modified for accent; see
// crates/drawio/NOTICE.
//! A page as a display list: what to paint, in paint order, in page coordinates. The toolkit
//! turns each [`Prim`] into its own drawing calls and measures text itself.

use std::collections::HashMap;

use crate::geom::{self, PathCmd, Point, Rect};
use crate::label::{self, Run};
use crate::marker;
use crate::model::{Cell, CellId, Geometry, Page};
use crate::placeholders::{self, Context};
use crate::route::{self, EdgeInput, Terminal};
use crate::shapes::{self, Fill, Placement};
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
}

impl Scene {
    /// The points edge `id` runs through, absolute and first end first: where its ends are on
    /// the page before arrow heads shorten it (mxGraph's `absolutePoints`). `None` for an edge
    /// not drawn.
    pub fn route(&self, id: &str) -> Option<&[Point]> {
        self.routes.get(id).map(Vec::as_slice)
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

/// How an outline is stroked. Joins are mitred and caps butt, as in draw.io's SVG.
#[derive(Debug, Clone, PartialEq)]
pub struct Stroke {
    pub color: Color,
    pub width: f64,
    /// Alternating on/off lengths in page units, already scaled by the width.
    pub dash: Option<Vec<f64>>,
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

/// One thing to paint. `cell` names the cell it belongs to, for hit testing and selection;
/// `locked` is set when the cell's layer is locked, which leaves it out of hit tests.
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
    },
    /// A label. With `wrap` the runs are wrapped to `rect.w` and the block is aligned inside
    /// `rect` by `align`/`valign`; without it the text is laid out unbounded and placed so that
    /// its (`align`, `valign`) point sits on `anchor`. Either way it is turned `rotation`
    /// degrees about `anchor`.
    Text {
        cell: CellId,
        locked: bool,
        rect: Rect,
        anchor: Point,
        align: Align,
        valign: VAlign,
        wrap: bool,
        rotation: f64,
        font: Font,
        runs: Vec<Run>,
        background: Option<Color>,
        border: Option<Color>,
        opacity: f64,
    },
    /// A picture filling `rect` (keeping its aspect within it when `keep_aspect`), turned
    /// `rotation` degrees about the centre of `rect`.
    Image {
        cell: CellId,
        locked: bool,
        rect: Rect,
        source: ImageSource,
        keep_aspect: bool,
        rotation: f64,
        opacity: f64,
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
}

/// The display list of `page`: every visible cell in paint order — layers bottom first, each
/// cell's shape, then its picture, then its label, then its children. The page is shown as a
/// lone page with no clock; see [`scene_with`].
pub fn scene(page: &Page) -> Scene {
    scene_with(page, &Context::default())
}

/// [`scene`] of `page` shown where and when `ctx` says, which labels with placeholders fill in.
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
    }
}

/// The points each edge drawn on `page` runs through, absolute and first end first: where its
/// ends are on the page (mxGraph's `absolutePoints`).
pub(crate) fn routes(page: &Page) -> HashMap<CellId, Vec<Point>> {
    Builder::run(page, &Context::default())
        .edge_points
        .into_iter()
        .map(|(id, points)| (id.to_string(), points))
        .collect()
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
const BASE_SPACING_TOP: f64 = 5.0;
const BASE_SPACING_BOTTOM: f64 = 1.0;

/// `mxConstants.DEFAULT_FONTSIZE`: a label's size when its style has none. The stylesheet gives
/// every vertex 12 and every edge 11, so this is for a style that skips it (a leading `;`).
const DEFAULT_FONTSIZE: f64 = 11.0;

/// `mxConstants.DEFAULT_FONTFAMILY`, likewise for a style that skips the stylesheet, which gives
/// every cell Helvetica. Pango reads the comma as CSS does: Arial first, Helvetica after it.
const DEFAULT_FONTFAMILY: &str = "Arial,Helvetica";

struct Builder<'a> {
    page: &'a Page,
    /// Where and when the page is shown, for labels with placeholders.
    ctx: &'a Context,
    /// The page has `math="1"`: `\(…\)` in labels is a formula.
    math: bool,
    prims: Vec<Prim>,
    /// Each edge's routed points, for the labels that sit on it.
    edge_points: HashMap<&'a str, Vec<Point>>,
    index: HashMap<&'a str, usize>,
    /// Each cell's children, by index into `page.cells`, in document order.
    children: HashMap<&'a str, Vec<usize>>,
}

impl<'a> Builder<'a> {
    fn cell(&self, id: &str) -> Option<&'a Cell> {
        self.index.get(id).map(|&i| &self.page.cells[i])
    }

    /// Paint the children of `parent`, whose own top-left is `origin`.
    fn walk(&mut self, parent: &str, origin: Point, locked: bool) {
        let children = self.children.get(parent).cloned().unwrap_or_default();
        for i in children {
            let cell = &self.page.cells[i];
            if !cell.is_visible() {
                continue;
            }
            let locked = locked || cell.style.get("locked") == Some("1");
            if cell.edge {
                self.edge(cell, origin, locked);
                self.walk(&cell.id, origin, locked);
            } else if cell.vertex {
                let Some(rect) = self.vertex_rect(cell, origin) else {
                    continue;
                };
                self.vertex(cell, rect, locked);
                self.walk(&cell.id, Point::new(rect.x, rect.y), locked);
            }
        }
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
        let mut stroke = stroke(&style);
        if !known && let Some(s) = stroke.as_mut() {
            // ponytail: a stencil or a shape not ported yet is drawn as its box, dashed so it
            // reads as a stand-in. Porting `mxStencil` is the upgrade.
            s.dash = Some(vec![3.0 * s.width, 3.0 * s.width]);
        }
        let shadow = style.flag("shadow", false);
        // An unfilled, unstroked shape still takes a click inside (draw.io's `pointerEvents`),
        // except a group's, which leaves the clicks to what is in it.
        let hittable = style.flag("pointerEvents", true);
        let place = Placement::of(rect, &style);
        let drawn = if known { shape } else { "label" };
        for (i, mut part) in shapes::vertex(drawn, place.bounds, &style)
            .into_iter()
            .enumerate()
        {
            let fill = fill(&style, part.fill, &part.path, &place);
            let stroke = stroke.clone().filter(|_| part.stroke);
            if fill.is_none() && stroke.is_none() && !hittable {
                continue;
            }
            geom::map_path(&mut part.path, |p| place.apply(p));
            self.prims.push(Prim::Path {
                cell: cell.id.clone(),
                locked,
                path: part.path,
                fill,
                stroke,
                opacity,
                shadow: shadow && i == 0,
            });
        }
        if shape == "image" {
            self.image(cell, rect, &style, rotation, opacity, locked);
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

    /// A picture cell's background, the picture, and its border (`mxImageShape`).
    fn image(
        &mut self,
        cell: &Cell,
        rect: Rect,
        style: &Resolved,
        rotation: f64,
        opacity: f64,
        locked: bool,
    ) {
        let mut outline = shapes::rect(rect);
        geom::rotate_path(&mut outline, rect.centre(), rotation);
        if let Some(bg) = style.color("imageBackground") {
            self.prims.push(Prim::Path {
                cell: cell.id.clone(),
                locked,
                path: outline.clone(),
                fill: Some(Paint::Solid(bg)),
                stroke: None,
                opacity,
                shadow: false,
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
                opacity,
            });
        }
        if let Some(border) = style.color("imageBorder") {
            self.prims.push(Prim::Path {
                cell: cell.id.clone(),
                locked,
                path: outline,
                fill: None,
                stroke: Some(Stroke {
                    color: border,
                    width: style.num("strokeWidth", 1.0),
                    dash: None,
                }),
                opacity,
                shadow: false,
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
        if style.shape() == "flexArrow" {
            // A band, filled; its outline is stroked only if it has a stroke colour.
            let width = style.num("strokeWidth", 1.0);
            for (i, part) in shapes::flex_arrow(&points, &style, width)
                .into_iter()
                .enumerate()
            {
                self.prims.push(Prim::Path {
                    cell: cell.id.clone(),
                    locked,
                    fill: fill(&style, part.fill, &part.path, &Placement::default()),
                    path: part.path,
                    stroke: line.clone().filter(|_| part.stroke),
                    opacity,
                    shadow: shadow && i == 0,
                });
            }
        } else if let Some(line) = line {
            // The heads first, because they shorten the line; painted after it
            // (`mxConnector.paintEdgeShape`).
            let heads = [
                self.head(&style, &mut points, false, &line),
                self.head(&style, &mut points, true, &line),
            ];
            self.prims.push(Prim::Path {
                cell: cell.id.clone(),
                locked,
                path: shapes::edge_line(&points, &style),
                fill: None,
                stroke: Some(line.clone()),
                opacity,
                shadow,
            });
            for (path, fill) in heads.into_iter().flatten() {
                self.prims.push(Prim::Path {
                    cell: cell.id.clone(),
                    locked,
                    path,
                    fill,
                    stroke: Some(Stroke {
                        dash: None,
                        ..line.clone()
                    }),
                    opacity,
                    shadow: false,
                });
            }
        }
        let routed = self.edge_points[cell.id.as_str()].clone();
        self.label(cell, &style, LabelAt::Edge(&routed, &g), 0.0, locked);
    }

    /// The arrow head at one end of `points`, which it shortens; its outline and fill.
    fn head(
        &self,
        style: &Resolved,
        points: &mut [Point],
        end: bool,
        line: &Stroke,
    ) -> Option<(Vec<PathCmd>, Option<Paint>)> {
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
        let marker = marker::marker(
            kind,
            &mut points[tip],
            unit,
            style.num(size, marker::DEFAULT_MARKERSIZE),
            line.width,
            style.flag(fill, true),
        )?;
        let fill = marker
            .filled
            .then(|| Paint::Solid(style.color(colour).unwrap_or(line.color)));
        Some((marker.path, fill))
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
                let on = match g.relative {
                    true => point_along(points, g.x, g.y, g.offset.unwrap_or_default()),
                    false => {
                        let (a, b) = (points[0], points[points.len() - 1]);
                        let off = g.offset.unwrap_or_default();
                        Point::new((a.x + b.x) / 2.0 + off.x, (a.y + b.y) / 2.0 + off.y)
                    }
                };
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
        let opacity = style.num("opacity", 100.0) / 100.0 * style.num("textOpacity", 100.0) / 100.0;
        self.prims.push(Prim::Text {
            cell: cell.id.clone(),
            locked,
            rect,
            anchor,
            align,
            valign,
            wrap: style.get("whiteSpace") == Some("wrap") && size.0 > 0.0,
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
        });
    }
}

/// Where a label hangs: on a vertex's rectangle (shifted by its geometry's offset), or on an
/// edge's route.
enum LabelAt<'p> {
    Vertex { rect: Rect, offset: Point },
    Edge(&'p [Point], &'p Geometry),
}

/// The point `x` of the way along `points` (-1 the start, 0 the middle, 1 the end), moved `y`
/// off the line to its left and then by `offset` (`mxGraphView.getPoint`).
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

/// A cell's fill of `outline`: its colour at its `fillOpacity`, as a gradient when it has a
/// `gradientColor`, running `gradientDirection` (south by default, or `radial` from the centre)
/// across the outline's own box before `place` mirrors and turns it. draw.io's SVG gradient is in
/// `objectBoundingBox` units of each path it fills (`mxSvgCanvas2D.createSvgGradient`), so a
/// level arrow's south gradient crosses its band.
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

/// A cell's stroke, or `None` for `strokeColor=none`. A dashed line's pattern is in multiples of
/// the width unless `fixDash=1` (`mxSvgCanvas2D.createDashPattern`).
fn stroke(style: &Resolved) -> Option<Stroke> {
    let alpha = style.num("strokeOpacity", 100.0) / 100.0;
    let color = style.color("strokeColor")?.fade(alpha);
    let width = style.num("strokeWidth", 1.0).max(0.0);
    let dash = style.flag("dashed", false).then(|| {
        let scale = if style.flag("fixDash", false) {
            1.0
        } else {
            width.max(1.0)
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
    Some(Stroke { color, width, dash })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Cell;

    fn page(cells: Vec<Cell>) -> Page {
        let mut p = Page::blank("P", "p");
        p.cells.extend(cells);
        p
    }

    fn cells_in_order(s: &Scene) -> Vec<&str> {
        let mut out: Vec<&str> = Vec::new();
        for prim in &s.prims {
            if out.last() != Some(&prim.cell()) {
                out.push(prim.cell());
            }
        }
        out
    }

    fn text(s: &Scene, id: &str) -> (Rect, Point, bool) {
        s.prims
            .iter()
            .find_map(|p| match p {
                Prim::Text {
                    cell,
                    rect,
                    anchor,
                    wrap,
                    ..
                } if cell == id => Some((*rect, *anchor, *wrap)),
                _ => None,
            })
            .expect("a label")
    }

    #[test]
    fn paint_order_is_layers_then_documents_then_children() {
        let mut p = page(vec![
            Cell::new_vertex("a", "1", Rect::new(0.0, 0.0, 10.0, 10.0), "", ""),
            Cell::new_vertex("g", "1", Rect::new(100.0, 100.0, 50.0, 50.0), "", ""),
            Cell::new_vertex("child", "g", Rect::new(5.0, 5.0, 10.0, 10.0), "", ""),
            Cell::new_vertex("b", "2", Rect::new(0.0, 0.0, 10.0, 10.0), "", ""),
        ]);
        p.cells.insert(2, Cell::layer("2", "0"));
        let s = scene(&p);
        assert_eq!(cells_in_order(&s), ["a", "g", "child", "b"]);
    }

    #[test]
    fn children_of_a_group_are_placed_from_it_and_a_group_takes_no_clicks() {
        let p = page(vec![
            Cell::new_vertex("g", "1", Rect::new(100.0, 50.0, 50.0, 50.0), "group", ""),
            Cell::new_vertex("c", "g", Rect::new(10.0, 20.0, 30.0, 10.0), "", ""),
        ]);
        let s = scene(&p);
        assert_eq!(cells_in_order(&s), ["c"], "an empty group paints nothing");
        let bounds = s.bounds_of("c").unwrap();
        assert!(
            (bounds.x - 109.5).abs() < 1e-9 && (bounds.y - 69.5).abs() < 1e-9,
            "{bounds:?}"
        );
    }

    #[test]
    fn labels_sit_where_draw_io_puts_them() {
        let p = page(vec![
            Cell::new_vertex(
                "mid",
                "1",
                Rect::new(0.0, 0.0, 120.0, 60.0),
                "whiteSpace=wrap;",
                "A",
            ),
            Cell::new_vertex("top", "1", Rect::new(0.0, 100.0, 120.0, 60.0), "text;", "B"),
            Cell::new_vertex(
                "img",
                "1",
                Rect::new(0.0, 200.0, 40.0, 40.0),
                "image;image=x.png;",
                "C",
            ),
        ]);
        let s = scene(&p);
        // Centre/middle: the anchor is the centre and the box loses 2 of spacing on each side.
        let (rect, anchor, wrap) = text(&s, "mid");
        assert!(wrap);
        assert_eq!(anchor, Point::new(60.0, 30.0));
        assert_eq!(rect, Rect::new(2.0, 2.0, 116.0, 56.0));
        // A text cell is left/top: 2 of spacing in, and draw.io's 5 more above.
        let (_, anchor, wrap) = text(&s, "top");
        assert!(!wrap);
        assert_eq!(anchor, Point::new(2.0, 100.0 + 2.0 + 5.0));
        // A picture's label hangs under it.
        let (_, anchor, _) = text(&s, "img");
        assert_eq!(anchor, Point::new(20.0, 240.0 + 2.0 + 5.0));
    }

    #[test]
    fn a_label_whose_style_skips_the_stylesheet_takes_draw_ios_own_defaults() {
        let font = |style: &str| {
            let r = Rect::new(0.0, 0.0, 40.0, 20.0);
            let p = page(vec![Cell::new_vertex("a", "1", r, style, "A")]);
            scene(&p)
                .prims
                .iter()
                .find_map(|prim| match prim {
                    Prim::Text { font, .. } => Some(font.clone()),
                    _ => None,
                })
                .expect("a label")
        };
        let bare = font(";");
        assert_eq!(
            (bare.size, bare.family.as_str()),
            (DEFAULT_FONTSIZE, DEFAULT_FONTFAMILY)
        );
        let sheet = font("rounded=1;");
        assert_eq!((sheet.size, sheet.family.as_str()), (12.0, "Helvetica"));
    }

    #[test]
    fn hidden_cells_and_their_children_are_not_painted() {
        let mut hidden = Cell::new_vertex("h", "1", Rect::new(0.0, 0.0, 10.0, 10.0), "", "");
        hidden.attrs.push(("visible".into(), "0".into()));
        let p = page(vec![
            hidden,
            Cell::new_vertex("under", "h", Rect::new(0.0, 0.0, 5.0, 5.0), "", ""),
            Cell::new_vertex("shown", "1", Rect::new(0.0, 0.0, 10.0, 10.0), "", ""),
        ]);
        assert_eq!(cells_in_order(&scene(&p)), ["shown"]);
    }

    #[test]
    fn rotation_is_applied_to_the_outline_and_the_label_turns_with_it() {
        let p = page(vec![Cell::new_vertex(
            "r",
            "1",
            Rect::new(0.0, 0.0, 100.0, 20.0),
            "rotation=90;",
            "R",
        )]);
        let s = scene(&p);
        let b = s.prims[0].bounds();
        assert!(
            (b.w - 21.0).abs() < 1e-6 && (b.h - 101.0).abs() < 1e-6,
            "{b:?}"
        );
        match &s.prims[1] {
            Prim::Text { rotation, .. } => assert_eq!(*rotation, 90.0),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_locked_layer_locks_what_is_on_it() {
        let mut p = page(vec![Cell::new_vertex(
            "a",
            "1",
            Rect::new(0.0, 0.0, 10.0, 10.0),
            "",
            "",
        )]);
        p.cells[1].style = crate::style::Style::parse("locked=1;");
        assert!(scene(&p).prims.iter().all(Prim::locked));
    }

    #[test]
    fn an_edge_runs_between_its_shapes_with_its_head_on_top_and_its_label_midway() {
        let mut edge = Cell::new_edge(
            "e",
            "1",
            (Some("a"), Point::default()),
            (Some("b"), Point::default()),
            "edgeStyle=none;endArrow=block;",
        );
        edge.set_label("go");
        let p = page(vec![
            Cell::new_vertex("a", "1", Rect::new(0.0, 0.0, 40.0, 40.0), "", ""),
            Cell::new_vertex("b", "1", Rect::new(200.0, 0.0, 40.0, 40.0), "", ""),
            edge,
        ]);
        let s = scene(&p);
        let route = s.route("e").expect("a route");
        assert!(
            route.len() == 2 && route[0].distance(Point::new(40.0, 20.0)) < 1e-9,
            "{route:?}"
        );
        assert!(
            route[1].distance(Point::new(200.0, 20.0)) < 1e-9,
            "the route runs to the outline, the head's room not taken off: {route:?}"
        );
        assert_eq!(s.route("a"), None);
        let edge: Vec<&Prim> = s.prims.iter().filter(|p| p.cell() == "e").collect();
        let (line, head) = match (edge[0], edge[1]) {
            (
                Prim::Path { path: line, .. },
                Prim::Path {
                    path: head, fill, ..
                },
            ) => {
                assert!(fill.is_some(), "a block head is filled");
                (line, head)
            }
            other => panic!("{other:?}"),
        };
        let line = geom::path_bounds(line).unwrap();
        let head = geom::path_bounds(head).unwrap();
        assert_eq!(line.x, 40.0, "leaves the right side of a");
        assert!(
            line.right() < 200.0 && head.right() <= 200.0 + 1e-9,
            "stops short for its head"
        );
        match edge[2] {
            Prim::Text { anchor, .. } => assert!((anchor.x - (40.0 + 200.0) / 2.0).abs() < 1.0),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_label_moves_along_its_edge_and_off_it() {
        let line = [Point::new(0.0, 0.0), Point::new(100.0, 0.0)];
        assert_eq!(
            point_along(&line, 0.0, 0.0, Point::default()),
            Point::new(50.0, 0.0)
        );
        assert_eq!(
            point_along(&line, -1.0, 0.0, Point::default()),
            Point::new(0.0, 0.0)
        );
        // Positive `y` is to the left of the way the edge runs: up, for one running right.
        assert_eq!(
            point_along(&line, 0.5, 10.0, Point::new(1.0, 2.0)),
            Point::new(76.0, -8.0)
        );
    }

    #[test]
    fn a_dash_pattern_keeps_only_lengths_a_line_can_be_drawn_with() {
        let style = crate::style::Style::parse("dashed=1;dashPattern=1e999 -2 NaN 3;");
        let dash = stroke(&style.resolve(false)).unwrap().dash;
        assert_eq!(dash, Some(vec![3.0]));
    }

    #[test]
    fn a_gradient_runs_its_direction_across_the_outline_it_fills() {
        let fill = |cell: Cell| match &scene(&page(vec![cell])).prims[0] {
            Prim::Path { fill, .. } => fill.clone().expect("a fill"),
            other => panic!("{other:?}"),
        };
        let colours = "fillColor=#ffffff;gradientColor=#000000;";
        // A level arrow's route has no height; draw.io's SVG spans each outline's own box, so
        // the default south still runs across the band, top to bottom.
        let arrow = Cell::new_edge(
            "e",
            "1",
            (None, Point::new(0.0, 50.0)),
            (None, Point::new(200.0, 50.0)),
            &format!("shape=flexArrow;endArrow=none;{colours}"),
        );
        match fill(arrow) {
            Paint::Linear { start, end, .. } => {
                assert_eq!((start.x, start.y, end.y), (end.x, 45.0, 55.0));
            }
            other => panic!("{other:?}"),
        }
        let r = Rect::new(0.0, 0.0, 100.0, 40.0);
        let shape = |direction: &str| {
            let style = format!("{colours}gradientDirection={direction};");
            fill(Cell::new_vertex("v", "1", r, &style, ""))
        };
        match shape("west") {
            Paint::Linear { start, end, .. } => {
                assert_eq!(
                    (start, end),
                    (Point::new(100.0, 20.0), Point::new(0.0, 20.0))
                );
            }
            other => panic!("{other:?}"),
        }
        match shape("radial") {
            Paint::Radial { centre, radii, .. } => {
                assert_eq!((centre, radii), (Point::new(50.0, 20.0), (50.0, 20.0)));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_shape_faces_its_direction_and_mirrors_with_its_flips() {
        let tip_at = |style: &str, tip: Point| {
            let r = Rect::new(0.0, 0.0, 80.0, 40.0);
            let p = page(vec![Cell::new_vertex("t", "1", r, style, "")]);
            match &scene(&p).prims[0] {
                Prim::Path { path, .. } => path.iter().any(|c| match c {
                    PathCmd::MoveTo(p) | PathCmd::LineTo(p) => p.distance(tip) < 1e-9,
                    _ => false,
                }),
                other => panic!("{other:?}"),
            }
        };
        assert!(tip_at("triangle;", Point::new(80.0, 20.0)));
        assert!(tip_at("triangle;direction=south;", Point::new(40.0, 40.0)));
        assert!(tip_at("triangle;flipH=1;", Point::new(0.0, 20.0)));
        assert!(tip_at(
            "triangle;direction=north;flipV=1;",
            Point::new(40.0, 40.0)
        ));
    }

    #[test]
    fn a_label_keeps_clear_of_a_bounded_shape_and_lies_across_a_turned_one() {
        let r = Rect::new(0.0, 0.0, 100.0, 60.0);
        let doc = page(vec![Cell::new_vertex(
            "d",
            "1",
            r,
            "shape=document;boundedLbl=1;",
            "D",
        )]);
        assert_eq!(
            text(&scene(&doc), "d").1,
            Point::new(50.0, 21.0),
            "above the wave"
        );
        // `horizontal=0` wraps across the height and turns the text a quarter back.
        let lane = page(vec![Cell::new_vertex(
            "s",
            "1",
            Rect::new(0.0, 0.0, 200.0, 100.0),
            "swimlane;horizontal=0;startSize=30;whiteSpace=wrap;",
            "S",
        )]);
        let s = scene(&lane);
        let (rect, anchor, wrap) = text(&s, "s");
        assert!(wrap && (rect.w - 96.0).abs() < 1e-9, "{rect:?}");
        assert!(
            anchor.distance(Point::new(15.0, 50.0)) < 1e-9,
            "in the title: {anchor:?}"
        );
        match s.prims.iter().find(|p| matches!(p, Prim::Text { .. })) {
            Some(Prim::Text { rotation, .. }) => assert_eq!(*rotation, -90.0),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_shape_not_ported_yet_is_a_dashed_box() {
        let p = page(vec![Cell::new_vertex(
            "s",
            "1",
            Rect::new(0.0, 0.0, 10.0, 10.0),
            "shape=mxgraph.aws4.lambda;",
            "",
        )]);
        match &scene(&p).prims[0] {
            Prim::Path { stroke, .. } => assert!(stroke.as_ref().unwrap().dash.is_some()),
            other => panic!("{other:?}"),
        }
    }
}
