//! Dragging a selection that has folded text in it.
//!
//! GTK's own drag carries `gtk_text_buffer_get_selection_content`, which is the selection's
//! *visible* text, and a move then deletes everything between the marks it set when the drag
//! began, hidden lines included (`dnd_finished_cb`, GTK 4.22): a folded section dragged elsewhere
//! arrived as its header and left its body nowhere. That one drag is ours instead, and carries
//! all of the selection, as a copy does. Every other drag stays GTK's.
//!
//! The view's own drop target answers a drag it did not start itself with a copy
//! (`gtk_text_view_drag_motion`), so one of ours answers these ahead of it, as GTK's answers its
//! own: a move, which the pointer shows and `Ctrl` turns into a copy ([`drop_target`]).

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

/// Take the drags GTK's own would carry less of, every one of them: a drag never takes away text it
/// does not carry.
///
/// A press on the selection is where GTK's click gesture claims the sequence for its drag gesture,
/// which denies every gesture on the view it is not grouped with, so this one joins that group.
/// It is added after GTK's, and a widget runs the controllers of one phase newest first, so it sees
/// the press before GTK's gestures change anything, and the motion that crosses the drag
/// threshold before GTK's drag gesture does: both decide as GTK does, the press on the selection
/// by the place nearest the pointer and without Shift, the threshold in fractions of a pixel, so
/// none of GTK's drags is one this gesture let by. On that motion every controller on the view is
/// reset, as GtkDragSource resets them when its drag begins: GTK's drag gesture lets go before the
/// motion reaches it, and the release, which goes to the drag, leaves no gesture holding the press
/// and missing the next one. The capture phase would not do: a claimed gesture there stops the
/// event, and GTK's drag never moved.
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

/// What a drag of the selection carries where GTK's would carry less: the whole selection, hidden
/// text included. `None` when nothing in it is hidden, and the drag is GTK's.
pub(crate) fn content(buffer: &gtk::TextBuffer) -> Option<gdk::ContentProvider> {
    let (start, end) = buffer.selection_bounds()?;
    let text = buffer.text(&start, &end, true);
    (text != buffer.text(&start, &end, false))
        .then(|| gdk::ContentProvider::for_value(&text.to_value()))
}

/// The view's answer to a drag of its own of ours, which GtkTextView's drop target would take
/// for a stranger's and answer with a copy. Added after GTK's, and a widget runs the controllers
/// of one phase newest first, so this answers first, and the first answer to a motion is the one
/// that counts (`gtk_drop_status`); its drop is this one's too, and goes in as GTK's would. Any
/// other drag it refuses, leaving it to GTK's.
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

/// Where a drop at widget `x`, `y` goes in, as GtkTextView answers its own drag there
/// (`gtk_text_view_drag_motion`): anywhere the text takes one but on the selection, either end
/// included.
fn drop_point(view: &sourceview5::View, x: f64, y: f64) -> Option<gtk::TextIter> {
    let at = at_pixel(view, x, y)?;
    let on_selection = view
        .buffer()
        .selection_bounds()
        .is_some_and(|(start, end)| start <= at && at <= end);
    (!on_selection && at.can_insert(view.is_editable())).then_some(at)
}

/// Whether a press at widget `x`, `y` was on the selection, which is where GTK's own gesture
/// starts a drag rather than a new selection: the place it takes the press for is in it, the end
/// left out (`gtk_text_view_click_gesture_pressed`).
fn pressed_in_selection(view: &sourceview5::View, x: f64, y: f64) -> bool {
    let at = at_pixel(view, x, y);
    view.buffer()
        .selection_bounds()
        .is_some_and(|(start, end)| at.is_some_and(|at| at.in_range(&start, &end)))
}

/// The place nearest widget `x`, `y`, or the end of the row beside it, as GTK takes a pointer's
/// place (`gtk_text_layout_get_iter_at_pixel`). `None` where asking would abort
/// (`fold::aborts_at`).
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
    /// The drag is over. A move takes the selection away, all of it, where GTK's took the hidden
    /// part too but had carried only the rest.
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
