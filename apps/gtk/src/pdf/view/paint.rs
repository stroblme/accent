//! The page painted from cached tiles: the paper, the tiles on screen over a blurred stand-in,
//! and what is drawn over a page — highlights, the selection, live strokes, the stroke the Adjust
//! tool holds and the search's marks — the line a dropped PDF's pages would go in at, and the
//! tiles still wanted, asked for once per change.

use adw::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, graphene, gsk};

use super::imp;
use crate::pdf::cache::{TILE, TileKey, Want, reach, tiles_across, tiles_within};
use crate::pdf::geometry::{gap_middle, page_at};
use crate::pdf::tools::{HANDLE, Mode, mapped};
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
        let dark = self.dark.get();
        let sf = obj.scale_factor().max(1);
        let device_scale = layout.scale * sf as f32;
        let scale_milli = (device_scale * 1000.0).round() as u32;
        let thumbnails = self.thumbnails.get();

        snapshot.save();
        snapshot.translate(&graphene::Point::new(-ox as f32, -oy as f32));

        let frame = obj.color();
        let accent = theme::accent();
        // What is on screen first, then what is within reach of it: the render thread works
        // down the list, and a list a scroll overtakes starts again from its head.
        let (mut wanted, mut ahead): (Vec<Want>, Vec<Want>) = (Vec::new(), Vec::new());
        #[cfg(feature = "bench")]
        let mut unsharp = 0;
        let cache = obj.cache();
        let marks = self.marks.borrow();
        let highlights = self.highlights.borrow();
        let strokes = self.strokes.borrow();
        let adjust = self.adjust.borrow();
        let selection = self.selection.borrow();
        let shown = ((ox, ox + vw), (oy, oy + vh));
        let near = reach(shown, sf);
        // The ends of that range are two binary searches; the pages outside it are not looked
        // at at all, which at 1 554 pages is the difference between a frame and a scan.
        let first = page_at(&layout, near.1.0);
        let last = page_at(&layout, near.1.1);
        for (index, rect) in layout.pages.iter().enumerate().take(last + 1).skip(first) {
            let bounds = graphene::Rect::new(rect.x, rect.y, rect.w, rect.h);
            // The page's own paper, so a tile that has not arrived is not a hole.
            snapshot.append_color(&self.paper(), &bounds);

            let page = index as u32;
            let low = cache.borrow_mut().lowres(page, dark);
            // A tile within reach has not arrived, which asks for the stand-in, and one on
            // screen has not, which paints it.
            let (mut missing, mut blank) = (false, false);
            let mut tiles = Vec::new();
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
                    dark,
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
                let on_x = tiles_within(rect.x, shown.0, sf, across);
                let on_y = tiles_within(rect.y, shown.1, sf, down);
                for ty in tiles_within(rect.y, near.1, sf, down) {
                    for tx in tiles_within(rect.x, near.0, sf, across) {
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
                    snapshot.append_scaled_texture(&low, gsk::ScalingFilter::Linear, &bounds);
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
            for (tx, ty, texture) in tiles {
                let x = rect.x + (tx * TILE) as f32 / sf as f32;
                let y = rect.y + (ty * TILE) as f32 / sf as f32;
                let w = texture.width() as f32 / sf as f32;
                let h = texture.height() as f32 / sf as f32;
                snapshot.append_texture(&texture, &graphene::Rect::new(x, y, w, h));
            }

            // A hairline, so a white page on a light background still reads as a page.
            snapshot.append_border(
                &gsk::RoundedRect::from_rect(bounds, 0.0),
                &[1.0; 4],
                &[theme::at(frame, theme::PAGE_EDGE_ALPHA); 4],
            );

            if thumbnails && self.page.get() == index {
                snapshot.append_border(
                    &gsk::RoundedRect::from_rect(bounds, 0.0),
                    &[2.0; 4],
                    &[accent; 4],
                );
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
            for stroke in strokes.iter().filter(|s| s.page == index) {
                let builder = gsk::PathBuilder::new();
                let point = |&(x, y): &(f32, f32)| {
                    graphene::Point::new(rect.x + x * layout.scale, rect.y + y * layout.scale)
                };
                let points = &stroke.points;
                match (stroke.tool, points.as_slice()) {
                    (Mode::Rect, &[a, b]) => {
                        builder.add_rect(
                            &layout.rect_of(rect, &accent_core::pdf::Rect::from_corners(a, b)),
                        );
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
                            // A stroke of one point is a dot, which a round cap draws from
                            // a zero-length line.
                            if points.len() == 1 {
                                builder.line_to(point(first).x(), point(first).y());
                            }
                        }
                    }
                }
                // The tool's own style, as the render will leave it: what the hand sees
                // must not change colour when the tile lands.
                let style = obj.ink_style(stroke.tool);
                let colour = as_rendered(style, dark, self.paper());
                let stroke_style = gsk::Stroke::new(style.width * layout.scale);
                stroke_style.set_line_cap(gsk::LineCap::Round);
                stroke_style.set_line_join(gsk::LineJoin::Round);
                snapshot.append_stroke(&builder.to_path(), &stroke_style, &colour);
            }
            // The stroke the Adjust tool holds: its box with eight handles, and while the
            // hand is on it, a ghost of the stroke where the drag has taken it.
            if let Some(a) = adjust.as_ref().filter(|a| a.page == index) {
                let point = |p: (f32, f32)| {
                    let (x, y) = accent_core::pdf::apply(a.matrix, p);
                    graphene::Point::new(rect.x + x * layout.scale, rect.y + y * layout.scale)
                };
                if a.handle.is_some()
                    && let Some(first) = a.points.first()
                {
                    let builder = gsk::PathBuilder::new();
                    builder.move_to(point(*first).x(), point(*first).y());
                    for &p in &a.points[1..] {
                        builder.line_to(point(p).x(), point(p).y());
                    }
                    let [r, g, b, _] = a.style.rgba;
                    let colour = theme::rgba([r, g, b], theme::GHOST_ALPHA);
                    let ghost = gsk::Stroke::new(a.style.width * layout.scale);
                    ghost.set_line_cap(gsk::LineCap::Round);
                    ghost.set_line_join(gsk::LineJoin::Round);
                    snapshot.append_stroke(&builder.to_path(), &ghost, &colour);
                }
                let moved = mapped(a.bounds, a.matrix);
                let frame = layout.rect_of(rect, &moved);
                let (l, t) = (frame.x(), frame.y());
                let (r, b) = (l + frame.width(), t + frame.height());
                snapshot.append_border(
                    &gsk::RoundedRect::from_rect(frame, 0.0),
                    &[1.0; 4],
                    &[accent; 4],
                );
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
                    let square =
                        graphene::Rect::new(hx - HANDLE / 2.0, hy - HANDLE / 2.0, HANDLE, HANDLE);
                    snapshot.append_color(&accent, &square);
                }
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
        }
        // A PDF dragged over the pages: the line across the gap its pages would go into.
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
        let stamp = (scale_milli, dark, cache.borrow().generation());
        wanted.append(&mut ahead);
        #[cfg(feature = "bench")]
        self.unrendered.replace(wanted.clone());
        #[cfg(feature = "bench")]
        self.unsharp.set(unsharp);
        if !wanted.is_empty() && (self.asked_for.get() != stamp || *self.asked.borrow() != wanted) {
            self.asked_for.set(stamp);
            *self.asked.borrow_mut() = wanted.clone();
            let handler = self.on_wants.borrow();
            if let Some(f) = handler.as_ref() {
                f(&obj, device_scale, dark, wanted);
            }
        }
    }
}

/// The colour a stroke will have once the tile carrying it arrives.
///
/// Two things happen to it on the way: every pixel of a page is put on the theme's paper–ink ramp
/// (see `accent_core::recolour::recolour_pixel`), and a highlighter multiplies into the page rather
/// than covering it. Painting the live stroke in its raw colour instead is why a highlighter used
/// to jump to another shade on dark and Solarized the moment the render landed.
///
/// The multiply is against the paper, which is what the stroke covers nearly all of; over a
/// letter it darkens a shade more than this, which is a pixel or two of the stroke's own width.
fn as_rendered(style: accent_core::pdf::InkStyle, dark: bool, paper: gdk::RGBA) -> gdk::RGBA {
    let [r, g, b, a] = style.rgba;
    let rgb = match crate::theme::page_colours(dark) {
        Some((page, ink)) => {
            let px = accent_core::recolour::recolour_pixel([r, g, b, 255], page, ink);
            [px[0], px[1], px[2]]
        }
        None => [r, g, b],
    };
    let colour = theme::rgba(rgb, f32::from(a) / 255.0);
    match style.multiply {
        true => gdk::RGBA::new(
            colour.red() * paper.red(),
            colour.green() * paper.green(),
            colour.blue() * paper.blue(),
            colour.alpha(),
        ),
        false => colour,
    }
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
/// produce so nothing flashes when they do.
pub(super) fn paper(dark: bool) -> gdk::RGBA {
    match crate::theme::view_bg(dark).parse::<gdk::RGBA>() {
        Ok(colour) => colour,
        Err(_) => gdk::RGBA::WHITE,
    }
}
