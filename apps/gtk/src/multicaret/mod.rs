//! A `sourceview5::View` with secondary carets, occurrence selection and box selection.
//!
//! GtkTextView supports one insert mark. Secondary carets use right-gravity marks for caret
//! and anchor, while GTK retains the primary caret for normal selection, IME and scrolling.
//! The view draws extra carets and replays supported edits and motions at each one.
//!
//! A shared `TextViewImpl` also corrects single- and multi-caret motions: Ctrl+Delete removes
//! whitespace before a word, and Up, Down, Home and End follow document lines.
//!
//! Clicks and external caret moves clear secondaries; Shift+Alt drag creates a box. Dead
//! keys end the column so IME preedit can reach the primary. All carets share custom blinking
//! while secondaries exist because GTK's blink phase is inaccessible.
//!
//! Automatic completion and ghost text are suppressed during multi-caret editing. Explicit
//! Ctrl+Space completes at the primary only, ending the column on typing or acceptance.
//!
//! Printable text, Return, Tab, deletion and supported motions replay at every caret;
//! Shift extends each selection, overlapping selections merge, and paste reaches every caret.
//! Undo and Redo restore caret state with the text; each multi-caret edit is one user action.
//! Unbound keys retain the column; external edits, moves and Escape end it.
//!
//! The view also paints ghost text and focus-mode veils, neither of which belongs in the buffer.

use crate::editor::{caret, line_end, line_prefix};
use gtk::glib::translate::IntoGlib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, gio, glib, graphene, pango};
use sourceview5::prelude::ViewExt as _;

mod replay;

/// What a key means at every caret. Anything outside this list is left to GTK.
enum Edit {
    Insert(String),
    /// Resolved per caret, because what Tab inserts depends on the column it is pressed in.
    Tab,
    Backspace,
    Delete,
    /// `Ctrl+Delete` forwards, `Ctrl+Backspace` backwards.
    DeleteWord(bool),
    /// A motion, and whether Shift holds each selection's anchor where it is while its caret
    /// moves.
    Move(Motion, bool),
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
    PageUp,
    PageDown,
}

/// Selection anchor and caret as character offsets, equal when empty.
/// Public for the undo record in `imp`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Span {
    anchor: i32,
    caret: i32,
}

impl Span {
    fn start(self) -> i32 {
        self.anchor.min(self.caret)
    }

    fn end(self) -> i32 {
        self.anchor.max(self.caret)
    }

    fn is_empty(self) -> bool {
        self.anchor == self.caret
    }
}

/// Merge overlapping selections and empty carets touching selections;
/// merely adjacent nonempty selections remain separate. Keep the earliest
/// caret (primary first), returning its index and union in its direction.
/// Empty survivors retain their union endpoint where possible.
fn merge(spans: &[Span]) -> Vec<(usize, Span)> {
    let mut order: Vec<usize> = (0..spans.len()).collect();
    order.sort_by_key(|&i| (spans[i].start(), spans[i].end()));
    // Each group as its survivor and the union it covers so far.
    let mut groups: Vec<(usize, i32, i32)> = Vec::new();
    for i in order {
        let span = spans[i];
        match groups.last_mut() {
            Some((survivor, start, end))
                if span.start() < *end
                    || (span.start() == *end && (span.is_empty() || start == end)) =>
            {
                *end = (*end).max(span.end());
                *survivor = (*survivor).min(i);
            }
            _ => groups.push((i, span.start(), span.end())),
        }
    }
    groups
        .into_iter()
        .map(|(survivor, start, end)| {
            let own = spans[survivor];
            let backward = match own.is_empty() {
                true => own.caret == start,
                false => own.caret < own.anchor,
            };
            let span = match backward {
                true => Span {
                    anchor: end,
                    caret: start,
                },
                false => Span {
                    anchor: start,
                    caret: end,
                },
            };
            (survivor, span)
        })
        .collect()
}

/// Without Shift, Left/Right collapse to selection start/end only;
/// Up/Page Up move from its start, Down/Page Down from its end.
/// Other motions start from the caret (VS Code semantics).
fn departure(motion: Motion, span: Span) -> (i32, bool) {
    if span.is_empty() {
        return (span.caret, true);
    }
    match motion {
        Motion::Left => (span.start(), false),
        Motion::Right => (span.end(), false),
        Motion::Up | Motion::PageUp => (span.start(), true),
        Motion::Down | Motion::PageDown => (span.end(), true),
        _ => (span.caret, true),
    }
}

/// End the column on Escape or IME-sequence starts (dead keys, Compose,
/// Ctrl+Shift+U), allowing following keys to reach the input method.
/// Standalone modifiers retain the column, including AltGr for `@`.
fn ends_column(key: gdk::Key, state: gdk::ModifierType) -> bool {
    let unicode_entry = state
        .contains(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::SHIFT_MASK)
        && key.to_lower() == gdk::Key::u;
    key == gdk::Key::Escape
        || key == gdk::Key::Multi_key
        || key.name().is_some_and(|name| name.starts_with("dead_"))
        || unicode_entry
}

/// `Some(true)` for GtkTextView's Undo chord, `Ctrl+Z`, and `Some(false)` for its Redo,
/// `Ctrl+Shift+Z` or `Ctrl+Y`.
fn undo_or_redo(key: gdk::Key, state: gdk::ModifierType) -> Option<bool> {
    if !state.contains(gdk::ModifierType::CONTROL_MASK)
        || state.contains(gdk::ModifierType::ALT_MASK)
    {
        return None;
    }
    let shift = state.contains(gdk::ModifierType::SHIFT_MASK);
    match key.to_lower() {
        gdk::Key::z => Some(!shift),
        gdk::Key::y if !shift => Some(false),
        _ => None,
    }
}

/// Spread paste one line per caret when counts match; otherwise paste
/// all text at each caret (VS Code "spread"). Ignore a trailing newline
/// from whole-line copies when counting lines.
fn spread(text: &str, carets: usize) -> Vec<&str> {
    let lines: Vec<&str> = text.lines().collect();
    match carets > 1 && lines.len() == carets {
        true => lines,
        false => vec![text; carets],
    }
}

/// Map `key` and `state` to a mirrored edit, or `None` for GTK handling.
///
/// Only supported Ctrl motions have mappings; Alt combinations remain GTK's. Shift+Delete
/// also stays GTK Cut, whose signal reaches the column-aware `editor::lines` handler.
fn edit_for(key: gdk::Key, state: gdk::ModifierType) -> Option<Edit> {
    if state.contains(gdk::ModifierType::ALT_MASK) {
        return None;
    }
    let extend = state.contains(gdk::ModifierType::SHIFT_MASK);
    if state.contains(gdk::ModifierType::CONTROL_MASK) {
        return match key {
            gdk::Key::Left | gdk::Key::KP_Left => Some(Edit::Move(Motion::WordLeft, extend)),
            gdk::Key::Right | gdk::Key::KP_Right => Some(Edit::Move(Motion::WordRight, extend)),
            gdk::Key::Delete | gdk::Key::KP_Delete => Some(Edit::DeleteWord(true)),
            gdk::Key::BackSpace => Some(Edit::DeleteWord(false)),
            _ => None,
        };
    }
    let motion = |motion| Some(Edit::Move(motion, extend));
    match key {
        gdk::Key::Return | gdk::Key::KP_Enter => Some(Edit::Insert("\n".to_string())),
        gdk::Key::Tab | gdk::Key::KP_Tab => Some(Edit::Tab),
        gdk::Key::BackSpace => Some(Edit::Backspace),
        gdk::Key::Delete | gdk::Key::KP_Delete if !extend => Some(Edit::Delete),
        gdk::Key::Left | gdk::Key::KP_Left => motion(Motion::Left),
        gdk::Key::Right | gdk::Key::KP_Right => motion(Motion::Right),
        gdk::Key::Up | gdk::Key::KP_Up => motion(Motion::Up),
        gdk::Key::Down | gdk::Key::KP_Down => motion(Motion::Down),
        gdk::Key::Home | gdk::Key::KP_Home => motion(Motion::Home),
        gdk::Key::End | gdk::Key::KP_End => motion(Motion::End),
        gdk::Key::Page_Up | gdk::Key::KP_Page_Up => motion(Motion::PageUp),
        gdk::Key::Page_Down | gdk::Key::KP_Page_Down => motion(Motion::PageDown),
        _ => key
            .to_unicode()
            .filter(|c| !c.is_control())
            .map(|c| Edit::Insert(c.to_string())),
    }
}

/// Clamp a vertical destination to `len` while retaining `goal`, restoring
/// the desired column after crossing short lines.
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

/// Find nonoverlapping literal, case-sensitive `needle` occurrences as
/// character ranges. With `whole`, reject adjacent letters, digits or
/// underscores, matching VS Code current-word selection semantics.
fn occurrences<'a>(
    text: &'a str,
    needle: &'a str,
    whole: bool,
) -> impl Iterator<Item = (i32, i32)> + 'a {
    let len = needle.chars().count() as i32;
    let (mut byte, mut chars) = (0, 0);
    let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
    text.match_indices(needle)
        .filter(move |_| !needle.is_empty())
        .filter(move |&(at, _)| {
            let (before, after) = (&text[..at], &text[at + needle.len()..]);
            !whole || !(word(before.chars().next_back()) || word(after.chars().next()))
        })
        .map(move |(at, _)| {
            chars += text[byte..at].chars().count() as i32;
            byte = at;
            (chars, chars + len)
        })
}

/// Find the next untaken occurrence at/after `from`, wrapping to the top.
/// Skip overlaps with `taken`; `None` means all are taken. `whole`
/// follows [`occurrences`].
fn next_occurrence(
    text: &str,
    needle: &str,
    from: i32,
    taken: &[(i32, i32)],
    whole: bool,
) -> Option<(i32, i32)> {
    let free: Vec<(i32, i32)> = occurrences(text, needle, whole)
        .filter(|&(start, end)| !taken.iter().any(|&(s, e)| start < e && s < end))
        .collect();
    free.iter()
        .find(|(start, _)| *start >= from)
        .or(free.first())
        .copied()
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

/// Whitespace word-delete range at `at`, bounded to the line because
/// a newline is a word boundary.
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

/// Convert effective glyph tags to Pango size, weight, slant and family attributes.
///
/// Read the character before the caret so heading-end styling is not lost at an untagged
/// newline. Ascending tag priority lets later attributes override earlier ones.
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

/// Caret alpha at `elapsed` µs within `period` µs: solid for two-thirds,
/// then fade out and back, matching GTK4 primary-caret blinking.
fn blink_alpha(elapsed: i64, period: i64) -> f32 {
    let phase = elapsed.rem_euclid(period) as f32 / period as f32;
    match phase < 2.0 / 3.0 {
        true => 1.0,
        false => ((phase - 5.0 / 6.0).abs() * 6.0).clamp(0.0, 1.0),
    }
}

/// Ghost-text opacity, keeping suggestions readable but distinct from saved text.
const GHOST_ALPHA: f32 = 0.45;

/// The class whose CSS (`main::install_chrome_css`) makes GTK's own caret transparent, so this
/// widget can paint every caret on one phase.
const CARETS_CLASS: &str = "accent-carets";

mod imp {
    use super::*;
    use std::cell::{Cell, RefCell};

    /// Secondary caret marks, anchor and retained vertical goal. `below`
    /// records Add Caret direction (`true` below), or `None` for other origins.
    pub struct Caret {
        pub mark: gtk::TextMark,
        pub anchor: gtk::TextMark,
        pub goal: Option<i32>,
        pub below: Option<bool>,
    }

    impl Caret {
        /// Create caret/anchor marks with right gravity so typing at their
        /// shared position carries both along, as with GTK's primary.
        pub fn new(buffer: &gtk::TextBuffer, from: &gtk::TextIter, at: &gtk::TextIter) -> Self {
            Caret {
                mark: buffer.create_mark(None, at, false),
                anchor: buffer.create_mark(None, from, false),
                goal: None,
                below: None,
            }
        }

        pub fn delete(&self, buffer: &gtk::TextBuffer) {
            buffer.delete_mark(&self.mark);
            buffer.delete_mark(&self.anchor);
        }
    }

    /// Primary-first carets before/after an undo step, with buffer character
    /// counts identifying the corresponding GTK history state.
    pub struct Step {
        pub before: Vec<Span>,
        pub after: Vec<Span>,
        pub lengths: (i32, i32),
    }

    #[derive(Default)]
    pub struct View {
        /// The secondary carets, whose marks ride along with the text.
        pub carets: RefCell<Vec<Caret>>,
        /// Column undo/redo records, newest last, supplementing GTK's one-caret
        /// history. `None` denotes an older step without column state.
        /// New columns start with empty stacks.
        pub undo: RefCell<Vec<Option<Step>>>,
        pub redo: RefCell<Vec<Option<Step>>>,
        /// The suggestion painted after the caret, if one is showing.
        pub ghost: RefCell<Option<String>>,
        /// Programmatic-edit guard so mark/change hooks do not discard carets
        /// as if external movement occurred.
        pub busy: Cell<bool>,
        /// Primary goal column, like [`Caret::goal`], cleared by edits or
        /// non-column movement.
        pub goal: Cell<Option<i32>>,
        /// Whole-word matching for a selected current word
        /// ([`super::View::match_whole_words`]), cleared by external caret movement.
        pub whole_words: Cell<bool>,
        /// The frame time the blink phase last restarted at, so every caret fades together and a
        /// caret being typed at is solid.
        pub blinked_at: Cell<i64>,
        /// The tick callback that repaints the blink, while there is one to repaint.
        pub blink: RefCell<Option<gtk::TickCallbackId>>,
        /// Document-line vertical movement for code. Wrapped prose keeps display
        /// rows because one paragraph may span several screens.
        pub logical_lines: Cell<bool>,
        /// Focus mode's line fade (`fade.rs`): whether it is wanted, how far in it is from 0 to
        /// 1, the frame time and strength its ramp started from, and the tick running the ramp.
        pub fade_on: Cell<bool>,
        pub fade: Cell<f32>,
        pub fade_from: Cell<(i64, f32)>,
        pub fade_tick: RefCell<Option<gtk::TickCallbackId>>,
        /// The tag the find bar paints its matches in this view's buffer with, which the fade
        /// leaves unveiled.
        pub find_tag: RefCell<Option<gtk::TextTag>>,
        /// Where the fade is measured from instead of this view's carets
        /// ([`super::View::fade_from`]).
        pub fade_follows: RefCell<Option<Follows>>,
        /// Told when a column starts ([`super::View::on_column_started`]).
        pub column_started: RefCell<Option<Box<dyn Fn()>>>,
    }

    /// The lines a view's fade is measured from when not its own carets', 0-based.
    pub type Follows = Box<dyn Fn() -> Option<std::ops::RangeInclusive<i32>>>;

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
            box_drag(&obj);

            // The buffer arrives after construction. Every `mark-set` on it that we did not
            // cause is the user moving the primary caret — a click, a selection, a find-bar
            // jump — so this one hook covers all of them without a gesture of its own.
            obj.connect_buffer_notify(|obj| {
                obj.buffer().connect_mark_set(glib::clone!(
                    #[weak]
                    obj,
                    move |buffer, at, mark| {
                        let moved = mark.name();
                        let moved = moved.as_deref();
                        if moved != Some("insert") && moved != Some("selection_bound") {
                            return;
                        }
                        // The fade is measured from the caret, so it follows it.
                        if obj.imp().fade.get() > 0.0 {
                            obj.queue_draw();
                        }
                        // The selection bound put back on a caret that did not move is GTK
                        // letting the primary selection go when another window takes it
                        // (`gtk_text_buffer_content_detach`): the column stays, less the
                        // primary caret's selection.
                        if moved == Some("selection_bound")
                            && *at == buffer.iter_at_mark(&buffer.get_insert())
                        {
                            return;
                        }
                        if !obj.imp().busy.get() {
                            obj.imp().goal.set(None);
                            obj.imp().whole_words.set(false);
                            obj.clear_carets();
                        }
                    }
                ));
                // An edit is not vertical movement, so the column the caret was aiming for goes
                // with it, whoever made the edit. One this widget did not make — a completion,
                // the input method, a chord GTK answers at the primary caret alone — went to one
                // caret of the column, so it ends the column the way a caret move does.
                obj.buffer().connect_changed(glib::clone!(
                    #[weak]
                    obj,
                    move |_| {
                        obj.imp().goal.set(None);
                        if !obj.imp().busy.get() {
                            obj.clear_carets();
                        }
                    }
                ));
            });
        }
    }

    impl WidgetImpl for View {
        /// Skip frames where gutter iter lookup would abort on newly hidden,
        /// unvalidated lines ([`crate::fold::aborts_at`]). Request another frame after
        /// GTK's idle measurement.
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();
            let visible = obj.visible_rect();
            if [visible.y(), visible.y() + visible.height()]
                .into_iter()
                .any(|y| crate::fold::aborts_at(&*obj, y))
            {
                let view = obj.downgrade();
                glib::idle_add_local_once(move || {
                    if let Some(view) = view.upgrade() {
                        view.queue_draw();
                    }
                });
                return;
            }
            self.parent_snapshot(snapshot);
        }
    }

    impl TextViewImpl for View {
        fn snapshot_layer(&self, layer: gtk::TextViewLayer, snapshot: gtk::Snapshot) {
            self.parent_snapshot_layer(layer, snapshot.clone());
            let obj = self.obj();
            // The other carets' selections go under the text, where GTK paints the primary's.
            if layer == gtk::TextViewLayer::BelowText {
                obj.paint_selections(&snapshot);
                return;
            }
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
                let tint = crate::theme::at(colour, colour.alpha() * alpha);
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
                let dim = crate::theme::at(colour, colour.alpha() * GHOST_ALPHA);
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

        /// Delete whitespace separately for Ctrl+Delete/Backspace. GTK word
        /// boundaries would cross the indent and consume the following word
        /// in one press.
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

        /// A paste at a column goes to every caret ([`super::View::paste_at_carets`]). The key
        /// bindings and the context menu all come through here.
        fn paste_clipboard(&self) {
            let obj = self.obj();
            if !obj.has_carets() {
                return self.parent_paste_clipboard();
            }
            // Asynchronous even from this process; the marks ride out anything typed meanwhile.
            obj.clipboard().read_text_async(
                gio::Cancellable::NONE,
                glib::clone!(
                    #[weak]
                    obj,
                    move |text| {
                        if let Ok(Some(text)) = text {
                            obj.paste_at_carets(&text);
                        }
                    }
                ),
            );
        }

        /// Use document lines for requested Up/Down/Home/End code movement.
        /// Retain GTK display paging and other motions, correcting carets
        /// left on hidden lines.
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
                    // GTK passes over a shut block, but where nothing is shown past it — Down at
                    // the end of the text — it lands inside, out of sight.
                    let obj = self.obj();
                    let buffer = obj.buffer();
                    let insert = buffer.get_insert();
                    if let Some(to) = crate::fold::shown(&buffer, &buffer.iter_at_mark(&insert)) {
                        match extend {
                            true => buffer.move_mark(&insert, &to),
                            false => buffer.place_cursor(&to),
                        }
                        obj.scroll_mark_onscreen(&insert);
                    }
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

    /// Use document-line Up/Down for code so wrapped statements remain one
    /// vertical step. Prose retains GTK display rows to avoid skipping
    /// whole wrapped paragraphs.
    pub fn set_logical_lines(&self, on: bool) {
        self.imp().logical_lines.set(on);
    }

    /// Paint ghost text without inserting into the buffer, preserving undo,
    /// save contents and `changed` state.
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

    /// Animate line fade with the chrome transition; jump when animations
    /// are disabled or the unrealized view lacks a frame clock.
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

    /// Anchor fade to `follows` lines for read-only comparison peers whose
    /// carets do not move.
    pub fn fade_from(&self, follows: impl Fn() -> Option<std::ops::RangeInclusive<i32>> + 'static) {
        self.imp().fade_follows.replace(Some(Box::new(follows)));
    }

    /// The lines [`View::fade_from`] names, if it was asked to.
    pub(crate) fn fade_followed(&self) -> Option<std::ops::RangeInclusive<i32>> {
        self.imp().fade_follows.borrow().as_ref()?()
    }

    /// Whether the line fade shows at all, so that whatever moves what it follows can redraw.
    pub fn fade_shown(&self) -> bool {
        self.imp().fade.get() > 0.0
    }

    /// Whether the line fade is on, for the headless check that cannot see it.
    #[cfg(feature = "bench")]
    pub(crate) fn fading(&self) -> bool {
        self.imp().fade_on.get()
    }

    /// Hand the view the find bar's match tag, so the line fade can leave its matches be.
    pub fn set_find_tag(&self, tag: &gtk::TextTag) {
        self.imp().find_tag.replace(Some(tag.clone()));
    }

    /// The find bar's match tag, which is on nothing while the bar's highlight is off.
    pub(crate) fn find_tag(&self) -> Option<gtk::TextTag> {
        self.imp().find_tag.borrow().clone()
    }

    /// Add a caret above or below the outermost caret, or remove the last opposite addition.
    ///
    /// Repeated actions grow away from the primary; reversing them unwinds the column first.
    /// A single-line primary selection extends as a box with columns clipped to each line.
    /// Hidden lines are revealed through `fold::reveal`; return their offset for relayout.
    pub fn add_caret(&self, below: bool) -> Option<i32> {
        // Out of the cell before the marks go: `delete_mark` emits `mark-deleted`.
        let newest = self
            .imp()
            .carets
            .borrow_mut()
            .pop_if(|caret| caret.below == Some(!below));
        if let Some(caret) = newest {
            caret.delete(&self.buffer());
            self.show_column();
            return None;
        }
        let buffer = self.buffer();
        let from = self.outermost(below);
        let line = from.line() + if below { 1 } else { -1 };
        if line < 0 || line >= buffer.line_count() {
            return None;
        }
        let mut target = buffer.iter_at_line(line)?;
        let opened = crate::fold::reveal(&buffer, &target).then(|| target.offset());
        let (anchor, caret) = self
            .box_columns()
            .unwrap_or((from.line_offset(), from.line_offset()));
        let len = line_length(&buffer, line);
        let (column, goal) = vertical_step(None, caret, len);
        target.set_line_offset(column);
        let mut start = target;
        start.set_line_offset(anchor.min(len));

        // A caret already there would be a second one on the same character, which is one caret.
        if self
            .spans()
            .iter()
            .any(|span| span.caret == target.offset())
        {
            return opened;
        }
        self.push_caret(&start, &target, Some(goal), Some(below));
        // One landing inside a selection is part of it.
        self.collapse();
        self.show_column();
        opened
    }

    /// The columns the primary's selection runs between, anchor first, where it is on one line:
    /// what Add Caret Above and Below grow a box from.
    fn box_columns(&self) -> Option<(i32, i32)> {
        let buffer = self.buffer();
        let anchor = buffer.iter_at_mark(&buffer.selection_bound());
        let at = caret(&buffer);
        (anchor != at && anchor.line() == at.line())
            .then(|| (anchor.line_offset(), at.line_offset()))
    }

    /// Add a caret for the next primary-selection occurrence (Alt+J),
    /// wrapping and skipping existing selections. Leave state unchanged
    /// when no occurrence remains.
    pub fn add_next_occurrence(&self) {
        let buffer = self.buffer();
        let Some((start, end)) = buffer.selection_bounds() else {
            return;
        };
        let needle = buffer.text(&start, &end, true);
        let (all_start, all_end) = buffer.bounds();
        let text = buffer.text(&all_start, &all_end, true);
        let spans = self.spans();
        // The primary is `spans[0]` and a new caret is pushed last, so the last is the newest.
        let from = spans.last().map_or(0, |span| span.end());
        let taken: Vec<(i32, i32)> = spans.iter().map(|s| (s.start(), s.end())).collect();
        let whole = self.imp().whole_words.get();
        let Some((from, to)) = next_occurrence(&text, &needle, from, &taken, whole) else {
            return;
        };
        let at = |offset| buffer.iter_at_offset(offset);
        self.push_caret(&at(from), &at(to), None, None);
        self.show_column();
        if let Some(caret) = self.imp().carets.borrow().last() {
            self.scroll_mark_onscreen(&caret.mark);
        }
    }

    /// Match the selected current word as whole words until manual movement,
    /// excluding `foo` inside `foo_bar` for occurrence commands.
    pub fn match_whole_words(&self) {
        self.imp().whole_words.set(true);
    }

    /// Select all remaining occurrences (Ctrl+Shift+L) in one text scan,
    /// avoiding a full buffer read per repeated [`Self::add_next_occurrence`].
    pub fn select_all_occurrences(&self) {
        let buffer = self.buffer();
        let Some((start, end)) = buffer.selection_bounds() else {
            return;
        };
        let needle = buffer.text(&start, &end, true);
        let (all_start, all_end) = buffer.bounds();
        let text = buffer.text(&all_start, &all_end, true);
        let taken: Vec<(i32, i32)> = self.spans().iter().map(|s| (s.start(), s.end())).collect();
        let at = |offset| buffer.iter_at_offset(offset);
        for (from, to) in occurrences(&text, &needle, self.imp().whole_words.get()) {
            if !taken.iter().any(|&(s, e)| from < e && s < to) {
                self.push_caret(&at(from), &at(to), None, None);
            }
        }
        self.show_column();
    }

    /// Add a secondary caret at `at` whose selection runs from `from`, aiming for column `goal`
    /// on its way up and down; `below` is [`imp::Caret::below`].
    fn push_caret(
        &self,
        from: &gtk::TextIter,
        at: &gtk::TextIter,
        goal: Option<i32>,
        below: Option<bool>,
    ) {
        if !self.has_carets() {
            self.start_column();
        }
        let mut caret = imp::Caret::new(&self.buffer(), from, at);
        caret.goal = goal;
        caret.below = below;
        self.imp().carets.borrow_mut().push(caret);
    }

    /// Start a new column with fresh undo records and dismiss completion
    /// so its popup cannot consume keys intended for all carets.
    fn start_column(&self) {
        self.imp().undo.take();
        self.imp().redo.take();
        if let Some(told) = self.imp().column_started.borrow().as_ref() {
            told();
        }
    }

    /// Call `f` whenever a column of carets starts: the completion popup goes then.
    pub(crate) fn on_column_started(&self, f: impl Fn() + 'static) {
        *self.imp().column_started.borrow_mut() = Some(Box::new(f));
    }

    /// Replace the column with a box between `from` and `to` in buffer coordinates.
    ///
    /// Each document line gets one caret selecting between x positions, clipped without padding.
    /// Short lines get an end caret; wrapped lines use the row nearest `from`. The starting
    /// caret stays primary and the ending caret is newest.
    pub(crate) fn select_box(&self, from: (i32, i32), to: (i32, i32)) {
        let buffer = self.buffer();
        let first = self.line_at_y(from.1).0.line();
        let last = self.line_at_y(to.1).0.line();
        let lines: Vec<i32> = match first <= last {
            true => (first..=last).collect(),
            false => (last..=first).rev().collect(),
        };
        let view = self.upcast_ref::<sourceview5::View>();
        let spans: Vec<Span> = lines
            .into_iter()
            .filter_map(|line| {
                let start = buffer.iter_at_line(line)?;
                let top = self.iter_location(&start);
                let bottom = self.iter_location(&line_end(&buffer, line));
                let y = from.1.clamp(top.y(), bottom.y() + bottom.height() - 1);
                let at = |x| crate::editor::pressed_at(view, x, y).offset();
                Some(Span {
                    anchor: at(from.0),
                    caret: at(to.0),
                })
            })
            .collect();
        if spans == self.spans() {
            return;
        }
        if spans.len() > 1 && !self.has_carets() {
            self.start_column();
        }
        self.imp().goal.set(None);
        self.put_carets(&spans);
        self.collapse();
        self.show_column();
        let newest = self.imp().carets.borrow().last().map(|c| c.mark.clone());
        self.scroll_mark_onscreen(&newest.unwrap_or_else(|| buffer.get_insert()));
    }

    /// Repaint after column changes, transferring blink ownership from GTK
    /// while extra carets exist and returning it afterwards.
    fn show_column(&self) {
        match self.has_carets() {
            true => self.blink_on(),
            false => self.blink_off(),
        }
        self.queue_draw();
    }

    /// Return blink elapsed time and period, or `None` after disabled blinking
    /// or GTK timeout leaves carets solid.
    fn blink_phase(&self) -> Option<(i64, i64)> {
        let settings = self.settings();
        let elapsed = self.frame_clock()?.frame_time() - self.imp().blinked_at.get();
        (settings.is_gtk_cursor_blink()
            && elapsed <= settings.gtk_cursor_blink_timeout() as i64 * 1_000_000)
            .then(|| (elapsed, settings.gtk_cursor_blink_time() as i64 * 1_000))
    }

    /// Restart the shared blink phase solid on typing, as GTK does.
    /// The tick stops when blinking settles.
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
        // Taken out of the cell before any of them is deleted: `delete_mark` emits
        // `mark-deleted`, and a handler of it that asks whether a column is up ([`has_carets`])
        // would borrow the list this used to still be holding mutably.
        let carets: Vec<_> = self.imp().carets.borrow_mut().drain(..).collect();
        if carets.is_empty() {
            return;
        }
        let buffer = self.buffer();
        for caret in carets {
            caret.delete(&buffer);
        }
        self.blink_off();
        self.queue_draw();
    }

    /// Run `hide` while retaining the column. Move hidden carets to the
    /// preceding visible line end via `fold::shown`, then merge converged
    /// carets. Treat `hide`'s own primary movement as folding, not a click
    /// that ends the column.
    pub fn keeping_carets(&self, hide: impl FnOnce()) {
        if !self.has_carets() {
            return hide();
        }
        let buffer = self.buffer();
        let imp = self.imp();
        imp.busy.set(true);
        hide();
        for (mark, anchor) in self.pairs() {
            if let Some(to) = crate::fold::shown(&buffer, &buffer.iter_at_mark(&mark)) {
                buffer.move_mark(&mark, &to);
                buffer.move_mark(&anchor, &to);
            }
        }
        imp.busy.set(false);
        self.collapse();
        self.show_column();
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

    /// Every caret's mark and its anchor, the primary's first: GTK's insert mark and selection
    /// bound.
    fn pairs(&self) -> Vec<(gtk::TextMark, gtk::TextMark)> {
        let buffer = self.buffer();
        let mut pairs = vec![(buffer.get_insert(), buffer.selection_bound())];
        pairs.extend(
            self.imp()
                .carets
                .borrow()
                .iter()
                .map(|caret| (caret.mark.clone(), caret.anchor.clone())),
        );
        pairs
    }

    /// Every caret's selection as offsets, the primary's first.
    fn spans(&self) -> Vec<Span> {
        let buffer = self.buffer();
        let at = |mark: &gtk::TextMark| buffer.iter_at_mark(mark).offset();
        self.pairs()
            .iter()
            .map(|(mark, anchor)| Span {
                anchor: at(anchor),
                caret: at(mark),
            })
            .collect()
    }

    /// Every caret's selection, start first and top to bottom, an empty one where a caret has
    /// nothing selected: what a cut or copy at a column takes (`editor::lines`).
    pub(crate) fn selections(&self) -> Vec<(gtk::TextIter, gtk::TextIter)> {
        let buffer = self.buffer();
        let mut spans = self.spans();
        spans.sort_by_key(|span| span.start());
        spans
            .iter()
            .map(|span| {
                let at = |offset| buffer.iter_at_offset(offset);
                (at(span.start()), at(span.end()))
            })
            .collect()
    }

    /// Return primary-first line/column positions for headless checks
    /// that cannot inspect painted carets.
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

    /// Paint secondary selections like GTK's primary, beneath text and
    /// through row ends for continuing selections. Visit visible rows only
    /// so long selections cost viewport work.
    fn paint_selections(&self, snapshot: &gtk::Snapshot) {
        let buffer = self.buffer();
        let spans: Vec<Span> = self.spans()[1..]
            .iter()
            .copied()
            .filter(|span| !span.is_empty())
            .collect();
        if spans.is_empty() {
            return;
        }
        let colour = self.selection_colour();
        let visible = self.visible_rect();
        let right = visible.x() + visible.width() - self.right_margin();
        let (top, _) = self.line_at_y(visible.y());
        for span in spans {
            let end = buffer.iter_at_offset(span.end());
            let mut row = buffer.iter_at_offset(span.start()).max(top);
            while row < end {
                let mut next = row;
                let more = self.forward_display_line(&mut next);
                let at = self.iter_location(&row);
                if at.y() > visible.y() + visible.height() {
                    break;
                }
                // A row's own height, and the space above or below the paragraph on its first or
                // last row, which GTK's selection covers too.
                let (line_y, line_height) = self.line_yrange(&row);
                let y = if row.starts_line() { line_y } else { at.y() };
                let bottom = match next.starts_line() || !more {
                    true => line_y + line_height,
                    false => at.y() + at.height(),
                };
                let past = end > next || (end == next && next.starts_line());
                let x = match past {
                    true => right,
                    false => self.iter_location(&end).x(),
                };
                snapshot.append_color(
                    &colour,
                    &graphene::Rect::new(
                        at.x() as f32,
                        y as f32,
                        (x - at.x()) as f32,
                        (bottom - y) as f32,
                    ),
                );
                if !more {
                    break;
                }
                row = next;
            }
        }
    }

    /// Match GTK selection colour: use the scheme's override when present
    /// (for example, Solarized), otherwise libadwaita's.
    pub(crate) fn selection_colour(&self) -> gdk::RGBA {
        use sourceview5::prelude::BufferExt as _;
        let scheme = self
            .buffer()
            .downcast::<sourceview5::Buffer>()
            .ok()
            .and_then(|buffer| buffer.style_scheme())
            .and_then(|scheme| scheme.style("selection"))
            .filter(|style| style.is_background_set())
            .and_then(|style| style.background())
            .and_then(|colour| {
                // A scheme may write `#rgba(…)`, which GDK reads without the `#`.
                gdk::RGBA::parse(colour.as_str())
                    .or_else(|_| gdk::RGBA::parse(colour.trim_start_matches('#')))
                    .ok()
            });
        // The state libadwaita's `selection:focus-within` is written against.
        let focused = self.state_flags().contains(gtk::StateFlags::FOCUS_WITHIN);
        scheme.unwrap_or_else(|| match focused {
            true => crate::theme::at(crate::theme::accent(), crate::theme::TEXT_SELECTION_ALPHA),
            false => {
                let text = self.color();
                crate::theme::at(text, text.alpha() * crate::theme::UNFOCUSED_SELECTION_ALPHA)
            }
        })
    }
}

/// Claim Shift+Alt primary drags for box selection ([`View::select_box`]).
///
/// Capture claiming prevents GTK Shift+click and drag handling. Keep the origin in buffer
/// coordinates across scrolling. Every [`SCAN`] redraws towards the pointer and scrolls
/// faster as it moves farther beyond the view's vertical edges.
fn box_drag(view: &View) {
    use std::{cell::Cell, rc::Rc};
    let drag = gtk::GestureDrag::builder()
        .button(gdk::BUTTON_PRIMARY)
        .propagation_phase(gtk::PropagationPhase::Capture)
        .build();
    let from = Rc::new(Cell::new((0, 0)));
    let pressed = from.clone();
    drag.connect_drag_begin(move |drag, x, y| {
        let chord = gdk::ModifierType::SHIFT_MASK | gdk::ModifierType::ALT_MASK;
        let view = drag.widget().and_downcast::<View>();
        let Some(view) = view.filter(|_| drag.current_event_state().contains(chord)) else {
            drag.set_state(gtk::EventSequenceState::Denied);
            return;
        };
        drag.set_state(gtk::EventSequenceState::Claimed);
        view.grab_focus();
        let at = view.window_to_buffer_coords(gtk::TextWindowType::Widget, x as i32, y as i32);
        pressed.set(at);
        view.select_box(at, at);
    });
    // Where the pointer is in the view, and whether the view is scrolling on past an edge for it.
    let pointer = Rc::new(Cell::new((0.0, 0.0)));
    let scanning = Rc::new(Cell::new(false));
    drag.connect_drag_update(move |drag, dx, dy| {
        let (Some(view), Some((x, y))) = (drag.widget().and_downcast::<View>(), drag.start_point())
        else {
            return;
        };
        pointer.set((x + dx, y + dy));
        box_to(&view, from.get(), pointer.get());
        if !past_edge(&view, y + dy) || scanning.replace(true) {
            return;
        }
        let (drag, from, pointer, scanning) = (
            drag.downgrade(),
            from.clone(),
            pointer.clone(),
            scanning.clone(),
        );
        glib::timeout_add_local(SCAN, move || {
            let view = drag
                .upgrade()
                .filter(|drag| drag.is_active())
                .and_then(|drag| drag.widget().and_downcast::<View>());
            match view.filter(|view| past_edge(view, pointer.get().1)) {
                Some(view) => {
                    box_to(&view, from.get(), pointer.get());
                    glib::ControlFlow::Continue
                }
                None => {
                    scanning.set(false);
                    glib::ControlFlow::Break
                }
            }
        });
    });
    view.add_controller(drag);
}

/// How often a box drag resting past the view's edge scrolls on: GTK's own drag-select scan.
const SCAN: std::time::Duration = std::time::Duration::from_millis(50);

/// The box from buffer point `from` to the pointer at `x`, `y` in the view.
fn box_to(view: &View, from: (i32, i32), (x, y): (f64, f64)) {
    let to = view.window_to_buffer_coords(gtk::TextWindowType::Widget, x as i32, y as i32);
    view.select_box(from, to);
}

/// Whether `y` in the view is above or below it, where a box drag scrolls on.
fn past_edge(view: &View, y: f64) -> bool {
    y < 0.0 || y >= f64::from(view.height())
}

fn line_length(buffer: &gtk::TextBuffer, line: i32) -> i32 {
    line_end(buffer, line).line_offset()
}

#[cfg(test)]
mod tests {
    use super::{
        Edit, Motion, Span, blink_alpha, departure, edit_for, ends_column, merge, next_occurrence,
        occurrences, spaces_ahead, spaces_behind, spread, tab_insert, undo_or_redo, vertical_step,
        visual_column,
    };
    use gtk::gdk::{Key, ModifierType as Mod};

    fn span(anchor: i32, caret: i32) -> Span {
        Span { anchor, caret }
    }

    /// Overlapping selections become one, and a caret with nothing selected joins a selection it
    /// only touches; two selections that only meet stay two, as VS Code keeps them.
    #[test]
    fn overlapping_selections_merge_and_touching_ones_stay_apart() {
        assert_eq!(merge(&[span(3, 3), span(3, 3)]), [(0, span(3, 3))]);
        assert_eq!(
            merge(&[span(2, 5), span(5, 8)]),
            [(0, span(2, 5)), (1, span(5, 8))]
        );
        assert_eq!(merge(&[span(2, 6), span(4, 8)]), [(0, span(2, 8))]);
        assert_eq!(
            merge(&[span(0, 3), span(2, 5), span(4, 7)]),
            [(0, span(0, 7))]
        );
        assert_eq!(
            merge(&[span(9, 9), span(1, 3)]),
            [(1, span(1, 3)), (0, span(9, 9))]
        );
    }

    /// The union keeps the survivor's direction, and a survivor with nothing selected keeps its
    /// caret at the end of the union it sat at: the primary does not jump.
    #[test]
    fn a_merged_selection_keeps_the_survivors_direction() {
        assert_eq!(merge(&[span(6, 2), span(4, 8)]), [(0, span(8, 2))]);
        assert_eq!(merge(&[span(8, 8), span(2, 8)]), [(0, span(2, 8))]);
        assert_eq!(merge(&[span(2, 2), span(8, 2)]), [(0, span(8, 2))]);
    }

    /// A plain Left or Right collapses a selection onto its start or end and goes no further, Up
    /// leaves from the start and Down from the end, and anything else, or a caret with nothing
    /// selected, sets off from the caret.
    #[test]
    fn a_plain_motion_leaves_a_selection_from_the_end_it_heads_for() {
        let (forward, backward) = (span(2, 5), span(5, 2));
        assert_eq!(departure(Motion::Left, forward), (2, false));
        assert_eq!(departure(Motion::Right, backward), (5, false));
        assert_eq!(departure(Motion::Up, forward), (2, true));
        assert_eq!(departure(Motion::PageDown, backward), (5, true));
        assert_eq!(departure(Motion::Home, backward), (2, true));
        assert_eq!(departure(Motion::End, forward), (5, true));
        assert_eq!(departure(Motion::Left, span(4, 4)), (4, true));
    }

    /// A modifier pressed on its own leaves the column where it is — AltGr and Shift come before
    /// the character they type — and only Escape and the keys an input method finishes end it.
    #[test]
    fn only_escape_and_composing_keys_end_a_column() {
        for key in [
            Key::ISO_Level3_Shift,
            Key::ISO_Level5_Shift,
            Key::Shift_L,
            Key::Control_R,
            Key::Alt_L,
            Key::Super_L,
            Key::Meta_L,
            Key::Hyper_L,
            Key::Caps_Lock,
            Key::Num_Lock,
            Key::F4,
            Key::at,
        ] {
            assert!(!ends_column(key, Mod::empty()), "{:?}", key.name());
        }
        assert!(!ends_column(Key::s, Mod::CONTROL_MASK));
        assert!(!ends_column(Key::u, Mod::CONTROL_MASK));
        assert!(ends_column(Key::Escape, Mod::empty()));
        assert!(ends_column(Key::dead_acute, Mod::empty()));
        assert!(ends_column(Key::Multi_key, Mod::empty()));
        assert!(ends_column(Key::U, Mod::CONTROL_MASK | Mod::SHIFT_MASK));
    }

    /// `Shift+Delete` is GTK's Cut binding, which a single caret and VS Code both answer with, so
    /// the column leaves the press to it; a plain Delete is the column's own.
    #[test]
    fn shift_delete_is_left_to_gtks_cut() {
        assert!(matches!(
            edit_for(Key::Delete, Mod::empty()),
            Some(Edit::Delete)
        ));
        assert!(edit_for(Key::Delete, Mod::SHIFT_MASK).is_none());
        assert!(edit_for(Key::KP_Delete, Mod::SHIFT_MASK).is_none());
        assert!(matches!(
            edit_for(Key::Delete, Mod::CONTROL_MASK | Mod::SHIFT_MASK),
            Some(Edit::DeleteWord(true)),
        ));
    }

    #[test]
    fn undo_and_redo_are_gtks_chords() {
        assert_eq!(undo_or_redo(Key::z, Mod::CONTROL_MASK), Some(true));
        assert_eq!(
            undo_or_redo(Key::Z, Mod::CONTROL_MASK | Mod::SHIFT_MASK),
            Some(false)
        );
        assert_eq!(undo_or_redo(Key::y, Mod::CONTROL_MASK), Some(false));
        assert_eq!(undo_or_redo(Key::z, Mod::empty()), None);
        assert_eq!(undo_or_redo(Key::v, Mod::CONTROL_MASK), None);
    }

    /// A paste with one line per caret hands them out; any other puts all of it at every caret.
    #[test]
    fn a_paste_spreads_only_when_its_lines_match_the_carets() {
        assert_eq!(spread("a\nb", 2), ["a", "b"]);
        assert_eq!(spread("a\nb\n", 2), ["a", "b"], "a copy of whole lines");
        assert_eq!(spread("a\r\nb\r\n", 2), ["a", "b"]);
        assert_eq!(spread("a\nb", 3), ["a\nb"; 3]);
        assert_eq!(spread("x", 2), ["x", "x"]);
        assert_eq!(spread("a\n", 1), ["a\n"], "one caret is a plain paste");
    }

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

    /// The next occurrence is the next one after the last caret, wrapping round at the end,
    /// passing over what a caret already holds, and matched literally and by case.
    #[test]
    fn the_next_occurrence_wraps_and_skips_what_is_taken() {
        let text = "foo bar Foo foo baz foo";
        assert_eq!(
            next_occurrence(text, "foo", 3, &[(0, 3)], false),
            Some((12, 15)),
            "not Foo"
        );
        assert_eq!(
            next_occurrence(text, "foo", 15, &[(0, 3), (12, 15)], false),
            Some((20, 23))
        );
        let all = [(0, 3), (12, 15), (20, 23)];
        assert_eq!(
            next_occurrence(text, "foo", 23, &all, false),
            None,
            "all taken"
        );
        assert_eq!(
            next_occurrence(text, "foo", 23, &[(12, 15), (20, 23)], false),
            Some((0, 3)),
            "wraps to the top"
        );
        assert_eq!(
            next_occurrence(text, "o.", 0, &[], false),
            None,
            "not a pattern"
        );
        // Offsets are characters, as the buffer counts them.
        assert_eq!(
            next_occurrence("äö foo", "foo", 0, &[], false),
            Some((3, 6))
        );
    }

    /// A word taken from under the caret matches whole words only, an underscore being part of
    /// one, as VS Code has it; a selection made by hand matches anywhere.
    #[test]
    fn a_word_from_under_the_caret_matches_whole_words_only() {
        let text = "a.foo foo_bar xfoo foo";
        let all = |whole| occurrences(text, "foo", whole).collect::<Vec<_>>();
        assert_eq!(all(false), [(2, 5), (6, 9), (15, 18), (19, 22)]);
        assert_eq!(all(true), [(2, 5), (19, 22)]);
        assert_eq!(
            next_occurrence(text, "foo", 5, &[(2, 5)], true),
            Some((19, 22))
        );
    }
}
