//! Paint PDF paper, cached tiles and blurred stand-ins, then highlights, selections,
//! strokes, search marks and the page-drop line. Request missing tiles once per change.

use accent_core::pdf::Theme;
use adw::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, graphene, gsk};

use super::imp;
use crate::pdf::cache::{TILE, TileKey, Want, reach, tiles_across, tiles_within};
use crate::pdf::geometry::{Layout, PageRect, gap_middle, on_grid, page_at};
use crate::pdf::tools::{HANDLE, Handle, Kind, Mode, ROTATE_GAP, Selection, Stroke};
use crate::theme;

/// How thick the line a dropped PDF's pages would go in at is: the thumbnail strip's drop bar.
const DROP_LINE: f32 = 3.0;

impl imp::PdfView {
    /// What `snapshot` draws.
    pub(super) fn paint(&self, snapshot: &gtk::Snapshot) {
        let obj = self.obj();
        // Borrowed rather than cloned: the clone was a `Vec` of every page in the document,
        // allocated and dropped once a frame. Nothing under here lays out again.
        let layout = self.layout.borrow();
        if layout.pages.is_empty() {
            return;
        }
        let (ox, oy) = obj.scroll_offset();
        let (vw, vh) = (f64::from(obj.width()), f64::from(obj.height()));
        let theme = self.theme.get();
        let sf = obj.scale_factor().max(1);
        let device_scale = layout.scale * sf as f32;
        let scale_milli = (device_scale * 1000.0).round() as u32;
        let thumbnails = self.thumbnails.get();

        snapshot.save();

        let frame = obj.color();
        let accent = theme::accent();
        // What is on screen first, then what is within reach of it: the render thread works
        // down the list, and a list a scroll overtakes starts again from its head.
        let (mut wanted, mut ahead): (Vec<Want>, Vec<Want>) = (Vec::new(), Vec::new());
        #[cfg(feature = "bench")]
        let mut unsharp = 0;
        #[cfg(feature = "bench")]
        let mut reused = 0;
        let cache = obj.cache();
        let marks = self.marks.borrow();
        let highlights = self.highlights.borrow();
        let strokes = self.strokes.borrow();
        let picked = self.picked.borrow();
        let lasso = self.lasso.borrow();
        let selection = self.selection.borrow();
        let shown = ((ox, ox + vw), (oy, oy + vh));
        let near = reach(shown, sf);
        // The ends of that range are two binary searches; the pages outside it are not looked
        // at at all, which at 1 554 pages is the difference between a frame and a scan.
        let first = page_at(&layout, near.1.0);
        let last = page_at(&layout, near.1.1);
        for (index, at) in layout.pages.iter().enumerate().take(last + 1).skip(first) {
            // Each page is drawn from its own corner, put on the device's pixel grid: a tile
            // edge between two pixels is antialiased by the tiles either side of it, which lets
            // the paper show through as a thin line, and a tile off the grid is resampled soft.
            snapshot.save();
            snapshot.translate(&graphene::Point::new(
                on_grid(at.x, ox, sf),
                on_grid(at.y, oy, sf),
            ));
            let rect = &PageRect {
                x: 0.0,
                y: 0.0,
                ..*at
            };
            let bounds = graphene::Rect::new(rect.x, rect.y, rect.w, rect.h);
            // The page as the render leaves it, the strokes no tile carries yet included, is
            // drawn on a snapshot of its own: a highlighter's stroke is that page again, seen
            // through its colour (`stroked`).
            let base = gtk::Snapshot::new();
            // The page's own paper, so a tile that has not arrived is not a hole.
            base.append_color(&self.paper(), &bounds);

            let page = index as u32;
            let low = cache.borrow_mut().lowres(page, theme);
            // A tile within reach has not arrived, which asks for the stand-in, and one on
            // screen has not, which paints it.
            let (mut missing, mut blank) = (false, false);
            let mut tiles = Vec::new();
            let mut holes = Vec::new();
            let refreshing = self.stale_pages.borrow_mut().remove(&page);
            if !thumbnails {
                let device_w = (rect.w * sf as f32).round() as i32;
                let device_h = (rect.h * sf as f32).round() as i32;
                let (across, down) = (tiles_across(device_w), tiles_across(device_h));
                let key = |tx: i32, ty: i32| TileKey {
                    page,
                    scale_milli,
                    tx: tx as u16,
                    ty: ty as u16,
                    theme,
                };
                // A page whose content changed becomes the set of tiles that are out of date,
                // once, here — this is where what is actually on screen is known. Every tile
                // the change touches, within reach or not, since the rest are kept and will
                // be painted once the reader gets to them.
                if let Some(area) = refreshing {
                    let mut stale = self.stale_tiles.borrow_mut();
                    for ty in 0..down {
                        for tx in 0..across {
                            if touches(area, device_scale, tx as u16, ty as u16) {
                                stale.insert(key(tx, ty));
                            }
                        }
                    }
                }
                let on_x = tiles_within(at.x, shown.0, sf, across);
                let on_y = tiles_within(at.y, shown.1, sf, down);
                for ty in tiles_within(at.y, near.1, sf, down) {
                    for tx in tiles_within(at.x, near.0, sf, across) {
                        let key = key(tx, ty);
                        let on = on_x.contains(&tx) && on_y.contains(&ty);
                        let list = if on { &mut wanted } else { &mut ahead };
                        let want = Want {
                            page,
                            tx: key.tx,
                            ty: key.ty,
                        };
                        let tile = cache.borrow_mut().get(&key);
                        match tile {
                            Some(texture) => {
                                // Painted, and still the old render: ask again, and keep
                                // asking until the replacement lands, so a batch pushed
                                // aside by a scroll is picked up by the next one.
                                if self.stale_tiles.borrow().contains(&key) {
                                    list.push(want);
                                }
                                if on {
                                    tiles.push((tx, ty, texture));
                                }
                            }
                            None => {
                                missing = true;
                                blank |= on;
                                if on {
                                    holes.push(key);
                                }
                                #[cfg(feature = "bench")]
                                {
                                    unsharp += usize::from(on);
                                }
                                list.push(want);
                            }
                        }
                    }
                }
            }
            // Under the tiles, so what has arrived stays sharp and only the holes are blurred,
            // and only while there are holes on screen: painting it every frame would cost a
            // scaled draw per page.
            match low {
                Some(low) if thumbnails || blank => {
                    base.append_scaled_texture(&low, gsk::ScalingFilter::Linear, &bounds);
                }
                // Not even a stand-in yet: ask for one, with what is on screen if it is to be
                // painted now. `u16::MAX` is the whole page.
                None if thumbnails || missing => {
                    let list = if thumbnails || blank {
                        &mut wanted
                    } else {
                        &mut ahead
                    };
                    list.push(Want {
                        page,
                        tx: u16::MAX,
                        ty: u16::MAX,
                    });
                }
                _ => {}
            }
            // Keep the previous zoom's detail until each replacement arrives. Clip each hole
            // so a scaled fallback never softens a tile already rendered at this zoom.
            let sf = sf as f32;
            let stale = self.stale_tiles.borrow();
            base.push_clip(&bounds);
            for key in holes {
                let area = graphene::Rect::new(
                    f32::from(key.tx) * TILE as f32,
                    f32::from(key.ty) * TILE as f32,
                    TILE as f32,
                    TILE as f32,
                );
                let logical = |r: graphene::Rect| {
                    graphene::Rect::new(r.x() / sf, r.y() / sf, r.width() / sf, r.height() / sf)
                };
                base.push_clip(&logical(area));
                for (old, texture) in cache.borrow_mut().fallbacks(key, &area) {
                    if !stale.contains(&old) {
                        #[cfg(feature = "bench")]
                        {
                            reused += 1;
                        }
                        let rect = logical(old.bounds_at(&texture, scale_milli));
                        base.append_scaled_texture(&texture, gsk::ScalingFilter::Linear, &rect);
                    }
                }
                base.pop();
            }
            base.pop();
            drop(stale);
            for (tx, ty, texture) in tiles {
                let x = rect.x + (tx * TILE) as f32 / sf;
                let y = rect.y + (ty * TILE) as f32 / sf;
                let w = texture.width() as f32 / sf;
                let h = texture.height() as f32 / sf;
                base.append_texture(&texture, &graphene::Rect::new(x, y, w, h));
            }
            // Under the highlights and the selection, as in the tile that will carry them.
            let mut page_node = base.to_node();
            for stroke in strokes.iter().filter(|s| s.page == index) {
                let style = obj.ink_style(stroke.tool);
                let next = gtk::Snapshot::new();
                if let Some(node) = &page_node {
                    next.append_node(node);
                }
                let path = stroke_path(stroke, rect, &layout);
                let line = gsk::Stroke::new(style.width * layout.scale);
                line.set_line_cap(gsk::LineCap::Round);
                line.set_line_join(gsk::LineJoin::Round);
                match (style.multiply, &page_node) {
                    (true, Some(node)) => stroked(&next, &path, &line, style, theme, node),
                    _ => next.append_stroke(&path, &line, &as_rendered(style, theme)),
                }
                page_node = next.to_node();
            }
            // A thumbnail's corners are as round as the buttons over them (`organize::CORNER`).
            let corner = if thumbnails {
                crate::organize::CORNER
            } else {
                0.0
            };
            let outline = gsk::RoundedRect::from_rect(bounds, corner);
            if thumbnails {
                snapshot.push_rounded_clip(&outline);
            }
            if let Some(node) = page_node {
                snapshot.append_node(&node);
            }

            // A hairline, so a white page on a light background still reads as a page.
            snapshot.append_border(
                &outline,
                &[1.0; 4],
                &[theme::at(frame, theme::PAGE_EDGE_ALPHA); 4],
            );

            if thumbnails && self.page.get() == index {
                snapshot.append_border(&outline, &[2.0; 4], &[accent; 4]);
            }

            // Under the selection and the search marks: a highlight is what the page says,
            // the other two are what the reader is doing to it right now.
            if let Some(page_highlights) = highlights.get(&index) {
                let colour = theme::at(accent, theme::HIGHLIGHT_ALPHA);
                for quad in page_highlights.iter().flat_map(|(quads, _)| quads) {
                    snapshot.append_color(&colour, &layout.rect_of(rect, quad));
                }
            }
            if let Some((_, boxes)) = selection.iter().find(|(at, _)| *at == index) {
                let colour = theme::at(accent, theme::SELECTION_ALPHA);
                for glyph in boxes {
                    snapshot.append_color(&colour, &layout.rect_of(rect, glyph));
                }
            }
            if let Some(s) = picked.as_ref().filter(|s| s.page == index) {
                paint_selection(snapshot, s, rect, &layout, &accent);
            }
            if let Some(lasso) = lasso.as_ref().filter(|l| l.page == index) {
                paint_lasso(snapshot, &lasso.points, rect, &layout, &accent);
            }
            if let Some(page_marks) = marks.get(&index) {
                for (n, mark) in page_marks.iter().enumerate() {
                    let alpha = match self.current_mark.get() == Some((index, n)) {
                        true => theme::CURRENT_MARK_ALPHA,
                        false => theme::MARK_ALPHA,
                    };
                    let colour = theme::at(accent, alpha);
                    snapshot.append_color(&colour, &layout.rect_of(rect, mark));
                }
            }
            if let Some((_, spot)) = self.spot.get().filter(|(at, _)| *at == index) {
                let colour = theme::at(accent, theme::CURRENT_MARK_ALPHA);
                snapshot.append_color(&colour, &layout.rect_of(rect, &spot));
            }
            if thumbnails {
                snapshot.pop();
            }
            snapshot.restore();
        }
        // A PDF dragged over the pages: the line across the gap its pages would go into.
        snapshot.translate(&graphene::Point::new(-ox as f32, -oy as f32));
        if let Some(gap) = self.drop_gap.get()
            && let Some(beside) = layout
                .pages
                .get(gap)
                .or_else(|| layout.pages.get(gap.checked_sub(1)?))
        {
            let y = gap_middle(&layout, gap) - DROP_LINE / 2.0;
            let line = graphene::Rect::new(beside.x, y, beside.w, DROP_LINE);
            snapshot.push_rounded_clip(&gsk::RoundedRect::from_rect(line, DROP_LINE / 2.0));
            snapshot.append_color(&accent, &line);
            snapshot.pop();
        }
        drop(marks);
        drop(selection);
        snapshot.restore();

        // Asked for once per change, not once per frame: a scroll that reveals nothing new
        // must not re-send the same list. The scale, the scheme and the cache's generation
        // are part of "the same", because the same tiles at another scale, or after the page
        // changed, are a different render.
        let stamp = (scale_milli, theme, cache.borrow().generation());
        wanted.append(&mut ahead);
        #[cfg(feature = "bench")]
        self.unrendered.replace(wanted.clone());
        #[cfg(feature = "bench")]
        self.unsharp.set(unsharp);
        #[cfg(feature = "bench")]
        self.reused.set(reused);
        if !wanted.is_empty() && (self.asked_for.get() != stamp || *self.asked.borrow() != wanted) {
            self.asked_for.set(stamp);
            *self.asked.borrow_mut() = wanted.clone();
            let handler = self.on_wants.borrow();
            if let Some(f) = handler.as_ref() {
                f(&obj, device_scale, theme, wanted);
            }
        }
    }
}

/// What the Select tool holds on this page. While the hand is on it, a ghost of each stroke
/// where the drag has taken it, a picture's being its outline filled as a highlight is; a dashed
/// box round each item when there are two or more; and round them all a box with eight handles
/// and the ring that turns them, or for texts alone, which only move, the box by itself. While it turns, the box turns with it and shows no handles, as a
/// diagram's shape does.
fn paint_selection(
    snapshot: &gtk::Snapshot,
    s: &Selection,
    rect: &PageRect,
    layout: &Layout,
    accent: &gdk::RGBA,
) {
    let point = |p: (f32, f32)| {
        let (x, y) = accent_core::pdf::apply(s.matrix, p);
        graphene::Point::new(rect.x + x * layout.scale, rect.y + y * layout.scale)
    };
    if s.handle.is_some() {
        for item in s.items.iter().filter(|item| !item.points.is_empty()) {
            let builder = gsk::PathBuilder::new();
            let first = point(item.points[0]);
            builder.move_to(first.x(), first.y());
            for &p in &item.points[1..] {
                builder.line_to(point(p).x(), point(p).y());
            }
            if item.kind == Kind::Picture {
                let fill = theme::at(*accent, theme::HIGHLIGHT_ALPHA);
                snapshot.append_fill(&builder.to_path(), gsk::FillRule::Winding, &fill);
                continue;
            }
            let [r, g, b, _] = item.style.rgba;
            let colour = theme::rgba([r, g, b], theme::GHOST_ALPHA);
            let ghost = gsk::Stroke::new(item.style.width * layout.scale);
            ghost.set_line_cap(gsk::LineCap::Round);
            ghost.set_line_join(gsk::LineJoin::Round);
            snapshot.append_stroke(&builder.to_path(), &ghost, &colour);
        }
    }
    let turning = s.handle == Some(Handle::Rotate);
    if s.items.len() > 1 && !turning {
        let dashed = gsk::Stroke::new(1.0);
        dashed.set_dash(&[3.0, 3.0]);
        for item in &s.items {
            let builder = gsk::PathBuilder::new();
            builder.add_rect(&layout.rect_of(rect, &item.under(s.matrix).bounds));
            snapshot.append_stroke(&builder.to_path(), &dashed, accent);
        }
    }
    if turning {
        let [tl, tr, bl, br] = s.bounds().quad_corners().map(point);
        let builder = gsk::PathBuilder::new();
        builder.move_to(tl.x(), tl.y());
        for corner in [tr, br, bl] {
            builder.line_to(corner.x(), corner.y());
        }
        builder.close();
        snapshot.append_stroke(&builder.to_path(), &gsk::Stroke::new(1.0), accent);
        return;
    }
    let moved = match s.moves_only() {
        // A text keeps its size under any drag, so its box only moves.
        true => (s.items.iter().map(|item| item.under(s.matrix).bounds))
            .reduce(accent_core::pdf::Rect::union)
            .unwrap_or(accent_core::pdf::Rect::ZERO),
        false => s.bounds().under(s.matrix),
    };
    let frame = layout.rect_of(rect, &moved);
    snapshot.append_border(
        &gsk::RoundedRect::from_rect(frame, 0.0),
        &[1.0; 4],
        &[*accent; 4],
    );
    if s.moves_only() {
        return;
    }
    let (l, t) = (frame.x(), frame.y());
    let (r, b) = (l + frame.width(), t + frame.height());
    let (cx, cy) = ((l + r) / 2.0, (t + b) / 2.0);
    for (hx, hy) in [
        (l, t),
        (cx, t),
        (r, t),
        (l, cy),
        (r, cy),
        (l, b),
        (cx, b),
        (r, b),
    ] {
        let square = graphene::Rect::new(hx - HANDLE / 2.0, hy - HANDLE / 2.0, HANDLE, HANDLE);
        snapshot.append_color(accent, &square);
    }
    // The rotate handle: the diagram's ring beyond the top-right corner.
    let ring = gsk::PathBuilder::new();
    ring.add_circle(
        &graphene::Point::new(r + ROTATE_GAP, t - ROTATE_GAP),
        HANDLE / 2.0,
    );
    snapshot.append_stroke(&ring.to_path(), &gsk::Stroke::new(1.5), accent);
}

/// A lasso being drawn, closed by the line back to where it began: filled as a highlight is and
/// outlined in a dashed accent line, the diagram's rubber band drawn free-hand.
fn paint_lasso(
    snapshot: &gtk::Snapshot,
    points: &[(f32, f32)],
    rect: &PageRect,
    layout: &Layout,
    accent: &gdk::RGBA,
) {
    let Some(&(x, y)) = points.first() else {
        return;
    };
    let at = |(x, y): (f32, f32)| (rect.x + x * layout.scale, rect.y + y * layout.scale);
    let builder = gsk::PathBuilder::new();
    let (x, y) = at((x, y));
    builder.move_to(x, y);
    for &p in &points[1..] {
        let (x, y) = at(p);
        builder.line_to(x, y);
    }
    builder.close();
    let path = builder.to_path();
    let fill = theme::at(*accent, theme::HIGHLIGHT_ALPHA);
    snapshot.append_fill(&path, gsk::FillRule::EvenOdd, &fill);
    let dashed = gsk::Stroke::new(1.0);
    dashed.set_dash(&[3.0, 3.0]);
    snapshot.append_stroke(&path, &dashed, accent);
}

/// A stroke no tile carries yet, in page-local pixels: a rectangle or a circle from its two
/// points, any other tool a line through them.
fn stroke_path(stroke: &Stroke, rect: &PageRect, layout: &Layout) -> gsk::Path {
    let builder = gsk::PathBuilder::new();
    let point = |&(x, y): &(f32, f32)| {
        graphene::Point::new(rect.x + x * layout.scale, rect.y + y * layout.scale)
    };
    let points = &stroke.points;
    match (stroke.tool, points.as_slice()) {
        (Mode::Rect, &[a, b]) => {
            builder.add_rect(&layout.rect_of(rect, &accent_core::pdf::Rect::from_corners(a, b)));
        }
        (Mode::Circle, &[a, b]) => {
            let radius = (b.0 - a.0).hypot(b.1 - a.1) * layout.scale;
            builder.add_circle(&point(&a), radius);
        }
        _ => {
            if let Some(first) = points.first() {
                builder.move_to(point(first).x(), point(first).y());
                for p in &points[1..] {
                    builder.line_to(point(p).x(), point(p).y());
                }
                // A stroke of one point is a dot, which a round cap draws from a zero-length
                // line.
                if points.len() == 1 {
                    builder.line_to(point(first).x(), point(first).y());
                }
            }
        }
    }
    builder.to_path()
}

/// A stroke that covers, in its colour as the render leaves it: the theme's remap of an opaque
/// pixel of it, laid over the page at its alpha, is what the remap of the two blended is.
fn as_rendered(style: accent_core::pdf::InkStyle, theme: Theme) -> gdk::RGBA {
    let [r, g, b, a] = style.rgba;
    theme::rgba(shown([r, g, b], theme), f32::from(a) / 255.0)
}

/// An opaque colour as a page in `theme` shows it: what the render's remap makes of it.
pub(crate) fn shown(rgb: [u8; 3], theme: Theme) -> [u8; 3] {
    match theme {
        Theme::Recolour { paper, ink } => {
            let [r, g, b, _] =
                accent_core::recolour::recolour_pixel([rgb[0], rgb[1], rgb[2], 255], paper, ink);
            [r, g, b]
        }
        Theme::Plain => rgb,
    }
}

/// A `Multiply` stroke as the render leaves it: `page`, the page drawn so far, again inside the
/// stroke, through the matrix that turns what is on screen into the remap of the file's pixel
/// times the stroke. pdfium multiplies on the file's white paper and the page is remapped after,
/// so a colour laid over the remapped page cannot be it: on a dark page the old overlay, the
/// stroke's remap times the paper, was nearly the paper itself.
fn stroked(
    snapshot: &gtk::Snapshot,
    path: &gsk::Path,
    line: &gsk::Stroke,
    style: accent_core::pdf::InkStyle,
    theme: Theme,
    page: &gsk::RenderNode,
) {
    let [r, g, b, a] = style.rgba.map(|v| f32::from(v) / 255.0);
    // One pass of the stroke at alpha `a` over a channel `c` leaves `c·(1 − a + a·s)`.
    let by = [r, g, b].map(|s| 1.0 - a + a * s);
    let onto = match theme {
        Theme::Recolour { paper, ink } => Some((paper, ink)),
        Theme::Plain => None,
    };
    let (matrix, offset) = theme::gsk_matrix(&accent_core::recolour::multiplied(onto, by));
    snapshot.push_stroke(path, line);
    snapshot.push_color_matrix(
        &graphene::Matrix::from_float(matrix),
        &graphene::Vec4::from_float(offset),
    );
    snapshot.append_node(page);
    snapshot.pop();
    snapshot.pop();
}

/// Whether the tile at `(tx, ty)` covers any of `area`, which is in page points, on a page
/// rendered at `scale` device pixels per point.
fn touches(area: accent_core::pdf::Rect, scale: f32, tx: u16, ty: u16) -> bool {
    let (x, y) = ((i32::from(tx) * TILE) as f32, (i32::from(ty) * TILE) as f32);
    let tile = accent_core::pdf::Rect {
        left: x,
        top: y,
        right: x + TILE as f32,
        bottom: y + TILE as f32,
    };
    let area = accent_core::pdf::Rect {
        left: area.left * scale,
        top: area.top * scale,
        right: area.right * scale,
        bottom: area.bottom * scale,
    };
    area.left < tile.right
        && area.right > tile.left
        && area.top < tile.bottom
        && area.bottom > tile.top
}

/// The colour a page's paper is drawn in before its tiles arrive, matching what the renderer will
/// produce so nothing flashes when they do: the theme's paper, or a plain page's white.
pub(super) fn paper(theme: Theme) -> gdk::RGBA {
    match theme {
        Theme::Recolour { paper, .. } => crate::theme::rgba(paper, 1.0),
        Theme::Plain => gdk::RGBA::WHITE,
    }
}

#[cfg(test)]
mod tests {
    use super::{Theme, shown};

    #[test]
    fn a_colour_shows_as_the_page_remaps_it() {
        let (paper, ink) = ([0, 43, 54], [131, 148, 150]);
        let page = Theme::Recolour { paper, ink };
        assert_eq!(shown([0, 0, 0], page), ink, "black is the page's ink");
        assert_eq!(shown([255, 255, 255], page), paper);
        assert_eq!(shown([0, 0, 0], Theme::Plain), [0, 0, 0]);
    }
}
