//! The completion popup, for every flavour of text tab: accent's own list over the text.
//!
//! One provider whatever the language — a note's `[[links]]` and `#tags` from the index, a C
//! file's members from clangd, a word from the dictionary — through one call and in one shape
//! ([`Source`]), so nothing here knows which it is looking at.
//!
//! A [`Session`] is an answer and the popup showing it. The answer is asked for at the caret, with
//! two marks left there ([`Anchor`]), and what is typed after that narrows it here ([`rank`])
//! rather than at the provider: a server ranks once, for where it was asked. Unless it said it
//! stopped short (`incomplete`); then every keystroke asks again, and the list on screen is
//! narrowed meanwhile, so nothing flickers over a slow link. Each item is matched against what was
//! typed since its own start, and accepted over its own range moved on by the typing
//! ([`apply::Moved`]).
//!
//! The popup never has the keyboard: the view keeps it, and `editor::keys` offers each press to
//! [`press`] first, which takes the arrows, Escape, and Return and Tab while a row is selected.
//! Everything else is typed into the text as ever, list helpers and paired brackets included,
//! and narrows the list on the way.

mod apply;
mod popup;
mod rank;

pub(crate) use popup::{CONTENTS_PADDING, ROW_PADDING, ROW_PADDING_Y, ROW_TEXT_MIN};

use std::cell::{Cell, RefCell};
use std::future::Future;
use std::pin::Pin;
use std::rc::{Rc, Weak};
use std::time::Duration;

use accent_api::{Completion, Completions, Kind, Pos};
use accent_core::fuzzy::Corpus;
use gtk::prelude::*;
use gtk::{gdk, glib};
use sourceview5::prelude::*;

use crate::editor::{Tab, caret, line_prefix};
use crate::{hover, lang, multicaret};

/// How many rows an answer puts in the list at most. A server can answer with thousands; this
/// many is more than anyone scrolls through, and splicing them all in on every keystroke is not.
const MOST: usize = 200;

/// How long a row has to stay selected before its documentation is asked for, so arrowing down a
/// list asks the server about the row it stops at rather than every row it passes.
const LOOK: Duration = Duration::from_millis(50);

/// A pinned future, which is what every reply here is.
pub(crate) type Reply<T> = Pin<Box<dyn Future<Output = T>>>;

/// Where a session's answers come from, and what it asks of the document it completes in.
pub(crate) trait Source {
    /// The answer at `at`, `trigger` being the character just typed when it is one the provider
    /// asked to be told about. A provider that cannot answer answers nothing.
    fn fetch(&self, at: Pos, trigger: Option<char>) -> Reply<Completions>;
    /// `item` with what the provider keeps back until a row is looked at: its documentation.
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

/// Where an answer was asked for: two marks at the caret then, the left one staying before what
/// is typed there and the right one going after it, so the distance between them is what was
/// typed since. Removed from the buffer with the answer.
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

    /// How the text moved since the ask, or `None` when it moved in a way the answer cannot
    /// follow: the caret off the line or back before where it was asked, or the text there
    /// deleted from under the marks.
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
    /// Bumped by every ask: an answer to an earlier one, or a row's resolve from it, is dropped.
    generation: Cell<u64>,
    /// The ask in flight, and where it was made. Replacing it aborts it, which cancels the
    /// request at the server.
    fetch: RefCell<Option<glib::JoinHandle<()>>>,
    asked_at: Cell<Option<Pos>>,
    /// The selected row's documentation being asked for.
    detail: RefCell<Option<glib::JoinHandle<()>>>,
    /// The session is editing the buffer itself, which opens nothing.
    muted: Cell<bool>,
    /// The input method holds text not committed yet: the keys are its own.
    preedit: Cell<bool>,
    /// A look at the caret is queued for the next idle: a keystroke edits the buffer and moves
    /// the caret, and one look covers both.
    queued: Cell<bool>,
}

impl Session {
    /// A session over `view`, asking `source`. Hides itself when the view goes off screen, loses
    /// the keyboard, or starts a column of carets; `Ctrl+Space` asks at the caret.
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
                preedit: Cell::new(false),
                queued: Cell::new(false),
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

    /// Something typed opens the popup (`rank::opens`): a trigger asks afresh, and a word asks
    /// unless there is an answer up to narrow already.
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

    /// Narrow the answer up to what was typed and show it, keeping `selected` selected; or end
    /// the session where the caret has left what it completes.
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
        let keep = selected.and_then(|label| {
            shown
                .rows
                .iter()
                .position(|&i| shown.items[i].completion.label == label)
        });
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

    /// What a key press does with the popup up, or `None` for a key that goes on to the rest of
    /// the editor (`editor::keys`). Under a column of carets everything the popup does not take
    /// goes to GTK at the primary caret, which ends the column, as accepting a row does.
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
        match rank::step(key, state, self.popup.selected(), self.popup.len()) {
            rank::Step::Select(row) => {
                self.popup.select(row);
                Some(glib::Propagation::Stop)
            }
            rank::Step::Accept(row) => {
                self.accept(row);
                Some(glib::Propagation::Stop)
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

    /// Write the item at `row` into the text, and end the session.
    fn accept(&self, row: u32) {
        let buffer = self.view.buffer();
        let caret = caret(&buffer);
        let accepted = {
            let shown = self.shown.borrow();
            shown.as_ref().and_then(|shown| {
                let item = &shown.items[*shown.rows.get(row as usize)?].completion;
                let moved = shown.anchor.moved(&buffer, &caret)?;
                let range = apply::replaced(item.replace, moved, lang::pos_of(&caret));
                Some((item.clone(), range))
            })
        };
        self.close();
        let Some((item, range)) = accepted else {
            return;
        };
        self.muted.set(true);
        apply::apply(&buffer, &item, range, |snippet, at| {
            self.source.push_snippet(&self.view, snippet, at)
        });
        self.muted.set(false);
    }

    /// The selection moved: the selected row's documentation in the pane beside the list, asked
    /// for once it has been looked at for [`LOOK`] where the provider keeps it back.
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

    /// A row's documentation arrived: kept with the row, and shown while it is still selected.
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
}

/// What was typed between `start` and the caret at `here`: read off `line`, the caret's line up to
/// it, where the two share it, which they nearly always do.
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

    /// The suggestion goes while the popup is up; the answers it was in the way of are asked
    /// for again once it goes, through a flush, the popup having held them back meanwhile.
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

/// Give a tab its popup, asking its vault. Called by [`lang::attach`], which is what decides
/// that this tab has a vault to ask at all.
pub fn install(tab: &Rc<Tab>) {
    let made = Session::new(&tab.view, Box::new(TabSource(Rc::downgrade(tab))));
    // What was typed is looked at once it has landed, one idle on: a single character, or the
    // pair `typing` writes for a bracket, at the caret. A paste, a template, the text of a reload
    // and the session's own insert open nothing.
    tab.buffer.connect_insert_text(glib::clone!(
        #[weak]
        tab,
        move |buffer, at, text| {
            let quiet = tab.is_loading()
                || text.chars().count() > 2
                || at.offset() != caret(buffer).offset()
                || session(&tab).is_none_or(|s| s.muted.get());
            if !quiet {
                glib::idle_add_local_once(move || typed(&tab));
            }
        }
    ));
    *tab.lang.completion.borrow_mut() = Some(made);
}

/// `tab`'s session, once [`install`] gave it one.
pub(crate) fn session(tab: &Tab) -> Option<Rc<Session>> {
    tab.lang.completion.borrow().clone()
}

/// Something was typed: open the popup where it asks for one (`rank::opens`). Never unasked
/// with the keyboard elsewhere or a column of carets up, as there is no ghost text then either.
fn typed(tab: &Rc<Tab>) {
    let Some(session) = session(tab) else {
        return;
    };
    if !tab.view.is_focus() || tab.ghost_view().is_some_and(|view| view.has_carets()) {
        return;
    }
    let triggers = tab
        .lang
        .support()
        .map(|s| s.completion_triggers.clone())
        .unwrap_or_default();
    let min_word = match accent_api::language::is_prose(&lang::language_id(tab)) {
        true => 2,
        false => 1,
    };
    let before = line_prefix(&tab.buffer, &caret(&tab.buffer));
    if let Some(trigger) = rank::opens(&before, &triggers, min_word) {
        session.open(trigger);
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

/// A source answering with the same items wherever it is asked: a drill's, which needs a popup
/// and no server.
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
