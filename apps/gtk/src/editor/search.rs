//! Finding, replacing and going somewhere: the find bar's query over the tab's own text, the
//! muted highlight over every other occurrence of the selection, and the caret and scroll moves
//! that follow a match.
//!
//! The widgets live in `find.rs`, one bar per window. What is here is what belongs to one buffer.

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

/// Paint `tag` with every property of the scheme's own `search-match` style, as GtkSourceView's
/// search context painted its matches, for the find bar's matches.
pub(super) fn match_style(buffer: &sourceview5::Buffer, tag: &gtk::TextTag) {
    if let Some(style) = buffer
        .style_scheme()
        .and_then(|scheme| scheme.style("search-match"))
    {
        style.apply(tag);
    }
}

/// Paint `tag` in the scheme's own `search-match` colour at full weight, for the reveal a jump
/// leaves behind: being sent somewhere and finding a match there are the same thing to a reader,
/// so they are the same colour. Derived rather than named, so it follows a theme switch.
pub(super) fn matched(buffer: &sourceview5::Buffer, tag: &gtk::TextTag) {
    if let Some(colour) = search_match_colour(buffer) {
        tag.set_background_rgba(Some(&colour));
    }
}

/// Where in `text` the first `#name` is written, as the byte range including the marker, or
/// `None` for a tag the note does not carry. The note is parsed rather than searched, so a `#tag`
/// inside a code span or a URL is not one, and a tag listed in the frontmatter is.
///
/// Matched exactly, because that is how the index groups the names the Tags pane lists: a note
/// writing both `#Rust` and `#rust` offers the pane two rows, and each opens onto its own.
fn tag_range(text: &str, name: &str) -> Option<Range<usize>> {
    let found = markdown::analyze(text)
        .tags
        .into_iter()
        .find(|t| t.name == name)?;
    Some(found.range)
}

/// How many lines above and below the ones on screen the find bar's matches are painted in.
const PAINT_MARGIN: i32 = 100;

/// The find bar's query over one buffer: what it asks, where that matches, and whether the
/// matches are painted. The query is the tab's rather than the bar's, so a note opened from a
/// search hit keeps it after the bar is pointed elsewhere.
///
/// Matched by `accent_core::search`, the Search pane's own matcher, over the whole text, folded
/// blocks included: a query and its three toggles find in a note what they find across the vault.
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

/// The match a step lands on from the selection `at`: forward, the first at or after its start
/// (`from_current`, so growing the query keeps the match in view) or else past its end; back, the
/// last before its start. Either end wraps round to the other, as the bar always has.
fn next_match(
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

    /// Match `text` under `options` again, paint it where the highlight is on, and say so to the
    /// bar pointed at this tab.
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

    /// Called whenever the query has been matched again, with whether the query itself changed —
    /// a jump handing it new text among the ways — rather than the text under it or the lines
    /// on screen. One callback, and the bar last pointed at this tab sets it.
    pub fn connect_found(&self, f: impl Fn(bool) + 'static) {
        *self.on_found.borrow_mut() = Some(Rc::new(f));
    }

    /// Cloned out of its cell before it runs, as [`Tab::emit`]'s hooks are.
    fn emit_found(&self, query: bool) {
        let f = self.on_found.borrow().clone();
        if let Some(f) = f {
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

    /// Put the match tag on the matches in and around the lines on screen while the highlight is
    /// on, and on nothing otherwise. A match of no width has nothing to paint.
    ///
    /// Only around the lines on screen, [`PAINT_MARGIN`] either way, and painted again as the view
    /// scrolls: a tag on each of a hundred thousand matches in a long file took seconds to put on
    /// and seconds again after every keystroke, where a screenful takes no time at all.
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

    /// The text changed under the query: its matches are out of date. A painted query is matched
    /// again and painted from an idle, as it is when the view scrolls ([`Tab::queue_paint`]).
    /// An unpainted one waits for the step that needs it.
    pub(super) fn find_edited(self: &Rc<Self>) {
        self.find.borrow_mut().stale = true;
        self.queue_paint();
    }

    /// Paint the matches again from an idle ahead of the next frame, where the highlight is on,
    /// so a run of edits — a Replace All is one per match — or of scroll steps is painted once.
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
    #[cfg(feature = "bench")]
    pub fn occurrence_highlight(&self) -> (Option<String>, gtk::TextTag) {
        (
            self.occurrence_query.borrow().clone(),
            self.occurrence_tag.clone(),
        )
    }

    /// The two match backgrounds, the find bar's first: what `ACCENT_BENCH_OCCUR` prints to show
    /// the hint really is the weaker of the pair. The find bar's tag is painted in the scheme's
    /// `search-match` style ([`match_style`]), so its colour is that style's.
    #[cfg(feature = "bench")]
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

    /// Replace the selection when it is a match, then step to the next one. A selection that is
    /// not itself a match is the "nothing to do" case.
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

    /// Replace every match, as one undo step, from the last to the first so the offsets of the
    /// ones before are still where they were.
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

    /// Every match with what `with` makes of it: `$1` expanded under Regular Expression and
    /// taken as written otherwise, as Replace All in the Search pane writes it.
    fn replacements(&self, with: &str) -> Vec<(Range<usize>, String)> {
        let find = self.find.borrow();
        match &find.re {
            Some(re) => search::replacements(re, &self.text(), with, find.options),
            None => Vec::new(),
        }
    }

    /// Put `with` where `s..e` is, as one undo step, leaving `s` and `e` round it. A match in a
    /// shut block stays shut: GTK puts text inserted where a hidden run begins outside the run,
    /// so a replacement at the top of a fold would otherwise show on its own under the header.
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

    /// "n of m", the way every find bar says it, or that the query does not compile.
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

    /// The one way the caret is sent somewhere: open whatever fold is hiding the destination,
    /// put the caret there, scroll it to `align` down the view and take the focus.
    ///
    /// Every jump goes through here — an outline row, a search hit, a go-to line, a definition —
    /// so none of them can land inside a folded block and leave the window looking unchanged.
    pub fn jump_to(&self, iter: &gtk::TextIter, align: f64) {
        let opened = self.reveal_select(iter, iter);
        self.scroll_to_caret(align, opened);
        self.view.grab_focus();
    }

    /// Select `start..end`, opening whatever hides it first, and say whether anything opened:
    /// lines GTK has yet to measure. A comparison opens the run it is in on both sides, as the
    /// run's button does, where `fold::reveal` opened it on the editor's side alone, under the
    /// other side's "unchanged lines" button; and so the run stays open through the edits made
    /// there, which leave no caret in it to keep it open around.
    fn reveal_select(&self, start: &gtk::TextIter, end: &gtk::TextIter) -> bool {
        let opened = fold::reveal(self.text_buffer(), start);
        self.buffer.select_range(start, end);
        if opened {
            // The lines that came back out want their end-of-line messages back.
            self.paint_diagnostics();
            if let Some(compare) = self.comparison()
                && !compare.open_hiding(start.offset())
            {
                compare.refresh();
            }
        }
        opened
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
    ///
    /// `measuring` is the same wait for a view that does have a size: a fold just opened, whose
    /// lines had no height until GTK measures them, and the caret below them sits as high up as
    /// the fold was.
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

    /// Put the caret at a server position, which is zero-based and counts characters.
    pub fn goto_pos(&self, pos: Pos) {
        self.jump_to(&lang::iter_at(&self.buffer, pos), 0.25);
    }

    /// Put the caret on the heading `anchor` names, by slug or by text as a link writes it, or on
    /// the block a `^id` marks, and say whether the note has it: one it does not have leaves the
    /// caret where it was.
    pub fn goto_heading(&self, anchor: &str) -> bool {
        let text = self.text();
        let Some(at) = markdown::anchor_range(&text, anchor) else {
            return false;
        };
        self.goto_pos(pos_of(&text, at.start));
        true
    }

    /// Jump to a character range, select it and reveal it: what a search result and a tag row
    /// open a note onto.
    ///
    /// The reveal is what the reader is shown, not the find bar. Handing the bar the matched text
    /// *and painting it* used to be how this was marked, and it marked too much for too long:
    /// every other occurrence in the note lit up as well, and none of it went away until the bar
    /// was opened and closed again. The query alone is kept, unpainted, because `F3` is worth
    /// having over the text the row pointed at and a query nothing paints costs nothing to look at.
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

    /// Put the caret on the first place the note writes `#name` and reveal it: what a row under a
    /// tag in the Tags pane opens onto, the way a search result opens onto its match. A note that
    /// no longer carries the tag leaves the caret where it was.
    pub fn goto_tag(&self, name: &str) {
        let text = self.text();
        let found = tag_range(&text, name).and_then(|at| crate::references::char_range(&text, at));
        if let Some(chars) = found {
            self.goto_range(chars);
        }
    }

    /// Mark where a jump landed, until the document is touched.
    ///
    /// The tag joins the table after the muted occurrence tag and before the find bar's match
    /// tag, so the three coexist by priority rather than by luck: a revealed match is never
    /// dimmed by the hint that may cover the same word, and where a find-bar match covers it the
    /// bar's tag wins — which paints the same `search-match` colour, so an overlap reads the same
    /// either way. Only the reveal expires; the other two are as long-lived as what they answer.
    pub fn reveal_range(&self, start: &gtk::TextIter, end: &gtk::TextIter) {
        self.clear_reveal();
        if start.offset() == end.offset() {
            return;
        }
        self.buffer.apply_tag(&self.reveal_tag, start, end);
        self.revealed.set(true);
    }

    /// Reveal the whole of a 1-based line: what Go to Line lands on, where the place asked for is
    /// the line rather than anything on it. An empty line has no text to paint and shows nothing.
    pub fn reveal_line(&self, line: i32) {
        let end = line_end(&self.buffer, line - 1);
        let mut start = end;
        start.set_line_offset(0);
        self.reveal_range(&start, &end);
    }

    /// Take the reveal down, which the first keystroke, click or caret move in the document does.
    ///
    /// No timer and no fade: the highlight answers "where was I sent?", and the reader is the one
    /// who knows when that has been answered. A timer either goes while they are still looking or
    /// outstays the question.
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

    /// Scroll a line into view without moving the caret: what the go-to entry previews while the
    /// number is still being typed.
    pub fn show_line(&self, line: i32) {
        let iter = self.line_iter(line, 1);
        // A comparison's collapsed run is opened through the comparison, as its own button opens
        // it: both sides at once, and it stays open the next time the diff is laid. `fold::reveal`
        // alone would take the tag off this buffer, leaving the lines under the other side's
        // "unchanged lines" button — there is no caret moved here for the comparison to keep the
        // run open around.
        let laid = self
            .comparison()
            .is_some_and(|compare| compare.open_hiding(iter.offset()));
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
