//! Dragging a selection that has folded text in it.
//!
//! GTK's own drag carries `gtk_text_buffer_get_selection_content`, which is the selection's
//! *visible* text, and a move then deletes everything between the marks it set when the drag
//! began, hidden lines included (`dnd_finished_cb`, GTK 4.22): a folded section dragged elsewhere
//! arrived as its header and left its body nowhere. That one drag is ours instead, and carries
//! all of the selection, as a copy does. Every other drag stays GTK's.

use super::lines::pressed_at;
use gtk::prelude::*;
use gtk::{gdk, glib, pango};
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

/// Take the drags GTK's own would carry less of.
///
/// A press on the selection is where GTK's click gesture claims the sequence for its drag gesture,
/// which denies every gesture on the view it is not grouped with, so this one joins that group.
/// It is added after GTK's, and a widget runs the controllers of one phase newest first, so it sees
/// the press before GTK's gestures change anything, and the motion that crosses the drag
/// threshold before GTK's drag gesture does. Denying the sequence once the drag is ours is what
/// GTK's own drag does to its gesture, and the group shares it, so GTK starts none. The capture
/// phase would not do: a claimed gesture there stops the event, and GTK's drag never moved.
pub(super) fn install(view: &sourceview5::View) {
    // GtkTextView's own drag gesture: the only `GtkGestureDrag` on the view, GtkSourceView adding
    // none. Looked up once, before this one is added.
    let controllers = view.observe_controllers();
    let theirs = (0..)
        .map_while(|i| controllers.item(i))
        .find_map(|c| c.downcast::<gtk::GestureDrag>().ok());
    drop(controllers);
    let gesture = gtk::GestureDrag::builder()
        .button(gdk::BUTTON_PRIMARY)
        .build();
    // Decided on the press, as GTK decides between a drag and a new selection there: the
    // selection a drag starts from is the one under the pointer before anything moved.
    let armed = Rc::new(Cell::new(false));
    gesture.connect_drag_begin(glib::clone!(
        #[strong]
        armed,
        move |gesture, x, y| {
            let ours = gesture
                .widget()
                .and_downcast::<sourceview5::View>()
                .is_some_and(|view| {
                    pressed_in_selection(&view, x, y) && content(&view.buffer()).is_some()
                });
            armed.set(ours);
        }
    ));
    gesture.connect_drag_update(move |gesture, dx, dy| {
        let Some(view) = gesture.widget().and_downcast::<sourceview5::View>() else {
            return;
        };
        let Some((x, y)) = gesture.start_point() else {
            return;
        };
        if !armed.get()
            || !view.drag_check_threshold(x as i32, y as i32, (x + dx) as i32, (y + dy) as i32)
        {
            return;
        }
        armed.set(false);
        if begin(&view, gesture, dx, dy) {
            gesture.set_state(gtk::EventSequenceState::Denied);
        }
    });
    view.add_controller(gesture.clone());
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

/// Whether a press at widget `x`, `y` was on the selection, which is where GTK's own gesture
/// starts a drag rather than a new selection.
fn pressed_in_selection(view: &sourceview5::View, x: f64, y: f64) -> bool {
    let (x, y) = view.window_to_buffer_coords(gtk::TextWindowType::Widget, x as i32, y as i32);
    let at = pressed_at(view, x, y);
    view.buffer()
        .selection_bounds()
        .is_some_and(|(start, end)| at.in_range(&start, &end))
}

/// A drag under way: where the selection was, what it carried and whether it landed in this
/// view.
struct Dragged {
    buffer: gtk::TextBuffer,
    from: gtk::TextMark,
    to: gtk::TextMark,
    text: String,
    here: Cell<bool>,
    watch: RefCell<Option<glib::SignalHandlerId>>,
}

impl Dragged {
    /// The drag is over. A move takes the selection away, all of it, where GTK's took the hidden
    /// part too but had carried only the rest. A drop in this view is a move as GTK makes its
    /// own, though the view's drop target answers a drag it did not start with a copy.
    fn finish(&self, moved: bool) {
        if let Some(id) = self.watch.take() {
            self.buffer.disconnect(id);
        }
        if moved || self.here.get() {
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

/// Start the drag, or say it could not be: then GTK's own gesture starts one on the same motion.
fn begin(view: &sourceview5::View, gesture: &gtk::GestureDrag, dx: f64, dy: f64) -> bool {
    let buffer = view.buffer();
    let (Some(content), Some((start, end))) = (content(&buffer), buffer.selection_bounds()) else {
        return false;
    };
    let surface = view.native().and_then(|native| native.surface());
    let (Some(surface), Some(device)) = (surface, gesture.device()) else {
        return false;
    };
    let actions = match view.is_editable() {
        true => gdk::DragAction::COPY | gdk::DragAction::MOVE,
        false => gdk::DragAction::COPY,
    };
    let Some(drag) = gdk::Drag::begin(&surface, &device, &content, actions, dx, dy) else {
        return false;
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
        text: buffer.text(&start, &end, true).into(),
        here: Cell::new(false),
        watch: RefCell::new(None),
        buffer: buffer.clone(),
    });
    // Nothing else inserts while the pointer is held, so the carried text going in is the drop.
    let weak: Weak<Dragged> = Rc::downgrade(&dragged);
    let watch = buffer.connect_insert_text(move |_, _, text| {
        if let Some(dragged) = weak.upgrade()
            && text == dragged.text
        {
            dragged.here.set(true);
        }
    });
    dragged.watch.replace(Some(watch));

    let finished = dragged.clone();
    drag.connect_dnd_finished(move |drag| {
        finished.finish(drag.selected_action() == gdk::DragAction::MOVE);
        drag.drop_done(true);
    });
    drag.connect_cancel(move |drag, _| {
        dragged.finish(false);
        drag.drop_done(false);
    });
    true
}
