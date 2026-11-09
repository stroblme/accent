//! Text completion through one [`Source`] interface.
//!
//! A [`Session`] holds an answer and its popup. [`Anchor`] marks track the fragment it completes,
//! which the caret may move about in. [`rank`] narrows complete answers locally by what of it
//! lies before the caret;
//! incomplete answers request updates per edit while the current list narrows. [`apply::Moved`]
//! tracks each item's range through edits.
//!
//! The view retains focus. `editor::keys` offers arrows, Escape, and Return or Tab with a
//! selected row to [`press`] first. Other keys continue normal typing or accept a selected
//! row through its commit character.

mod abbrev;
mod apply;
mod popup;
mod rank;

pub(crate) use popup::{CONTENTS_PADDING, ROW_PADDING, ROW_PADDING_Y, ROW_TEXT_MIN};

use std::cell::{Cell, RefCell};
use std::future::Future;
use std::pin::Pin;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::Duration;

use accent_api::{Completion, Completions, Kind, Pos, Range, TextEdit, Vault};
use accent_core::fuzzy::Corpus;
use gtk::prelude::*;
use gtk::{gdk, glib};
use sourceview5::prelude::*;

use crate::editor::{Tab, caret, line_end, line_prefix};
use crate::lang::Editor;
use crate::{hover, lang, multicaret};

/// Maximum displayed rows, bounding per-keystroke splicing of servers'
/// large answers to a practical scrollable list.
const MOST: usize = 200;

/// Selection-rest delay before resolving docs, avoiding server requests
/// for every row crossed by arrow keys.
const LOOK: Duration = Duration::from_millis(50);

/// How often a late import waiting for a snippet's walk to end looks again.
const WALK: Duration = Duration::from_millis(100);

/// A pinned future for completion replies.
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
    /// A snippet's stops are being walked.
    fn walking(&self) -> bool {
        false
    }
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

/// The fragment an answer completes ([`Completions::fragment`]), followed through the edits
/// since by a left-gravity mark where it starts and a right-gravity one where it ends, so typing
/// at either end joins it. Asked for, it is the caret alone until the answer lands ([`fit`]).
///
/// [`fit`]: Anchor::fit
struct Anchor {
    /// Where it was when asked, in the coordinates of the answer's ranges.
    asked: Range,
    from: gtk::TextMark,
    to: gtk::TextMark,
}

impl Anchor {
    fn new(buffer: &gtk::TextBuffer, caret: &gtk::TextIter) -> Anchor {
        let at = lang::pos_of(caret);
        Anchor {
            asked: Range { start: at, end: at },
            from: buffer.create_mark(None, caret, true),
            to: buffer.create_mark(None, caret, false),
        }
    }

    /// Take the fragment `answer` completes ([`apply::fragment`]), moved on by what was typed at
    /// the caret while it was on its way.
    fn fit(&mut self, buffer: &gtk::TextBuffer, answer: &Completions) {
        let Some(moved) = self.moved(buffer) else {
            return;
        };
        let fragment = apply::fragment(answer, self.asked.end);
        buffer.move_mark(&self.from, &lang::iter_at(buffer, fragment.start));
        buffer.move_mark(&self.to, &lang::iter_at(buffer, moved.end(fragment.end)));
        self.asked = fragment;
    }

    /// Map the edits since, or `None` once the text before the fragment has changed or its end
    /// has left its line.
    fn moved(&self, buffer: &gtk::TextBuffer) -> Option<apply::Moved> {
        let (from, to) = (self.from(buffer), self.to(buffer));
        let asked = self.asked.end.character as i32 - self.asked.start.character as i32;
        let still = lang::pos_of(&from) == self.asked.start && to.line() == from.line();
        still.then(|| apply::Moved {
            at: self.asked.end,
            typed: to.line_offset() - from.line_offset() - asked,
        })
    }

    /// Whether `caret` is in the fragment, at either end of it included.
    fn holds(&self, buffer: &gtk::TextBuffer, caret: &gtk::TextIter) -> bool {
        let from = self.from(buffer);
        caret.line() == from.line() && from <= *caret && *caret <= self.to(buffer)
    }

    fn from(&self, buffer: &gtk::TextBuffer) -> gtk::TextIter {
        buffer.iter_at_mark(&self.from)
    }

    /// Where the fragment ends now.
    fn to(&self, buffer: &gtk::TextBuffer) -> gtk::TextIter {
        buffer.iter_at_mark(&self.to)
    }
}

impl Drop for Anchor {
    fn drop(&mut self) {
        if let Some(buffer) = self.from.buffer() {
            buffer.delete_mark(&self.from);
            buffer.delete_mark(&self.to);
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
    /// In-flight request. Replacement aborts it, cancelling the server request.
    fetch: RefCell<Option<glib::JoinHandle<()>>>,
    /// The buffer's edits so far, and how many there had been at the last ask: a move alone
    /// asks nothing again.
    edits: Cell<u64>,
    asked: Cell<Option<u64>>,
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
                edits: Cell::new(0),
                asked: Cell::new(None),
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
        view.buffer().connect_changed(glib::clone!(
            #[weak(rename_to = me)]
            self,
            move |_| me.edits.set(me.edits.get() + 1)
        ));
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
        for vertical in [true, false] {
            let me = Rc::downgrade(self);
            crate::widgets::follow_adjustment(view, vertical, move |adjustment| {
                let me = me.clone();
                vec![adjustment.connect_value_changed(move |_| {
                    if let Some(me) = me.upgrade() {
                        me.follow();
                    }
                })]
            });
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
        self.asked.set(Some(self.edits.get()));
        let fetch = self.source.fetch(anchor.asked.end, trigger);
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

    fn land(self: &Rc<Self>, generation: u64, mut anchor: Anchor, mut answer: Completions) {
        if generation != self.generation.get() {
            return;
        }
        let buffer = self.view.buffer();
        // Typed past while it was on its way: the provider read text holding some of that typing,
        // how much is not known, so what it says lies past the ask is cut back to it, and asked
        // again below.
        let typed_past = anchor.moved(&buffer).is_some_and(|moved| moved.typed != 0);
        if typed_past {
            apply::cut_at(&mut answer, anchor.asked.end);
        }
        anchor.fit(&buffer, &answer);
        let incomplete = answer.incomplete;
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
        // A capped answer narrowed to the text now misses what the provider ranks first for it,
        // and one cut back replaces less than it should: ask again, the narrowed rows up meanwhile.
        let again = incomplete || typed_past;
        if again && self.shown.borrow().is_some() && self.typed_since_asked() {
            self.ask(None);
        }
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
        let short = self.shown.borrow().as_ref().is_some_and(|s| s.incomplete);
        if short && self.typed_since_asked() {
            self.ask(None);
        }
        let selected = self.selected_label();
        self.refresh(selected);
    }

    /// Whether the text has changed since the last ask.
    fn typed_since_asked(&self) -> bool {
        self.asked.get() != Some(self.edits.get())
    }

    /// Filter the answer by its fragment up to the caret as it now reads, preserving `selected`,
    /// or end completion once the caret has left the fragment.
    fn refresh(&self, selected: Option<String>) {
        let buffer = self.view.buffer();
        let caret = caret(&buffer);
        let mut guard = self.shown.borrow_mut();
        let Some(shown) = guard.as_mut() else {
            return;
        };
        let moved = shown.anchor.moved(&buffer);
        let Some(moved) = moved.filter(|_| shown.anchor.holds(&buffer, &caret)) else {
            drop(guard);
            return self.close();
        };
        let line: Vec<char> = line_prefix(&buffer, &caret).chars().collect();
        let to = lang::pos_of(&caret);
        // What is typed from each distinct start to the caret, read once per start: an answer's
        // items mostly share one.
        let mut typed: Vec<(Pos, String)> = Vec::new();
        let starts: Vec<Pos> = shown
            .items
            .iter()
            .map(|item| moved.start(item.completion.replace.start))
            .collect();
        for start in &starts {
            if !typed.iter().any(|(s, _)| s == start) {
                typed.push((*start, typed_since(&buffer, &line, *start, to)));
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
                let moved = s.anchor.moved(&buffer)?;
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
        self.asked.set(None);
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
        let accepted = {
            let shown = self.shown.borrow();
            shown.as_ref().and_then(|shown| {
                let item = &shown.items[*shown.rows.get(row as usize)?].completion;
                let moved = shown.anchor.moved(&buffer)?;
                let to = lang::pos_of(&shown.anchor.to(&buffer));
                let range = apply::replaced(item.replace, moved, to);
                let extras = item.extra_edits.iter().map(|e| moved.edit(e)).collect();
                Some((item.clone(), range, extras))
            })
        };
        self.close();
        accepted
    }

    /// Insert the item at `row` now, as it stands, and end the session: accepting waits on no
    /// server. A row whose resolve has not answered yet is resolved after, and what that adds,
    /// an import, goes in once it lands and any snippet walk is over, as its own undo step,
    /// unless the text before it has changed meanwhile ([`apply::still_fits`]): VS Code's rule.
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
            // GtkSourceView takes any insert during a snippet's walk for typing at the caret and
            // collapses the stop's placeholder there, so it would no longer be typed over: what
            // landed waits for the walk to end.
            while me.upgrade().is_some_and(|me| me.source.walking()) {
                glib::timeout_future(WALK).await;
            }
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

/// Read the text from `start` to `to`, the caret, from `line`, its line up to there, when both
/// positions share a line.
fn typed_since(buffer: &gtk::TextBuffer, line: &[char], start: Pos, to: Pos) -> String {
    if start.line == to.line {
        let from = (start.character as usize).min(line.len());
        return line[from..].iter().collect();
    }
    let from = lang::iter_at(buffer, start);
    let to = lang::iter_at(buffer, to);
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

// --------------------------------------------------------------------------------- an editor's

/// An editor's document, asked through its vault at the editor's place in it: a text tab's whole
/// file, a notebook cell's lines of the notebook's ([`Editor::to_document`]).
///
/// Weak: the session is the editor's, and the editor holds the view the session's handlers hang
/// off.
pub(crate) struct EditorSource(pub(crate) Weak<dyn Editor>);

impl EditorSource {
    /// The editor, with the vault its document is open on.
    fn asked(&self) -> Option<(Rc<dyn Editor>, Arc<Vault>)> {
        let editor = self.0.upgrade()?;
        let vault = editor.document().vault()?;
        Some((editor, vault))
    }
}

impl Source for EditorSource {
    fn fetch(&self, at: Pos, trigger: Option<char>) -> Reply<Completions> {
        let editor = self.0.upgrade();
        if let Some(answer) = editor.as_deref().and_then(|e| abbreviations(e, at)) {
            return Box::pin(async move { answer });
        }
        let asked = self.asked();
        Box::pin(async move {
            let Some((editor, vault)) = asked else {
                return Completions::default();
            };
            editor.clone().flush().await;
            let (key, Some(pos)) = (editor.document().key(), editor.to_document(at)) else {
                return Completions::default();
            };
            match vault.completion(&key, pos, trigger).await {
                Ok(answer) => {
                    tracing::debug!(
                        "completion for {key} at {pos:?}: {} items{}",
                        answer.items.len(),
                        if answer.incomplete {
                            ", more to ask"
                        } else {
                            ""
                        }
                    );
                    in_view(answer, |at| editor.to_view(at))
                }
                Err(e) => {
                    tracing::debug!("completion for {key}: {e:#}");
                    Completions::default()
                }
            }
        })
    }

    fn resolve(&self, item: Completion) -> Reply<Completion> {
        let asked = self.asked();
        Box::pin(async move {
            let Some((editor, vault)) = asked else {
                return item;
            };
            editor.clone().flush().await;
            let Some(out) = moved(item.clone(), |at| editor.to_document(at)) else {
                return item;
            };
            match vault
                .resolve_completion(&editor.document().key(), out)
                .await
            {
                Ok(full) => moved(full, |at| editor.to_view(at)).unwrap_or(item),
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
        let Some(editor) = self.0.upgrade() else {
            return;
        };
        match up {
            true => crate::ghost::clear(editor.as_ref()),
            false => {
                glib::spawn_future_local(async move {
                    editor.clone().flush().await;
                    crate::ghost::request(editor.as_ref()).await;
                });
            }
        }
    }

    fn walking(&self) -> bool {
        self.0.upgrade().is_some_and(|e| e.snippet_active())
    }

    fn push_snippet(
        &self,
        view: &sourceview5::View,
        snippet: &sourceview5::Snippet,
        at: &mut gtk::TextIter,
    ) {
        match self.0.upgrade() {
            Some(editor) => editor.push_snippet(snippet, at),
            None => view.push_snippet(snippet, Some(at)),
        }
    }
}

/// `answer`, given in the document's positions, in the view's (`to`): the rows and the fragment
/// that are not all in the view left out.
fn in_view(mut answer: Completions, to: impl Fn(Pos) -> Option<Pos>) -> Completions {
    answer.items = answer
        .items
        .into_iter()
        .filter_map(|item| moved(item, &to))
        .collect();
    answer.fragment = answer.fragment.and_then(|f| moved_range(f, &to));
    answer
}

/// `item` with its ranges moved by `to`, between the view's positions and its document's: `None`
/// when what it replaces does not move, and an edit it makes that does not, an import at the top
/// of another notebook cell, left out (user decision 2026-10-20).
fn moved(mut item: Completion, to: impl Fn(Pos) -> Option<Pos>) -> Option<Completion> {
    item.replace = moved_range(item.replace, &to)?;
    item.extra_edits
        .retain_mut(|edit| match moved_range(edit.range, &to) {
            Some(range) => {
                edit.range = range;
                true
            }
            None => false,
        });
    Some(item)
}

fn moved_range(range: Range, to: impl Fn(Pos) -> Option<Pos>) -> Option<Range> {
    Some(Range {
        start: to(range.start)?,
        end: to(range.end)?,
    })
}

/// Complete Lean backslash abbreviations locally ([`abbrev`]); the server
/// has no answer for this input prefix. The name runs on past the caret to its end, matched up
/// to the caret and replaced all of it, the backslash opening it being no part of the fragment.
fn abbreviations(editor: &dyn Editor, at: Pos) -> Option<Completions> {
    if editor.language_id() != abbrev::LANGUAGE {
        return None;
    }
    let buffer = editor.view().buffer();
    let caret = lang::iter_at(&buffer, at);
    let before = line_prefix(&buffer, &caret);
    let typed = abbrev::typed(&before)?;
    let after = buffer.text(&caret, &line_end(&buffer, caret.line()), true);
    let rest = abbrev::rest(&after);
    let at_char = |character| Pos {
        line: at.line,
        character,
    };
    let start = at.character - typed.chars().count() as u32;
    let end = at_char(at.character + rest.chars().count() as u32);
    Some(Completions {
        fragment: Some(Range {
            start: at_char(start + 1),
            end,
        }),
        ..abbrev::rows(
            typed,
            Range {
                start: at_char(start),
                end,
            },
        )
    })
}

/// Complete in `editor` through `source`, opening on what is typed there ([`typed`]), once there
/// is a vault to ask. The caller keeps the session, which [`Editor::completion`] answers.
pub(crate) fn install(editor: &Rc<dyn Editor>, source: Box<dyn Source>) -> Rc<Session> {
    let made = Session::new(editor.view(), source);
    // What was typed is looked at once it has landed, one idle on: a single character, or the
    // pair `typing` writes for a bracket, at the caret, its first character being the one typed.
    // A paste, a template, the text of a reload and the session's own insert open nothing.
    // Weak: the editor holds the view the handler hangs off.
    let weak = Rc::downgrade(editor);
    editor
        .view()
        .buffer()
        .connect_insert_text(move |buffer, at, text| {
            let Some(editor) = weak.upgrade() else {
                return;
            };
            let quiet = editor.is_loading()
                || text.chars().count() > 2
                || at.offset() != caret(buffer).offset()
                || editor.completion().is_none_or(|s| s.muted.get());
            if !quiet {
                let first = text.chars().next();
                glib::idle_add_local_once(move || typed(editor.as_ref(), first));
            }
        });
    // A character deleted at the caret, by Backspace or Delete, is looked at the same way.
    let weak = Rc::downgrade(editor);
    editor
        .view()
        .buffer()
        .connect_delete_range(move |buffer, start, end| {
            let Some(editor) = weak.upgrade() else {
                return;
            };
            let caret = caret(buffer).offset();
            let quiet = editor.is_loading()
                || end.offset() - start.offset() != 1
                || (start.offset() != caret && end.offset() != caret)
                || editor.completion().is_none_or(|s| s.muted.get());
            if !quiet {
                glib::idle_add_local_once(move || typed(editor.as_ref(), None));
            }
        });
    made
}

/// `tab`'s session, once [`install`] gave it one.
pub(crate) fn session(tab: &Tab) -> Option<Rc<Session>> {
    tab.lang.completion.borrow().clone()
}

/// Handle typed completion triggers ([`rank::opens`]) only while focused
/// and without a caret column, matching ghost-text eligibility. `typed` is
/// the character the keystroke typed, `None` for one it deleted, which opens the popup only
/// where a trigger leads the caret: `[[AB` of `[[ABC]]` once its `C` is gone.
fn typed(editor: &dyn Editor, typed: Option<char>) {
    let Some(session) = editor.completion() else {
        return;
    };
    let view = editor.view();
    let column = view.downcast_ref::<multicaret::View>();
    if !view.is_focus() || column.is_some_and(|view| view.has_carets()) {
        return;
    }
    let language = editor.language_id();
    let mut triggers = editor
        .document()
        .support()
        .map(|s| s.completion_triggers.clone())
        .unwrap_or_default();
    // A Lean abbreviation opens on its backslash, which the server does not ask for.
    if language == abbrev::LANGUAGE {
        triggers.push('\\');
    }
    // A word shortened is no reason to open, unless a trigger leads it.
    let min_word = match (typed, accent_api::language::is_prose(&language)) {
        (None, _) => usize::MAX,
        (Some(_), true) => 2,
        (Some(_), false) => 1,
    };
    let buffer = view.buffer();
    let before = line_prefix(&buffer, &caret(&buffer));
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
    editor: &dyn Editor,
    key: gdk::Key,
    state: gdk::ModifierType,
) -> Option<glib::Propagation> {
    editor.completion()?.press(key, state)
}

/// Whether `editor`'s popup is on screen.
pub(crate) fn is_shown(editor: &dyn Editor) -> bool {
    editor.completion().is_some_and(|s| s.is_shown())
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

#[cfg(test)]
mod tests {
    use super::*;

    fn range(line: u32, from: u32, to_line: u32, to: u32) -> Range {
        Range {
            start: Pos {
                line,
                character: from,
            },
            end: Pos {
                line: to_line,
                character: to,
            },
        }
    }

    /// A notebook cell's view over line 2 of its document, as `lang::Editor::to_view` maps it.
    fn cell_view(at: Pos) -> Option<Pos> {
        (at.line == 2).then_some(Pos { line: 0, ..at })
    }

    #[test]
    fn an_answer_is_the_views_and_edits_outside_it_go() {
        let item = |replace, extra: Vec<TextEdit>| Completion {
            label: "alpha".to_string(),
            kind: Kind::Function,
            detail: None,
            doc: None,
            filter: None,
            insert: "alpha".to_string(),
            is_snippet: false,
            replace,
            extra_edits: extra,
            resolve: None,
            preselect: false,
            commit: Vec::new(),
        };
        let edit = |line, text: &str| TextEdit {
            range: range(line, 0, line, 0),
            text: text.to_string(),
        };
        let extra = vec![edit(0, "import alpha\n"), edit(2, "alpha.")];
        let answer = in_view(
            Completions {
                items: vec![
                    item(range(2, 8, 2, 12), extra),
                    item(range(1, 0, 2, 1), vec![]),
                ],
                incomplete: true,
                fragment: Some(range(2, 8, 2, 12)),
            },
            cell_view,
        );
        assert_eq!(answer.items.len(), 1, "a row reaching out of the view goes");
        let kept = &answer.items[0];
        assert_eq!(kept.replace, range(0, 8, 0, 12));
        assert_eq!(
            kept.extra_edits,
            [edit(0, "alpha.")],
            "the import into another cell is gone"
        );
        assert_eq!(answer.fragment, Some(range(0, 8, 0, 12)));
        // Back into the document, as a resolve asks about it.
        let down = |at: Pos| Some(Pos { line: 2, ..at });
        let back = moved(kept.clone(), down).unwrap();
        assert_eq!(back.replace, range(2, 8, 2, 12));
    }
}
