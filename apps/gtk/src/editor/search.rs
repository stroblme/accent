//! Buffer-local find, replace, occurrence highlighting and match navigation.
//!
//! Widgets live in `find.rs`, one bar per pane; tabs keep their own query and matches.

use super::{Tab, caret, line_end};
use crate::{fold, lang, multicaret};
use accent_api::Pos;
use accent_api::language::pos_of;
use accent_core::markdown;
use accent_core::search::{self, Options, Regex};
use gtk::prelude::*;
use gtk::{gdk, glib};
use sourceview5::prelude::*;
use std::cell::Ref;
use std::ops::Range;
use std::rc::Rc;

/// How much of the find bar's match colour an occurrence of the selection keeps.
const OCCURRENCE_WEIGHT: f32 = 0.35;
/// Maximum painted occurrences, avoiding tag churn for selections too
/// common to provide a useful hint.
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

/// Paint a muted `search-match` colour at one-third weight so occurrences
/// remain weaker than find hits in every theme.
pub(crate) fn mute(buffer: &sourceview5::Buffer, tag: &gtk::TextTag) {
    let Some(mut colour) = search_match_colour(buffer) else {
        return;
    };
    colour.set_alpha(colour.alpha() * OCCURRENCE_WEIGHT);
    tag.set_background_rgba(Some(&colour));
}

/// `buffer`'s tag for the other occurrences of the selection. Made before the find bar's match
/// tag, so it stays under it: a tag added later to the table outranks an earlier one, and the
/// match tag is only ever raised.
pub(crate) fn occurrence_tag(buffer: &sourceview5::Buffer) -> gtk::TextTag {
    let tag = gtk::TextTag::new(Some("occurrence"));
    buffer.tag_table().add(&tag);
    mute(buffer, &tag);
    tag
}

/// The selection whose other occurrences are hinted: one line of two characters or more. One
/// character is too common, and a selection of lines is a block.
pub(crate) fn selected_occurrence(buffer: &impl IsA<gtk::TextBuffer>) -> Option<String> {
    let (start, end) = buffer.selection_bounds()?;
    let text = buffer.text(&start, &end, false);
    (text.chars().count() >= 2 && !text.contains('\n')).then(|| text.to_string())
}

/// Tag `query`'s case-insensitive occurrences in `buffer`, at most [`OCCURRENCE_CAP`], with
/// `tag`, taking the last ones off; none for no query. The `selected` ranges stay untagged: a
/// tag's background paints over GTK's selection and would colour the primary and the other
/// selections differently.
pub(crate) fn paint_occurrences(
    buffer: &impl IsA<gtk::TextBuffer>,
    tag: &gtk::TextTag,
    query: Option<&str>,
    selected: &[(i32, i32)],
) {
    buffer.remove_tag(tag, &buffer.start_iter(), &buffer.end_iter());
    let Some(query) = query else { return };
    let mut at = buffer.start_iter();
    for _ in 0..OCCURRENCE_CAP {
        let found = at.forward_search(query, gtk::TextSearchFlags::CASE_INSENSITIVE, None);
        let Some((from, to)) = found else {
            break;
        };
        if !selected.contains(&(from.offset(), to.offset())) {
            buffer.apply_tag(tag, &from, &to);
        }
        at = to;
    }
}

/// `view`'s selected character ranges, every caret's of a column or the primary's alone.
pub(crate) fn selected_ranges(view: &sourceview5::View) -> Vec<(i32, i32)> {
    let offsets = |(start, end): (gtk::TextIter, gtk::TextIter)| (start.offset(), end.offset());
    match view.downcast_ref::<multicaret::View>() {
        Some(column) => column.selections().into_iter().map(offsets).collect(),
        None => view
            .buffer()
            .selection_bounds()
            .map(offsets)
            .into_iter()
            .collect(),
    }
}

/// Apply all properties of the scheme's `search-match` style for find hits.
pub(crate) fn match_style(buffer: &sourceview5::Buffer, tag: &gtk::TextTag) {
    if let Some(style) = buffer
        .style_scheme()
        .and_then(|scheme| scheme.style("search-match"))
    {
        style.apply(tag);
    }
}

/// Paint jump reveals at full `search-match` weight, matching find hits
/// and following theme changes.
pub(super) fn matched(buffer: &sourceview5::Buffer, tag: &gtk::TextTag) {
    if let Some(colour) = search_match_colour(buffer) {
        tag.set_background_rgba(Some(&colour));
    }
}

/// Return the byte range of the first exact `#name`, including its marker, or `None`.
///
/// Parse the note to exclude code and URLs while including frontmatter tags. Exact matching
/// agrees with the index's case-sensitive tag groups.
fn tag_range(text: &str, name: &str) -> Option<Range<usize>> {
    let found = markdown::analyze(text)
        .tags
        .into_iter()
        .find(|t| t.name == name)?;
    Some(found.range)
}

/// How many lines above and below the ones on screen the find bar's matches are painted in.
const PAINT_MARGIN: i32 = 100;

/// Tab-owned query, matches and highlight state, independent of the current find bar target.
///
/// Use `accent_core::search` over all text, including folds, so toggles agree with vault search.
/// Search-result queries remain on tabs after the bar retargets.
#[derive(Default)]
pub(super) struct Find {
    query: String,
    options: Options,
    /// The query compiled, or `None` while it is empty or does not compile.
    re: Option<Regex>,
    /// The query does not compile, which only Regular Expression can make it do.
    invalid: bool,
    /// Where `re` matches, in characters; out of date while `stale`.
    matches: Vec<Range<i32>>,
    stale: bool,
    highlight: bool,
    /// A repaint is waiting for the main loop, so a run of edits is matched once.
    queued: bool,
}

/// Choose the next match from selection `at`, wrapping at both ends.
/// Forward starts at the current start with `from_current`, otherwise
/// after its end; backward chooses the last match before its start.
pub(crate) fn next_match(
    matches: &[Range<i32>],
    at: Range<i32>,
    forward: bool,
    from_current: bool,
) -> Option<Range<i32>> {
    let found = match (forward, from_current) {
        (true, true) => matches.iter().find(|m| m.start >= at.start),
        // Not the selection itself, which a match of no width at the caret would be.
        (true, false) => matches.iter().find(|m| m.start >= at.end && **m != at),
        (false, _) => matches.iter().rev().find(|m| m.start < at.start),
    };
    let wrapped = match forward {
        true => matches.first(),
        false => matches.last(),
    };
    found.or(wrapped).cloned()
}

impl Tab {
    // The widgets live in `find.rs`, one bar per window. What stays here is what belongs to one
    // buffer: its query and matches, and the caret and scroll moves that follow a match.

    /// Search for `text`, under the options last set.
    pub fn set_query(&self, text: &str) {
        let options = self.find.borrow().options;
        self.set_find(text, options);
    }

    /// Search for the same text under `options`, the find bar's three toggles.
    pub fn set_find_options(&self, options: Options) {
        let query = self.find.borrow().query.clone();
        self.set_find(&query, options);
    }

    /// Rematch under `options`, repaint if enabled and notify the targeted bar.
    fn set_find(&self, text: &str, options: Options) {
        {
            let mut find = self.find.borrow_mut();
            if find.query == text && find.options == options {
                return;
            }
            let compiled = (!text.is_empty()).then(|| search::pattern(text, options));
            find.invalid = matches!(compiled, Some(Err(_)));
            find.re = compiled.and_then(Result::ok);
            find.query = text.to_string();
            find.options = options;
            find.stale = true;
        }
        self.paint_matches();
        self.emit_found(true);
    }

    pub fn set_highlight(&self, on: bool) {
        if std::mem::replace(&mut self.find.borrow_mut().highlight, on) != on {
            self.paint_matches();
        }
    }

    /// What the find bar's query is for this tab, which the bar's box follows.
    pub fn find_query(&self) -> String {
        self.find.borrow().query.clone()
    }

    /// Whether the query does not compile, which the bar marks as the Search pane does.
    pub fn find_invalid(&self) -> bool {
        self.find.borrow().invalid
    }

    /// Whether the matches are painted. Only the drills read it.
    #[cfg(feature = "bench")]
    pub fn is_highlighting(&self) -> bool {
        self.find.borrow().highlight
    }

    /// Notify the last bar targeting this tab after rematching. The flag
    /// means query changes, not buffer edits or viewport changes.
    pub fn connect_found(&self, f: impl Fn(bool) + 'static) {
        self.on_found.set(Rc::new(f));
    }

    /// Taken out of its cell before it runs, so the bar may reach the tab again.
    fn emit_found(&self, query: bool) {
        if let Some(f) = self.on_found.get() {
            f(query);
        }
    }

    /// Where the query matches now, matched again first if the text changed since.
    fn matches(&self) -> Ref<'_, Vec<Range<i32>>> {
        if self.find.borrow().stale {
            let text = self.find.borrow().re.is_some().then(|| self.text());
            let find = &mut *self.find.borrow_mut();
            find.matches = match (&find.re, text) {
                (Some(re), Some(text)) => search::char_ranges(re, &text)
                    .into_iter()
                    .map(|m| m.start as i32..m.end as i32)
                    .collect(),
                _ => Vec::new(),
            };
            find.stale = false;
        }
        Ref::map(self.find.borrow(), |find| &find.matches)
    }

    /// Paint nonempty matches only within [`PAINT_MARGIN`] of visible lines, or clear highlighting.
    ///
    /// Refresh on scroll. Restricting tags avoids seconds of tag churn for hundreds of thousands
    /// of matches in long files.
    fn paint_matches(&self) {
        let (start, end) = self.buffer.bounds();
        self.buffer.remove_tag(&self.find_tag, &start, &end);
        if !self.find.borrow().highlight {
            return;
        }
        // To the top of the table, as GtkSourceView raised its own search tag: a match has to
        // read as one over whatever a tag made since paints under it.
        self.find_tag
            .set_priority(self.buffer.tag_table().size() - 1);
        let shown = self.view.visible_rect();
        let line = |y: i32, margin: i32| {
            let (at, _) = self.view.line_at_y(y);
            at.line() + margin
        };
        let from = line(shown.y(), -PAINT_MARGIN).max(0);
        let to = line(shown.y() + shown.height(), PAINT_MARGIN);
        let from = self.buffer.iter_at_line(from).unwrap_or(start).offset();
        let to = self.buffer.iter_at_line(to + 1).unwrap_or(end).offset();
        let matches = self.matches();
        let first = matches.partition_point(|m| m.end < from);
        for m in matches[first..]
            .iter()
            .take_while(|m| m.start <= to)
            .filter(|m| !m.is_empty())
        {
            let (from, to) = (
                self.buffer.iter_at_offset(m.start),
                self.buffer.iter_at_offset(m.end),
            );
            self.buffer.apply_tag(&self.find_tag, &from, &to);
        }
    }

    /// Invalidate matches after edits. Rematch painted queries from an idle
    /// ([`Tab::queue_paint`]); defer unpainted queries until navigation needs them.
    pub(super) fn find_edited(self: &Rc<Self>) {
        self.find.borrow_mut().stale = true;
        self.queue_paint();
    }

    /// Coalesce edits and scrolls into one idle repaint before the next frame.
    pub(super) fn queue_paint(self: &Rc<Self>) {
        let mut find = self.find.borrow_mut();
        if !find.highlight || std::mem::replace(&mut find.queued, true) {
            return;
        }
        glib::idle_add_local_full(
            glib::Priority::HIGH_IDLE,
            glib::clone!(
                #[weak(rename_to = tab)]
                self,
                #[upgrade_or]
                glib::ControlFlow::Break,
                move || {
                    tab.find.borrow_mut().queued = false;
                    tab.paint_matches();
                    tab.emit_found(false);
                    glib::ControlFlow::Break
                }
            ),
        );
    }

    /// Current selection, or none; callers add their own eligibility rules.
    fn selection_text(&self) -> Option<String> {
        let (start, end) = self.buffer.selection_bounds()?;
        Some(self.buffer.text(&start, &end, false).to_string())
    }

    /// Highlight other case-insensitive occurrences of the selection ([`paint_occurrences`]),
    /// re-tagging nothing for a caret move that leaves the query as it was.
    pub(super) fn highlight_occurrences(&self) {
        let query = selected_occurrence(&self.buffer);
        if *self.occurrence_query.borrow() == query {
            return;
        }
        *self.occurrence_query.borrow_mut() = query.clone();
        paint_occurrences(
            &self.buffer,
            &self.occurrence_tag,
            query.as_deref(),
            &selected_ranges(&self.view),
        );
    }

    /// What the muted highlight is showing, and the tag it paints it with. Only
    /// `ACCENT_BENCH_OCCUR` reads them.
    #[cfg(feature = "bench")]
    pub fn occurrence_highlight(&self) -> (Option<String>, gtk::TextTag) {
        (
            self.occurrence_query.borrow().clone(),
            self.occurrence_tag.clone(),
        )
    }

    /// Find and occurrence backgrounds for `ACCENT_BENCH_OCCUR`, verifying
    /// the hint is weaker. Find colour comes from [`match_style`].
    #[cfg(feature = "bench")]
    pub fn match_colours(&self) -> (Option<String>, Option<String>) {
        let text = |colour: gdk::RGBA| colour.to_str().to_string();
        (
            search_match_colour(&self.buffer).map(text),
            self.occurrence_tag.background_rgba().map(text),
        )
    }

    /// Return a one-line selection for the find bar's initial query.
    pub fn selected_query(&self) -> Option<String> {
        self.selection_text()
            .filter(|s| !s.is_empty() && !s.contains('\n'))
    }

    /// First selection line for the single-line sidebar search box.
    pub fn selected_search(&self) -> Option<String> {
        let selected = self.selection_text()?;
        let first = selected.lines().next().unwrap_or_default().to_string();
        (!first.is_empty()).then_some(first)
    }

    /// Step between matches. `from_current` starts at the selection start
    /// so refining the query keeps the current match visible.
    pub fn step(&self, forward: bool, from_current: bool) {
        let insert = caret(&self.buffer);
        let (start, end) = self.buffer.selection_bounds().unwrap_or((insert, insert));
        let found = next_match(
            &self.matches(),
            start.offset()..end.offset(),
            forward,
            from_current,
        );
        if let Some(m) = found {
            let (s, e) = (
                self.buffer.iter_at_offset(m.start),
                self.buffer.iter_at_offset(m.end),
            );
            // A match inside a folded block opens it, or the selection is invisible.
            let opened = self.reveal_select(&s, &e);
            self.when_measured(opened, |view| {
                view.scroll_to_mark(&view.buffer().get_insert(), 0.1, false, 0.0, 0.5)
            });
        }
    }

    /// Replace a selected match and advance; do nothing for other selections.
    pub fn replace_current(&self, with: &str) {
        if let Some((mut s, mut e)) = self.buffer.selection_bounds() {
            let at = s.offset() as usize..e.offset() as usize;
            let found = self.replacements(with).into_iter().find(|(m, _)| *m == at);
            if let Some((_, text)) = found {
                self.replace_range(&mut s, &mut e, &text);
            }
        }
        self.step(true, false);
    }

    /// Replace all matches in one undo step, back to front to preserve offsets.
    pub fn replace_all(&self, with: &str) {
        let all = self.replacements(with);
        self.buffer.begin_user_action();
        for (at, text) in all.into_iter().rev() {
            let mut s = self.buffer.iter_at_offset(at.start as i32);
            let mut e = self.buffer.iter_at_offset(at.end as i32);
            self.replace_range(&mut s, &mut e, &text);
        }
        self.buffer.end_user_action();
    }

    /// Build replacements, expanding `$1` only in regex mode, as vault Replace All does.
    fn replacements(&self, with: &str) -> Vec<(Range<usize>, String)> {
        let find = self.find.borrow();
        match &find.re {
            Some(re) => search::replacements(re, &self.text(), with, find.options),
            None => Vec::new(),
        }
    }

    /// Replace `s..e` in one undo step and leave both iters around the result.
    /// Preserve hidden-fold tags where GTK would otherwise expose inserted
    /// text at a hidden run's start.
    fn replace_range(&self, s: &mut gtk::TextIter, e: &mut gtk::TextIter, with: &str) {
        let hidden = fold::hiding(self.text_buffer(), s);
        let from = s.offset();
        self.buffer.begin_user_action();
        self.buffer.delete(s, e);
        self.buffer.insert(s, with);
        self.buffer.end_user_action();
        *e = *s;
        *s = self.buffer.iter_at_offset(from);
        for tag in hidden {
            self.buffer.apply_tag(&tag, s, e);
        }
    }

    /// Return the match count as "n of m", or an invalid-query message.
    pub fn matches_label(&self) -> String {
        let (blank, invalid) = {
            let find = self.find.borrow();
            (find.query.is_empty(), find.invalid)
        };
        if blank {
            return String::new();
        }
        if invalid {
            return "Invalid pattern".to_string();
        }
        let matches = self.matches();
        let at = self
            .buffer
            .selection_bounds()
            .map(|(s, e)| s.offset()..e.offset());
        let position = at.and_then(|at| matches.iter().position(|m| *m == at));
        match (matches.len(), position) {
            (0, _) => "No results".to_string(),
            (count, Some(i)) => format!("{} of {count}", i + 1),
            (count, None) => format!("{count} matches"),
        }
    }

    pub fn line_count(&self) -> i32 {
        self.buffer.line_count()
    }

    /// Reveal folds, place the caret, scroll to `align` and focus the view.
    /// Route all jumps here so outline, search and definition targets cannot
    /// remain invisibly inside folded text.
    pub fn jump_to(&self, iter: &gtk::TextIter, align: f64) {
        self.show_at(iter, align);
        self.view.grab_focus();
    }

    /// [`jump_to`](Self::jump_to) leaving the keyboard where it is: where a debugged program
    /// paused, which the reader may be typing elsewhere than in.
    pub fn show_at(&self, iter: &gtk::TextIter, align: f64) {
        if let Some(editor) = self.bibliography.borrow().as_ref() {
            editor.show_source();
        }
        let opened = self.reveal_select(iter, iter);
        self.scroll_to_caret(align, opened);
    }

    /// Select `start..end` and report whether hidden lines opened. In a
    /// comparison, reveal the run on both sides and retain that open state
    /// through edits, rather than revealing only the editor under its peer's button.
    fn reveal_select(&self, start: &gtk::TextIter, end: &gtk::TextIter) -> bool {
        let opened = fold::reveal(self.text_buffer(), start);
        self.buffer.select_range(start, end);
        if opened {
            self.revealed_at(start.offset());
        }
        opened
    }

    /// After fold reveal at `offset`, restore line diagnostics and open
    /// any comparison run on both sides.
    pub(super) fn revealed_at(&self, offset: i32) {
        self.paint_diagnostics();
        if let Some(hosted) = self.hosted()
            && !hosted.open_hiding(offset)
        {
            hosted.refresh();
        }
    }

    /// Scroll the caret to `align` after allocation and line measurement settle.
    ///
    /// New tabs can process scroll idles before allocation or lazy measurement, stopping short.
    /// Wait for an allocated frame, then a default idle after GTK's higher-priority validation.
    /// `measuring` also waits after revealing folds whose lines previously had no height.
    pub(crate) fn scroll_to_caret(&self, align: f64, measuring: bool) {
        self.when_measured(measuring, move |view| {
            view.scroll_to_mark(&view.buffer().get_insert(), 0.0, true, 0.0, align)
        });
    }

    /// Run `scroll` now, or after the wait [`Tab::scroll_to_caret`] describes: while the view has
    /// no size yet, or while it is `measuring` the lines a fold just showed.
    fn when_measured(&self, measuring: bool, scroll: impl Fn(&sourceview5::View) + Copy + 'static) {
        if self.view.height() > 0 && !measuring {
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

    /// Move the caret to a zero-based, character-counted server position.
    pub fn goto_pos(&self, pos: Pos) {
        self.jump_to(&lang::iter_at(&self.buffer, pos), 0.25);
    }

    /// Jump to a heading slug/text or `^id` block and report whether it exists.
    /// Missing anchors leave the caret unchanged.
    pub fn goto_heading(&self, anchor: &str) -> bool {
        let text = self.text();
        let Some(at) = markdown::anchor_range(&text, anchor) else {
            return false;
        };
        self.goto_pos(pos_of(&text, at.start));
        true
    }

    /// Select and reveal a character range from a search or tag result.
    ///
    /// Keep the find query for F3 without painting all matches. A temporary reveal marks only
    /// the destination, avoiding persistent highlights on unrelated occurrences.
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
        // So `F3` and `Shift+F3` step through the matched text from here on. Painting it is turned
        // off with the same breath, and not merely left alone: a query handed over while the bar
        // was up would light up every match. The bar turns it back on the moment it is opened or
        // typed in (`find.rs::search`), which is when a reader has asked to see every match
        // rather than the one they were sent to.
        self.set_query(&self.buffer.text(&start, &end, false));
        self.set_highlight(false);
        // After the caret and the selection have moved: both are mark moves, and a mark move is
        // one of the interactions that takes the reveal back down again. The query's own rescan
        // moves no mark and changes no text, so it cannot take the reveal down with it.
        self.reveal_range(&start, &end);
    }

    /// Jump to the first exact `#name` and reveal it for a Tags result.
    /// Leave the caret unchanged if the tag no longer exists.
    pub fn goto_tag(&self, name: &str) {
        let text = self.text();
        let found = tag_range(&text, name).and_then(|at| crate::references::char_range(&text, at));
        if let Some(chars) = found {
            self.goto_range(chars);
        }
    }

    /// Highlight a jump destination until the document is touched.
    ///
    /// Insert the reveal tag above occurrence hints and below find matches so dim hints cannot
    /// override it and find highlighting retains priority. Only the reveal expires.
    pub fn reveal_range(&self, start: &gtk::TextIter, end: &gtk::TextIter) {
        self.clear_reveal();
        if start.offset() == end.offset() {
            return;
        }
        self.buffer.apply_tag(&self.reveal_tag, start, end);
        self.revealed.set(true);
    }

    /// Reveal a whole one-based line for Go to Line. Empty lines have no
    /// text to highlight.
    pub fn reveal_line(&self, line: i32) {
        let end = line_end(&self.buffer, line - 1);
        let mut start = end;
        start.set_line_offset(0);
        self.reveal_range(&start, &end);
    }

    /// Clear jump reveal on the first edit, click or caret move. Avoid timers:
    /// interaction indicates the reader has located the destination.
    pub fn clear_reveal(&self) {
        if !self.revealed.replace(false) {
            return;
        }
        self.buffer.remove_tag(
            &self.reveal_tag,
            &self.buffer.start_iter(),
            &self.buffer.end_iter(),
        );
    }

    /// Whether the reveal is up, and the tag it paints with. Only `ACCENT_BENCH_REVEAL` reads it.
    #[cfg(feature = "bench")]
    pub fn reveal_highlight(&self) -> (bool, gtk::TextTag) {
        (self.revealed.get(), self.reveal_tag.clone())
    }

    /// Preview a line while typing in Go to Line, scrolling without moving the caret.
    pub fn show_line(&self, line: i32) {
        let iter = self.line_iter(line, 1);
        // A comparison's collapsed run is opened through the comparison, as its own button opens
        // it: both sides at once, and it stays open the next time the diff is laid. `fold::reveal`
        // alone would take the tag off this buffer, leaving the lines under the other side's
        // "unchanged lines" button — there is no caret moved here for the comparison to keep the
        // run open around.
        let laid = self
            .hosted()
            .is_some_and(|hosted| hosted.open_hiding(iter.offset()));
        let revealed = fold::reveal(self.text_buffer(), &iter);
        if revealed {
            // As above: a fold opened here has lines back on screen to write on.
            self.paint_diagnostics();
        }
        let opened = revealed || laid;
        let line = iter.line();
        self.when_measured(opened, move |view| {
            if let Some(mut iter) = view.buffer().iter_at_line(line) {
                view.scroll_to_iter(&mut iter, 0.0, true, 0.0, 0.25);
            }
        });
    }

    /// A 1-based line and column as an iter, both clamped to what the note has.
    fn line_iter(&self, line: i32, column: i32) -> gtk::TextIter {
        let end = line_end(&self.buffer, line - 1);
        let mut iter = end;
        iter.set_line_offset((column - 1).clamp(0, end.line_offset()));
        iter
    }
}

#[cfg(test)]
mod tests {
    use super::{next_match, tag_range};

    /// Forward from the caret, forward past the match selected, back before it, and round at
    /// either end.
    #[test]
    fn a_step_lands_on_the_next_match_and_wraps() {
        let matches = [2..5, 8..11, 14..17];
        assert_eq!(next_match(&matches, 0..0, true, false), Some(2..5));
        assert_eq!(next_match(&matches, 2..5, true, true), Some(2..5));
        assert_eq!(next_match(&matches, 2..5, true, false), Some(8..11));
        assert_eq!(next_match(&matches, 8..11, false, false), Some(2..5));
        assert_eq!(next_match(&matches, 14..17, true, false), Some(2..5));
        assert_eq!(next_match(&matches, 2..5, false, false), Some(14..17));
        assert_eq!(next_match(&[], 0..0, true, false), None);
        // A match of no width at the caret is where the caret is, not the next one.
        assert_eq!(next_match(&[3..3, 6..6], 3..3, true, false), Some(6..6));
    }

    #[test]
    fn a_tag_is_found_where_the_note_writes_it() {
        let text = "---\ntags: [draft]\n---\n\nSee #rust and `#rust` and #rust again.\n";
        let at = tag_range(text, "rust").expect("the note writes the tag");
        assert_eq!(
            &text[at.clone()],
            "#rust",
            "the marker is part of the range"
        );
        assert_eq!(at.start, text.find("#rust").unwrap(), "the first one");
        // The frontmatter is where a tag listed there is, so that is where the jump lands.
        let front = tag_range(text, "draft").expect("the frontmatter lists it");
        assert!(front.start < text.find("See").unwrap());
        assert_eq!(tag_range(text, "python"), None);
    }
}
