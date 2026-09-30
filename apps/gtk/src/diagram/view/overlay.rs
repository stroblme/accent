//! What the canvas draws over the page: the selection, its handles, the connection points in
//! reach and whatever a drag is doing.

use accent_drawio::Rect;
use accent_drawio::geom::{self, rotate};
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{graphene, gsk};

use super::drag::Drag;
use super::preview::Preview;
use super::{DiagramView, Edit};
use crate::diagram::geometry::{self, Frame, HANDLE, Handle, Sheet, TOLERANCE};
use crate::diagram::paint;
use crate::diagram::tools::Tool;
use crate::theme;

impl DiagramView {
    /// The selection, its handles and whatever a drag is doing, over the page.
    pub(super) fn paint_overlays(&self, snapshot: &gtk::Snapshot, sheet: &Sheet, frame: &Frame) {
        let imp = self.imp();
        let accent = theme::accent();
        let outline = |r: &Rect| {
            snapshot.append_border(
                &gsk::RoundedRect::from_rect(paint::grect(r), 0.0),
                &[1.0; 4],
                &[accent; 4],
            );
        };
        // Borrowed, not cloned: this runs every frame, and nothing it calls changes either.
        let drag = imp.drag.borrow();
        let pointer = imp.pointer.get();
        let free = imp.free.get();
        let selection = imp.selection.borrow();
        let moving = matches!(drag.as_ref(), Some(Drag::Move { .. } | Drag::Rotate { .. }))
            && imp.moved.get();
        // Where the live preview has the selection: its frames and handles go with it, as
        // draw.io's do (`mxGraphHandler.redrawHandles`).
        let preview = imp.preview.borrow();
        let shown = match preview.as_ref() {
            Some(Preview::Live(live)) => live.shown.as_ref(),
            _ => None,
        };
        let placed = |id: &str, r: Rect| match shown {
            Some(Edit::Move { ids, delta }) if ids.iter().any(|m| sheet.is_within(id, m)) => {
                (r.translate(delta.x, delta.y), sheet.rotation(id))
            }
            Some(Edit::Resize { id: resized, rect }) if resized == id => {
                (*rect, sheet.rotation(id))
            }
            Some(Edit::Rotate {
                id: turned,
                degrees,
            }) if turned == id => (r, *degrees),
            _ => (r, sheet.rotation(id)),
        };
        // A page rectangle turned `rotation` degrees, outlined on screen.
        let turned = |r: &Rect, rotation: f64| {
            if rotation == 0.0 {
                return outline(&frame.rect(r));
            }
            let builder = gsk::PathBuilder::new();
            for (i, corner) in geom::corners(r, r.centre(), rotation)
                .into_iter()
                .enumerate()
            {
                let c = frame.to_content(corner);
                match i {
                    0 => builder.move_to(c.x as f32, c.y as f32),
                    _ => builder.line_to(c.x as f32, c.y as f32),
                }
            }
            builder.close();
            snapshot.append_stroke(&builder.to_path(), &gsk::Stroke::new(1.0), &accent);
        };
        for id in selection.iter() {
            if let Some(r) = sheet.frame_of(id) {
                let (r, rotation) = placed(id, r);
                turned(&r, rotation);
            }
        }
        if let [id] = selection.as_slice()
            && let Some(r) = sheet.rect(id)
            && !sheet.is_pinned(id)
            && !moving
        {
            let (r, rotation) = placed(id, r);
            for h in Handle::ALL {
                let at = frame.to_content(rotate(h.at(&r), r.centre(), rotation));
                let square = Rect::new(at.x - HANDLE / 2.0, at.y - HANDLE / 2.0, HANDLE, HANDLE);
                snapshot.append_color(&accent, &paint::grect(&square));
            }
            // The rotate handle, a ring beyond the top-right corner, turned with the frame.
            if sheet.is_turnable(id) {
                let b = frame.rect(&r);
                let at = rotate(geometry::rotate_handle(&b), b.centre(), rotation);
                let ring = gsk::PathBuilder::new();
                ring.add_circle(
                    &graphene::Point::new(at.x as f32, at.y as f32),
                    (HANDLE / 2.0) as f32,
                );
                snapshot.append_stroke(&ring.to_path(), &gsk::Stroke::new(1.5), &accent);
            }
        }
        // The connector in hand: the shape under the pointer shows its connection points as
        // draw.io's small crosses, and the one an end would pin to is lit.
        if imp.tool.get() == Tool::Connector {
            let (tolerance, reach) = (TOLERANCE / frame.scale, HANDLE / frame.scale);
            let near = sheet.anchor_near(pointer, reach);
            let shape = match &near {
                Some((id, ..)) => Some(id.clone()),
                None => sheet.vertex_at(pointer, tolerance),
            };
            let arm = (HANDLE / 2.0 - 1.0) as f32;
            for (at, _) in shape.as_deref().map_or(&[][..], |id| sheet.anchors_of(id)) {
                let c = frame.to_content(*at);
                let (x, y) = (c.x as f32, c.y as f32);
                let builder = gsk::PathBuilder::new();
                builder.move_to(x - arm, y - arm);
                builder.line_to(x + arm, y + arm);
                builder.move_to(x + arm, y - arm);
                builder.line_to(x - arm, y + arm);
                snapshot.append_stroke(&builder.to_path(), &gsk::Stroke::new(1.5), &accent);
                if near.as_ref().is_some_and(|n| n.1 == *at) {
                    let lit = Rect::new(c.x - HANDLE / 2.0, c.y - HANDLE / 2.0, HANDLE, HANDLE);
                    let tint = theme::at(accent, theme::HIGHLIGHT_ALPHA);
                    snapshot.append_color(&tint, &paint::grect(&lit));
                }
            }
        }
        let Some(drag) = drag.as_ref().filter(|_| imp.moved.get()) else {
            return;
        };
        match drag {
            Drag::Move {
                from,
                ids,
                bounds,
                guides,
                ..
            } => {
                let (d, lines) = self.move_delta(*from, pointer, bounds, guides, free);
                // Too many cells to show live: their box moves, dashed as draw.io's preview
                // shape is (`mxGraphHandler.createPreviewShape`).
                let moved = ids.iter().filter_map(|id| sheet.frame_of(id));
                if matches!(*preview, Some(Preview::Boxed))
                    && let Some(r) = moved.reduce(|a, b| a.union(&b))
                {
                    let builder = gsk::PathBuilder::new();
                    builder.add_rect(&paint::grect(&frame.rect(&r.translate(d.x, d.y))));
                    let stroke = gsk::Stroke::new(1.0);
                    stroke.set_dash(&[3.0, 3.0]);
                    snapshot.append_stroke(&builder.to_path(), &stroke, &accent);
                }
                // The guides, a pixel wide whatever the zoom.
                if !lines.is_empty() {
                    let builder = gsk::PathBuilder::new();
                    for (a, b) in lines {
                        let (a, b) = (frame.to_content(a), frame.to_content(b));
                        builder.move_to(a.x as f32, a.y as f32);
                        builder.line_to(b.x as f32, b.y as f32);
                    }
                    snapshot.append_stroke(&builder.to_path(), &gsk::Stroke::new(1.0), &accent);
                }
            }
            Drag::Band { from, .. } => {
                let r = frame.rect(&Rect::from_corners(*from, pointer));
                snapshot.append_color(
                    &theme::at(accent, theme::HIGHLIGHT_ALPHA),
                    &paint::grect(&r),
                );
                outline(&r);
            }
            Drag::Draw { tool, from } => {
                let r = frame.rect(&Rect::from_corners(*from, pointer));
                let builder = gsk::PathBuilder::new();
                match tool {
                    Tool::Ellipse => builder.add_rounded_rect(&gsk::RoundedRect::from_rect(
                        paint::grect(&r),
                        (r.w.min(r.h) / 2.0) as f32,
                    )),
                    _ => builder.add_rect(&paint::grect(&r)),
                }
                snapshot.append_stroke(&builder.to_path(), &gsk::Stroke::new(1.0), &accent);
            }
            Drag::Connect { from } => {
                let (tolerance, reach) = (TOLERANCE / frame.scale, HANDLE / frame.scale);
                let (s, t) = sheet.connect_ends(*from, pointer, tolerance, reach);
                let (a, b) = (frame.to_content(s.1), frame.to_content(t.1));
                let builder = gsk::PathBuilder::new();
                builder.move_to(a.x as f32, a.y as f32);
                builder.line_to(b.x as f32, b.y as f32);
                snapshot.append_stroke(&builder.to_path(), &gsk::Stroke::new(1.0), &accent);
            }
            Drag::Resize { .. } | Drag::Rotate { .. } | Drag::Pan { .. } => {}
        }
    }
}
