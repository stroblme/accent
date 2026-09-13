//! Finding, replacing and going somewhere: the tab's own `SearchContext`, the muted highlight
//! over every other occurrence of the selection, and the caret and scroll moves that follow a
//! match.
//!
//! The widgets live in `find.rs`, one bar per window. What is here is what belongs to one buffer.

use super::{Tab, caret, line_end};
use crate::{fold, lang, multicaret};
use accent_api::Pos;
use accent_api::language::pos_of;
use accent_core::markdown;
use gtk::prelude::*;
use gtk::{gdk, glib};
use sourceview5::prelude::*;
use std::ops::Range;

/// How much of the find bar's match colour an occurrence of the selection keeps.
const OCCURRENCE_WEIGHT: f32 = 0.35;
/// Occurrences past this many are left unpainted. A selection with more than this in one note is
/// a word too common for the hint to say anything about, and tagging every one of them is the
/// tag churn a full re-style is already dominated by.
const OCCURRENCE_CAP: usize = 500;

/// The background the style scheme paints a find-bar match in, which is the one colour both
/// highlights are derived from.
fn search_match_colour(buffer: &sourceview5::Buffer) -> Option<gdk::RGBA> {
    buffer
        .style_scheme()
        .and_then(|scheme| scheme.style("search-match"))
        .and_then(|style| style.background())
        .and_then(|colour| gdk::RGBA::parse(&colour).ok())
}

/// Paint `tag` in the muted twin of the scheme's own `search-match` colour: the same hue at a
/// third of its weight, so an occurrence of the selection reads as a hint while a find-bar match
/// still reads as a hit. Derived rather than named, so the pair keeps its order in every scheme.
pub(super) fn mute(buffer: &sourceview5::Buffer, tag: &gtk::TextTag) {
    let Some(mut colour) = search_match_colour(buffer) else {
        return;
    };
    colour.set_alpha(colour.alpha() * OCCURRENCE_WEIGHT);
    tag.set_background_rgba(Some(&colour));
}

impl Tab {
    // The widgets live in `find.rs`, one bar per window. What stays here is what belongs to one
    // buffer: its `SearchContext`, and the caret and scroll moves that follow a match.

    /// The context the window's find bar drives, so it can watch the occurrence count.
    pub fn search_context(&self) -> &sourceview5::SearchContext {
        &self.context
    }

    pub fn set_query(&self, text: &str) {
        self.context.settings().set_search_text(Some(text));
    }

    pub fn set_highlight(&self, on: bool) {
        self.context.set_highlight(on);
    }

    /// What is selected, or nothing when nothing is. The three readers below are this plus the
    /// rule each of them applies to it.
    fn selection_text(&self) -> Option<String> {
        let (start, end) = self.buffer.selection_bounds()?;
        Some(self.buffer.text(&start, &end, false).to_string())
    }

    /// The selection, when it is worth showing every other occurrence of: two or more characters
    /// on one line. One character is in almost every line, and a selection that spans lines is a
    /// block being moved rather than a word being looked at.
    fn selected_occurrence(&self) -> Option<String> {
        self.selection_text()
            .filter(|s| s.chars().count() >= 2 && !s.contains('\n'))
    }

    /// Point the muted highlight at what is selected now: every *other* occurrence of it in this
    /// note, matched the way the find bar matches — without regard to case — and capped at
    /// [`OCCURRENCE_CAP`].
    ///
    /// The selected ranges themselves are left untagged. Tagging them was invisible only in
    /// theory: a selection is painted *under* the tag backgrounds, so the muted colour showed
    /// through, and the primary's selection and the other carets' — painted by two different
    /// mechanisms — took it differently, so a column read as three shades of one selection.
    ///
    /// Nothing is re-tagged when the selection says what it said last time, which is what makes
    /// this cheap enough to run on every caret move.
    pub(super) fn highlight_occurrences(&self) {
        let query = self.selected_occurrence();
        if *self.occurrence_query.borrow() == query {
            return;
        }
        *self.occurrence_query.borrow_mut() = query.clone();
        self.buffer.remove_tag(
            &self.occurrence_tag,
            &self.buffer.start_iter(),
            &self.buffer.end_iter(),
        );
        let Some(query) = query else { return };
        let selected = self.selected_ranges();
        let mut at = self.buffer.start_iter();
        for _ in 0..OCCURRENCE_CAP {
            let found = at.forward_search(&query, gtk::TextSearchFlags::CASE_INSENSITIVE, None);
            let Some((from, to)) = found else {
                break;
            };
            if !selected.contains(&(from.offset(), to.offset())) {
                self.buffer.apply_tag(&self.occurrence_tag, &from, &to);
            }
            at = to;
        }
    }

    /// What is selected right now, as character offsets: every caret's range where the tab holds
    /// a column of them, the primary's alone otherwise.
    fn selected_ranges(&self) -> Vec<(i32, i32)> {
        let offsets = |(start, end): (gtk::TextIter, gtk::TextIter)| (start.offset(), end.offset());
        match self.view.downcast_ref::<multicaret::View>() {
            Some(column) => column.selections().into_iter().map(offsets).collect(),
            None => self
                .buffer
                .selection_bounds()
                .map(offsets)
                .into_iter()
                .collect(),
        }
    }

    /// What the muted highlight is showing, and the tag it paints it with. Only
    /// `ACCENT_BENCH_OCCUR` reads them.
    pub fn occurrence_highlight(&self) -> (Option<String>, gtk::TextTag) {
        (
            self.occurrence_query.borrow().clone(),
            self.occurrence_tag.clone(),
        )
    }

    /// The two match backgrounds, the find bar's first: what `ACCENT_BENCH_OCCUR` prints to show
    /// the hint really is the weaker of the pair. The find bar's context has no match style of
    /// its own, so its colour is the scheme's `search-match`.
    pub fn match_colours(&self) -> (Option<String>, Option<String>) {
        let text = |colour: gdk::RGBA| colour.to_str().to_string();
        (
            search_match_colour(&self.buffer).map(text),
            self.occurrence_tag.background_rgba().map(text),
        )
    }

    /// A one-line selection, which is what the find bar prefills itself from.
    pub fn selected_query(&self) -> Option<String> {
        self.selection_text()
            .filter(|s| !s.is_empty() && !s.contains('\n'))
    }

    /// The selection as the sidebar search takes it. Its first line only: the box is one line
    /// high, and a whole paragraph pasted into it matches nothing anyway.
    pub fn selected_search(&self) -> Option<String> {
        let selected = self.selection_text()?;
        let first = selected.lines().next().unwrap_or_default().to_string();
        (!first.is_empty()).then_some(first)
    }

    /// Move to the next or previous match. `from_current` searches from the start of the current
    /// selection, so growing the query keeps the match the user is looking at.
    pub fn step(&self, forward: bool, from_current: bool) {
        let insert = caret(&self.buffer);
        let (start, end) = self.buffer.selection_bounds().unwrap_or((insert, insert));
        let found = match (forward, from_current) {
            (true, true) => self.context.forward(&start),
            (true, false) => self.context.forward(&end),
            (false, _) => self.context.backward(&start),
        };
        if let Some((s, e, _)) = found {
            // A match inside a folded block opens it, or the selection is invisible.
            fold::reveal(self.text_buffer(), &s);
            self.buffer.select_range(&s, &e);
            self.view
                .scroll_to_mark(&self.buffer.get_insert(), 0.1, false, 0.0, 0.5);
        }
    }

    pub fn replace_current(&self, with: &str) {
        if let Some((mut s, mut e)) = self.buffer.selection_bounds() {
            // Fails when the selection is not itself a match, which is the "nothing to do" case.
            let _ = self.context.replace(&mut s, &mut e, with);
        }
        self.step(true, false);
    }

    /// sourceview5 0.11 exposes no `replace_all` binding, so this walks the matches. Each pass
    /// resumes after the text just inserted, so a replacement containing the query terminates.
    pub fn replace_all(&self, with: &str) {
        let mut from = self.buffer.start_iter();
        self.buffer.begin_user_action();
        while let Some((mut s, mut e, _)) = self.context.forward(&from) {
            if self.context.replace(&mut s, &mut e, with).is_err() {
                break;
            }
            from = e;
        }
        self.buffer.end_user_action();
    }

    /// "n of m", the way every find bar says it. Blank while GtkSourceView is still counting.
    pub fn matches_label(&self) -> String {
        let count = self.context.occurrences_count();
        let blank = self
            .context
            .settings()
            .search_text()
            .is_none_or(|t| t.is_empty());
        match (blank, count) {
            (true, _) | (_, ..0) => String::new(),
            (_, 0) => "No results".to_string(),
            _ => match self
                .buffer
                .selection_bounds()
                .map(|(s, e)| self.context.occurrence_position(&s, &e))
            {
                Some(position) if position > 0 => format!("{position} of {count}"),
                _ => format!("{count} matches"),
            },
        }
    }

    pub fn line_count(&self) -> i32 {
        self.buffer.line_count()
    }

    /// The one way the caret is sent somewhere: open whatever fold is hiding the destination,
    /// put the caret there, scroll it to `align` down the view and take the focus.
    ///
    /// Every jump goes through here — an outline row, a search hit, a go-to line, a definition —
    /// so none of them can land inside a folded block and leave the window looking unchanged.
    pub fn jump_to(&self, iter: &gtk::TextIter, align: f64) {
        fold::reveal(self.text_buffer(), iter);
        self.buffer.place_cursor(iter);
        self.scroll_to_caret(align);
        self.view.grab_focus();
    }

    /// Scroll the caret to `align` down the view, once the view can say where the caret is.
    ///
    /// A tab that has just opened cannot yet: GTK flushes a queued `scroll_to_mark` from an idle
    /// that runs before the view's first allocation, against a height of 0, and it measures lines
    /// lazily, so until the lines above the caret are measured the caret sits too high and the
    /// animated scroll heads for that spot. Either way the view settles short of the caret. So a
    /// jump into a tab that is still opening waits for a frame in which the view has a size, then
    /// for a default idle, which GLib runs only once GTK's measuring idle
    /// (`GTK_TEXT_VIEW_PRIORITY_VALIDATE`, a higher priority) has measured every line.
    pub(crate) fn scroll_to_caret(&self, align: f64) {
        let scroll = move |view: &sourceview5::View| {
            view.scroll_to_mark(&view.buffer().get_insert(), 0.0, true, 0.0, align)
        };
        if self.view.height() > 0 {
            return scroll(&self.view);
        }
        self.view.add_tick_callback(move |view, _| {
            if view.height() == 0 {
                return glib::ControlFlow::Continue;
            }
            let view = view.downgrade();
            glib::idle_add_local_once(move || {
                if let Some(view) = view.upgrade() {
                    scroll(&view);
                }
            });
            glib::ControlFlow::Break
        });
    }

    /// Put the caret on a 1-based line and column, both clamped to what the note has.
    pub fn goto_line(&self, line: i32, column: i32) {
        self.jump_to(&self.line_iter(line, column), 0.25);
    }

    /// Put the caret at a server position, which is zero-based and counts characters.
    pub fn goto_pos(&self, pos: Pos) {
        self.jump_to(&lang::iter_at(&self.buffer, pos), 0.25);
    }

    /// Put the caret on the heading `anchor` names, by slug or by text as a link writes it. A
    /// heading the note does not have leaves the caret where it was.
    pub fn goto_heading(&self, anchor: &str) {
        let text = self.text();
        if let Some(h) = markdown::heading_for(&markdown::analyze(&text).headings, anchor) {
            self.goto_pos(pos_of(&text, h.range.start));
        }
    }

    /// Jump to a character range and mark it the way the find bar marks a match it found: the
    /// range itself is selected, and the text inside it becomes this tab's search query with the
    /// highlight on, so every other occurrence in the note is marked too.
    ///
    /// Nothing here expires. The marks are the find bar's own and go the way they always do — a
    /// new query replaces them, an edit moves them, closing the bar clears them.
    pub fn goto_range(&self, chars: Range<usize>) {
        let last = self.buffer.char_count();
        let start = self
            .buffer
            .iter_at_offset((chars.start as i32).clamp(0, last));
        let end = self
            .buffer
            .iter_at_offset((chars.end as i32).clamp(0, last));
        self.jump_to(&start, 0.3);
        if start == end {
            return;
        }
        self.buffer.select_range(&start, &end);
        self.set_query(&self.buffer.text(&start, &end, false));
        self.set_highlight(true);
    }

    /// Scroll a line into view without moving the caret: what the go-to entry previews while the
    /// number is still being typed.
    pub fn show_line(&self, line: i32) {
        let mut iter = self.line_iter(line, 1);
        fold::reveal(self.text_buffer(), &iter);
        self.view.scroll_to_iter(&mut iter, 0.0, true, 0.0, 0.25);
    }

    /// A 1-based line and column as an iter, both clamped to what the note has.
    fn line_iter(&self, line: i32, column: i32) -> gtk::TextIter {
        let end = line_end(&self.buffer, line - 1);
        let mut iter = end;
        iter.set_line_offset((column - 1).clamp(0, end.line_offset()));
        iter
    }
}
