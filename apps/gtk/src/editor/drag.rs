//! Preserve folded text when dragging a selection.
//!
//! GTK 4.22 drags only visible text but deletes the full selection on a move, losing folded
//! bodies. This module carries the full text and handles its drops before GTK's target,
//! which treats externally started drags as copies. Ctrl switches a move to a copy.

use super::lines::pressed_at;
use gtk::prelude::*;
use gtk::{gdk, glib, pango};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

#[cfg(feature = "bench")]
thread_local! {
    /// The action the last drag of ours ended with, empty for one cancelled: what
    /// `ACCENT_BENCH_STYLE=dragfold:` prints.
    pub(crate) static ENDED: Cell<Option<gdk::DragAction>> = const { Cell::new(None) };
}

/// Intercept selection drags that GTK would truncate to visible text.
///
/// Group with GTK's click gesture so claimed selections do not deny this gesture. Install
/// after GTK's handlers to observe presses and threshold-crossing motion first in the same
/// phase, matching GTK's selection and fractional-threshold rules. Reset view controllers
/// when dragging begins so GTK releases the press and can handle the next one.
/// Capture-phase claiming would prevent GTK's drag from moving.
pub(super) fn install(view: &sourceview5::View) {
    // GtkTextView's own drag gesture: the oldest `GtkGestureDrag` on the view, made with it, the
    // list running newest first. Not merely the first found, which is the column's box drag
    // (`multicaret::box_drag`): grouped with that one, this gesture was denied with it on every
    // press and cancelled by GTK's claim, and GTK's own drag, which carries only what shows, took
    // the selection away hidden lines and all. Looked up once, before this one is added.
    let controllers = view.observe_controllers();
    let theirs = (0..)
        .map_while(|i| controllers.item(i))
        .filter_map(|c| c.downcast::<gtk::GestureDrag>().ok())
        .last();
    drop(controllers);
    let gesture = gtk::GestureDrag::builder()
        .button(gdk::BUTTON_PRIMARY)
        .build();
    // Decided on the press, as GTK decides between a drag and a new selection there: the
    // selection a drag starts from is the one under the pointer before anything moved.
    let armed = Rc::new(Cell::new(false));
    let flight = Flight::default();
    gesture.connect_drag_begin(glib::clone!(
        #[strong]
        armed,
        move |gesture, x, y| {
            let extends = gesture
                .current_event_state()
                .contains(gdk::ModifierType::SHIFT_MASK);
            let ours = gesture
                .widget()
                .and_downcast::<sourceview5::View>()
                .is_some_and(|view| {
                    !extends
                        && pressed_in_selection(&view, x, y)
                        && content(&view.buffer()).is_some()
                });
            armed.set(ours);
        }
    ));
    let started = flight.clone();
    gesture.connect_drag_update(move |gesture, dx, dy| {
        let Some(view) = gesture.widget().and_downcast::<sourceview5::View>() else {
            return;
        };
        // `gtk_drag_check_threshold_double`, as GTK's drag gesture asks on the same motion.
        let threshold = f64::from(view.settings().gtk_dnd_drag_threshold());
        if !armed.get() || (dx.abs() <= threshold && dy.abs() <= threshold) {
            return;
        }
        armed.set(false);
        begin(&view, gesture, dx, dy, &started);
        // Started or not, no other drag does, GTK's carrying only what shows: every controller on
        // the view lets go of the press, GTK's drag gesture before this motion reaches it.
        let controllers = view.observe_controllers();
        for controller in (0..).map_while(|i| controllers.item(i)) {
            if let Ok(controller) = controller.downcast::<gtk::EventController>() {
                controller.reset();
            }
        }
    });
    view.add_controller(gesture.clone());
    view.add_controller(drop_target(flight));
    if let Some(theirs) = theirs {
        gesture.group_with(&theirs);
    }
}

/// Return full selected text when hidden content requires replacing GTK's
/// drag payload. `None` leaves ordinary visible selections to GTK.
pub(crate) fn content(buffer: &gtk::TextBuffer) -> Option<gdk::ContentProvider> {
    let (start, end) = buffer.selection_bounds()?;
    let text = buffer.text(&start, &end, true);
    (text != buffer.text(&start, &end, false))
        .then(|| gdk::ContentProvider::for_value(&text.to_value()))
}

/// Handle our own selection drag before GTK, which would treat it as
/// external and copy it. Controllers run newest first within the phase,
/// so this target supplies motion status and handles the drop. Refuse
/// other drags so GTK handles them.
fn drop_target(flight: Flight) -> gtk::DropTarget {
    let target = gtk::DropTarget::new(
        glib::Type::STRING,
        gdk::DragAction::COPY | gdk::DragAction::MOVE,
    );
    target.connect_accept(move |_, drop| {
        drop.drag()
            .is_some_and(|drag| flight.borrow().as_ref() == Some(&drag))
    });
    let answer = |target: &gtk::DropTarget, x: f64, y: f64| {
        let view = target.widget().and_downcast::<sourceview5::View>();
        match view.and_then(|view| drop_point(&view, x, y)) {
            Some(_) => gdk::DragAction::MOVE,
            None => gdk::DragAction::empty(),
        }
    };
    target.connect_enter(answer);
    target.connect_motion(answer);
    target.connect_drop(|target, value, x, y| {
        let Some(view) = target.widget().and_downcast::<sourceview5::View>() else {
            return false;
        };
        let (Some(mut at), Ok(text)) = (drop_point(&view, x, y), value.get::<String>()) else {
            return false;
        };
        let buffer = view.buffer();
        buffer.begin_user_action();
        let landed = buffer.insert_interactive(&mut at, &text, view.is_editable());
        buffer.place_cursor(&at);
        buffer.end_user_action();
        landed
    });
    target
}

/// Resolve a drop at widget coordinates using GTK's drag rules, excluding
/// the selection and both endpoints (`gtk_text_view_drag_motion`).
fn drop_point(view: &sourceview5::View, x: f64, y: f64) -> Option<gtk::TextIter> {
    let at = at_pixel(view, x, y)?;
    let on_selection = view
        .buffer()
        .selection_bounds()
        .is_some_and(|(start, end)| start <= at && at <= end);
    (!on_selection && at.can_insert(view.is_editable())).then_some(at)
}

/// Check whether widget coordinates fall inside the selection, excluding
/// its end, where GTK starts a drag instead of a new selection
/// (`gtk_text_view_click_gesture_pressed`).
fn pressed_in_selection(view: &sourceview5::View, x: f64, y: f64) -> bool {
    let at = at_pixel(view, x, y);
    view.buffer()
        .selection_bounds()
        .is_some_and(|(start, end)| at.is_some_and(|at| at.in_range(&start, &end)))
}

/// Find the nearest text position or adjacent row end, matching
/// `gtk_text_layout_get_iter_at_pixel`. `None` avoids positions that
/// would abort GTK ([`crate::fold::aborts_at`]).
fn at_pixel(view: &sourceview5::View, x: f64, y: f64) -> Option<gtk::TextIter> {
    let (x, y) = view.window_to_buffer_coords(gtk::TextWindowType::Widget, x as i32, y as i32);
    if crate::fold::aborts_at(view, y) {
        return None;
    }
    Some(match view.iter_at_position(x, y) {
        Some((mut at, trailing)) => {
            at.forward_chars(trailing);
            at
        }
        None => pressed_at(view, x, y),
    })
}

/// The drag of ours under way from a view, if one is.
type Flight = Rc<RefCell<Option<gdk::Drag>>>;

/// A drag under way: where the selection was.
struct Dragged {
    buffer: gtk::TextBuffer,
    from: gtk::TextMark,
    to: gtk::TextMark,
    flight: Flight,
}

impl Dragged {
    /// Finish the drag, deleting the full selection only after a move.
    /// Our payload includes hidden text that GTK would otherwise lose.
    fn finish(&self, moved: bool) {
        self.flight.take();
        if moved {
            let (mut from, mut to) = (
                self.buffer.iter_at_mark(&self.from),
                self.buffer.iter_at_mark(&self.to),
            );
            self.buffer.begin_user_action();
            self.buffer.delete(&mut from, &mut to);
            self.buffer.end_user_action();
        }
        self.buffer.delete_mark(&self.from);
        self.buffer.delete_mark(&self.to);
    }
}

/// Start the drag, unless there is no selection, nothing hidden in it, or no surface to drag from.
fn begin(view: &sourceview5::View, gesture: &gtk::GestureDrag, dx: f64, dy: f64, flight: &Flight) {
    let buffer = view.buffer();
    let (Some(content), Some((start, end))) = (content(&buffer), buffer.selection_bounds()) else {
        return;
    };
    let surface = view.native().and_then(|native| native.surface());
    let (Some(surface), Some(device)) = (surface, gesture.device()) else {
        return;
    };
    let actions = match view.is_editable() {
        true => gdk::DragAction::COPY | gdk::DragAction::MOVE,
        false => gdk::DragAction::COPY,
    };
    let Some(drag) = gdk::Drag::begin(&surface, &device, &content, actions, dx, dy) else {
        return;
    };
    // What GTK's icon shows: the text as it reads on screen, a few lines of it.
    let icon = gtk::Label::builder()
        .label(start.visible_text(&end))
        .wrap(true)
        .lines(7)
        .max_width_chars(40)
        .ellipsize(pango::EllipsizeMode::End)
        .build();
    gtk::DragIcon::for_drag(&drag).set_child(Some(&icon));

    // Left gravity both, as GTK's own marks. A drop on the selection, either end included, is one
    // the view refuses, so nothing lands between them.
    let dragged = Rc::new(Dragged {
        from: buffer.create_mark(None, &start, true),
        to: buffer.create_mark(None, &end, true),
        flight: flight.clone(),
        buffer: buffer.clone(),
    });
    flight.replace(Some(drag.clone()));

    let finished = dragged.clone();
    drag.connect_dnd_finished(move |drag| {
        #[cfg(feature = "bench")]
        ENDED.set(Some(drag.selected_action()));
        finished.finish(drag.selected_action() == gdk::DragAction::MOVE);
        drag.drop_done(true);
    });
    drag.connect_cancel(move |drag, _| {
        #[cfg(feature = "bench")]
        ENDED.set(Some(gdk::DragAction::empty()));
        dragged.finish(false);
        drag.drop_done(false);
    });
}
