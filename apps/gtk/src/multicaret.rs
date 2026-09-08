//! A `sourceview5::View` that can hold extra carets, VS Code's Add Cursor Above / Below.
//!
//! GtkTextView has exactly one insert mark and no notion of a second one, so the secondary carets
//! are plain right-gravity `TextMark`s that this widget draws itself and replays edits at. The
//! primary caret stays GTK's, which is what keeps selection, IME, spellcheck and the scroll
//! machinery working normally the rest of the time.
//!
//! It is also where this view's key semantics are corrected, because a `TextViewImpl` is the one
//! place the single-caret and the multi-caret case both pass through: `Ctrl+Delete` takes a run of
//! whitespace before it takes a word, and Up, Down, Home and End work on a line of the document
//! rather than on a row of the screen.
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
//! It also paints the ghost text (`ghost.rs`): a suggestion is not in the buffer, so there is
//! nothing to give it a text tag, and this widget is already the one drawing over the text.
//!
//! Mirrored at every caret: printable characters, Return, Tab, Backspace, Delete, the arrow, Home
//! and End motions, and the four wordwise chords `Ctrl+Left`, `Ctrl+Right`, `Ctrl+Delete` and
//! `Ctrl+Backspace`, so a column of carets can be moved and edited as one. Everything else —
//! Escape, any Alt combination, any other Ctrl combination, any other key — clears the carets and
//! is then handled as usual, so undo, paste and every accelerator keep working on the primary
//! caret. One undo step covers a whole multi-caret edit, because each replay runs inside a single
//! `begin_user_action`.

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene};
use sourceview5::prelude::ViewExt as _;

/// What a key means at every caret. Anything outside this list clears the carets instead.
enum Edit {
    Insert(String),
    /// Resolved per caret, because what Tab inserts depends on the column it is pressed in.
    Tab,
    Backspace,
    Delete,
    /// `Ctrl+Delete` forwards, `Ctrl+Backspace` backwards.
    DeleteWord(bool),
    Move(Motion),
}

#[derive(Clone, Copy)]
enum Motion {
    Left,
    Right,
    WordLeft,
    WordRight,
    Up,
    Down,
    Home,
    End,
}

/// The edit `key` with `state` held stands for, or `None` for a key this widget does not mirror.
///
/// Ctrl has a table of its own — the four chords a column of carets is worth moving as one — and
/// Alt has none, so every other combination still falls through to the primary caret alone.
fn edit_for(key: gdk::Key, state: gdk::ModifierType) -> Option<Edit> {
    if state.contains(gdk::ModifierType::ALT_MASK) {
        return None;
    }
    if state.contains(gdk::ModifierType::CONTROL_MASK) {
        return match key {
            gdk::Key::Left | gdk::Key::KP_Left => Some(Edit::Move(Motion::WordLeft)),
            gdk::Key::Right | gdk::Key::KP_Right => Some(Edit::Move(Motion::WordRight)),
            gdk::Key::Delete | gdk::Key::KP_Delete => Some(Edit::DeleteWord(true)),
            gdk::Key::BackSpace => Some(Edit::DeleteWord(false)),
            _ => None,
        };
    }
    match key {
        gdk::Key::Return | gdk::Key::KP_Enter => Some(Edit::Insert("\n".to_string())),
        gdk::Key::Tab | gdk::Key::KP_Tab => Some(Edit::Tab),
        gdk::Key::BackSpace => Some(Edit::Backspace),
        gdk::Key::Delete | gdk::Key::KP_Delete => Some(Edit::Delete),
        gdk::Key::Left | gdk::Key::KP_Left => Some(Edit::Move(Motion::Left)),
        gdk::Key::Right | gdk::Key::KP_Right => Some(Edit::Move(Motion::Right)),
        gdk::Key::Up | gdk::Key::KP_Up => Some(Edit::Move(Motion::Up)),
        gdk::Key::Down | gdk::Key::KP_Down => Some(Edit::Move(Motion::Down)),
        gdk::Key::Home | gdk::Key::KP_Home => Some(Edit::Move(Motion::Home)),
        gdk::Key::End | gdk::Key::KP_End => Some(Edit::Move(Motion::End)),
        _ => key
            .to_unicode()
            .filter(|c| !c.is_control())
            .map(|c| Edit::Insert(c.to_string())),
    }
}

/// Where a caret aiming at column `goal` lands on a line `len` characters long, and the goal it
/// keeps. The goal outliving the landing is what brings the caret back to its own column after a
/// trip across a shorter line.
fn vertical_step(goal: Option<i32>, column: i32, len: i32) -> (i32, i32) {
    let goal = goal.unwrap_or(column);
    (goal.min(len), goal)
}

/// The column `prefix` ends at, a tab counting on to the next stop rather than as one character.
fn visual_column(prefix: &str, width: usize) -> usize {
    prefix.chars().fold(0, |column, c| match c {
        '\t' => column + width - column % width,
        _ => column + 1,
    })
}

/// What Tab inserts at `column`: a literal tab, or the spaces that reach the next tab stop.
fn tab_insert(column: usize, width: usize, spaces: bool) -> String {
    match spaces {
        true => " ".repeat(width - column % width),
        false => "\t".to_string(),
    }
}

/// The run of spaces and tabs the caret is sitting in front of, or `None` where it is not on one
/// and the word deletion GTK already does is the right answer.
fn spaces_ahead(rest: &str) -> Option<usize> {
    let run = rest.chars().take_while(|c| *c == ' ' || *c == '\t').count();
    (run > 0).then_some(run)
}

/// The same run behind the caret, for `Ctrl+Backspace`.
fn spaces_behind(head: &str) -> Option<usize> {
    let run = head
        .chars()
        .rev()
        .take_while(|c| *c == ' ' || *c == '\t')
        .count();
    (run > 0).then_some(run)
}

/// The range a wordwise delete takes at `at` while it is on whitespace. Bounded by the line: a
/// newline is a word boundary, not whitespace to swallow.
fn space_range(
    buffer: &gtk::TextBuffer,
    at: &gtk::TextIter,
    forward: bool,
) -> Option<(gtk::TextIter, gtk::TextIter)> {
    let mut other = *at;
    if forward {
        let end = line_end(buffer, at.line());
        let run = spaces_ahead(&buffer.text(at, &end, true))?;
        other.forward_chars(run as i32);
        return Some((*at, other));
    }
    let mut start = *at;
    start.set_line_offset(0);
    let run = spaces_behind(&buffer.text(&start, at, true))?;
    other.backward_chars(run as i32);
    Some((other, *at))
}

/// What a wordwise delete takes at `at`: the whitespace run it is sitting on, or the word past it.
fn word_range(
    buffer: &gtk::TextBuffer,
    at: gtk::TextIter,
    forward: bool,
) -> (gtk::TextIter, gtk::TextIter) {
    if let Some(range) = space_range(buffer, &at, forward) {
        return range;
    }
    // Visible, because `fold.rs` hides folded text and a hidden word is not one to delete into.
    let mut other = at;
    if forward {
        other.forward_visible_word_end();
        (at, other)
    } else {
        other.backward_visible_word_start();
        (other, at)
    }
}

/// How much of the text colour ghost text keeps. Enough to read, little enough that it is never
/// mistaken for what the document says.
const GHOST_ALPHA: f32 = 0.45;

mod imp {
    use super::*;
    use std::cell::{Cell, RefCell};

    /// A secondary caret: the mark that rides the text, and the column vertical movement aims
    /// for, which is what a caret keeps while it crosses a shorter line.
    pub struct Caret {
        pub mark: gtk::TextMark,
        pub goal: Option<i32>,
    }

    #[derive(Default)]
    pub struct View {
        /// One right-gravity mark per secondary caret, so they ride along with the text.
        pub carets: RefCell<Vec<Caret>>,
        /// The suggestion painted after the caret, if one is showing.
        pub ghost: RefCell<Option<String>>,
        /// Set while a key is replayed, so the `mark-set` hook does not read our own edits as
        /// the user moving the primary caret and drop every caret mid-edit.
        pub busy: Cell<bool>,
        /// The primary caret's goal column, the counterpart of [`Caret::goal`]. Dropped by any
        /// other movement, any edit and any caret move this widget did not make.
        pub goal: Cell<Option<i32>>,
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
                move |_, key, _, state| obj.press(key, state)
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
                            obj.imp().goal.set(None);
                            obj.clear_carets();
                        }
                    }
                ));
                // An edit is not vertical movement, so the column the caret was aiming for goes
                // with it, whoever made the edit.
                obj.buffer().connect_changed(glib::clone!(
                    #[weak]
                    obj,
                    move |_| obj.imp().goal.set(None)
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
            for caret in self.carets.borrow().iter() {
                let at = obj.iter_location(&buffer.iter_at_mark(&caret.mark));
                snapshot.append_color(
                    &colour,
                    &graphene::Rect::new(at.x() as f32, at.y() as f32, 1.0, at.height() as f32),
                );
            }
            // The suggestion sits after the caret in the text's own font, dimmed enough to read
            // as not-yet-written. It is only ever asked for at the end of a line, so there is
            // nothing to its right to draw over.
            if let Some(text) = self.ghost.borrow().as_deref() {
                let at = obj.iter_location(&buffer.iter_at_mark(&buffer.get_insert()));
                let dim = gdk::RGBA::new(
                    colour.red(),
                    colour.green(),
                    colour.blue(),
                    colour.alpha() * GHOST_ALPHA,
                );
                snapshot.save();
                snapshot.translate(&graphene::Point::new(at.x() as f32, at.y() as f32));
                snapshot.append_layout(&obj.create_pango_layout(Some(text)), &dim);
                snapshot.restore();
            }
        }

        /// `Ctrl+Delete` and `Ctrl+Backspace` take a run of whitespace on its own, and only the
        /// next press takes the word past it. GTK's word boundaries step straight over the run to
        /// the far side of the word, which is a whole indent lost to one keystroke.
        fn delete_from_cursor(&self, type_: gtk::DeleteType, count: i32) {
            let obj = self.obj();
            let buffer = obj.buffer();
            if type_ == gtk::DeleteType::WordEnds
                && count.abs() == 1
                && obj.is_editable()
                && !buffer.has_selection()
                && let Some((mut from, mut to)) = space_range(
                    &buffer,
                    &buffer.iter_at_mark(&buffer.get_insert()),
                    count > 0,
                )
            {
                buffer.begin_user_action();
                buffer.delete(&mut from, &mut to);
                buffer.end_user_action();
                obj.scroll_mark_onscreen(&buffer.get_insert());
                return;
            }
            self.parent_delete_from_cursor(type_, count);
        }

        /// Up and Down move by a line of the document and Home and End go to that line's ends: a
        /// wrapped paragraph is one line to move through, not a screenful of rows. `Pages` and
        /// everything else keep GTK's display-based behaviour, which is what they are for.
        fn move_cursor(&self, step: gtk::MovementStep, count: i32, extend: bool) {
            match step {
                gtk::MovementStep::DisplayLines => self.obj().move_by_lines(count, extend),
                gtk::MovementStep::DisplayLineEnds => {
                    self.goal.set(None);
                    // GTK implements `ParagraphEnds` as the start and the end of the line.
                    self.parent_move_cursor(gtk::MovementStep::ParagraphEnds, count, extend);
                }
                _ => {
                    self.goal.set(None);
                    self.parent_move_cursor(step, count, extend);
                }
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

    /// Paint `text` after the caret, or nothing. The suggestion never enters the buffer, so it
    /// costs no undo step, no save and no `changed`.
    pub fn set_ghost(&self, text: Option<String>) {
        let mut ghost = self.imp().ghost.borrow_mut();
        if *ghost == text {
            return;
        }
        *ghost = text;
        drop(ghost);
        self.queue_draw();
    }

    /// What is painted after the caret, if anything.
    pub fn ghost(&self) -> Option<String> {
        self.imp().ghost.borrow().clone()
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
        let (column, goal) = vertical_step(None, from.line_offset(), line_length(&buffer, line));
        target.set_line_offset(column);

        // A caret already there would be a second one on the same character, which is one caret.
        if self.caret_offsets().contains(&target.offset()) {
            return;
        }
        let mark = buffer.create_mark(None, &target, false);
        self.imp().carets.borrow_mut().push(imp::Caret {
            mark,
            goal: Some(goal),
        });
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
            for caret in carets.drain(..) {
                buffer.delete_mark(&caret.mark);
            }
        }
        self.queue_draw();
    }

    /// The caret furthest down (or up), which is the one the next line is measured from.
    fn outermost(&self, below: bool) -> gtk::TextIter {
        let buffer = self.buffer();
        let mut furthest = buffer.iter_at_mark(&buffer.get_insert());
        for caret in self.imp().carets.borrow().iter() {
            let at = buffer.iter_at_mark(&caret.mark);
            let further = match below {
                true => at.line() > furthest.line(),
                false => at.line() < furthest.line(),
            };
            if further {
                furthest = at;
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
                .map(|caret| buffer.iter_at_mark(&caret.mark).offset()),
        );
        offsets
    }

    /// Every caret's line and column, the primary one first. Where the carets are is otherwise
    /// only visible on screen, so this is what a headless check reads them from.
    pub(crate) fn caret_positions(&self) -> Vec<(i32, i32)> {
        let buffer = self.buffer();
        let at = |mark: &gtk::TextMark| {
            let iter = buffer.iter_at_mark(mark);
            (iter.line(), iter.line_offset())
        };
        let mut positions = vec![at(&buffer.get_insert())];
        positions.extend(
            self.imp()
                .carets
                .borrow()
                .iter()
                .map(|caret| at(&caret.mark)),
        );
        positions
    }

    /// Up or Down by `count` lines of the document. The column the caret is aiming for outlives
    /// the lines it crosses, so a trip over a short line and back lands where it started.
    fn move_by_lines(&self, count: i32, extend: bool) {
        let imp = self.imp();
        self.reset_im_context();
        let buffer = self.buffer();
        let insert = buffer.get_insert();
        let mut at = buffer.iter_at_mark(&insert);
        let column = at.line_offset();
        // Visible lines, because `fold.rs` hides folded text and a hidden line is not one to
        // stop on.
        let mut off_end = false;
        for _ in 0..count.abs() {
            let stepped = match count > 0 {
                true => at.forward_visible_line(),
                false => at.backward_visible_line(),
            };
            if !stepped {
                off_end = true;
                break;
            }
        }
        let (landing, goal) =
            vertical_step(imp.goal.get(), column, line_length(&buffer, at.line()));
        match off_end {
            // Past the last line the caret parks at the end of the buffer, which is what GTK
            // does and what keeps Down at the bottom doing something.
            true => {
                at = match count > 0 {
                    true => buffer.end_iter(),
                    false => buffer.start_iter(),
                }
            }
            false => at.set_line_offset(landing),
        }
        imp.goal.set(Some(goal));

        // Ours, not the user's: the `mark-set` hook would read it as a click and drop both the
        // goal we just set and every secondary caret.
        imp.busy.set(true);
        match extend {
            true => buffer.move_mark(&insert, &at),
            false => buffer.place_cursor(&at),
        }
        imp.busy.set(false);
        self.scroll_mark_onscreen(&insert);
    }

    /// One key press, at every caret. `pub(crate)` so a headless check can drive the carets the
    /// way the key controller does, which is the only way to see them without a screen.
    pub(crate) fn press(&self, key: gdk::Key, state: gdk::ModifierType) -> glib::Propagation {
        if self.imp().carets.borrow().is_empty() {
            return glib::Propagation::Proceed;
        }
        let Some(edit) = edit_for(key, state).filter(|_| key != gdk::Key::Escape) else {
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
        let imp = self.imp();
        // Mark and goal column per caret, the primary appended so it is edited like any other.
        // Copied out of the cells first: the edits below move marks, and a borrow held across
        // them would meet the hooks that fire on the way.
        let mut carets: Vec<(gtk::TextMark, Option<i32>)> = imp
            .carets
            .borrow()
            .iter()
            .map(|caret| (caret.mark.clone(), caret.goal))
            .collect();
        carets.push((insert.clone(), imp.goal.get()));

        imp.busy.set(true);
        buffer.begin_user_action();
        for (mark, goal) in &mut carets {
            let mut at = buffer.iter_at_mark(mark);
            // Only vertical movement leaves a column behind to aim at; everything else drops it.
            let mut aim = None;
            match edit {
                Edit::Insert(text) => buffer.insert(&mut at, text),
                // The view already knows what Tab means here — `editor.rs` sets both properties
                // for code and leaves a note with its literal tab — so every caret answers the
                // way the primary one does, each from the column it is actually in.
                Edit::Tab => {
                    let width = self.tab_width() as usize;
                    let mut start = at;
                    start.set_line_offset(0);
                    let column = visual_column(&buffer.text(&start, &at, true), width);
                    let text = tab_insert(column, width, self.is_insert_spaces_instead_of_tabs());
                    buffer.insert(&mut at, &text);
                }
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
                // The same function the primary caret's `Ctrl+Delete` goes through, so the two
                // cannot drift apart.
                Edit::DeleteWord(forward) => {
                    let (mut from, mut to) = word_range(&buffer, at, *forward);
                    buffer.delete(&mut from, &mut to);
                }
                Edit::Move(motion) => {
                    match motion {
                        Motion::Left => {
                            at.backward_char();
                        }
                        Motion::Right => {
                            at.forward_char();
                        }
                        Motion::WordLeft => {
                            at.backward_visible_word_start();
                        }
                        Motion::WordRight => {
                            at.forward_visible_word_end();
                        }
                        Motion::Up | Motion::Down => {
                            let step = if matches!(motion, Motion::Down) {
                                1
                            } else {
                                -1
                            };
                            let line = at.line() + step;
                            if let Some(mut moved) = (0..buffer.line_count())
                                .contains(&line)
                                .then(|| buffer.iter_at_line(line))
                                .flatten()
                            {
                                let (column, kept) = vertical_step(
                                    *goal,
                                    at.line_offset(),
                                    line_length(&buffer, line),
                                );
                                moved.set_line_offset(column);
                                at = moved;
                                aim = Some(kept);
                            }
                        }
                        Motion::Home => at.set_line_offset(0),
                        Motion::End => at = line_end(&buffer, at.line()),
                    }
                    // `place_cursor` also carries the selection bound along, which `move_mark`
                    // would leave behind as a selection nobody asked for.
                    match *mark == insert {
                        true => buffer.place_cursor(&at),
                        false => buffer.move_mark(mark, &at),
                    }
                }
            }
            *goal = aim;
        }
        buffer.end_user_action();
        // Back into the cells the goals came out of; the primary's is the one pushed last.
        imp.goal.set(carets.pop().and_then(|(_, goal)| goal));
        for (caret, (_, goal)) in imp.carets.borrow_mut().iter_mut().zip(&carets) {
            caret.goal = *goal;
        }
        imp.busy.set(false);

        self.collapse();
        self.scroll_mark_onscreen(&insert);
        self.queue_draw();
    }

    /// Two carets driven onto the same character are one caret from here on.
    fn collapse(&self) {
        let buffer = self.buffer();
        let mut seen = vec![buffer.iter_at_mark(&buffer.get_insert()).offset()];
        self.imp().carets.borrow_mut().retain(|caret| {
            let offset = buffer.iter_at_mark(&caret.mark).offset();
            if seen.contains(&offset) {
                buffer.delete_mark(&caret.mark);
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
    use super::{spaces_ahead, spaces_behind, tab_insert, vertical_step, visual_column};

    /// A caret moving into a shorter line stops at its end rather than off it, and keeps aiming
    /// at the column it came from, so the line after that brings it back.
    #[test]
    fn a_caret_crossing_a_short_line_keeps_aiming_at_its_column() {
        assert_eq!(vertical_step(None, 9, 3), (3, 9));
        assert_eq!(vertical_step(Some(9), 3, 10), (9, 9));
        assert_eq!(vertical_step(None, 2, 3), (2, 2));
        assert_eq!(vertical_step(None, 0, 0), (0, 0));
    }

    /// `Ctrl+Delete` on whitespace takes the run and stops; inside a word it is not the
    /// whitespace case at all and GTK's own word deletion is left to it.
    #[test]
    fn a_wordwise_delete_takes_the_whitespace_run_first() {
        assert_eq!(spaces_ahead("   a"), Some(3));
        assert_eq!(spaces_ahead("\t \tx"), Some(3));
        assert_eq!(spaces_ahead("a b"), None);
        assert_eq!(spaces_ahead(""), None);
        assert_eq!(spaces_behind("a   "), Some(3));
        assert_eq!(spaces_behind("a b"), None);
        assert_eq!(spaces_behind(""), None);
    }

    /// Tab reaches the next stop from wherever the caret happens to be, which is why each caret
    /// in a column has to be asked separately.
    #[test]
    fn tab_lands_on_the_next_stop_at_whatever_column_the_caret_is_in() {
        assert_eq!(visual_column("", 4), 0);
        assert_eq!(visual_column("ab", 4), 2);
        assert_eq!(visual_column("\ta", 4), 5);
        assert_eq!(visual_column("ab\t", 4), 4);
        assert_eq!(tab_insert(0, 4, true), "    ");
        assert_eq!(tab_insert(3, 4, true), " ");
        assert_eq!(tab_insert(4, 4, true), "    ");
        assert_eq!(tab_insert(2, 4, false), "\t");
    }
}
