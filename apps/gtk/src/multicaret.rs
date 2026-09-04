//! A `sourceview5::View` that can hold extra carets, VS Code's Add Cursor Above / Below.
//!
//! GtkTextView has exactly one insert mark and no notion of a second one, so the secondary carets
//! are plain right-gravity `TextMark`s that this widget draws itself and replays edits at. The
//! primary caret stays GTK's, which is what keeps selection, IME, spellcheck and the scroll
//! machinery working normally the rest of the time.
//!
//! What it deliberately does not do:
//!
//! * carets only, no per-caret selection, and a selection made before the extra carets were added
//!   is not replaced by what gets typed;
//! * no mouse-added carets: any click, selection or find-bar jump moves the primary caret, which
//!   drops the secondaries;
//! * while secondaries exist the key controller runs ahead of the input method, so dead keys and
//!   CJK preedit go to the primary caret only, once the secondaries are cleared;
//! * secondary carets do not blink, they are painted;
//! * the completion popup can open at several carets at once, since it follows the primary.
//!
//! Mirrored at every caret: printable characters, Return, Tab, Backspace, Delete, and the arrow,
//! Home and End motions, so a column of carets can be moved and edited as one. Everything else —
//! Escape, any Ctrl or Alt combination, any other key — clears the carets and is then handled as
//! usual, so undo, paste and every accelerator keep working on the primary caret. One undo step
//! covers a whole multi-caret edit, because each replay runs inside a single `begin_user_action`.

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene};

/// What a key means at every caret. Anything outside this list clears the carets instead.
enum Edit {
    Insert(String),
    Backspace,
    Delete,
    Move(Motion),
}

#[derive(Clone, Copy)]
enum Motion {
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
}

/// The edit `key` stands for, or `None` for a key this widget does not mirror.
fn edit_for(key: gdk::Key) -> Option<Edit> {
    match key {
        gdk::Key::Return | gdk::Key::KP_Enter => Some(Edit::Insert("\n".to_string())),
        gdk::Key::Tab | gdk::Key::KP_Tab => Some(Edit::Insert("\t".to_string())),
        gdk::Key::BackSpace => Some(Edit::Backspace),
        gdk::Key::Delete | gdk::Key::KP_Delete => Some(Edit::Delete),
        gdk::Key::Left | gdk::Key::KP_Left => Some(Edit::Move(Motion::Left)),
        gdk::Key::Right | gdk::Key::KP_Right => Some(Edit::Move(Motion::Right)),
        gdk::Key::Up | gdk::Key::KP_Up => Some(Edit::Move(Motion::Up)),
        gdk::Key::Down | gdk::Key::KP_Down => Some(Edit::Move(Motion::Down)),
        gdk::Key::Home | gdk::Key::KP_Home => Some(Edit::Move(Motion::Home)),
        gdk::Key::End | gdk::Key::KP_End => Some(Edit::Move(Motion::End)),
        // ponytail: a literal tab above, and no `insert-spaces-instead-of-tabs`, because the
        // editor does not turn that on. Ask the view when it ever does.
        _ => key
            .to_unicode()
            .filter(|c| !c.is_control())
            .map(|c| Edit::Insert(c.to_string())),
    }
}

/// Where a new caret lands on its line: the column it came from, or the end of a shorter line.
fn clamp_offset(wanted: i32, line_length: i32) -> i32 {
    wanted.min(line_length)
}

mod imp {
    use super::*;
    use std::cell::{Cell, RefCell};

    #[derive(Default)]
    pub struct View {
        /// One right-gravity mark per secondary caret, so they ride along with the text.
        pub carets: RefCell<Vec<gtk::TextMark>>,
        /// Set while a key is replayed, so the `mark-set` hook does not read our own edits as
        /// the user moving the primary caret and drop every caret mid-edit.
        pub busy: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for View {
        const NAME: &'static str = "AccentMultiCaretView";
        type Type = super::View;
        type ParentType = sourceview5::View;
    }

    impl ObjectImpl for View {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj().clone();

            // Capture, so the keys we mirror never reach the view's own bindings. The window's
            // capture-phase controller runs first and takes the caret and scroll chords with it,
            // so those never arrive here either.
            let keys = gtk::EventControllerKey::new();
            keys.set_propagation_phase(gtk::PropagationPhase::Capture);
            keys.connect_key_pressed(glib::clone!(
                #[weak]
                obj,
                #[upgrade_or]
                glib::Propagation::Proceed,
                move |_, key, _, state| obj.on_key(key, state)
            ));
            obj.add_controller(keys);

            // The buffer arrives after construction. Every `mark-set` on it that we did not
            // cause is the user moving the primary caret — a click, a selection, a find-bar
            // jump — so this one hook covers all of them without a gesture of its own.
            obj.connect_buffer_notify(|obj| {
                obj.buffer().connect_mark_set(glib::clone!(
                    #[weak]
                    obj,
                    move |_, _, mark| {
                        let moved = mark.name();
                        let moved = moved.as_deref();
                        if !obj.imp().busy.get()
                            && (moved == Some("insert") || moved == Some("selection_bound"))
                        {
                            obj.clear_carets();
                        }
                    }
                ));
            });
        }
    }

    impl WidgetImpl for View {}

    impl TextViewImpl for View {
        fn snapshot_layer(&self, layer: gtk::TextViewLayer, snapshot: gtk::Snapshot) {
            self.parent_snapshot_layer(layer, snapshot.clone());
            if layer != gtk::TextViewLayer::AboveText {
                return;
            }
            let obj = self.obj();
            let buffer = obj.buffer();
            let colour = obj.color();
            // This layer draws in buffer coordinates, which is what `iter_location` reports.
            for mark in self.carets.borrow().iter() {
                let at = obj.iter_location(&buffer.iter_at_mark(mark));
                snapshot.append_color(
                    &colour,
                    &graphene::Rect::new(at.x() as f32, at.y() as f32, 1.0, at.height() as f32),
                );
            }
        }
    }

    impl sourceview5::subclass::prelude::ViewImpl for View {}
}

glib::wrapper! {
    pub struct View(ObjectSubclass<imp::View>)
        @extends sourceview5::View, gtk::TextView, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Scrollable;
}

impl Default for View {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl View {
    pub fn new() -> Self {
        Self::default()
    }

    /// Put a caret one line below (or above) the outermost caret in that direction, so repeating
    /// the action grows the column away from the primary caret.
    pub fn add_caret(&self, below: bool) {
        let buffer = self.buffer();
        let from = self.outermost(below);
        let line = from.line() + if below { 1 } else { -1 };
        if line < 0 || line >= buffer.line_count() {
            return;
        }
        let Some(mut target) = buffer.iter_at_line(line) else {
            return;
        };
        target.set_line_offset(clamp_offset(from.line_offset(), line_length(&buffer, line)));

        // A caret already there would be a second one on the same character, which is one caret.
        if self.caret_offsets().contains(&target.offset()) {
            return;
        }
        let mark = buffer.create_mark(None, &target, false);
        self.imp().carets.borrow_mut().push(mark);
        self.queue_draw();
    }

    /// Whether a key press is going to be replayed at more than one caret. `typing.rs` asks
    /// before it acts: its controller sits on the same widget in the same phase, so the order
    /// GTK runs the two in is not something to depend on.
    pub fn has_carets(&self) -> bool {
        !self.imp().carets.borrow().is_empty()
    }

    pub fn clear_carets(&self) {
        let buffer = self.buffer();
        {
            let mut carets = self.imp().carets.borrow_mut();
            if carets.is_empty() {
                return;
            }
            for mark in carets.drain(..) {
                buffer.delete_mark(&mark);
            }
        }
        self.queue_draw();
    }

    /// The caret furthest down (or up), which is the one the next line is measured from.
    fn outermost(&self, below: bool) -> gtk::TextIter {
        let buffer = self.buffer();
        let mut furthest = buffer.iter_at_mark(&buffer.get_insert());
        for mark in self.imp().carets.borrow().iter() {
            let caret = buffer.iter_at_mark(mark);
            let further = match below {
                true => caret.line() > furthest.line(),
                false => caret.line() < furthest.line(),
            };
            if further {
                furthest = caret;
            }
        }
        furthest
    }

    /// Every caret's character offset, the primary one included.
    fn caret_offsets(&self) -> Vec<i32> {
        let buffer = self.buffer();
        let mut offsets = vec![buffer.iter_at_mark(&buffer.get_insert()).offset()];
        offsets.extend(
            self.imp()
                .carets
                .borrow()
                .iter()
                .map(|mark| buffer.iter_at_mark(mark).offset()),
        );
        offsets
    }

    fn on_key(&self, key: gdk::Key, state: gdk::ModifierType) -> glib::Propagation {
        if self.imp().carets.borrow().is_empty() {
            return glib::Propagation::Proceed;
        }
        let modified =
            state.intersects(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK);
        let Some(edit) = edit_for(key).filter(|_| !modified && key != gdk::Key::Escape) else {
            self.clear_carets();
            return glib::Propagation::Proceed;
        };
        self.replay(&edit);
        glib::Propagation::Stop
    }

    /// Apply `edit` at every caret as one undoable step.
    fn replay(&self, edit: &Edit) {
        let buffer = self.buffer();
        let insert = buffer.get_insert();
        let mut marks = self.imp().carets.borrow().clone();
        marks.push(insert.clone());

        self.imp().busy.set(true);
        buffer.begin_user_action();
        for mark in &marks {
            let mut at = buffer.iter_at_mark(mark);
            match edit {
                Edit::Insert(text) => buffer.insert(&mut at, text),
                Edit::Backspace => {
                    let mut from = at;
                    if from.backward_char() {
                        buffer.delete(&mut from, &mut at);
                    }
                }
                Edit::Delete => {
                    let mut to = at;
                    if to.forward_char() {
                        buffer.delete(&mut at, &mut to);
                    }
                }
                Edit::Move(motion) => {
                    match motion {
                        Motion::Left => {
                            at.backward_char();
                        }
                        Motion::Right => {
                            at.forward_char();
                        }
                        Motion::Up | Motion::Down => {
                            let step = if matches!(motion, Motion::Down) {
                                1
                            } else {
                                -1
                            };
                            let line = at.line() + step;
                            // The column is kept, not remembered: a caret that walks past a short
                            // line settles at its end, the way one caret does in any editor.
                            if let Some(mut moved) = (0..buffer.line_count())
                                .contains(&line)
                                .then(|| buffer.iter_at_line(line))
                                .flatten()
                            {
                                moved.set_line_offset(clamp_offset(
                                    at.line_offset(),
                                    line_length(&buffer, line),
                                ));
                                at = moved;
                            }
                        }
                        Motion::Home => at.set_line_offset(0),
                        Motion::End => at = line_end(&buffer, at.line()),
                    }
                    // `place_cursor` also carries the selection bound along, which `move_mark`
                    // would leave behind as a selection nobody asked for.
                    match mark == &insert {
                        true => buffer.place_cursor(&at),
                        false => buffer.move_mark(mark, &at),
                    }
                }
            }
        }
        buffer.end_user_action();
        self.imp().busy.set(false);

        self.collapse();
        self.scroll_mark_onscreen(&insert);
        self.queue_draw();
    }

    /// Two carets driven onto the same character are one caret from here on.
    fn collapse(&self) {
        let buffer = self.buffer();
        let mut seen = vec![buffer.iter_at_mark(&buffer.get_insert()).offset()];
        self.imp().carets.borrow_mut().retain(|mark| {
            let offset = buffer.iter_at_mark(mark).offset();
            if seen.contains(&offset) {
                buffer.delete_mark(mark);
                return false;
            }
            seen.push(offset);
            true
        });
    }
}

/// The end of `line`, before its newline. `forward_to_line_end` would run on to the next line
/// from an empty one, so a line that is already at its end is left alone.
fn line_end(buffer: &gtk::TextBuffer, line: i32) -> gtk::TextIter {
    let mut at = buffer
        .iter_at_line(line)
        .unwrap_or_else(|| buffer.end_iter());
    if !at.ends_line() {
        at.forward_to_line_end();
    }
    at
}

fn line_length(buffer: &gtk::TextBuffer, line: i32) -> i32 {
    line_end(buffer, line).line_offset()
}

#[cfg(test)]
mod tests {
    use super::clamp_offset;

    /// A caret moving into a shorter line stops at its end rather than off it.
    #[test]
    fn a_new_caret_clamps_to_the_line_it_lands_on() {
        assert_eq!(clamp_offset(9, 3), 3);
        assert_eq!(clamp_offset(2, 3), 2);
        assert_eq!(clamp_offset(0, 0), 0);
    }
}
