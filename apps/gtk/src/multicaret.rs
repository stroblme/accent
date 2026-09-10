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
//! * while a column of carets exists this widget paints every caret, the primary one included,
//!   because GTK's blink phase cannot be read and two blinks out of step read worse than one:
//!   GTK's own caret goes transparent (`main::install_chrome_css`) and comes back with the column;
//! * the completion popup can open at several carets at once, since it follows the primary.
//!
//! It also paints the ghost text (`ghost.rs`): a suggestion is not in the buffer, so there is
//! nothing to give it a text tag, and this widget is already the one drawing over the text. Focus
//! mode's line fade (`fade.rs`) is drawn here for the same reason.
//!
//! Mirrored at every caret: printable characters, Return, Tab, Backspace, Delete, the arrow, Home
//! and End motions, and the four wordwise chords `Ctrl+Left`, `Ctrl+Right`, `Ctrl+Delete` and
//! `Ctrl+Backspace`, so a column of carets can be moved and edited as one. Everything else —
//! Escape, any Alt combination, any other Ctrl combination, any other key — clears the carets and
//! is then handled as usual, so undo, paste and every accelerator keep working on the primary
//! caret. One undo step covers a whole multi-caret edit, because each replay runs inside a single
//! `begin_user_action`.

use crate::editor::{caret, line_end, line_prefix};
use gtk::glib::translate::IntoGlib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene, pango};
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
    let run = spaces_behind(&line_prefix(buffer, at))?;
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

/// What the text tags at `iter` do to a glyph, as Pango attributes: the size, the weight, the
/// slant and the face. Read from the character *before* the caret, which is the one the caret is
/// writing after — at the end of a heading line the character after it is the newline, which the
/// heading's own span does not cover.
///
/// The tags arrive in ascending priority, and a later attribute of the same kind wins, so the
/// answer is the tag that would win on the character itself.
fn tag_attributes(iter: &gtk::TextIter) -> pango::AttrList {
    let attrs = pango::AttrList::new();
    let mut probe = *iter;
    if !probe.starts_line() {
        probe.backward_char();
    }
    for tag in probe.tags() {
        if tag.is_scale_set() {
            attrs.insert(pango::AttrFloat::new_scale(tag.scale()));
        }
        // Bold or not: the editor's own tags are the only ones here and they are all 700.
        if tag.is_weight_set() && tag.weight() >= pango::Weight::Bold.into_glib() {
            attrs.insert(pango::AttrInt::new_weight(pango::Weight::Bold));
        }
        if tag.is_style_set() {
            attrs.insert(pango::AttrInt::new_style(tag.style()));
        }
        if tag.is_family_set()
            && let Some(family) = tag.family()
        {
            attrs.insert(pango::AttrString::new_family(&family));
        }
    }
    attrs
}

/// The alpha a caret is painted at `elapsed` µs into a blink of period `period` µs: solid for the
/// first two thirds, then down to nothing and back over the last third. Ramped rather than
/// snapped, which is what GTK4 does with the primary caret this stands in for.
fn blink_alpha(elapsed: i64, period: i64) -> f32 {
    let phase = elapsed.rem_euclid(period) as f32 / period as f32;
    match phase < 2.0 / 3.0 {
        true => 1.0,
        false => ((phase - 5.0 / 6.0).abs() * 6.0).clamp(0.0, 1.0),
    }
}

/// How much of the text colour ghost text keeps. Enough to read, little enough that it is never
/// mistaken for what the document says.
const GHOST_ALPHA: f32 = 0.45;

/// The class whose CSS (`main::install_chrome_css`) makes GTK's own caret transparent, so this
/// widget can paint every caret on one phase.
const CARETS_CLASS: &str = "accent-carets";

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
        /// The frame time the blink phase last restarted at, so every caret fades together and a
        /// caret being typed at is solid.
        pub blinked_at: Cell<i64>,
        /// The tick callback that repaints the blink, while there is one to repaint.
        pub blink: RefCell<Option<gtk::TickCallbackId>>,
        /// Whether Up and Down move by a line of the document rather than by a row of the
        /// screen. What a column of carets asks for in code; in wrapped prose one Down would be
        /// a whole paragraph, which can be several screens.
        pub logical_lines: Cell<bool>,
        /// Focus mode's line fade (`fade.rs`): whether it is wanted, how far in it is from 0 to
        /// 1, the frame time and strength its ramp started from, and the tick running the ramp.
        pub fade_on: Cell<bool>,
        pub fade: Cell<f32>,
        pub fade_from: Cell<(i64, f32)>,
        pub fade_tick: RefCell<Option<gtk::TickCallbackId>>,
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
                        if moved != Some("insert") && moved != Some("selection_bound") {
                            return;
                        }
                        // The fade is measured from the caret, so it follows it.
                        if obj.imp().fade.get() > 0.0 {
                            obj.queue_draw();
                        }
                        if !obj.imp().busy.get() {
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
            // Focus mode's veil first, so nothing drawn after it — the carets, the suggestion —
            // is ever veiled.
            let fade = self.fade.get();
            if fade > 0.0 {
                crate::fade::paint(&obj, &snapshot, fade);
            }
            let buffer = obj.buffer();
            let colour = obj.color();
            // Every caret on one phase, the primary one included: GTK's is transparent while the
            // column exists, because its own blink cannot be read and two out of step is worse
            // than one we draw. This layer draws in buffer coordinates, which is what
            // `iter_location` reports.
            let carets = self.carets.borrow();
            if !carets.is_empty() {
                let alpha = obj.blink_phase().map_or(1.0, |(e, p)| blink_alpha(e, p));
                let tint = crate::highlight::with_alpha(colour, colour.alpha() * alpha);
                let insert = buffer.get_insert();
                for mark in carets.iter().map(|c| &c.mark).chain([&insert]) {
                    let at = obj.iter_location(&buffer.iter_at_mark(mark));
                    snapshot.append_color(
                        &tint,
                        &graphene::Rect::new(at.x() as f32, at.y() as f32, 1.0, at.height() as f32),
                    );
                }
            }
            // The suggestion sits after the caret in the text's own font, dimmed enough to read
            // as not-yet-written. It is only ever asked for at the end of a line, so there is
            // nothing to its right to draw over.
            if let Some(text) = self.ghost.borrow().as_deref() {
                let caret = caret(&buffer);
                let at = obj.iter_location(&caret);
                let dim = crate::highlight::with_alpha(colour, colour.alpha() * GHOST_ALPHA);
                let layout = obj.create_pango_layout(Some(text));
                // The layout carries the view's font and nothing the tags at the caret say, so
                // inside a heading the suggestion was drawn at body size next to text at 1.6.
                layout.set_attributes(Some(&tag_attributes(&caret)));
                snapshot.save();
                snapshot.translate(&graphene::Point::new(at.x() as f32, at.y() as f32));
                snapshot.append_layout(&layout, &dim);
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
                && let Some((mut from, mut to)) = space_range(&buffer, &caret(&buffer), count > 0)
            {
                buffer.begin_user_action();
                buffer.delete(&mut from, &mut to);
                buffer.end_user_action();
                obj.scroll_mark_onscreen(&buffer.get_insert());
                return;
            }
            self.parent_delete_from_cursor(type_, count);
        }

        /// Up and Down move by a line of the document where the view asked for it, and Home and
        /// End go to that line's ends: a wrapped line of code is one line to move through, not a
        /// screenful of rows. `Pages` and everything else keep GTK's display-based behaviour,
        /// which is what they are for.
        fn move_cursor(&self, step: gtk::MovementStep, count: i32, extend: bool) {
            match step {
                gtk::MovementStep::DisplayLines if self.logical_lines.get() => {
                    self.obj().move_by_lines(count, extend)
                }
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

    /// Make Up and Down step a line of the document rather than a row of the screen. Set for the
    /// code flavours, where a wrapped line is one statement and the column is what the key is
    /// asked for; prose keeps GTK's own behaviour, where a paragraph is a line and stepping over
    /// one would be several screens.
    pub fn set_logical_lines(&self, on: bool) {
        self.imp().logical_lines.set(on);
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

    /// Bring focus mode's line fade in or take it away, over the chrome's own transition. It
    /// jumps where animations are off, as the chrome does, and where the view is not realised
    /// and has no frame clock to ramp on.
    pub fn set_fade(&self, on: bool) {
        let imp = self.imp();
        if imp.fade_on.replace(on) == on {
            return;
        }
        let clock = self
            .frame_clock()
            .filter(|_| self.settings().is_gtk_enable_animations());
        let Some(clock) = clock else {
            imp.fade.set(if on { 1.0 } else { 0.0 });
            self.queue_draw();
            return;
        };
        // From wherever a ramp still running has got to, so a quick toggle turns it round.
        imp.fade_from.set((clock.frame_time(), imp.fade.get()));
        if imp.fade_tick.borrow().is_some() {
            return;
        }
        let id = self.add_tick_callback(|obj, clock| {
            let imp = obj.imp();
            let (start, from) = imp.fade_from.get();
            let to = if imp.fade_on.get() { 1.0 } else { 0.0 };
            let t = ((clock.frame_time() - start) as f32 / (crate::fade::RAMP_MS * 1000) as f32)
                .min(1.0);
            imp.fade.set(from + (to - from) * t);
            obj.queue_draw();
            if t < 1.0 {
                return glib::ControlFlow::Continue;
            }
            imp.fade_tick.take();
            glib::ControlFlow::Break
        });
        *imp.fade_tick.borrow_mut() = Some(id);
    }

    /// Whether the line fade is on, for the headless check that cannot see it.
    pub(crate) fn fading(&self) -> bool {
        self.imp().fade_on.get()
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
        self.blink_on();
        self.queue_draw();
    }

    /// How long the blink has been running and the period it runs at, or `None` once it has
    /// settled: blinking switched off, or GTK's blink timeout passed with the carets left solid.
    fn blink_phase(&self) -> Option<(i64, i64)> {
        let settings = self.settings();
        let elapsed = self.frame_clock()?.frame_time() - self.imp().blinked_at.get();
        (settings.is_gtk_cursor_blink()
            && elapsed <= settings.gtk_cursor_blink_timeout() as i64 * 1_000_000)
            .then(|| (elapsed, settings.gtk_cursor_blink_time() as i64 * 1_000))
    }

    /// Take the blink over and restart its phase, so a caret is solid the moment it is typed at
    /// the way GTK's own is. The tick callback stops itself once the phase has settled.
    fn blink_on(&self) {
        let imp = self.imp();
        self.add_css_class(CARETS_CLASS);
        if let Some(clock) = self.frame_clock() {
            imp.blinked_at.set(clock.frame_time());
        }
        if imp.blink.borrow().is_some() {
            return;
        }
        let id = self.add_tick_callback(|obj, _| {
            obj.queue_draw();
            if obj.blink_phase().is_some() {
                return glib::ControlFlow::Continue;
            }
            obj.imp().blink.take();
            glib::ControlFlow::Break
        });
        *imp.blink.borrow_mut() = Some(id);
    }

    /// Hand the caret back to GTK: no column left to keep in step.
    fn blink_off(&self) {
        self.remove_css_class(CARETS_CLASS);
        if let Some(id) = self.imp().blink.take() {
            id.remove();
        }
    }

    /// Whether a key press is going to be replayed at more than one caret, which is what
    /// `editor::keys` asks before offering a press to anything below the carets in its chain.
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
        self.blink_off();
        self.queue_draw();
    }

    /// The caret furthest down (or up), which is the one the next line is measured from.
    fn outermost(&self, below: bool) -> gtk::TextIter {
        let buffer = self.buffer();
        let mut furthest = caret(&buffer);
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
        let mut offsets = vec![caret(&buffer).offset()];
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

    /// One key press, at every caret. Called by `editor::keys`, and by the headless check that
    /// drives the carets the way that dispatcher does, which is the only way to see them without
    /// a screen.
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
                    let column = visual_column(&line_prefix(&buffer, &at), width);
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
        // Solid again from here, and back to GTK's caret if the column has collapsed into one.
        match self.has_carets() {
            true => self.blink_on(),
            false => self.blink_off(),
        }
        self.scroll_mark_onscreen(&insert);
        self.queue_draw();
    }

    /// Two carets driven onto the same character are one caret from here on.
    fn collapse(&self) {
        let buffer = self.buffer();
        let mut seen = vec![caret(&buffer).offset()];
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

fn line_length(buffer: &gtk::TextBuffer, line: i32) -> i32 {
    line_end(buffer, line).line_offset()
}

#[cfg(test)]
mod tests {
    use super::{
        blink_alpha, spaces_ahead, spaces_behind, tab_insert, vertical_step, visual_column,
    };

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

    /// One phase for every caret: solid most of the way through, a ramp down to nothing and back,
    /// and the same value again a period later.
    #[test]
    fn the_blink_is_solid_most_of_the_period_and_ramps_through_the_rest() {
        assert_eq!(blink_alpha(0, 1200), 1.0);
        assert_eq!(blink_alpha(600, 1200), 1.0);
        assert!(blink_alpha(1000, 1200) < 0.01);
        assert!((0.01..0.99).contains(&blink_alpha(900, 1200)));
        assert_eq!(blink_alpha(1200, 1200), blink_alpha(0, 1200));
        assert_eq!(blink_alpha(2400, 1200), 1.0);
    }
}
