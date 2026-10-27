//! PDF pointer gestures for selection, drawing, erasing and stroke adjustment.
//! Choose tools by input device and handle stylus dots.

use adw::prelude::*;
use gtk::glib::translate::IntoGlib;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene};

use super::PdfView;
use crate::pdf::Span;
use crate::pdf::geometry::page_at;
use crate::pdf::protocol::{Pass, fresh_id};
use crate::pdf::tools::{
    ADJUST_RADIUS, HANDLE, Hand, Handle, Mode, Selected, Stroke, drag_matrix, drag_of, handle_at,
    mapped, shape_of, snap,
};

impl PdfView {
    /// The pointer's controllers: hover, a drag that selects, draws, erases or adjusts by the
    /// tool in hand, and a click that follows a link or, on the strip, goes to a page.
    pub(super) fn wire_gestures(&self) {
        let obj = self.clone();
        let motion = gtk::EventControllerMotion::new();
        motion.connect_motion(glib::clone!(
            #[weak]
            obj,
            move |motion, x, y| {
                obj.imp().over.set(Some((x, y)));
                obj.show_dot(motion);
                let handler = obj.imp().on_motion.borrow();
                if let Some(f) = handler.as_ref() {
                    f(&obj, x, y);
                }
            }
        ));
        motion.connect_leave(glib::clone!(
            #[weak]
            obj,
            move |_| obj.imp().over.set(None)
        ));
        obj.add_controller(motion);

        // Dragging over the page selects the text under it. Claimed on the first motion
        // rather than on the press, so a plain click still reaches the link handler below.
        // A finger's drag is never claimed here: the scrolled window's own touch drag takes
        // it past GTK's drag threshold and pans, kinetically, and its long press stops that.
        let drag = gtk::GestureDrag::new();
        drag.connect_drag_begin(glib::clone!(
            #[weak]
            obj,
            move |gesture, x, y| {
                obj.imp().drag_from.set(Some((x, y)));
                obj.imp().held.set(false);
                // A pen or an eraser claims the sequence at once, unlike a selection, which
                // waits to see whether the pointer moves: a stroke that let the scrolled
                // window have the first few pixels would scroll the page under the hand.
                let mode = obj.effective_mode(gesture);
                obj.imp().drag_mode.set(mode);
                let Some(mode) = mode else {
                    return;
                };
                match mode {
                    Mode::Select => {}
                    mode if mode.draws() => {
                        let Some((page, _, _)) = obj.page_point(x, y) else {
                            return;
                        };
                        gesture.set_state(gtk::EventSequenceState::Claimed);
                        let at = obj.point_on(page, x, y);
                        obj.imp().strokes.borrow_mut().push(Stroke {
                            page,
                            points: vec![at],
                            tool: mode,
                            done: false,
                        });
                    }
                    Mode::Adjust => {
                        if obj.adjust_press(x, y) {
                            gesture.set_state(gtk::EventSequenceState::Claimed);
                        }
                    }
                    _ => {
                        gesture.set_state(gtk::EventSequenceState::Claimed);
                        obj.imp().erasing.set(None);
                        obj.imp().erased.set(false);
                        obj.erase_at(x, y);
                    }
                }
            }
        ));
        drag.connect_drag_update(glib::clone!(
            #[weak]
            obj,
            move |gesture, dx, dy| {
                let Some((x, y)) = obj.imp().drag_from.get() else {
                    return;
                };
                // A finger's drag is a selection once a long press has made it one, and until
                // then the scrolled window's to pan.
                let held = obj.imp().held.get().then_some(Mode::Select);
                let Some(mode) = obj.imp().drag_mode.get().or(held) else {
                    return;
                };
                match mode {
                    Mode::Select => {
                        // A few pixels of travel is a click with a shaky hand, not a
                        // selection. And the strip selects nothing: a drag there moves the
                        // page, which is its drag source's to claim (`organize.rs`).
                        if dx.abs() < 3.0 && dy.abs() < 3.0 || obj.imp().thumbnails.get() {
                            return;
                        }
                        gesture.set_state(gtk::EventSequenceState::Claimed);
                        obj.select_between(x, y, x + dx, y + dy);
                    }
                    mode if mode.draws() => {
                        // The page is whichever one the stroke began on: a hand that runs
                        // over the edge keeps drawing on the paper it started on.
                        let page = match obj.imp().strokes.borrow().last() {
                            Some(stroke) if !stroke.done => stroke.page,
                            _ => return,
                        };
                        let at = obj.point_on(page, x + dx, y + dy);
                        let merged: Vec<(f32, f32)> = match mode.shapes() {
                            true => Vec::new(),
                            false => merged(gesture, &obj)
                                .into_iter()
                                .map(|(x, y)| obj.point_on(page, x, y))
                                .collect(),
                        };
                        if let Some(stroke) = obj.imp().strokes.borrow_mut().last_mut()
                            && !stroke.done
                        {
                            // A shape is its two ends, the press and wherever the hand is.
                            if mode.shapes() {
                                let anchor = stroke.points[0];
                                stroke.points.truncate(1);
                                stroke.points.push(match mode {
                                    Mode::Line => snap(anchor, at),
                                    _ => at,
                                });
                            } else {
                                stroke.points.extend(merged);
                                stroke.points.push(at);
                            }
                        }
                        obj.queue_draw();
                    }
                    Mode::Adjust => obj.adjust_drag(dx, dy),
                    _ => obj.erase_at(x + dx, y + dy),
                }
            }
        ));
        drag.connect_drag_end(glib::clone!(
            #[weak]
            obj,
            move |drag, dx, dy| {
                let from = obj.imp().drag_from.replace(None);
                let held = obj.imp().held.replace(false);
                let mode = obj.imp().drag_mode.get();
                if mode == Some(Mode::Adjust) {
                    return obj.adjust_release();
                }
                if let Some(mode) = mode.filter(|m| m.draws()) {
                    let mut strokes = obj.imp().strokes.borrow_mut();
                    // A click in a shape mode is no shape, and leaves nothing behind waiting
                    // for a tile that will never carry it.
                    let click = matches!(strokes.last(), Some(s) if !s.done && mode.shapes()
                        && shape_of(mode, s.points[0], s.points[s.points.len() - 1]).is_none());
                    if click {
                        strokes.pop();
                    }
                    // The stroke stays painted until a tile carries it, so the page never
                    // blinks between the hand letting go and pdfium answering.
                    let finished = match strokes.last_mut() {
                        Some(stroke) if !stroke.done => {
                            stroke.done = true;
                            Some((stroke.page, stroke.points.clone()))
                        }
                        _ => None,
                    };
                    drop(strokes);
                    #[cfg(feature = "bench")]
                    if let Some((_, points)) = &finished {
                        obj.imp().drawn.set(obj.imp().drawn.get() + points.len());
                    }
                    if let Some((page, points)) = finished
                        && let Some(f) = obj.imp().on_ink.borrow().as_ref()
                    {
                        f(page, points);
                    }
                    return;
                }
                // The same few pixels the update handler calls a shaky hand rather than a
                // selection: what is left is a click, and a click can be on a highlight. A long
                // press is none, nor a touch a pinch took: a denial leaves its state, and a
                // cancel forgot where the drag began.
                let taken = drag.current_sequence().is_some_and(|sequence| {
                    drag.sequence_state(&sequence) == gtk::EventSequenceState::Denied
                });
                if let Some((x, y)) = from
                    && dx.abs() < 3.0
                    && dy.abs() < 3.0
                    && !held
                    && !taken
                    && !obj.imp().thumbnails.get()
                {
                    // A finger's press waited to be known as a tap: a touch that pans has not
                    // pressed the link under it.
                    if mode.is_none()
                        && let Some(f) = obj.imp().on_pressed.borrow().as_ref()
                    {
                        f(&obj, x, y);
                    }
                    if let Some(f) = obj.imp().on_clicked.borrow().as_ref() {
                        f(&obj, x, y, drag.current_event_state());
                    }
                }
            }
        ));
        drag.connect_cancel(glib::clone!(
            #[weak]
            obj,
            move |_, _| obj.imp().drag_from.set(None)
        ));
        obj.add_controller(drag);

        // A finger held still selects from where it is held, as on a phone: the scrolled
        // window's own long press stops its drag from panning that touch, and this one lets
        // the drag above select.
        let hold = gtk::GestureLongPress::new();
        hold.set_touch_only(true);
        hold.connect_pressed(glib::clone!(
            #[weak]
            obj,
            move |_, _, _| obj.imp().held.set(true)
        ));
        obj.add_controller(hold);

        let click = gtk::GestureClick::builder().button(0).build();
        click.connect_pressed(glib::clone!(
            #[weak]
            obj,
            move |gesture, _, x, y| {
                obj.grab_focus();
                // While a pen is out, a press is the start of a mark, not a link to follow; a
                // finger's may be the start of a pan, and waits to be a tap (`drag-end`).
                if obj.effective_mode(gesture) != Some(Mode::Select) {
                    return;
                }
                match gesture.current_button() {
                    1 if obj.imp().thumbnails.get() => {}
                    1 => {
                        let handler = obj.imp().on_pressed.borrow();
                        if let Some(f) = handler.as_ref() {
                            f(&obj, x, y);
                        }
                    }
                    _ => {}
                }
            }
        ));
        // A thumbnail is a button: clicking one goes to its page. On the release, as a button
        // does, because a press is also how a drag that moves the page begins, and that must
        // not send the reader to the page first. A drag takes the sequence, so it never gets
        // here.
        click.connect_released(glib::clone!(
            #[weak]
            obj,
            move |gesture, _, x, y| {
                if gesture.current_button() != 1 || !obj.imp().thumbnails.get() {
                    return;
                }
                let page = obj.page_point(x, y).map(|(page, _, _)| page);
                let handler = obj.imp().on_goto.borrow();
                if let (Some(page), Some(f)) = (page, handler.as_ref()) {
                    f(page);
                }
            }
        ));
        obj.add_controller(click);
    }

    /// What a press means, by what pressed and the tool in hand (`tools::drag_of`): `None` for
    /// a finger, which moves the page. A mouse draws while nothing but a mouse is plugged in;
    /// with a pen attached the hand selects, unless the preference says otherwise.
    fn effective_mode(&self, controller: &impl IsA<gtk::EventController>) -> Option<Mode> {
        let mouse_draws = self.imp().style.borrow().mouse || !stylus_attached();
        drag_of(self.imp().mode.get(), hand_of(controller), mouse_draws)
    }

    /// Under a stylus with a tool in hand the pointer is a dot where the ink will go; under
    /// anything else it is the arrow a tool shows. Set only when that changes: a cursor per event
    /// of travel is what the link hover avoids too.
    fn show_dot(&self, controller: &impl IsA<gtk::EventController>) {
        let pen = matches!(hand_of(controller), Hand::Pen { .. });
        let dot = self.imp().mode.get() != Mode::Select && pen;
        if self.imp().dot.replace(dot) == dot {
            return;
        }
        match dot {
            true => self.set_cursor(Some(&dot_cursor())),
            false => self.set_cursor_from_name(Some("default")),
        }
    }

    /// The Adjust tool's press: a handle of the stroke already held, else whichever stroke is
    /// under the pointer, else nothing. Whether the press took hold of anything.
    fn adjust_press(&self, x: f64, y: f64) -> bool {
        let Some((page, at)) = self.nearest_page_point(x, y) else {
            return false;
        };
        let grip = HANDLE / self.imp().layout.borrow().scale;
        let mut adjust = self.imp().adjust.borrow_mut();
        if let Some(a) = adjust.as_mut()
            && a.page == page
            && let Some(handle) = handle_at(a.bounds, at, grip)
        {
            a.handle = Some(handle);
            return true;
        }
        // The stroke under the pointer first, then the first whose box it is inside: a thin
        // line wants the pointer on it, a circle's empty middle still belongs to the circle.
        let inks = self.imp().inks.borrow();
        let found = inks.get(&page).and_then(|inks| {
            inks.iter()
                .find(|(_, i)| accent_core::pdf::hit(&i.points, at, ADJUST_RADIUS))
                .or_else(|| {
                    inks.iter()
                        .find(|(_, i)| handle_at(i.bounds, at, grip).is_some())
                })
        });
        *adjust = found.map(|(id, i)| Selected {
            page,
            id: *id,
            points: i.points.clone(),
            bounds: i.bounds,
            style: i.style,
            handle: Some(Handle::Move),
            matrix: accent_core::pdf::IDENTITY,
        });
        let held = adjust.is_some();
        drop(adjust);
        self.queue_draw();
        held
    }

    fn adjust_drag(&self, dx: f64, dy: f64) {
        let scale = self.imp().layout.borrow().scale;
        if let Some(a) = self.imp().adjust.borrow_mut().as_mut()
            && let Some(handle) = a.handle
        {
            a.matrix = drag_matrix(handle, a.bounds, dx as f32 / scale, dy as f32 / scale);
        }
        self.queue_draw();
    }

    /// The hand let go: what the drag amounted to goes to the tab, and the selection keeps
    /// painting where the stroke will be until the fresh list confirms it.
    fn adjust_release(&self) {
        let sent = {
            let mut adjust = self.imp().adjust.borrow_mut();
            let Some(a) = adjust.as_mut() else {
                return;
            };
            a.handle = None;
            let m = std::mem::replace(&mut a.matrix, accent_core::pdf::IDENTITY);
            if m == accent_core::pdf::IDENTITY {
                return;
            }
            a.points = a
                .points
                .iter()
                .map(|&p| accent_core::pdf::apply(m, p))
                .collect();
            a.bounds = mapped(a.bounds, m);
            (a.page, a.id, m)
        };
        if let Some(f) = self.imp().on_transform.borrow().as_ref() {
            f(sent.0, sent.1, sent.2);
        }
        self.queue_draw();
    }

    /// Tell the tab which strokes the eraser passed over since it was last reported, and what a
    /// partial eraser leaves of each.
    fn erase_at(&self, x: f64, y: f64) {
        let Some((page, _)) = self.nearest_page_point(x, y) else {
            return;
        };
        let at = self.point_on(page, x, y);
        // The line from where the drag was last reported on this page, not the point alone: a
        // drag reports once a frame, and a quick pass lands its reports either side of a stroke.
        let from = match self.imp().erasing.replace(Some((page, at))) {
            Some((was, from)) if was == page => from,
            _ => at,
        };
        let (radius, partial) = {
            let style = self.imp().style.borrow();
            (style.eraser_radius, style.eraser_partial)
        };
        // Hit-tested here, against the strokes the tab keeps for this page, rather than on the
        // render thread: that read and flattened every annotation on the page under the pdfium
        // lock, once per pointer event of the drag. Taken out of the list at once, so a drag
        // that passes over one again does not ask for it twice; what a partial eraser leaves goes
        // in at once, named here, so the next report can cut it again before the thread answers.
        let mut taken = Vec::new();
        if let Some(list) = self.imp().inks.borrow_mut().get_mut(&page) {
            for (id, ink) in std::mem::take(list) {
                // To the stroke's edge rather than its middle: a highlighter is 14 pt across.
                let reach = radius + ink.style.width / 2.0;
                if !accent_core::pdf::swept(&ink.points, from, at, reach) {
                    list.push((id, ink));
                    continue;
                }
                if !partial {
                    taken.push((id, None));
                    continue;
                }
                // Another editor's stroke that a cut would redraw wrongly is left alone.
                let runs = ink
                    .cuttable
                    .then(|| accent_core::pdf::cut(&ink.points, from, at, reach));
                let Some(Some(runs)) = runs else {
                    list.push((id, ink));
                    continue;
                };
                let mut pieces = Vec::with_capacity(runs.len());
                for points in runs {
                    let name = fresh_id();
                    pieces.push(name);
                    list.push((name, piece_of(&ink, points)));
                }
                taken.push((
                    id,
                    Some(Pass {
                        from,
                        to: at,
                        radius,
                        pieces,
                    }),
                ));
            }
        }
        if let Some(f) = self.imp().on_erase.borrow().as_ref() {
            for (id, pass) in taken {
                self.imp().erases.set(self.imp().erases.get() + 1);
                f(page, id, pass, self.imp().erased.replace(true));
            }
        }
    }

    /// A point on a page in that page's own points, clamped into the paper.
    ///
    /// The clamp is the page edge: a stroke pulled off the paper stops at it rather than being
    /// drawn where no viewer would show it.
    fn point_on(&self, page: usize, x: f64, y: f64) -> (f32, f32) {
        let (cx, cy) = self.content_at(x, y);
        let layout = self.imp().layout.borrow();
        let at = layout.to_page(page, cx, cy);
        let Some(((x, y), (pw, ph))) = at.zip(self.page_size(page)) else {
            return (0.0, 0.0);
        };
        (x.clamp(0.0, pw), y.clamp(0.0, ph))
    }

    /// Report the two ends of a drag, each as a page and a point on it.
    fn select_between(&self, x0: f64, y0: f64, x1: f64, y1: f64) {
        let (Some(from), Some(to)) = (
            self.nearest_page_point(x0, y0),
            self.nearest_page_point(x1, y1),
        ) else {
            return;
        };
        let handler = self.imp().on_select.borrow();
        if let Some(f) = handler.as_ref() {
            f(self, Span { from, to });
        }
    }

    /// Map off-paper points to page coordinates for selection drags through margins and gaps.
    /// Coordinates may be negative or exceed page height; nearest-glyph lookup anchors them
    /// without clamping.
    fn nearest_page_point(&self, x: f64, y: f64) -> Option<(usize, (f32, f32))> {
        let (cx, cy) = self.content_at(x, y);
        let layout = self.imp().layout.borrow();
        let page = page_at(&layout, f64::from(cy));
        Some((page, layout.to_page(page, cx, cy)?))
    }
}

/// The positions GDK merged into the motion `gesture` is handling, oldest first, in `view`'s
/// coordinates. A frame's motions reach a gesture as the last of them, the earlier ones its
/// history, in surface coordinates; a pen's too, which is all `GestureStylus::backlog` maps.
fn merged(gesture: &gtk::GestureDrag, view: &PdfView) -> Vec<(f64, f64)> {
    let event = gesture.current_event();
    let event = event.filter(|e| e.event_type() == gdk::EventType::MotionNotify);
    let (Some(event), Some(native)) = (event, view.native()) else {
        return Vec::new();
    };
    let (sx, sy) = native.surface_transform();
    let (x, y) = (gdk::AxisUse::X.into_glib(), gdk::AxisUse::Y.into_glib());
    event
        .history()
        .iter()
        .filter_map(|c| {
            let (hx, hy) = (c.axes()[x as usize] - sx, c.axes()[y as usize] - sy);
            let at = graphene::Point::new(hx as f32, hy as f32);
            let at = native.compute_point(view, &at)?;
            Some((f64::from(at.x()), f64::from(at.y())))
        })
        .collect()
}

/// Whether the seat has a stylus at all. Asked on the press rather than cached: a tablet can be
/// plugged in mid-session.
fn stylus_attached() -> bool {
    gdk::Display::default()
        .and_then(|d| d.default_seat())
        .is_some_and(|s| !s.devices(gdk::SeatCapabilities::TABLET_STYLUS).is_empty())
}

/// A piece a partial eraser leaves of `ink`: its style along `points`, as the render thread will
/// draw it. The place in `/Annots` is the parent's and means nothing here; ids name strokes now.
fn piece_of(
    ink: &accent_core::pdf::InkShape,
    points: Vec<(f32, f32)>,
) -> accent_core::pdf::InkShape {
    let bounds = points
        .iter()
        .map(|&p| accent_core::pdf::Rect::from_corners(p, p))
        .reduce(accent_core::pdf::Rect::union)
        .unwrap_or(ink.bounds)
        .grow(ink.style.width / 2.0 + 1.0);
    accent_core::pdf::InkShape {
        index: ink.index,
        points,
        bounds,
        style: ink.style,
        cuttable: true,
    }
}

/// What pressed: a stylus by its tool, since Wayland exposes a tablet as a logical mouse, or by
/// its device's source on X11 without libwacom; a finger by its touchscreen.
fn hand_of(controller: &impl IsA<gtk::EventController>) -> Hand {
    let tool = controller.current_event().and_then(|e| e.device_tool());
    let source = controller.current_event_device().map(|d| d.source());
    match (tool, source) {
        (Some(tool), _) => Hand::Pen {
            eraser: tool.tool_type() == gdk::DeviceToolType::Eraser,
        },
        (None, Some(gdk::InputSource::Pen)) => Hand::Pen { eraser: false },
        (None, Some(gdk::InputSource::Touchscreen)) => Hand::Finger,
        _ => Hand::Mouse,
    }
}

/// The stylus's pointer: a dark dot ringed in white so it reads on any paper, its hotspot in the
/// middle. Drawn here because no cursor theme names a dot, and built once per thread.
fn dot_cursor() -> gdk::Cursor {
    const SIZE: i32 = 7;
    thread_local! {
        static DOT: gdk::Cursor = {
            let middle = (SIZE / 2) as f32;
            let pixels: Vec<u8> = (0..SIZE * SIZE)
                .flat_map(|at| {
                    let d = ((at % SIZE) as f32 - middle).hypot((at / SIZE) as f32 - middle);
                    // How much of the pixel the ring and the dot inside it cover: a soft pixel
                    // at each edge rather than a jagged one.
                    let (ring, dot) = ((3.5 - d).clamp(0.0, 1.0), (2.5 - d).clamp(0.0, 1.0));
                    let grey = (255.0 * (1.0 - dot)) as u8;
                    [grey, grey, grey, (255.0 * ring) as u8]
                })
                .collect();
            let texture = gdk::MemoryTexture::new(
                SIZE,
                SIZE,
                gdk::MemoryFormat::R8g8b8a8,
                &glib::Bytes::from_owned(pixels),
                SIZE as usize * 4,
            );
            gdk::Cursor::from_texture(&texture, SIZE / 2, SIZE / 2, None)
        };
    }
    DOT.with(Clone::clone)
}
