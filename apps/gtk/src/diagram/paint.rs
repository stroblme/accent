//! Painting a page's display list with GSK: outlines as `gsk::Path`s, labels through Pango,
//! pictures as textures. Colours are the document's, painted through a [`Tint`]: as the file
//! writes them, or moved onto the theme's paper and ink as a PDF page is (DESIGN.md, Colour);
//! the frame puts page units on screen.

use std::cell::{OnceCell, RefCell};
use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::rc::Rc;

use accent_core::recolour;
use accent_drawio::{
    Align, Color, Font, ImageSource, Marks, Paint, PathCmd, Point, Prim, Rect, Run, Stroke, VAlign,
};
use gtk::prelude::*;
use gtk::{gdk, glib, graphene, gsk, pango};

use super::geometry::Frame;
use super::math::{self, Typesetter};
use crate::theme;

/// What painting keeps between frames: labels laid out at the scale they were laid out for, and
/// pictures decoded once.
#[derive(Default)]
pub struct Cache {
    /// By prim index, with the scale it was laid out at.
    layouts: RefCell<HashMap<usize, (f64, pango::Layout)>>,
    /// By prim index, for the display list on screen.
    textures: RefCell<HashMap<usize, Option<Rc<Picture>>>>,
    /// By a hash of the data URI, across display lists, so an edit does not decode every
    /// picture on the page again.
    decoded: RefCell<HashMap<u64, Option<Rc<Picture>>>>,
    /// By prim index: a label with a formula as the HTML the typesetter is given, and its key.
    math: RefCell<HashMap<usize, (u64, String)>>,
    /// By prim index: where each label on screen was painted, for finding a label by its text.
    boxes: RefCell<HashMap<usize, Painted>>,
}

/// Where a label was painted, in page units: the block its text took, turned `rotation`
/// degrees about `anchor`.
#[derive(Clone, Copy)]
struct Painted {
    block: Rect,
    anchor: Point,
    rotation: f64,
}

impl Cache {
    /// A new display list is on screen: its prims are numbered afresh.
    pub fn forget(&self) {
        self.layouts.borrow_mut().clear();
        self.textures.borrow_mut().clear();
        self.math.borrow_mut().clear();
        self.boxes.borrow_mut().clear();
    }

    /// What of this cache holds for `new`, a display list replacing `old` prim for prim, as each
    /// frame of a drag's preview replaces the one before: a label or a picture is kept where the
    /// prim in its place is the same cell's same kind, a label wrapping at the same width. Only a
    /// drag's frames are alike enough for that: they move and resize cells, and change nothing
    /// a cell says.
    pub fn carried(&self, old: &[Prim], new: &[Prim]) -> Cache {
        let keep = |i: usize| old.len() == new.len() && alike(&old[i], &new[i]);
        Cache {
            layouts: kept(&self.layouts, keep),
            textures: kept(&self.textures, keep),
            decoded: self.decoded.clone(),
            math: kept(&self.math, keep),
            boxes: RefCell::default(),
        }
    }

    /// The topmost unlocked label whose painted text is under page point `p`. The display list
    /// gives an edge's label no box of its own, only a point, so the text as painted is the
    /// only place it can be aimed at.
    pub fn label_at<'a>(&self, prims: &'a [Prim], p: Point) -> Option<&'a str> {
        let boxes = self.boxes.borrow();
        prims
            .iter()
            .enumerate()
            .rev()
            .find(|(i, prim)| {
                !prim.locked()
                    && boxes.get(i).is_some_and(|b| {
                        b.block
                            .contains(accent_drawio::geom::rotate(p, b.anchor, -b.rotation))
                    })
            })
            .map(|(_, prim)| prim.cell())
    }

    /// Keep where label `index` was painted: `block` in content coordinates, turned about the
    /// label's `anchor` (page units).
    fn painted(&self, index: usize, frame: &Frame, block: Rect, anchor: Point, rotation: f64) {
        let o = frame.to_page(Point::new(block.x, block.y));
        let block = Rect::new(o.x, o.y, block.w / frame.scale, block.h / frame.scale);
        let painted = Painted {
            block,
            anchor,
            rotation,
        };
        self.boxes.borrow_mut().insert(index, painted);
    }

    fn picture(&self, index: usize, uri: &str) -> Option<Rc<Picture>> {
        if let Some(t) = self.textures.borrow().get(&index) {
            return t.clone();
        }
        let mut h = DefaultHasher::new();
        uri.hash(&mut h);
        let key = h.finish();
        let picture = self
            .decoded
            .borrow_mut()
            .entry(key)
            .or_insert_with(|| {
                let (mime, bytes) = accent_drawio::decode_data_uri(uri)?;
                let texture = gdk::Texture::from_bytes(&glib::Bytes::from_owned(bytes)).ok()?;
                // As the image tab takes them: an SVG is line art, a GIF likely an animation.
                let document = match mime.as_str() {
                    "image/svg+xml" => OnceCell::from(true),
                    "image/gif" => OnceCell::from(false),
                    _ => OnceCell::new(),
                };
                Some(Rc::new(Picture { texture, document }))
            })
            .clone();
        self.textures.borrow_mut().insert(index, picture.clone());
        picture
    }
}

/// A cell's picture, decoded, and whether it reads as a document.
pub struct Picture {
    texture: gdk::Texture,
    /// Set from the type for an SVG and a GIF; a raster's is measured the first time a theme
    /// that remaps documents asks (`look::measure`, on the main thread).
    document: OnceCell<bool>,
}

impl Picture {
    fn document(&self) -> bool {
        *self
            .document
            .get_or_init(|| crate::look::measure(&self.texture).document)
    }
}

/// The entries of `map` whose prim `keep` keeps.
fn kept<V: Clone>(
    map: &RefCell<HashMap<usize, V>>,
    keep: impl Fn(usize) -> bool,
) -> RefCell<HashMap<usize, V>> {
    let map = map.borrow();
    RefCell::new(
        map.iter()
            .filter(|(i, _)| keep(**i))
            .map(|(i, v)| (*i, v.clone()))
            .collect(),
    )
}

/// Whether `b` paints what `a` does, wherever it is: the same cell's same kind of prim, a label
/// wrapping at the same width.
fn alike(a: &Prim, b: &Prim) -> bool {
    match (a, b) {
        (
            Prim::Text {
                cell, wrap, rect, ..
            },
            Prim::Text {
                cell: c,
                wrap: w,
                rect: r,
                ..
            },
        ) => cell == c && wrap == w && (!wrap || rect.w == r.w),
        (Prim::Image { cell, .. }, Prim::Image { cell: c, .. })
        | (Prim::Path { cell, .. }, Prim::Path { cell: c, .. }) => cell == c,
        _ => false,
    }
}

/// A colour as the file writes it, which the Properties pane shows; the canvas paints through
/// [`Tint::rgba`].
pub fn rgba(c: Color) -> gdk::RGBA {
    theme::rgba([c.r, c.g, c.b], f32::from(c.a) / 255.0)
}

/// How the canvas paints the file's colours: as they are, or moved onto a theme's paper and ink
/// as a PDF page is (`accent_core::recolour`), white onto paper and black onto ink, every other
/// colour keeping its chroma. Painting takes it as a parameter and never reads the theme, so a
/// printout or an export can paint with [`Tint::FILE`].
#[derive(Clone, Copy, PartialEq)]
pub struct Tint(Option<theme::Page>);

impl Tint {
    /// The file's own colours.
    pub const FILE: Tint = Tint(None);

    /// Onto `page`'s paper and ink, or the file's colours for `None` (`theme::page_colours`).
    pub fn onto(page: Option<theme::Page>) -> Tint {
        Tint(page)
    }

    /// `c` as the canvas paints it, its alpha kept: a remap is affine, so a colour faded over
    /// the paper lands where the remapped colour faded over the remapped paper does.
    pub fn colour(self, c: Color) -> Color {
        let Some((paper, ink)) = self.0 else {
            return c;
        };
        let [r, g, b, a] = recolour::recolour_pixel([c.r, c.g, c.b, c.a], paper, ink);
        Color { r, g, b, a }
    }

    pub fn rgba(self, c: Color) -> gdk::RGBA {
        rgba(self.colour(c))
    }

    /// The tint for a picture: this one for a document, asked only when this one remaps, and the
    /// file's colours for a photo, as an image tab shows one.
    fn picture(self, document: impl FnOnce() -> bool) -> Tint {
        match self.0 {
            Some(_) if document() => self,
            _ => Tint::FILE,
        }
    }

    /// Paint under the remap as a colour matrix, for pixels painting does not choose one by one:
    /// a picture, a typeset formula.
    fn under(self, snapshot: &gtk::Snapshot, paint: impl FnOnce()) {
        let Some((paper, ink)) = self.0 else {
            return paint();
        };
        let (matrix, offset) = gsk_matrix(&recolour::colour_matrix(paper, ink));
        snapshot.push_color_matrix(
            &graphene::Matrix::from_float(matrix),
            &graphene::Vec4::from_float(offset),
        );
        paint();
        snapshot.pop();
    }
}

/// `recolour::colour_matrix`'s rows of five as GSK's matrix and offset. GSK multiplies the
/// unpremultiplied pixel as a row vector by the matrix, so its row `j` holds what channel `j`
/// adds to each output.
fn gsk_matrix(m: &[f32; 20]) -> ([f32; 16], [f32; 4]) {
    let mut matrix = [0.0; 16];
    for i in 0..4 {
        for j in 0..4 {
            matrix[j * 4 + i] = m[i * 5 + j];
        }
    }
    (matrix, [m[4], m[9], m[14], m[19]])
}

fn gpoint(p: Point) -> graphene::Point {
    graphene::Point::new(p.x as f32, p.y as f32)
}

pub fn grect(r: &Rect) -> graphene::Rect {
    graphene::Rect::new(r.x as f32, r.y as f32, r.w as f32, r.h as f32)
}

/// Paint one prim, `index` being its place in the display list. A label with a formula goes to
/// `typesetter` when there is one, and is painted as its source until it has been typeset.
#[allow(clippy::too_many_arguments)]
pub fn prim(
    snapshot: &gtk::Snapshot,
    widget: &gtk::Widget,
    index: usize,
    prim: &Prim,
    frame: &Frame,
    cache: &Cache,
    typesetter: Option<&Rc<Typesetter>>,
    tint: Tint,
) {
    match prim {
        Prim::Path {
            path,
            fill,
            stroke,
            opacity,
            shadow,
            ..
        } => outline(
            snapshot,
            &to_gsk(path, frame),
            fill.as_ref(),
            stroke.as_ref(),
            *opacity,
            *shadow,
            frame,
            tint,
        ),
        Prim::Text {
            rect,
            anchor,
            align,
            valign,
            wrap,
            rotation,
            font,
            runs,
            background,
            border,
            opacity,
            ..
        } => {
            let placed = Placed {
                rect: frame.rect(rect),
                anchor: frame.to_content(*anchor),
                align: *align,
                valign: *valign,
                wrap: *wrap,
            };
            let look = Look {
                background: *background,
                border: *border,
                opacity: *opacity,
            };
            let has_math = runs.iter().any(|r| matches!(r, Run::Math { .. }));
            if let Some(typesetter) = typesetter.filter(|_| has_math) {
                let (key, html) = cache
                    .math
                    .borrow_mut()
                    .entry(index)
                    .or_insert_with(|| {
                        let html = math::label_html(runs, font, *align, wrap.then_some(rect.w));
                        (math::key_of(&html), html)
                    })
                    .clone();
                match typesetter.get(key) {
                    Some(Some(rendered)) => {
                        let block =
                            typeset(snapshot, &rendered, &placed, *rotation, frame, look, tint);
                        return cache.painted(index, frame, block, *anchor, *rotation);
                    }
                    Some(None) => {}
                    None => typesetter.ask(math::Label { key, html }),
                }
            }
            let layout = {
                let mut layouts = cache.layouts.borrow_mut();
                match layouts.get(&index) {
                    Some((scale, layout)) if *scale == frame.scale => layout.clone(),
                    _ => {
                        let width = wrap.then_some(rect.w);
                        let layout = lay_out(widget, runs, font, width, *align, frame.scale, tint);
                        layouts.insert(index, (frame.scale, layout.clone()));
                        layout
                    }
                }
            };
            let block = label(
                snapshot, &layout, &placed, *rotation, font.color, look, tint,
            );
            cache.painted(index, frame, block, *anchor, *rotation);
        }
        Prim::Image {
            rect,
            source,
            keep_aspect,
            rotation,
            opacity,
            ..
        } => {
            let r = frame.rect(rect);
            let picture = match source {
                ImageSource::DataUri(uri) => cache.picture(index, uri),
                // ponytail: a picture on the web is not fetched, so it paints as its box. A
                // download into the ssh-style cache is the upgrade.
                ImageSource::Url(_) => None,
            };
            rotated(snapshot, r.centre(), *rotation, || {
                with_opacity(snapshot, *opacity, || match &picture {
                    Some(p) => {
                        let t = &p.texture;
                        let dest = match keep_aspect {
                            true => fitted(t.width() as f64, t.height() as f64, &r),
                            false => r,
                        };
                        tint.picture(|| p.document()).under(snapshot, || {
                            snapshot.append_scaled_texture(
                                t,
                                gsk::ScalingFilter::Linear,
                                &grect(&dest),
                            )
                        });
                    }
                    None => {
                        let edge = theme::at(widget.color(), theme::PAGE_EDGE_ALPHA * 3.0);
                        let stroke = gsk::Stroke::new(1.0);
                        stroke.set_dash(&[4.0, 4.0]);
                        let builder = gsk::PathBuilder::new();
                        builder.add_rect(&grect(&r));
                        snapshot.append_stroke(&builder.to_path(), &stroke, &edge);
                    }
                })
            });
        }
    }
}

/// The page's grid of `step` page units over `area` (page units, the page on screen) as
/// draw.io paints it (`mxGraphView.createSvgGrid`): a step doubled until it is 4 px on screen
/// (`minGridSize`), every fourth line strong and the others at a fifth of it, in black on light
/// paper and white on dark.
pub fn grid(snapshot: &gtk::Snapshot, frame: &Frame, area: Rect, step: f64, paper: Color) {
    const MIN_PX: f64 = 4.0;
    const STRONG_EVERY: i64 = 4;
    let mut step = step;
    while step * frame.scale < MIN_PX {
        step *= 2.0;
    }
    let light =
        0.299 * f64::from(paper.r) + 0.587 * f64::from(paper.g) + 0.114 * f64::from(paper.b);
    let ink = if light > 127.0 {
        gdk::RGBA::BLACK
    } else {
        gdk::RGBA::WHITE
    };
    let (strong, faint) = (gsk::PathBuilder::new(), gsk::PathBuilder::new());
    let lines = |from: f64, to: f64, line: &dyn Fn(f64, &gsk::PathBuilder)| {
        let mut i = (from / step).ceil() as i64;
        while i as f64 * step <= to {
            let pick = if i % STRONG_EVERY == 0 {
                &strong
            } else {
                &faint
            };
            line(i as f64 * step, pick);
            i += 1;
        }
    };
    let at = |p: Point| frame.to_content(p);
    lines(area.x, area.right(), &|x, b| {
        let (a, e) = (at(Point::new(x, area.y)), at(Point::new(x, area.bottom())));
        b.move_to(a.x as f32, a.y as f32);
        b.line_to(e.x as f32, e.y as f32);
    });
    lines(area.y, area.bottom(), &|y, b| {
        let (a, e) = (at(Point::new(area.x, y)), at(Point::new(area.right(), y)));
        b.move_to(a.x as f32, a.y as f32);
        b.line_to(e.x as f32, e.y as f32);
    });
    let stroke = gsk::Stroke::new(1.0);
    let colour = theme::at(ink, theme::GRID_ALPHA);
    snapshot.append_stroke(
        &faint.to_path(),
        &stroke,
        &theme::at(ink, theme::GRID_ALPHA / 5.0),
    );
    snapshot.append_stroke(&strong.to_path(), &stroke, &colour);
}

/// A path's commands in content coordinates.
pub fn to_gsk(path: &[PathCmd], frame: &Frame) -> gsk::Path {
    let builder = gsk::PathBuilder::new();
    let at = |p: &Point| {
        let c = frame.to_content(*p);
        (c.x as f32, c.y as f32)
    };
    for cmd in path {
        match cmd {
            PathCmd::MoveTo(p) => {
                let (x, y) = at(p);
                builder.move_to(x, y);
            }
            PathCmd::LineTo(p) => {
                let (x, y) = at(p);
                builder.line_to(x, y);
            }
            PathCmd::QuadTo(c, p) => {
                let ((cx, cy), (x, y)) = (at(c), at(p));
                builder.quad_to(cx, cy, x, y);
            }
            PathCmd::CurveTo(c1, c2, p) => {
                let ((ax, ay), (bx, by), (x, y)) = (at(c1), at(c2), at(p));
                builder.cubic_to(ax, ay, bx, by, x, y);
            }
            PathCmd::Close => builder.close(),
        }
    }
    builder.to_path()
}

/// A stroke as draw.io's SVG draws one: mitred joins, butt caps, dashes in multiples of the
/// width (the crate has already scaled them by it).
pub fn stroke_of(s: &Stroke, scale: f64) -> gsk::Stroke {
    let stroke = gsk::Stroke::new((s.width * scale) as f32);
    stroke.set_line_join(gsk::LineJoin::Miter);
    stroke.set_line_cap(gsk::LineCap::Butt);
    if let Some(dash) = &s.dash {
        let dash: Vec<f32> = dash.iter().map(|d| (d * scale) as f32).collect();
        stroke.set_dash(&dash);
    }
    stroke
}

/// An outline: its shadow, its fill, its stroke, all under one opacity.
#[allow(clippy::too_many_arguments)]
pub fn outline(
    snapshot: &gtk::Snapshot,
    path: &gsk::Path,
    fill: Option<&Paint>,
    stroke: Option<&Stroke>,
    opacity: f64,
    shadow: bool,
    frame: &Frame,
    tint: Tint,
) {
    with_opacity(snapshot, opacity, || {
        if shadow {
            let off = accent_drawio::scene::SHADOW_OFFSET;
            let colour = tint.rgba(accent_drawio::scene::SHADOW_COLOR);
            snapshot.save();
            snapshot.translate(&graphene::Point::new(
                (off.x * frame.scale) as f32,
                (off.y * frame.scale) as f32,
            ));
            match (fill, stroke) {
                (Some(_), _) => snapshot.append_fill(path, gsk::FillRule::Winding, &colour),
                (None, Some(s)) => {
                    snapshot.append_stroke(path, &stroke_of(s, frame.scale), &colour)
                }
                (None, None) => {}
            }
            snapshot.restore();
        }
        match fill {
            Some(Paint::Solid(c)) => {
                snapshot.append_fill(path, gsk::FillRule::Winding, &tint.rgba(*c))
            }
            Some(Paint::Linear {
                from,
                to,
                start,
                end,
            }) => {
                if let Some(bounds) = path.bounds() {
                    snapshot.push_fill(path, gsk::FillRule::Winding);
                    snapshot.append_linear_gradient(
                        &bounds,
                        &gpoint(frame.to_content(*start)),
                        &gpoint(frame.to_content(*end)),
                        &[
                            gsk::ColorStop::new(0.0, tint.rgba(*from)),
                            gsk::ColorStop::new(1.0, tint.rgba(*to)),
                        ],
                    );
                    snapshot.pop();
                }
            }
            Some(Paint::Radial {
                from,
                to,
                centre,
                radii: (rx, ry),
                rotation,
            }) => {
                // An outline with no area fills nothing, and GSK refuses a zero radius.
                if let Some(bounds) = path.bounds().filter(|_| *rx > 0.0 && *ry > 0.0) {
                    let c = frame.to_content(*centre);
                    // A square about the centre reaching the outline's far corner covers it
                    // under any turn.
                    let mid = bounds.center();
                    let reach = (bounds.width().hypot(bounds.height()) / 2.0) as f64
                        + c.distance(Point::new(mid.x() as f64, mid.y() as f64));
                    let cover = Rect::new(c.x - reach, c.y - reach, 2.0 * reach, 2.0 * reach);
                    snapshot.push_fill(path, gsk::FillRule::Winding);
                    rotated(snapshot, c, *rotation, || {
                        snapshot.append_radial_gradient(
                            &grect(&cover),
                            &gpoint(c),
                            (rx * frame.scale) as f32,
                            (ry * frame.scale) as f32,
                            0.0,
                            1.0,
                            &[
                                gsk::ColorStop::new(0.0, tint.rgba(*from)),
                                gsk::ColorStop::new(1.0, tint.rgba(*to)),
                            ],
                        )
                    });
                    snapshot.pop();
                }
            }
            None => {}
        }
        if let Some(s) = stroke {
            snapshot.append_stroke(path, &stroke_of(s, frame.scale), &tint.rgba(s.color));
        }
    });
}

fn with_opacity(snapshot: &gtk::Snapshot, opacity: f64, paint: impl FnOnce()) {
    if opacity < 1.0 {
        snapshot.push_opacity(opacity.max(0.0));
        paint();
        snapshot.pop();
    } else {
        paint();
    }
}

/// Paint under a rotation of `degrees` about `centre` (content coordinates).
fn rotated(snapshot: &gtk::Snapshot, centre: Point, degrees: f64, paint: impl FnOnce()) {
    if degrees == 0.0 {
        return paint();
    }
    snapshot.save();
    snapshot.translate(&gpoint(centre));
    snapshot.rotate(degrees as f32);
    snapshot.translate(&graphene::Point::new(-centre.x as f32, -centre.y as f32));
    paint();
    snapshot.restore();
}

/// The largest rectangle of a `w`×`h` picture's shape that fits in `r`, centred.
fn fitted(w: f64, h: f64, r: &Rect) -> Rect {
    if w <= 0.0 || h <= 0.0 {
        return *r;
    }
    let s = (r.w / w).min(r.h / h);
    let (fw, fh) = (w * s, h * s);
    Rect::new(r.x + (r.w - fw) / 2.0, r.y + (r.h - fh) / 2.0, fw, fh)
}

/// What a label's box looks like around its text.
#[derive(Clone, Copy)]
struct Look {
    background: Option<Color>,
    border: Option<Color>,
    opacity: f64,
}

impl Look {
    /// The box's background and border around `block` (content coordinates).
    fn frame(&self, snapshot: &gtk::Snapshot, block: &Rect, tint: Tint) {
        if let Some(bg) = self.background {
            snapshot.append_color(&tint.rgba(bg), &grect(block));
        }
        if let Some(b) = self.border {
            let c = tint.rgba(b);
            snapshot.append_border(
                &gsk::RoundedRect::from_rect(grect(block), 0.0),
                &[1.0; 4],
                &[c; 4],
            );
        }
    }
}

/// A label WebKit typeset, painted where Pango would have put the same block, which is returned
/// (content coordinates, unturned).
fn typeset(
    snapshot: &gtk::Snapshot,
    rendered: &math::Rendered,
    at: &Placed,
    rotation: f64,
    frame: &Frame,
    look: Look,
    tint: Tint,
) -> Rect {
    let (w, h) = (rendered.size.0 * frame.scale, rendered.size.1 * frame.scale);
    let (x, y) = text_origin(at, (w, h), 0.0);
    let dest = Rect::new(x, y, w, h);
    rotated(snapshot, at.anchor, rotation, || {
        with_opacity(snapshot, look.opacity, || {
            look.frame(snapshot, &dest, tint);
            // Typeset in the file's colours, so a theme switch typesets nothing again.
            tint.under(snapshot, || {
                snapshot.append_scaled_texture(
                    &rendered.texture,
                    gsk::ScalingFilter::Trilinear,
                    &grect(&dest),
                )
            });
        })
    });
    dest
}

/// Where a label goes, in content coordinates.
struct Placed {
    rect: Rect,
    anchor: Point,
    align: Align,
    valign: VAlign,
    wrap: bool,
}

/// A laid-out label, painted; the block its text took is returned (content coordinates,
/// unturned).
fn label(
    snapshot: &gtk::Snapshot,
    layout: &pango::Layout,
    at: &Placed,
    rotation: f64,
    colour: Color,
    look: Look,
    tint: Tint,
) -> Rect {
    let (_, logical) = layout.pixel_extents();
    let (tw, th) = (f64::from(logical.width()), f64::from(logical.height()));
    let (x, y) = text_origin(at, (tw, th), f64::from(logical.x()));
    let block = Rect::new(x + f64::from(logical.x()), y, tw, th);
    rotated(snapshot, at.anchor, rotation, || {
        with_opacity(snapshot, look.opacity, || {
            look.frame(snapshot, &block, tint);
            snapshot.save();
            snapshot.translate(&graphene::Point::new(x as f32, y as f32));
            snapshot.append_layout(layout, &tint.rgba(colour));
            snapshot.restore();
        })
    });
    block
}

/// The top-left a laid-out block of `size` is drawn from. Wrapped, it sits in the rectangle by
/// its vertical alignment (Pango aligns the lines across the width); unwrapped, its alignment
/// point sits on the anchor.
fn text_origin(at: &Placed, size: (f64, f64), logical_x: f64) -> (f64, f64) {
    let (tw, th) = size;
    let my = match at.valign {
        VAlign::Top => 0.0,
        VAlign::Middle => 0.5,
        VAlign::Bottom => 1.0,
    };
    if at.wrap {
        return (at.rect.x, at.rect.y + (at.rect.h - th) * my);
    }
    let mx = match at.align {
        Align::Left => 0.0,
        Align::Center => 0.5,
        Align::Right => 1.0,
    };
    (at.anchor.x - tw * mx - logical_x, at.anchor.y - th * my)
}

/// A label's text as one string, with the byte ranges each run's marks cover. A formula is
/// shown as its source until the canvas can typeset it.
fn text_of(runs: &[Run]) -> (String, Vec<(Range<usize>, Marks, bool)>) {
    let (mut text, mut spans) = (String::new(), Vec::new());
    for run in runs {
        match run {
            Run::Text { text: t, marks } => {
                let start = text.len();
                text.push_str(t);
                spans.push((start..text.len(), marks.clone(), false));
            }
            Run::Math { tex, display } => {
                let start = text.len();
                match display {
                    true => text.push_str(&format!("$${tex}$$")),
                    false => text.push_str(&format!("\\({tex}\\)")),
                }
                spans.push((start..text.len(), Marks::default(), true));
            }
            Run::Break => text.push('\n'),
            Run::Bullet => text.push_str("• "),
        }
    }
    (text, spans)
}

/// A Pango layout of `runs` in `font`, at `scale`, wrapped to `width` page units when given, its
/// runs' own colours through `tint`.
fn lay_out(
    widget: &gtk::Widget,
    runs: &[Run],
    font: &Font,
    width: Option<f64>,
    align: Align,
    scale: f64,
    tint: Tint,
) -> pango::Layout {
    let (text, spans) = text_of(runs);
    let layout = widget.create_pango_layout(Some(&text));
    let px = |size: f64| (size * scale * f64::from(pango::SCALE)) as i32;
    let mut desc = pango::FontDescription::new();
    desc.set_family(&font.family);
    desc.set_absolute_size(f64::from(px(font.size)));
    if font.bold {
        desc.set_weight(pango::Weight::Bold);
    }
    if font.italic {
        desc.set_style(pango::Style::Italic);
    }
    layout.set_font_description(Some(&desc));
    let attrs = pango::AttrList::new();
    let put = |mut attr: pango::Attribute, range: &Range<usize>| {
        attr.set_start_index(range.start as u32);
        attr.set_end_index(range.end as u32);
        attrs.insert(attr);
    };
    if font.underline {
        put(
            pango::AttrInt::new_underline(pango::Underline::Single).into(),
            &(0..text.len()),
        );
    }
    for (range, marks, math) in &spans {
        if *math {
            put(pango::AttrString::new_family("monospace").into(), range);
            put(
                pango::AttrInt::new_foreground_alpha(u16::MAX / 5 * 3).into(),
                range,
            );
            continue;
        }
        if marks.bold {
            put(
                pango::AttrInt::new_weight(pango::Weight::Bold).into(),
                range,
            );
        }
        if marks.italic {
            put(
                pango::AttrInt::new_style(pango::Style::Italic).into(),
                range,
            );
        }
        if marks.underline {
            put(
                pango::AttrInt::new_underline(pango::Underline::Single).into(),
                range,
            );
        }
        if let Some(c) = marks.color.map(|c| tint.colour(c)) {
            let wide = |v: u8| u16::from(v) * 257;
            put(
                pango::AttrColor::new_foreground(wide(c.r), wide(c.g), wide(c.b)).into(),
                range,
            );
        }
        if let Some(size) = marks.size {
            put(pango::AttrSize::new_size_absolute(px(size)).into(), range);
        }
    }
    layout.set_attributes(Some(&attrs));
    if let Some(w) = width {
        layout.set_width(px(w).max(1));
        // Words only: draw.io's labels let a word too long for its box run over rather than
        // break it (CSS `word-wrap: normal`).
        layout.set_wrap(pango::WrapMode::Word);
    }
    layout.set_alignment(match align {
        Align::Left => pango::Alignment::Left,
        Align::Center => pango::Alignment::Center,
        Align::Right => pango::Alignment::Right,
    });
    layout
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_become_one_string_with_their_marks() {
        let bold = Marks {
            bold: true,
            ..Marks::default()
        };
        let runs = [
            Run::Bullet,
            Run::Text {
                text: "ab".into(),
                marks: bold.clone(),
            },
            Run::Break,
            Run::Math {
                tex: "x^2".into(),
                display: false,
            },
        ];
        let (text, spans) = text_of(&runs);
        assert_eq!(text, "• ab\n\\(x^2\\)");
        assert_eq!(spans[0], ("• ".len().."• ab".len(), bold, false));
        assert_eq!(&text[spans[1].0.clone()], "\\(x^2\\)");
        assert!(spans[1].2);
    }

    #[test]
    fn an_unwrapped_label_sits_on_its_anchor_by_its_alignment() {
        let at = Placed {
            rect: Rect::new(0.0, 0.0, 100.0, 40.0),
            anchor: Point::new(50.0, 20.0),
            align: Align::Center,
            valign: VAlign::Middle,
            wrap: false,
        };
        assert_eq!(text_origin(&at, (30.0, 10.0), 0.0), (35.0, 15.0));
        let wrapped = Placed { wrap: true, ..at };
        assert_eq!(text_origin(&wrapped, (30.0, 10.0), 0.0), (0.0, 15.0));
    }

    #[test]
    fn a_tint_puts_white_on_paper_and_black_on_ink() {
        let (paper, ink) = ([29, 29, 32], [235, 235, 235]);
        let fill = Color {
            r: 0xda,
            g: 0xe8,
            b: 0xfc,
            a: 128,
        };
        assert_eq!(Tint::FILE.colour(fill), fill);
        let dark = Tint::onto(Some((paper, ink)));
        assert_eq!(dark.colour(Color::WHITE), Color::rgb(29, 29, 32));
        assert_eq!(dark.colour(Color::BLACK), Color::rgb(235, 235, 235));
        // Any other colour as a PDF page's pixel is moved, its alpha kept.
        let [r, g, b, a] = recolour::recolour_pixel([0xda, 0xe8, 0xfc, 128], paper, ink);
        assert_eq!(dark.colour(fill), Color { r, g, b, a });
    }

    #[test]
    fn the_gsk_matrix_is_the_pixel_remap() {
        let (paper, ink) = ([0xfd, 0xf6, 0xe3], [0x65, 0x7b, 0x83]);
        let (m, offset) = gsk_matrix(&recolour::colour_matrix(paper, ink));
        for r in (0..=255u8).step_by(51) {
            for g in (0..=255u8).step_by(51) {
                for b in (0..=255u8).step_by(51) {
                    let c = [r, g, b, 255].map(|v| f32::from(v) / 255.0);
                    let want = recolour::recolour_pixel([r, g, b, 255], paper, ink);
                    // GSK takes the pixel as a row vector times the matrix, then the offset.
                    for i in 0..4 {
                        let out = (0..4).map(|j| m[j * 4 + i] * c[j]).sum::<f32>() + offset[i];
                        let out = (out * 255.0).round().clamp(0.0, 255.0);
                        assert!((out - f32::from(want[i])).abs() <= 1.0, "{r},{g},{b}");
                    }
                }
            }
        }
    }

    #[test]
    fn a_picture_keeping_its_aspect_is_centred_in_its_box() {
        let r = fitted(200.0, 100.0, &Rect::new(0.0, 0.0, 100.0, 100.0));
        assert_eq!(r, Rect::new(0.0, 25.0, 100.0, 50.0));
    }
}
