//! Completion for every text flavour through one [`Source`] interface.
//!
//! A [`Session`] holds an answer and its popup. [`Anchor`] marks track the request position;
//! [`rank`] narrows complete answers locally as typing continues. Incomplete answers request
//! updates per keystroke while the current list narrows, avoiding flicker over slow links.
//! Each item uses its own start and range, moved with edits through [`apply::Moved`].
//!
//! The view retains focus. `editor::keys` offers presses to [`press`] first for arrows,
//! Escape, and Return or Tab with a selected row; other keys keep normal typing behaviour, a
//! commit character after accepting the selected row.

mod abbrev;
mod apply;
mod popup;
mod rank;

pub(crate) use popup::{CONTENTS_PADDING, ROW_PADDING, ROW_PADDING_Y, ROW_TEXT_MIN};

use std::cell::{Cell, RefCell};
use std::future::Future;
use std::pin::Pin;
use std::rc::{Rc, Weak};
use std::time::Duration;

use accent_api::{Completion, Completions, Kind, Pos, Range, TextEdit};
use accent_core::fuzzy::Corpus;
use gtk::prelude::*;
use gtk::{gdk, glib};
use sourceview5::prelude::*;

use crate::editor::{Tab, caret, line_prefix};
use crate::{hover, lang, multicaret};

/// Maximum displayed rows, bounding per-keystroke splicing of servers'
/// large answers to a practical scrollable list.
const MOST: usize = 200;

/// Selection-rest delay before resolving docs, avoiding server requests
/// for every row crossed by arrow keys.
const LOOK: Duration = Duration::from_millis(50);

/// A pinned future, which is what every reply here is.
pub(crate) type Reply<T> = Pin<Box<dyn Future<Output = T>>>;

/// Where a session's answers come from, and what it asks of the document it completes in.
pub(crate) trait Source {
    /// Request at `at` with a provider-recognized typed `trigger`, or none
    /// if the provider cannot answer.
    fn fetch(&self, at: Pos, trigger: Option<char>) -> Reply<Completions>;
    /// Resolve deferred docs/imports, returning `item` unchanged if unavailable.
    fn resolve(&self, item: Completion) -> Reply<Completion>;
    /// The popup came up, or went.
    fn shown(&self, _up: bool) {}
    /// Park `snippet` in `view` at `at`, its stops to be walked with Tab.
    fn push_snippet(
        &self,
        view: &sourceview5::View,
        snippet: &sourceview5::Snippet,
        at: &mut gtk::TextIter,
    ) {
        view.push_snippet(snippet, Some(at));
    }
}

/// Left/right-gravity marks around the request caret. Their distance
/// tracks later typing for range remapping; remove them with the answer.
struct Anchor {
    at: Pos,
    left: gtk::TextMark,
    right: gtk::TextMark,
}

impl Anchor {
    fn new(buffer: &gtk::TextBuffer, caret: &gtk::TextIter) -> Anchor {
        Anchor {
            at: lang::pos_of(caret),
            left: buffer.create_mark(None, caret, true),
            right: buffer.create_mark(None, caret, false),
        }
    }

    /// Map subsequent typing, or `None` if the caret left the line, moved
    /// before the request, or text under its marks was deleted.
    fn moved(&self, buffer: &gtk::TextBuffer, caret: &gtk::TextIter) -> Option<apply::Moved> {
        let (left, right) = (
            buffer.iter_at_mark(&self.left),
            buffer.iter_at_mark(&self.right),
        );
        let still = lang::pos_of(&left) == self.at
            && right.line() == left.line()
            && caret.line() == left.line()
            && *caret >= left;
        still.then(|| apply::Moved {
            at: self.at,
            typed: (right.line_offset() - left.line_offset()) as u32,
        })
    }
}

impl Drop for Anchor {
    fn drop(&mut self) {
        if let Some(buffer) = self.left.buffer() {
            buffer.delete_mark(&self.left);
            buffer.delete_mark(&self.right);
        }
    }
}

/// An answer being shown, narrowed as the typing goes on.
struct Shown {
    anchor: Anchor,
    items: Vec<Item>,
    incomplete: bool,
    /// The rows on screen, as indices into `items`, best first.
    rows: Vec<usize>,
}

struct Item {
    completion: Completion,
    /// Its documentation has been asked for: once per row.
    resolved: bool,
}

pub(crate) struct Session {
    view: sourceview5::View,
    source: Box<dyn Source>,
    popup: popup::Popup,
    shown: RefCell<Option<Shown>>,
    /// Request generation, rejecting stale answers and their row resolves.
    generation: Cell<u64>,
    /// In-flight request and location. Replacement aborts it, cancelling
    /// the server request.
    fetch: RefCell<Option<glib::JoinHandle<()>>>,
    asked_at: Cell<Option<Pos>>,
    /// The selected row's documentation being asked for.
    detail: RefCell<Option<glib::JoinHandle<()>>>,
    /// The session is editing the buffer itself, which opens nothing.
    muted: Cell<bool>,
    /// An accepted row's resolve, bringing what the provider kept back.
    accepting: RefCell<Option<glib::JoinHandle<()>>>,
    /// The input method holds text not committed yet: the keys are its own.
    preedit: Cell<bool>,
    /// Queued idle caret check, coalescing a keystroke's edit and movement.
    queued: Cell<bool>,
    /// Whether selection came from [`Completion::preselect`]; later typing
    /// reselects it instead of preserving an arrow-selected row.
    preselected: Cell<bool>,
}

impl Session {
    /// Complete `view` through `source`, with Ctrl+Space requesting at the
    /// caret. Hide on unmap, focus loss or a new caret column.
    pub(crate) fn new(view: &sourceview5::View, source: Box<dyn Source>) -> Rc<Session> {
        let session = Rc::new_cyclic(|me: &Weak<Session>| {
            let (accept, look) = (me.clone(), me.clone());
            let popup = popup::Popup::new(
                view,
                move |row| {
                    if let Some(me) = accept.upgrade() {
                        me.accept(row);
                    }
                },
                move || {
                    if let Some(me) = look.upgrade() {
                        me.look();
                    }
                },
            );
            Session {
                view: view.clone(),
                source,
                popup,
                shown: RefCell::default(),
                generation: Cell::new(0),
                fetch: RefCell::default(),
                asked_at: Cell::new(None),
                detail: RefCell::default(),
                muted: Cell::new(false),
                accepting: RefCell::default(),
                preedit: Cell::new(false),
                queued: Cell::new(false),
                preselected: Cell::new(false),
            }
        });
        session.watch();
        session
    }

    fn watch(self: &Rc<Self>) {
        let view = &self.view;
        view.buffer().connect_cursor_position_notify(glib::clone!(
            #[weak(rename_to = me)]
            self,
            move |_| me.queue()
        ));
        // Ahead of GtkSourceView's own handler, which runs last and would bring up its own popup.
        view.connect_show_completion(glib::clone!(
            #[weak(rename_to = me)]
            self,
            move |view| {
                view.stop_signal_emission_by_name("show-completion");
                me.ask(None);
            }
        ));
        view.connect_unmap(glib::clone!(
            #[weak(rename_to = me)]
            self,
            move |_| me.close()
        ));
        view.connect_preedit_changed(glib::clone!(
            #[weak(rename_to = me)]
            self,
            move |_, text| me.preedit.set(!text.is_empty())
        ));
        let focus = gtk::EventControllerFocus::new();
        focus.connect_leave(glib::clone!(
            #[weak(rename_to = me)]
            self,
            move |_| me.close()
        ));
        view.add_controller(focus);
        if let Some(column) = view.downcast_ref::<multicaret::View>() {
            let me = Rc::downgrade(self);
            column.on_column_started(move || {
                if let Some(me) = me.upgrade() {
                    me.close();
                }
            });
        }
        // The text scrolled under the popup: it follows what it completes, and goes once that
        // is out of sight. Whatever adjustments the view has now, and any a comparison gives it.
        self.follow_scrolling(view.vadjustment());
        self.follow_scrolling(view.hadjustment());
        view.connect_vadjustment_notify(glib::clone!(
            #[weak(rename_to = me)]
            self,
            move |view| me.follow_scrolling(view.vadjustment())
        ));
        view.connect_hadjustment_notify(glib::clone!(
            #[weak(rename_to = me)]
            self,
            move |view| me.follow_scrolling(view.hadjustment())
        ));
    }

    fn follow_scrolling(self: &Rc<Self>, adjustment: Option<gtk::Adjustment>) {
        if let Some(adjustment) = adjustment {
            adjustment.connect_value_changed(glib::clone!(
                #[weak(rename_to = me)]
                self,
                move |_| me.follow()
            ));
        }
    }

    /// Whether the popup is on screen.
    pub(crate) fn is_shown(&self) -> bool {
        self.popup.is_shown()
    }

    /// Whether there is an answer up or on its way, which is what the typing narrows.
    fn active(&self) -> bool {
        self.shown.borrow().is_some() || self.fetch.borrow().is_some()
    }

    /// Open on eligible typing ([`rank::opens`]). Triggers requery; words
    /// query only when no existing answer can narrow.
    fn open(self: &Rc<Self>, trigger: Option<char>) {
        if trigger.is_some() || !self.active() {
            self.ask(trigger);
        }
    }

    /// Ask at the caret. The answer up, if any, stays and is narrowed until this one lands.
    pub(crate) fn ask(self: &Rc<Self>, trigger: Option<char>) {
        let buffer = self.view.buffer();
        let anchor = Anchor::new(&buffer, &caret(&buffer));
        let generation = self.generation.get().wrapping_add(1);
        self.generation.set(generation);
        self.asked_at.set(Some(anchor.at));
        let fetch = self.source.fetch(anchor.at, trigger);
        let me = Rc::downgrade(self);
        let handle = glib::spawn_future_local(async move {
            let answer = fetch.await;
            if let Some(me) = me.upgrade() {
                me.land(generation, anchor, answer);
            }
        });
        if let Some(old) = self.fetch.replace(Some(handle)) {
            old.abort();
        }
    }

    fn land(&self, generation: u64, anchor: Anchor, answer: Completions) {
        if generation != self.generation.get() {
            return;
        }
        // Done, so let go of it rather than abort it: this is its own last step.
        drop(self.fetch.take());
        // The row selected stays selected, if the new answer has it.
        let selected = self.selected_label();
        let items = answer
            .items
            .into_iter()
            .map(|completion| Item {
                completion,
                resolved: false,
            })
            .collect();
        *self.shown.borrow_mut() = Some(Shown {
            anchor,
            items,
            incomplete: answer.incomplete,
            rows: Vec::new(),
        });
        self.refresh(selected);
    }

    fn selected_label(&self) -> Option<String> {
        let row = self.popup.selected()?;
        let shown = self.shown.borrow();
        let shown = shown.as_ref()?;
        let item = shown.rows.get(row as usize)?;
        Some(shown.items[*item].completion.label.clone())
    }

    /// The caret moved or the text changed with an answer up: narrow it, at the next idle.
    fn queue(self: &Rc<Self>) {
        if !self.active() || self.queued.replace(true) {
            return;
        }
        let me = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            if let Some(me) = me.upgrade() {
                me.queued.set(false);
                me.moved();
            }
        });
    }

    fn moved(self: &Rc<Self>) {
        let here = lang::pos_of(&caret(&self.view.buffer()));
        let short = self.shown.borrow().as_ref().is_some_and(|s| s.incomplete);
        if short && self.asked_at.get() != Some(here) {
            self.ask(None);
        }
        let selected = self.selected_label();
        self.refresh(selected);
    }

    /// Filter the answer by subsequent typing, preserving `selected`, or
    /// end completion when the caret leaves its prefix.
    fn refresh(&self, selected: Option<String>) {
        let buffer = self.view.buffer();
        let caret = caret(&buffer);
        let mut guard = self.shown.borrow_mut();
        let Some(shown) = guard.as_mut() else {
            return;
        };
        let Some(moved) = shown.anchor.moved(&buffer, &caret) else {
            drop(guard);
            return self.close();
        };
        let here = lang::pos_of(&caret);
        let line: Vec<char> = line_prefix(&buffer, &caret).chars().collect();
        // What was typed since each distinct start, read once per start: an answer's items
        // mostly share one.
        let mut typed: Vec<(Pos, String)> = Vec::new();
        let starts: Vec<Pos> = shown
            .items
            .iter()
            .map(|item| moved.start(item.completion.replace.start))
            .collect();
        for start in &starts {
            if !typed.iter().any(|(s, _)| s == start) {
                typed.push((*start, typed_since(&buffer, &line, *start, here)));
            }
        }
        let typed_at = |start: &Pos| {
            typed
                .iter()
                .find(|(s, _)| s == start)
                .map_or("", |(_, t)| t.as_str())
        };
        let candidates: Vec<rank::Candidate> = shown
            .items
            .iter()
            .zip(&starts)
            .map(|(item, start)| rank::Candidate {
                filter: item
                    .completion
                    .filter
                    .as_deref()
                    .unwrap_or(&item.completion.label),
                typed: typed_at(start),
                corpus: match item.completion.kind {
                    Kind::File | Kind::Folder => Corpus::Paths,
                    _ => Corpus::Words,
                },
            })
            .collect();
        let mut ranked = rank::rank(&candidates);
        ranked.truncate(MOST);
        shown.rows = ranked;
        if shown.rows.is_empty() {
            // Nothing matches now; an ask in flight may still bring something.
            let waiting = self.fetch.borrow().is_some();
            drop(guard);
            return match waiting {
                true => self.put_away(),
                false => self.close(),
            };
        }
        let words = shown
            .rows
            .iter()
            .all(|&i| shown.items[i].completion.kind == Kind::Text);
        let rows = shown
            .rows
            .iter()
            .map(|&i| {
                let item = &shown.items[i].completion;
                popup::Row {
                    label: item.label.clone(),
                    bold: rank::highlight(&item.label, typed_at(&starts[i])),
                    detail: item.detail.clone(),
                    icon: (!words).then(|| lang::icon_name(item.kind)),
                }
            })
            .collect();
        // A row asking to be preselected is, once what was typed is its filter exactly (a Lean
        // abbreviation typed out, so Tab writes it); otherwise the row the arrows picked stays.
        let exact = shown.rows.iter().position(|&i| {
            shown.items[i].completion.preselect && candidates[i].filter == candidates[i].typed
        });
        let arrowed = selected.filter(|_| !self.preselected.get());
        let keep = exact.or_else(|| {
            let label = arrowed?;
            shown
                .rows
                .iter()
                .position(|&i| shown.items[i].completion.label == label)
        });
        self.preselected.set(exact.is_some());
        let start = starts[shown.rows[0]];
        drop(guard);
        match self.rect_at(start) {
            Some(at) => {
                let was = self.is_shown();
                self.popup.show(at, rows, keep.map(|i| i as u32));
                if !was && self.is_shown() {
                    self.source.shown(true);
                }
            }
            None => self.close(),
        }
    }

    /// Where `pos` is in the view's widget coordinates, or `None` when it is scrolled out of
    /// sight.
    fn rect_at(&self, pos: Pos) -> Option<gdk::Rectangle> {
        let iter = lang::iter_at(&self.view.buffer(), pos);
        let at = self.view.iter_location(&iter);
        let (x, y) = self
            .view
            .buffer_to_window_coords(gtk::TextWindowType::Widget, at.x(), at.y());
        let (width, height) = (self.view.width(), self.view.height());
        let inside = x >= 0 && x <= width && y + at.height() >= 0 && y <= height;
        inside.then(|| gdk::Rectangle::new(x, y, 1, at.height().max(1)))
    }

    /// The text scrolled: point the popup at what it completes again, or end it out of sight.
    fn follow(&self) {
        if !self.is_shown() {
            return;
        }
        let start = {
            let shown = self.shown.borrow();
            let buffer = self.view.buffer();
            shown.as_ref().and_then(|s| {
                let moved = s.anchor.moved(&buffer, &caret(&buffer))?;
                let first = s.rows.first()?;
                Some(moved.start(s.items[*first].completion.replace.start))
            })
        };
        match start.and_then(|start| self.rect_at(start)) {
            Some(at) => self.popup.point(at),
            None => self.close(),
        }
    }

    /// End the session: the ask in flight, the answer, and the popup.
    pub(crate) fn close(&self) {
        if let Some(handle) = self.fetch.take() {
            handle.abort();
        }
        if let Some(handle) = self.detail.take() {
            handle.abort();
        }
        self.asked_at.set(None);
        self.shown.replace(None);
        self.preselected.set(false);
        self.put_away();
    }

    /// Take the popup off the screen, the session staying.
    fn put_away(&self) {
        let was = self.is_shown();
        self.popup.hide();
        if was {
            self.source.shown(false);
        }
    }

    /// Handle popup keys or return `None` to `editor::keys`. With a caret
    /// column, unhandled keys reach GTK at the primary and end the column,
    /// as accepting a row does.
    pub(crate) fn press(
        self: &Rc<Self>,
        key: gdk::Key,
        state: gdk::ModifierType,
    ) -> Option<glib::Propagation> {
        if !self.is_shown() || self.preedit.get() {
            return None;
        }
        let column = self
            .view
            .downcast_ref::<multicaret::View>()
            .is_some_and(|view| view.has_carets());
        let past = column.then_some(glib::Propagation::Proceed);
        let selected = self.popup.selected();
        let commit = selected.map(|row| self.commit_of(row)).unwrap_or_default();
        match rank::step(key, state, selected, self.popup.len(), &commit) {
            rank::Step::Select(row) => {
                self.preselected.set(false);
                self.popup.select(row);
                Some(glib::Propagation::Stop)
            }
            rank::Step::Accept(row) => {
                self.accept(row);
                Some(glib::Propagation::Stop)
            }
            rank::Step::Commit(row) => {
                self.accept(row);
                past
            }
            rank::Step::Dismiss => {
                self.close();
                Some(glib::Propagation::Stop)
            }
            rank::Step::Close => {
                self.close();
                past
            }
            rank::Step::Pass => past,
        }
    }

    /// The commit characters of the item at `row`.
    fn commit_of(&self, row: u32) -> Vec<char> {
        let shown = self.shown.borrow();
        let item = shown.as_ref().and_then(|shown| {
            let i = *shown.rows.get(row as usize)?;
            Some(&shown.items[i].completion)
        });
        item.map_or_else(Vec::new, |item| item.commit.clone())
    }

    /// The item at `row` with the range it replaces and its extra edits, moved on by what was
    /// typed since it was asked for; the session ends.
    fn take(&self, row: u32) -> Option<(Completion, Range, Vec<TextEdit>)> {
        let buffer = self.view.buffer();
        let caret = caret(&buffer);
        let accepted = {
            let shown = self.shown.borrow();
            shown.as_ref().and_then(|shown| {
                let item = &shown.items[*shown.rows.get(row as usize)?].completion;
                let moved = shown.anchor.moved(&buffer, &caret)?;
                let range = apply::replaced(item.replace, moved, lang::pos_of(&caret));
                let extras = item.extra_edits.iter().map(|e| moved.edit(e)).collect();
                Some((item.clone(), range, extras))
            })
        };
        self.close();
        accepted
    }

    /// Insert the item at `row` now, as it stands, and end the session: accepting waits on no
    /// server. A row whose resolve has not answered yet is resolved after, and what that adds,
    /// an import, goes in once it lands, as its own undo step, unless the text before it has
    /// changed meanwhile ([`apply::still_fits`]): VS Code's rule.
    fn accept(self: &Rc<Self>, row: u32) {
        let Some((item, range, extras)) = self.take(row) else {
            return;
        };
        self.write(&item, range, extras);
        if item.resolve.is_none() {
            return;
        }
        let then = buffer_text(&self.view.buffer());
        let asked = self.source.resolve(item.clone());
        let me = Rc::downgrade(self);
        let handle = glib::spawn_future_local(async move {
            let full = asked.await;
            let Some(me) = me.upgrade() else {
                return;
            };
            drop(me.accepting.take());
            // The answer's own went in with the row.
            let late: Vec<TextEdit> = full
                .extra_edits
                .into_iter()
                .filter(|e| !item.extra_edits.contains(e))
                .collect();
            let buffer = me.view.buffer();
            if late.is_empty() || !apply::still_fits(&then, &buffer_text(&buffer), &late) {
                return;
            }
            me.muted.set(true);
            apply::edits(&buffer, late);
            me.muted.set(false);
        });
        if let Some(old) = self.accepting.replace(Some(handle)) {
            old.abort();
        }
    }

    fn write(&self, item: &Completion, range: accent_api::Range, extras: Vec<TextEdit>) {
        self.muted.set(true);
        apply::apply(&self.view.buffer(), item, range, extras, |snippet, at| {
            self.source.push_snippet(&self.view, snippet, at)
        });
        self.muted.set(false);
    }

    /// After selection rests for [`LOOK`], resolve deferred docs and display
    /// them beside the list.
    fn look(self: &Rc<Self>) {
        if let Some(handle) = self.detail.take() {
            handle.abort();
        }
        let picked = self.popup.selected().and_then(|row| {
            let shown = self.shown.borrow();
            let shown = shown.as_ref()?;
            let i = *shown.rows.get(row as usize)?;
            let item = &shown.items[i];
            Some((i, item.completion.clone(), item.resolved))
        });
        let Some((i, completion, resolved)) = picked else {
            return self.popup.set_doc(None);
        };
        self.popup.set_doc(doc_markup(&completion).as_deref());
        if resolved || completion.resolve.is_none() {
            return;
        }
        let (me, generation) = (Rc::downgrade(self), self.generation.get());
        let handle = glib::spawn_future_local(async move {
            glib::timeout_future(LOOK).await;
            let Some(asked) = me.upgrade().map(|me| {
                me.mark_resolved(i);
                me.source.resolve(completion)
            }) else {
                return;
            };
            let full = asked.await;
            if let Some(me) = me.upgrade() {
                me.resolved(generation, i, full);
            }
        });
        *self.detail.borrow_mut() = Some(handle);
    }

    fn mark_resolved(&self, i: usize) {
        if let Some(item) = self
            .shown
            .borrow_mut()
            .as_mut()
            .and_then(|s| s.items.get_mut(i))
        {
            item.resolved = true;
        }
    }

    /// A row's resolve arrived: kept with the row, imports included for its acceptance, and its
    /// documentation shown while it is still selected.
    fn resolved(&self, generation: u64, i: usize, full: Completion) {
        if generation != self.generation.get() {
            return;
        }
        drop(self.detail.take());
        let markup = {
            let mut shown = self.shown.borrow_mut();
            let Some(shown) = shown.as_mut() else { return };
            let Some(item) = shown.items.get_mut(i) else {
                return;
            };
            item.completion.doc = full.doc;
            item.completion.detail = full.detail.or(item.completion.detail.take());
            item.completion.extra_edits = full.extra_edits;
            // Answered: an acceptance has nothing left to ask.
            item.completion.resolve = None;
            let still = self
                .popup
                .selected()
                .and_then(|row| shown.rows.get(row as usize))
                == Some(&i);
            still.then(|| doc_markup(&item.completion))
        };
        if let Some(markup) = markup {
            self.popup.set_doc(markup.as_deref());
        }
    }

    /// Show `items` as an answer asked at the caret, as a provider would have answered: what a
    /// drill stages without a server.
    #[cfg(feature = "bench")]
    pub(crate) fn show_items(self: &Rc<Self>, items: Vec<Completion>) {
        let buffer = self.view.buffer();
        let anchor = Anchor::new(&buffer, &caret(&buffer));
        let generation = self.generation.get().wrapping_add(1);
        self.generation.set(generation);
        let answer = Completions {
            items,
            ..Completions::default()
        };
        self.land(generation, anchor, answer);
    }

    /// The labels of the rows on screen, in order, and the selected row.
    #[cfg(feature = "bench")]
    pub(crate) fn rows(&self) -> (Vec<String>, Option<u32>) {
        (self.popup.labels(), self.popup.selected())
    }

    /// The documentation pane's text, while it shows.
    #[cfg(feature = "bench")]
    pub(crate) fn doc(&self) -> Option<String> {
        self.popup.doc()
    }

    /// Whether no request, docs/acceptance resolve or idle narrowing remains.
    #[cfg(feature = "bench")]
    pub(crate) fn settled(&self) -> bool {
        self.fetch.borrow().is_none()
            && self.detail.borrow().is_none()
            && self.accepting.borrow().is_none()
            && !self.queued.get()
    }
}

/// Read typing from `start` to `here` in the caret-line prefix `line`
/// when both positions share a line.
fn typed_since(buffer: &gtk::TextBuffer, line: &[char], start: Pos, here: Pos) -> String {
    if start.line == here.line {
        let from = (start.character as usize).min(line.len());
        return line[from..].iter().collect();
    }
    let from = lang::iter_at(buffer, start);
    let to = lang::iter_at(buffer, here);
    match from < to {
        true => buffer.text(&from, &to, true).to_string(),
        false => String::new(),
    }
}

/// The whole of `buffer`'s text, folded lines included.
fn buffer_text(buffer: &gtk::TextBuffer) -> String {
    buffer
        .text(&buffer.start_iter(), &buffer.end_iter(), true)
        .to_string()
}

/// A row's detail in monospace over its documentation, or `None` with neither.
fn doc_markup(item: &Completion) -> Option<String> {
    let detail = item
        .detail
        .as_deref()
        .filter(|d| !d.is_empty())
        .map(|d| format!("<tt>{}</tt>", glib::markup_escape_text(d)));
    let doc = item
        .doc
        .as_deref()
        .filter(|d| !d.is_empty())
        .map(hover::markup_of);
    match (detail, doc) {
        (Some(detail), Some(doc)) => Some(format!("{detail}\n\n{doc}")),
        (detail, doc) => detail.or(doc),
    }
}

// ------------------------------------------------------------------------------------ a tab's

/// A tab's document, asked through its vault.
///
/// Weak: the session is the tab's, and the tab holds the view the session's handlers hang off.
struct TabSource(Weak<Tab>);

impl Source for TabSource {
    fn fetch(&self, at: Pos, trigger: Option<char>) -> Reply<Completions> {
        let tab = self.0.upgrade();
        if let Some(answer) = tab.as_deref().and_then(|tab| abbreviations(tab, at)) {
            return Box::pin(async move { answer });
        }
        Box::pin(async move {
            let Some((tab, vault)) = tab.and_then(|tab| Some((tab.clone(), tab.lang.vault()?)))
            else {
                return Completions::default();
            };
            lang::flush(tab.clone()).await;
            let rel = tab.rel();
            match vault.completion(&rel, at, trigger).await {
                Ok(answer) => {
                    tracing::debug!(
                        "completion for {rel} at {at:?}: {} items{}",
                        answer.items.len(),
                        if answer.incomplete {
                            ", more to ask"
                        } else {
                            ""
                        }
                    );
                    answer
                }
                Err(e) => {
                    tracing::debug!("completion for {rel}: {e:#}");
                    Completions::default()
                }
            }
        })
    }

    fn resolve(&self, item: Completion) -> Reply<Completion> {
        let tab = self.0.upgrade();
        Box::pin(async move {
            let Some((tab, vault)) = tab.and_then(|tab| Some((tab.clone(), tab.lang.vault()?)))
            else {
                return item;
            };
            lang::flush(tab.clone()).await;
            match vault.resolve_completion(&tab.rel(), item.clone()).await {
                Ok(full) => full,
                Err(e) => {
                    tracing::debug!("completionItem/resolve: {e:#}");
                    item
                }
            }
        })
    }

    /// Clear ghost text while the popup is shown. Flush deferred ghost
    /// requests when it closes so unobscured suggestions are refreshed.
    fn shown(&self, up: bool) {
        let Some(tab) = self.0.upgrade() else { return };
        match up {
            true => crate::ghost::clear(&tab),
            false => {
                glib::spawn_future_local(async move {
                    lang::flush(tab.clone()).await;
                    crate::ghost::request(&tab).await;
                });
            }
        }
    }

    fn push_snippet(
        &self,
        view: &sourceview5::View,
        snippet: &sourceview5::Snippet,
        at: &mut gtk::TextIter,
    ) {
        match self.0.upgrade() {
            Some(tab) => tab.push_snippet(snippet, at),
            None => view.push_snippet(snippet, Some(at)),
        }
    }
}

/// Complete Lean backslash abbreviations locally ([`abbrev`]); the server
/// has no answer for this input prefix.
fn abbreviations(tab: &Tab, at: Pos) -> Option<Completions> {
    if lang::language_id(tab) != abbrev::LANGUAGE {
        return None;
    }
    let before = line_prefix(&tab.buffer, &lang::iter_at(&tab.buffer, at));
    let typed = abbrev::typed(&before)?;
    let start = Pos {
        line: at.line,
        character: at.character - typed.chars().count() as u32,
    };
    Some(abbrev::rows(typed, Range { start, end: at }))
}

/// Install vault-backed completion after [`lang::attach`] establishes
/// that the tab has a vault provider.
pub fn install(tab: &Rc<Tab>) {
    let made = Session::new(&tab.view, Box::new(TabSource(Rc::downgrade(tab))));
    // What was typed is looked at once it has landed, one idle on: a single character, or the
    // pair `typing` writes for a bracket, at the caret, its first character being the one typed.
    // A paste, a template, the text of a reload and the session's own insert open nothing.
    tab.buffer.connect_insert_text(glib::clone!(
        #[weak]
        tab,
        move |buffer, at, text| {
            let quiet = tab.is_loading()
                || text.chars().count() > 2
                || at.offset() != caret(buffer).offset()
                || session(&tab).is_none_or(|s| s.muted.get());
            if !quiet {
                let first = text.chars().next();
                glib::idle_add_local_once(move || typed(&tab, first));
            }
        }
    ));
    *tab.lang.completion.borrow_mut() = Some(made);
}

/// `tab`'s session, once [`install`] gave it one.
pub(crate) fn session(tab: &Tab) -> Option<Rc<Session>> {
    tab.lang.completion.borrow().clone()
}

/// Handle typed completion triggers ([`rank::opens`]) only while focused
/// and without a caret column, matching ghost-text eligibility. `typed` is
/// the character the keystroke typed.
fn typed(tab: &Rc<Tab>, typed: Option<char>) {
    let Some(session) = session(tab) else {
        return;
    };
    if !tab.view.is_focus() || tab.ghost_view().is_some_and(|view| view.has_carets()) {
        return;
    }
    let language = lang::language_id(tab);
    let mut triggers = tab
        .lang
        .support()
        .map(|s| s.completion_triggers.clone())
        .unwrap_or_default();
    // A Lean abbreviation opens on its backslash, which the server does not ask for.
    if language == abbrev::LANGUAGE {
        triggers.push('\\');
    }
    let min_word = match accent_api::language::is_prose(&language) {
        true => 2,
        false => 1,
    };
    let before = line_prefix(&tab.buffer, &caret(&tab.buffer));
    match rank::opens(&before, &triggers, min_word) {
        // This keystroke typed a trigger that more typing follows by now, the main loop having
        // been busy: an answer up was asked before it, so ask afresh where the caret is.
        Some(None) if typed.is_some_and(|c| triggers.contains(&c)) => session.ask(None),
        Some(trigger) => session.open(trigger),
        None => {}
    }
}

/// A press offered to the popup first: see [`Session::press`].
pub(crate) fn press(
    tab: &Tab,
    key: gdk::Key,
    state: gdk::ModifierType,
) -> Option<glib::Propagation> {
    session(tab)?.press(key, state)
}

/// Whether `tab`'s popup is on screen.
pub(crate) fn is_shown(tab: &Tab) -> bool {
    session(tab).is_some_and(|s| s.is_shown())
}

/// Fixed-answer source for staging drill popups without a server.
#[cfg(feature = "bench")]
pub(crate) struct Fixed(pub Vec<Completion>);

#[cfg(feature = "bench")]
impl Source for Fixed {
    fn fetch(&self, _: Pos, _: Option<char>) -> Reply<Completions> {
        let items = self.0.clone();
        Box::pin(async move {
            Completions {
                items,
                ..Completions::default()
            }
        })
    }

    fn resolve(&self, item: Completion) -> Reply<Completion> {
        Box::pin(async move { item })
    }
}
