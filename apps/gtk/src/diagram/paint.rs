//! Painting a page's display list with GSK: outlines as `gsk::Path`s, labels through Pango,
//! pictures as textures. Colours are the document's, painted as authored (DESIGN.md, Colour: a
//! diagram's page is data, like a rendered PDF page); the frame puts page units on screen.

use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::rc::Rc;

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
    textures: RefCell<HashMap<usize, Option<gdk::Texture>>>,
    /// By a hash of the data URI, across display lists, so an edit does not decode every
    /// picture on the page again.
    decoded: RefCell<HashMap<u64, Option<gdk::Texture>>>,
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

    fn texture(&self, index: usize, uri: &str) -> Option<gdk::Texture> {
        if let Some(t) = self.textures.borrow().get(&index) {
            return t.clone();
        }
        let mut h = DefaultHasher::new();
        uri.hash(&mut h);
        let key = h.finish();
        let texture = self
            .decoded
            .borrow_mut()
            .entry(key)
            .or_insert_with(|| {
                let (_, bytes) = accent_drawio::decode_data_uri(uri)?;
                gdk::Texture::from_bytes(&glib::Bytes::from_owned(bytes)).ok()
            })
            .clone();
        self.textures.borrow_mut().insert(index, texture.clone());
        texture
    }
}

pub fn rgba(c: Color) -> gdk::RGBA {
    theme::rgba([c.r, c.g, c.b], f32::from(c.a) / 255.0)
}

fn gpoint(p: Point) -> graphene::Point {
    graphene::Point::new(p.x as f32, p.y as f32)
}

pub fn grect(r: &Rect) -> graphene::Rect {
    graphene::Rect::new(r.x as f32, r.y as f32, r.w as f32, r.h as f32)
}

/// Paint one prim, `index` being its place in the display list. A label with a formula goes to
/// `typesetter` when there is one, and is painted as its source until it has been typeset.
pub fn prim(
    snapshot: &gtk::Snapshot,
    widget: &gtk::Widget,
    index: usize,
    prim: &Prim,
    frame: &Frame,
    cache: &Cache,
    typesetter: Option<&Rc<Typesetter>>,
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
                        let look = Look {
                            background: *background,
                            border: *border,
                            opacity: *opacity,
                        };
                        let block = typeset(snapshot, &rendered, &placed, *rotation, frame, look);
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
                        let layout = lay_out(widget, runs, font, width, *align, frame.scale);
                        layouts.insert(index, (frame.scale, layout.clone()));
                        layout
                    }
                }
            };
            let block = label(
                snapshot,
                &layout,
                &placed,
                *rotation,
                font.color,
                *background,
                *border,
                *opacity,
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
            let texture = match source {
                ImageSource::DataUri(uri) => cache.texture(index, uri),
                // ponytail: a picture on the web is not fetched, so it paints as its box. A
                // download into the ssh-style cache is the upgrade.
                ImageSource::Url(_) => None,
            };
            rotated(snapshot, r.centre(), *rotation, || {
                with_opacity(snapshot, *opacity, || match &texture {
                    Some(t) => {
                        let dest = match keep_aspect {
                            true => fitted(t.width() as f64, t.height() as f64, &r),
                            false => r,
                        };
                        snapshot.append_scaled_texture(
                            t,
                            gsk::ScalingFilter::Linear,
                            &grect(&dest),
                        );
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
pub fn outline(
    snapshot: &gtk::Snapshot,
    path: &gsk::Path,
    fill: Option<&Paint>,
    stroke: Option<&Stroke>,
    opacity: f64,
    shadow: bool,
    frame: &Frame,
) {
    with_opacity(snapshot, opacity, || {
        if shadow {
            let off = accent_drawio::scene::SHADOW_OFFSET;
            let colour = rgba(accent_drawio::scene::SHADOW_COLOR);
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
            Some(Paint::Solid(c)) => snapshot.append_fill(path, gsk::FillRule::Winding, &rgba(*c)),
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
                            gsk::ColorStop::new(0.0, rgba(*from)),
                            gsk::ColorStop::new(1.0, rgba(*to)),
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
                                gsk::ColorStop::new(0.0, rgba(*from)),
                                gsk::ColorStop::new(1.0, rgba(*to)),
                            ],
                        )
                    });
                    snapshot.pop();
                }
            }
            None => {}
        }
        if let Some(s) = stroke {
            snapshot.append_stroke(path, &stroke_of(s, frame.scale), &rgba(s.color));
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

/// A label WebKit typeset, painted where Pango would have put the same block, which is returned
/// (content coordinates, unturned).
fn typeset(
    snapshot: &gtk::Snapshot,
    rendered: &math::Rendered,
    at: &Placed,
    rotation: f64,
    frame: &Frame,
    look: Look,
) -> Rect {
    let (w, h) = (rendered.size.0 * frame.scale, rendered.size.1 * frame.scale);
    let (x, y) = text_origin(at, (w, h), 0.0);
    let dest = Rect::new(x, y, w, h);
    rotated(snapshot, at.anchor, rotation, || {
        with_opacity(snapshot, look.opacity, || {
            if let Some(bg) = look.background {
                snapshot.append_color(&rgba(bg), &grect(&dest));
            }
            if let Some(b) = look.border {
                let c = rgba(b);
                snapshot.append_border(
                    &gsk::RoundedRect::from_rect(grect(&dest), 0.0),
                    &[1.0; 4],
                    &[c; 4],
                );
            }
            let crop = rendered.crop;
            if crop.width() <= 0.0 {
                return;
            }
            // The whole picture, scaled so the crop lands on `dest`, clipped to it.
            let k = w / f64::from(crop.width());
            let (tw, th) = (
                f64::from(rendered.texture.width()) * k,
                f64::from(rendered.texture.height()) * k,
            );
            snapshot.push_clip(&grect(&dest));
            snapshot.append_scaled_texture(
                &rendered.texture,
                gsk::ScalingFilter::Trilinear,
                &graphene::Rect::new(
                    (x - f64::from(crop.x()) * k) as f32,
                    (y - f64::from(crop.y()) * k) as f32,
                    tw as f32,
                    th as f32,
                ),
            );
            snapshot.pop();
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
#[allow(clippy::too_many_arguments)]
fn label(
    snapshot: &gtk::Snapshot,
    layout: &pango::Layout,
    at: &Placed,
    rotation: f64,
    colour: Color,
    background: Option<Color>,
    border: Option<Color>,
    opacity: f64,
) -> Rect {
    let (_, logical) = layout.pixel_extents();
    let (tw, th) = (f64::from(logical.width()), f64::from(logical.height()));
    let (x, y) = text_origin(at, (tw, th), f64::from(logical.x()));
    let block = Rect::new(x + f64::from(logical.x()), y, tw, th);
    rotated(snapshot, at.anchor, rotation, || {
        with_opacity(snapshot, opacity, || {
            if let Some(bg) = background {
                snapshot.append_color(&rgba(bg), &grect(&block));
            }
            if let Some(b) = border {
                let c = rgba(b);
                snapshot.append_border(
                    &gsk::RoundedRect::from_rect(grect(&block), 0.0),
                    &[1.0; 4],
                    &[c; 4],
                );
            }
            snapshot.save();
            snapshot.translate(&graphene::Point::new(x as f32, y as f32));
            snapshot.append_layout(layout, &rgba(colour));
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

/// A Pango layout of `runs` in `font`, at `scale`, wrapped to `width` page units when given.
fn lay_out(
    widget: &gtk::Widget,
    runs: &[Run],
    font: &Font,
    width: Option<f64>,
    align: Align,
    scale: f64,
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
        if let Some(c) = marks.color {
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
    fn a_picture_keeping_its_aspect_is_centred_in_its_box() {
        let r = fitted(200.0, 100.0, &Rect::new(0.0, 0.0, 100.0, 100.0));
        assert_eq!(r, Rect::new(0.0, 25.0, 100.0, 50.0));
    }
}
