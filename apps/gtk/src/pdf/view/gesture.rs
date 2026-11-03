//! PDF pointer gestures for text selection, drawing, erasing and the Select tool's lasso, picks
//! and handles. Choose tools by input device and handle stylus dots.

use adw::prelude::*;
use gtk::glib::translate::IntoGlib;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene};

use super::PdfView;
use crate::pdf::Span;
use crate::pdf::geometry::page_at;
use crate::pdf::protocol::{NamedInk, NamedPicture, Pass, fresh_id};
use crate::pdf::tools::{
    HANDLE, Hand, Handle, Lasso, Mode, PICK_RADIUS, Picked, Selection, Stroke, clamp_move,
    drag_matrix, drag_of, handle_at, rotate_handle, shape_of, snap, turn_matrix, width_of,
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
                let hand = hand_of(motion);
                obj.show_dot(hand);
                obj.note_hand(hand);
                obj.hover(x, y);
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
                obj.note_hand(hand_of(gesture));
                // A pen or an eraser claims the sequence at once, unlike a selection, which
                // waits to see whether the pointer moves: a stroke that let the scrolled
                // window have the first few pixels would scroll the page under the hand.
                let mode = obj.effective_mode(gesture);
                obj.imp().drag_mode.set(mode);
                let Some(mode) = mode else {
                    return;
                };
                match mode {
                    Mode::Read => {}
                    // Typing into a box the tab opens, or into a text of ours under the press.
                    Mode::Text => {
                        let Some((page, _, _)) = obj.page_point(x, y) else {
                            return;
                        };
                        gesture.set_state(gtk::EventSequenceState::Claimed);
                        let at = obj.point_on(page, x, y);
                        let texts = obj.imp().texts.borrow();
                        let hit = (texts.get(&page).into_iter().flatten())
                            .find(|(_, t)| t.bounds.contains(at))
                            .cloned();
                        drop(texts);
                        if let Some(f) = obj.imp().on_type.borrow().as_ref() {
                            f(page, at, hit);
                        }
                    }
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
                    // Claimed at once, as a pen's stroke is: a lasso is drawn from the press.
                    Mode::Select => {
                        gesture.set_state(gtk::EventSequenceState::Claimed);
                        let state = gesture.current_event_state();
                        obj.select_press(x, y, state.contains(crate::platform::PRIMARY_MODIFIER));
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
                let held = obj.imp().held.get().then_some(Mode::Read);
                let Some(mode) = obj.imp().drag_mode.get().or(held) else {
                    return;
                };
                match mode {
                    Mode::Text => {}
                    Mode::Read => {
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
                    Mode::Select => {
                        let state = gesture.current_event_state();
                        let shift = state.contains(gdk::ModifierType::SHIFT_MASK);
                        obj.select_drag(gesture, dx, dy, shift)
                    }
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
                // The pages an eraser drag changed are read again now it is over.
                if obj.imp().erasing.take().is_some()
                    && let Some(f) = obj.imp().on_erased.borrow().as_ref()
                {
                    f();
                }
                if mode == Some(Mode::Select) {
                    return obj.select_release();
                }
                if mode == Some(Mode::Text) {
                    return;
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
                if obj.effective_mode(gesture) != Some(Mode::Read) {
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
    fn show_dot(&self, hand: Hand) {
        let pen = matches!(hand, Hand::Pen { .. });
        let dot = self.imp().mode.get() != Mode::Read && pen;
        if self.imp().dot.replace(dot) == dot {
            return;
        }
        match (dot, self.imp().mode.get()) {
            (true, _) => self.set_cursor(Some(&dot_cursor())),
            (false, Mode::Text) => self.set_cursor_from_name(Some("text")),
            (false, _) => self.set_cursor_from_name(Some("default")),
        }
    }

    /// Tell the tab when the pen's eraser end comes to the page, and when another hand does after
    /// it, so the ring shows the eraser while that end is in use. Its hover is enough: GTK hands a
    /// widget no proximity events, but a stylus in the air moves the pointer with its tool, on
    /// Wayland in the same frame as its proximity, before it touches.
    fn note_hand(&self, hand: Hand) {
        let eraser = hand == Hand::Pen { eraser: true };
        if self.imp().eraser.replace(eraser) != eraser
            && let Some(f) = self.imp().on_eraser.borrow().as_ref()
        {
            f(eraser);
        }
    }

    /// As though the pen's eraser end, or its tip, had come to the page, which no stylus under
    /// Xvfb can.
    #[cfg(feature = "bench")]
    pub fn bench_pen(&self, eraser: bool) {
        self.note_hand(Hand::Pen { eraser });
    }

    /// The Select tool's press. The rotate ring, a corner or an edge of what it holds takes hold
    /// of all of it, and so does a press on one of its items or anywhere in their box; a stroke
    /// or a text of ours not held is picked alone, held by its middle, so a drag moves it at once.
    /// With Ctrl a stroke or a text is added or let go of, and nothing is taken hold of. Anywhere
    /// else starts a lasso, which with Ctrl adds to what is held.
    fn select_press(&self, x: f64, y: f64, ctrl: bool) {
        let Some((page, at)) = self.nearest_page_point(x, y) else {
            return;
        };
        let handle = self.handle_under(x, y);
        let hit = self.pick_at(page, at);
        let mut picked = self.imp().picked.borrow_mut();
        let held = |id: u32| {
            (picked.as_ref().filter(|s| s.page == page))
                .is_some_and(|s| s.items.iter().any(|item| item.id == id))
        };
        let grab = match (handle, hit, ctrl) {
            (Some(h), _, false) if h != Handle::Move => Some(h),
            (_, Some(item), true) => {
                match picked.as_mut().filter(|s| s.page == page) {
                    Some(s) => match s.items.iter().position(|i| i.id == item.id) {
                        Some(at) => drop(s.items.remove(at)),
                        None => s.items.push(item),
                    },
                    None => *picked = Some(Selection::of(page, vec![item])),
                }
                None
            }
            (_, Some(item), false) if !held(item.id) => {
                *picked = Some(Selection::of(page, vec![item]));
                Some(Handle::Move)
            }
            (_, Some(_), false) | (Some(Handle::Move), None, false) => Some(Handle::Move),
            _ => {
                let lasso = Lasso {
                    page,
                    points: vec![at],
                    add: ctrl,
                };
                self.imp().lasso.replace(Some(lasso));
                None
            }
        };
        if picked.as_ref().is_some_and(|s| s.items.is_empty()) {
            *picked = None;
        }
        if let Some(s) = picked.as_mut() {
            s.handle = grab;
            s.matrix = accent_core::pdf::IDENTITY;
        }
        drop(picked);
        self.picked_changed();
    }

    /// Pick the stroke or text of ours at this point of the view, unless the Select tool holds it
    /// already: what a secondary click does before its menu offers the commands.
    pub fn pick_under(&self, x: f64, y: f64) {
        let Some((page, at)) = self.nearest_page_point(x, y) else {
            return;
        };
        let Some(item) = self.pick_at(page, at) else {
            return;
        };
        let held = (self.imp().picked.borrow().as_ref())
            .is_some_and(|s| s.page == page && s.items.iter().any(|i| i.id == item.id));
        if !held {
            self.imp()
                .picked
                .replace(Some(Selection::of(page, vec![item])));
            self.picked_changed();
        }
    }

    /// Which handle of what the Select tool holds is under this point of the view: its rotate
    /// ring first, then a corner, an edge or its middle. Texts alone have only their middle.
    fn handle_under(&self, x: f64, y: f64) -> Option<Handle> {
        let picked = self.imp().picked.borrow();
        let s = picked.as_ref()?;
        let scale = self.imp().layout.borrow().scale;
        let grip = HANDLE / scale;
        // Measured on the selection's page, wherever the pointer is: the ring may stand off it.
        let at = self.on_page(s.page, x, y)?;
        let b = s.bounds();
        let (rx, ry) = rotate_handle(b, scale);
        if !s.moves_only() && (at.0 - rx).hypot(at.1 - ry) <= grip {
            return Some(Handle::Rotate);
        }
        let handle = handle_at(b, at, grip)?;
        Some(if s.moves_only() { Handle::Move } else { handle })
    }

    /// The stroke, text or picture of ours at `at` on `page`: a stroke whose edge is within
    /// [`PICK_RADIUS`] of it, the topmost first, then a text whose box holds it, then a picture,
    /// so ink over a picture stays within reach. A circle's empty middle is no part of the
    /// circle, so a press there starts a lasso.
    fn pick_at(&self, page: usize, at: (f32, f32)) -> Option<Picked> {
        let (inks, texts) = (self.imp().inks.borrow(), self.imp().texts.borrow());
        let pictures = self.imp().pictures.borrow();
        let inks = inks.get(&page).map(Vec::as_slice).unwrap_or_default();
        let texts = texts.get(&page).map(Vec::as_slice).unwrap_or_default();
        let pictures = pictures.get(&page).map(Vec::as_slice).unwrap_or_default();
        let on = |(_, ink): &&NamedInk| {
            let reach = PICK_RADIUS + ink.style.width / 2.0;
            ink.bounds.grow(reach).contains(at) && accent_core::pdf::hit(&ink.points, at, reach)
        };
        // A turned picture's corners are no part of it: a dot inside its outline is.
        let inside = |(_, p): &&NamedPicture| {
            let outline = accent_core::pdf::picture_corners(p.place);
            p.bounds.contains(at) && accent_core::pdf::lassoed(&[at], &outline)
        };
        (inks.iter().rev().find(on).map(Picked::stroke))
            .or_else(|| {
                let text = texts.iter().rev().find(|(_, t)| t.bounds.contains(at));
                text.map(Picked::text)
            })
            .or_else(|| pictures.iter().rev().find(inside).map(Picked::picture))
    }

    /// The drag so far: the lasso drawn on, or what the Select tool holds moved, stretched or
    /// turned, `snap` (Shift) turning in steps of 15°. A move stops at the page's edge.
    fn select_drag(&self, gesture: &gtk::GestureDrag, dx: f64, dy: f64, snap: bool) {
        let Some((x, y)) = self.imp().drag_from.get() else {
            return;
        };
        if let Some(lasso) = self.imp().lasso.borrow_mut().as_mut() {
            let page = lasso.page;
            let merged = merged(gesture, self).into_iter();
            let merged = merged.filter_map(|(x, y)| self.on_page(page, x, y));
            lasso.points.extend(merged);
            lasso.points.extend(self.on_page(page, x + dx, y + dy));
            self.queue_draw();
            return;
        }
        let scale = self.imp().layout.borrow().scale;
        if let Some(s) = self.imp().picked.borrow_mut().as_mut()
            && let Some(handle) = s.handle
        {
            let (dx, dy) = (dx as f32 / scale, dy as f32 / scale);
            let (b, keep) = (s.bounds(), s.keeps_aspect());
            s.matrix = match handle {
                Handle::Rotate => match self.on_page(s.page, x, y) {
                    Some(p) => turn_matrix(b, p, (p.0 + dx, p.1 + dy), snap),
                    None => s.matrix,
                },
                Handle::Move => {
                    let size = self.page_size(s.page).unwrap_or((f32::MAX, f32::MAX));
                    clamp_move(b, drag_matrix(handle, b, dx, dy, keep), size)
                }
                _ => drag_matrix(handle, b, dx, dy, keep),
            };
        }
        self.queue_draw();
    }

    /// A point of the view in page `page`'s own points, wherever it is.
    fn on_page(&self, page: usize, x: f64, y: f64) -> Option<(f32, f32)> {
        let (cx, cy) = self.content_at(x, y);
        self.imp().layout.borrow().to_page(page, cx, cy)
    }

    /// The hand let go. A lasso holds what it took, at least half of each stroke inside it and a
    /// text by its box, or with Ctrl adds that to what is held; a click on the paper lets go
    /// of everything. A drag of what is held goes to the tab, and its items keep painting where
    /// they will be until the page's fresh list confirms it.
    fn select_release(&self) {
        if let Some(lasso) = self.imp().lasso.take() {
            // A click's lasso, a point or a shaky few, encloses nothing.
            let taken = self.lassoed(&lasso);
            let mut picked = self.imp().picked.borrow_mut();
            match picked
                .as_mut()
                .filter(|s| lasso.add && s.page == lasso.page)
            {
                Some(s) => {
                    for item in taken {
                        if !s.items.iter().any(|held| held.id == item.id) {
                            s.items.push(item);
                        }
                    }
                }
                None if lasso.add && taken.is_empty() => {}
                None => *picked = (!taken.is_empty()).then(|| Selection::of(lasso.page, taken)),
            }
            drop(picked);
            return self.picked_changed();
        }
        let m = {
            let mut picked = self.imp().picked.borrow_mut();
            let Some(s) = picked.as_mut() else {
                return;
            };
            s.handle = None;
            std::mem::replace(&mut s.matrix, accent_core::pdf::IDENTITY)
        };
        match m == accent_core::pdf::IDENTITY {
            true => self.queue_draw(),
            false => self.transform_picked(m),
        }
    }

    /// What `lasso` takes of its page: each stroke at least half inside it, and each text and
    /// picture of ours whose outline is.
    fn lassoed(&self, lasso: &Lasso) -> Vec<Picked> {
        let (inks, texts) = (self.imp().inks.borrow(), self.imp().texts.borrow());
        let pictures = self.imp().pictures.borrow();
        let inks = inks.get(&lasso.page).map(Vec::as_slice).unwrap_or_default();
        let texts = texts
            .get(&lasso.page)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let pictures = (pictures.get(&lasso.page))
            .map(Vec::as_slice)
            .unwrap_or_default();
        let strokes = (inks.iter())
            .filter(|(_, ink)| accent_core::pdf::lassoed(&ink.points, &lasso.points))
            .map(Picked::stroke);
        let texts = (texts.iter())
            .filter(|(_, text)| accent_core::pdf::lassoed(&outline(text.bounds), &lasso.points))
            .map(Picked::text);
        let pictures = (pictures.iter().map(Picked::picture))
            .filter(|picture| accent_core::pdf::lassoed(&picture.points, &lasso.points));
        strokes.chain(texts).chain(pictures).collect()
    }

    /// Under the Select tool the pointer says what a press would do: a handle's own cursor, the
    /// move cursor over a stroke or a text, the arrow elsewhere. Set only when that changes.
    fn hover(&self, x: f64, y: f64) {
        let imp = self.imp();
        if imp.mode.get() != Mode::Select || imp.dot.get() || imp.drag_from.get().is_some() {
            return;
        }
        let picks = || {
            let (page, at) = self.nearest_page_point(x, y)?;
            self.pick_at(page, at)
        };
        let name = match self.handle_under(x, y) {
            Some(handle) => handle.cursor(),
            None if picks().is_some() => "move",
            None => "default",
        };
        if self.cursor().and_then(|c| c.name()).as_deref() != Some(name) {
            self.set_cursor_from_name(Some(name));
        }
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
            (
                width_of(Mode::Eraser, style.eraser_radius),
                style.eraser_partial,
            )
        };
        // Hit-tested here, against the strokes the tab keeps for this page, rather than on the
        // render thread: that read and flattened every annotation on the page under the pdfium
        // lock, once per pointer event of the drag. Taken out of the list at once, so a drag
        // that passes over one again does not ask for it twice; what a partial eraser leaves goes
        // in at once, named here, so the next report can cut it again before the thread answers.
        let mut taken = Vec::new();
        if let Some(list) = self.imp().inks.borrow_mut().get_mut(&page) {
            for (id, ink) in std::mem::take(list) {
                // To the stroke's edge rather than its middle: a highlighter is 10.5 pt across.
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
        // A text of ours goes whole, under either eraser, once the pass comes within reach of
        // its box.
        if let Some(list) = self.imp().texts.borrow_mut().get_mut(&page) {
            list.retain(|(id, text)| {
                let b = text.bounds;
                let hit = b.contains(at) || accent_core::pdf::swept(&outline(b), from, at, radius);
                if hit {
                    taken.push((*id, None));
                }
                !hit
            });
        }
        if let Some(f) = self.imp().on_erase.borrow().as_ref() {
            for (id, pass) in taken {
                self.relisted(page);
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

/// The edge of a box, round from its top-left corner, as a polyline: how a lasso measures a
/// text, and the eraser too.
fn outline(b: accent_core::pdf::Rect) -> [(f32, f32); 5] {
    let (l, t, r, bt) = (b.left, b.top, b.right, b.bottom);
    [(l, t), (r, t), (r, bt), (l, bt), (l, t)]
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
/// draw it. The object is the parent's until the render thread draws it; names are what the
/// tools go by.
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
        at: ink.at,
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
