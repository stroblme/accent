//! The bar along the bottom of the editor column.
//!
//! It says what is happening (indexing, opening), what the file is, how long it is and how far
//! it is zoomed, the readout being the control that puts the zoom back. All of
//! that used to be spread across the header bar, where it competed with the vault name and the
//! note path; a document's own facts belong under it, not beside its title. The branch is the
//! repository the document sits in, which is not always the vault's own.
//!
//! The whole bar fades with the chrome, the unsaved dot included: it is a footer of facts about a
//! document nobody is looking at while they are writing into it, and an exception would be one
//! thing left lit under a window that is otherwise out of the way.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;

/// What the vault's line says while it is paused. One string, because [`crate::App::sync_opening`]
/// borrows the slot for a PDF and has to put this back: no further progress will.
pub const PAUSED: &str = "Indexing paused";

/// What the vault's own indexing is doing, which is what the control beside the line offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Indexing {
    /// A walk is running: Stop.
    Running,
    /// A walk was stopped and the index is partial: Resume.
    Paused,
    /// Nothing to offer.
    Idle,
}

/// The bar itself. Every label hides when it has nothing to say, so an empty bar is an empty
/// line rather than a row of dashes.
pub struct Bar {
    row: gtk::Box,
    progress: gtk::Label,
    /// What the writers of the [`Bar::progress`] label have each said, so none erases another:
    /// the vault's own work, the copies to and from a host, and a background job in a language
    /// provider. The copies are a list because two can overlap, and the first to finish must not
    /// clear the other.
    vault_busy: RefCell<Option<String>>,
    transfers: RefCell<Vec<String>>,
    provider_busy: RefCell<Option<String>>,
    /// Stop while the vault is indexing, Resume while it is paused, nothing otherwise. It sits
    /// beside [`Bar::progress`] and belongs to the vault's line alone: a transfer and the
    /// suggestion index, which share that slot, have nothing to stop.
    index: gtk::Button,
    index_label: gtk::Label,
    indexing: Cell<Indexing>,
    /// The branch readout, which is also the Sync control.
    branch: gtk::Button,
    branch_label: gtk::Label,
    kind: gtk::Label,
    /// The dot a dirty tab wears, so one symbol means "unsaved" wherever it appears.
    unsaved: gtk::Label,
    /// The document's own count, and the button around it. A code tab's diagnostic count is also
    /// the switch that keeps them out of the text, and a PDF's page count opens the page commands,
    /// so the readout is a control there and plain text everywhere else — a word count has nothing
    /// to press.
    words: gtk::Label,
    facts: gtk::Button,
    /// The zoom readout, which is also the control that resets it.
    zoom: gtk::Button,
    zoom_label: gtk::Label,
    /// How much is selected in a text tab, while anything is.
    selected: gtk::Label,
}

impl Bar {
    pub fn new() -> Bar {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        row.add_css_class("accent-flat");
        // The bar goes with the rest of the chrome while the user types (DESIGN.md).
        row.add_css_class("chrome-fade");
        row.add_css_class("accent-statusbar");

        let progress = label(false);
        // The control for the line beside it. No action name: nothing else offers a Stop, so it
        // would be an action for one button, and the window wires it in `wire_window`.
        let index_label = label(false);
        let index = bar_button(&index_label, None, "");
        // The branch is the Sync control as well as the readout: it names the repository the
        // document sits in, and clicking it pulls and pushes that one (DESIGN.md, Layout map).
        let branch_label = label(true);
        let branch = bar_button(&branch_label, Some("win.git-sync"), "Sync");
        let kind = label(false);
        // Between what the file is and how long it is, so the right-hand group still reads left
        // to right: Markdown, unsaved, 12 words. Its own label rather than a prefix on the kind,
        // because the two facts change for different reasons.
        let unsaved = label(false);
        unsaved.set_label("•");
        unsaved.set_tooltip_text(Some("Unsaved changes"));
        let words = label(true);
        let facts = bar_button(&words, None, "");

        // The readout is the reset control: clicking it is Ctrl+0, which is 100 % for a document
        // and Fit Height for a PDF.
        let zoom_label = label(true);
        let zoom = bar_button(&zoom_label, Some("win.zoom-reset"), "Reset Zoom");
        let selected = label(true);

        row.append(&progress);
        row.append(&index);
        row.append(&branch);
        // The file's own facts sit at the far end, away from what the window is busy with.
        kind.set_hexpand(true);
        kind.set_halign(gtk::Align::End);
        row.append(&kind);
        row.append(&unsaved);
        row.append(&facts);
        row.append(&zoom);
        // Last, at the corner: it comes and goes with the selection, and moves nothing as it does.
        row.append(&selected);

        Bar {
            row,
            progress,
            vault_busy: RefCell::new(None),
            transfers: RefCell::new(Vec::new()),
            provider_busy: RefCell::new(None),
            index,
            index_label,
            indexing: Cell::new(Indexing::Idle),
            branch,
            branch_label,
            kind,
            unsaved,
            words,
            facts,
            zoom,
            zoom_label,
            selected,
        }
    }

    pub fn widget(&self) -> &gtk::Widget {
        self.row.upcast_ref()
    }

    /// What the window is busy with: "Indexing… 1200/42700 files", "Opening the document…".
    pub fn set_progress(&self, text: Option<&str>) {
        *self.vault_busy.borrow_mut() = text.map(str::to_string);
        self.show_busy();
    }

    /// What the vault's own indexing is doing, and so what the control beside its line offers.
    /// Separate from [`set_progress`], because that slot is also where a transfer and the
    /// suggestion index write and neither of those can be stopped.
    ///
    /// [`set_progress`]: Self::set_progress
    pub fn set_indexing(&self, state: Indexing) {
        self.indexing.set(state);
        let (label, tooltip) = match state {
            Indexing::Running => ("Stop", "Stop indexing; what is indexed so far is kept"),
            Indexing::Paused => ("Resume", "Finish indexing this vault"),
            Indexing::Idle => ("", ""),
        };
        set(
            &self.index_label,
            (state != Indexing::Idle).then_some(label),
        );
        self.index.set_tooltip_text(Some(tooltip));
        self.index.set_visible(state != Indexing::Idle);
    }

    pub fn indexing(&self) -> Indexing {
        self.indexing.get()
    }

    /// The control itself, which the window hangs its click on.
    pub fn index_control(&self) -> &gtk::Button {
        &self.index
    }

    /// A language provider is busy with something worth waiting for, named: "suggestions" while
    /// the ghost-text index is rebuilt. Nothing is shown when it is idle.
    pub fn set_provider_busy(&self, what: Option<&str>) {
        *self.provider_busy.borrow_mut() = what.map(|w| format!("Indexing {w}…"));
        self.show_busy();
    }

    /// A copy to or from the host has started (`running`) or ended: "Downloading a.pdf…". A file's
    /// size is not asked for, so one file says that it runs rather than how far it has got; a
    /// batch counts its files ([`Bar::retell_transfer`]). A close waiting for git says so here too.
    pub fn set_transfer(&self, text: &str, running: bool) {
        {
            let mut transfers = self.transfers.borrow_mut();
            match running {
                true => transfers.push(text.to_string()),
                false => {
                    if let Some(at) = transfers.iter().position(|t| t == text) {
                        transfers.remove(at);
                    }
                }
            }
        }
        self.show_busy();
    }

    /// A running copy's line says something new, in its place among the others: how many of its
    /// files a batch has sent ("Copying Photos… 12/120 files").
    pub fn retell_transfer(&self, from: &str, to: &str) {
        if let Some(line) = self.transfers.borrow_mut().iter_mut().find(|t| *t == from) {
            *line = to.to_string();
        }
        self.show_busy();
    }

    /// What the busy line says now, for the drills: nothing while it is hidden, which keeps the
    /// last text it had.
    #[cfg(feature = "bench")]
    pub fn progress_text(&self) -> String {
        match self.progress.is_visible() {
            true => self.progress.label().to_string(),
            false => String::new(),
        }
    }

    /// One line for all of them, and the vault's own work wins: opening a document or reading the
    /// vault is what the reader is waiting for. A copy they asked for comes next, the latest one
    /// still running, and a suggestion index last, being a convenience nobody asked about.
    fn show_busy(&self) {
        let vault = self.vault_busy.borrow();
        let transfers = self.transfers.borrow();
        let provider = self.provider_busy.borrow();
        let transfer = transfers.last().map(String::as_str);
        set(
            &self.progress,
            vault.as_deref().or(transfer).or(provider.as_deref()),
        );
    }

    /// The branch of the repository holding the active document, "• main ↑1 ↓2": the branch, how
    /// far it has drifted from its upstream, and — behind the same dot a dirty tab wears — whether
    /// it has anything uncommitted. Composed by [`crate::git::Panel::branch_label`], which is the
    /// only thing that knows any of it.
    pub fn set_branch(&self, branch: Option<&str>) {
        set(&self.branch_label, branch);
        self.branch.set_visible(branch.is_some());
    }

    /// Whether a sync is running. The branch greys out for the duration, a second click having
    /// nothing to start, and its tooltip says why; the spinner is the Git pane's, in place of its
    /// own Sync button.
    pub fn set_syncing(&self, on: bool) {
        self.branch.set_sensitive(!on);
        self.branch
            .set_tooltip_text(Some(if on { "Syncing…" } else { "Sync" }));
    }

    /// What the file is: "Markdown", "PDF", or "Rust · UTF-8 · LF" for code.
    pub fn set_kind(&self, text: Option<&str>) {
        set(&self.kind, text);
    }

    /// The document's own count, whatever it counts: a note's words, a code tab's errors and
    /// warnings, a PDF's page. One slot, because a tab has one such fact and it is the same
    /// corner of the bar.
    ///
    /// `press` is what pressing it does, said in the tooltip, or `None` for a count that is only
    /// a readout. Without one the slot keeps the pointer and the keyboard out rather than going
    /// insensitive: a dimmed count reads as a count that is out of date.
    pub fn set_facts(&self, text: Option<&str>, press: Option<&str>) {
        set(&self.words, text);
        self.facts.set_visible(text.is_some());
        self.facts.set_can_target(press.is_some());
        self.facts.set_can_focus(press.is_some());
        self.facts.set_tooltip_text(press);
    }

    /// The count itself, which the window hangs the diagnostics toggle and a PDF's page menu off.
    pub fn facts_control(&self) -> &gtk::Button {
        &self.facts
    }

    /// Whether the document has edits the disk does not. Nothing is shown when it is saved: a
    /// bar that says "Saved" all day is a bar nobody reads.
    pub fn set_unsaved(&self, unsaved: bool) {
        self.unsaved.set_visible(unsaved);
    }

    /// The zoom, while it is worth saying: "110 %", or a PDF's "Fit Width" / "Fit Height".
    pub fn set_zoom(&self, text: Option<&str>) {
        set(&self.zoom_label, text);
        self.zoom.set_visible(text.is_some());
    }

    /// What a selection in a text tab holds ([`characters_label`]), or `None` for none.
    pub fn set_selected(&self, text: Option<&str>) {
        set(&self.selected, text);
    }

    /// The zoom control itself, which the window hangs its Fit Width / Fit Height menu off.
    pub fn zoom(&self) -> &gtk::Widget {
        self.zoom.upcast_ref()
    }
}

/// A readout that is also a control: flat and label-only, so it reads as the rest of the bar
/// rather than as a button parked in it.
///
/// `.accent-bar-button` is what keeps the bar one height. A button's minimum is 24 px plus 5 px of
/// padding either side, and a box is as tall as its tallest child whatever its alignment, so one
/// of these appearing pushed the bar from 29 px to 46 px. The class pins it to the caption's own
/// line height; it stays a button, so it keeps its focus, its role and its tooltip.
///
/// `action` is `None` for a button the window connects by hand rather than through the action map.
fn bar_button(child: &impl IsA<gtk::Widget>, action: Option<&str>, tooltip: &str) -> gtk::Button {
    let button = gtk::Button::builder()
        .child(child)
        .tooltip_text(tooltip)
        .valign(gtk::Align::Center)
        .visible(false)
        .build();
    if let Some(action) = action {
        button.set_action_name(Some(action));
    }
    button.add_css_class("flat");
    button.add_css_class("accent-bar-button");
    button
}

fn label(numeric: bool) -> gtk::Label {
    let label = gtk::Label::new(None);
    label.add_css_class("caption");
    label.add_css_class("dim-label");
    if numeric {
        label.add_css_class("numeric");
    }
    label.set_visible(false);
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    label
}

fn set(label: &gtk::Label, text: Option<&str>) {
    match text {
        Some(text) => {
            label.set_label(text);
            label.set_visible(true);
        }
        None => label.set_visible(false),
    }
}

/// Words as a reader counts them, which is whitespace-separated runs. Markdown markers ride along
/// on the words they mark, so a heading's `#` is not a word of its own but `- item` counts the
/// dash; close enough for a readout that exists to show a draft growing.
pub fn word_count(text: &str) -> usize {
    text.split_whitespace().count()
}

pub fn words_label(count: usize) -> String {
    match count {
        1 => "1 word".to_string(),
        n => format!("{n} words"),
    }
}

pub fn characters_label(count: usize) -> String {
    match count {
        1 => "1 character".to_string(),
        n => format!("{n} characters"),
    }
}

/// The vault's line while a walk runs. The scan has no total until it ends (`total` 0), so until
/// then the line says how many files it has found.
pub fn indexing_label(done: usize, total: usize) -> String {
    match (done, total) {
        (1, 0) => "Indexing… 1 file found".to_string(),
        (found, 0) => format!("Indexing… {found} files found"),
        (done, total) => format!("Indexing… {done}/{total} files"),
    }
}

/// How often a running copy's line is brought up to date with how far it has got.
const TICK: Duration = Duration::from_millis(200);

/// How far one file's copy to or from a host has got, told by the worker moving it and read by
/// its line on the bar.
#[derive(Default)]
pub struct Bytes {
    done: AtomicU64,
    total: AtomicU64,
}

impl Bytes {
    /// What a transfer's progress callback hands on: the bytes moved so far, of how many.
    pub fn set(&self, done: u64, total: u64) {
        self.done.store(done, Ordering::Relaxed);
        self.total.store(total, Ordering::Relaxed);
    }

    /// `what` ("Downloading paper.pdf") and how far it has got, as the connection bar counts the
    /// server's upload: "Downloading paper.pdf… 12.3/80.0 MB". `None` until a byte has moved,
    /// so a copy that turns out to be current says nothing at all.
    pub fn line(&self, what: &str) -> Option<String> {
        let (done, total) = (
            self.done.load(Ordering::Relaxed),
            self.total.load(Ordering::Relaxed),
        );
        let mb = |bytes: u64| format!("{:.1}", bytes as f64 / (1024.0 * 1024.0));
        (total > 0).then(|| format!("{what}… {}/{} MB", mb(done), mb(total)))
    }
}

/// Wait for `work`, keeping its line on the bar to what `line` says every [`TICK`]: put up with
/// `say(text, true)` once there is one, changed in place with `retell`, and taken down with
/// `say(text, false)` when the work is over, which is also what ends the timer.
pub async fn counting<T>(
    say: impl Fn(&str, bool) + 'static,
    retell: impl Fn(&str, &str) + 'static,
    line: impl Fn() -> Option<String> + 'static,
    work: impl std::future::Future<Output = T>,
) -> T {
    let say = Rc::new(say);
    let shown = Rc::new(RefCell::new(line()));
    if let Some(text) = shown.borrow().as_deref() {
        say(text, true);
    }
    let tick = glib::timeout_add_local(TICK, {
        let (say, shown) = (say.clone(), shown.clone());
        move || {
            let next = line();
            let before = shown.borrow().clone();
            match (before, next) {
                (None, Some(next)) => {
                    say(&next, true);
                    *shown.borrow_mut() = Some(next);
                }
                (Some(before), Some(next)) if before != next => {
                    retell(&before, &next);
                    *shown.borrow_mut() = Some(next);
                }
                _ => {}
            }
            glib::ControlFlow::Continue
        }
    });
    let answer = work.await;
    tick.remove();
    if let Some(text) = shown.borrow().as_deref() {
        say(text, false);
    }
    answer
}

/// Where the reader is in a PDF, which is what that tab has to say where a note has its word
/// count. `page` is 0-based, the way the viewer counts them, and the readout is not.
///
/// `None` until the document is open: a PDF whose pages are not known yet would otherwise read
/// "Page 1 of 0" for as long as it takes to load.
pub fn page_label(page: usize, count: usize) -> Option<String> {
    (count > 0).then(|| format!("Page {} of {count}", page + 1))
}

/// The readout for a code tab: what the language is, then how the bytes are encoded and how the
/// lines end. Only code says the last two — a note is UTF-8 with LF endings or it would not be a
/// note, and a readout that never changes is chrome for nothing.
pub fn code_label(language: Option<&str>, encoding: &str) -> String {
    format!("{} · {encoding}", language.unwrap_or("Plain Text"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_count_splits_on_whitespace() {
        assert_eq!(word_count(""), 0);
        assert_eq!(word_count("   \n\t "), 0);
        assert_eq!(word_count("a  b\n\nc"), 3);
        assert_eq!(word_count("héllo wörld"), 2);
        // Markers count with the word they mark, except a list dash, which stands alone.
        assert_eq!(word_count("# Title\n- item"), 4);
    }

    #[test]
    fn words_label_says_one_word_in_the_singular() {
        assert_eq!(words_label(0), "0 words");
        assert_eq!(words_label(1), "1 word");
        assert_eq!(words_label(42), "42 words");
    }

    #[test]
    fn characters_label_says_one_character_in_the_singular() {
        assert_eq!(characters_label(1), "1 character");
        assert_eq!(characters_label(42), "42 characters");
    }

    #[test]
    fn indexing_label_counts_the_files_found_until_the_scan_has_a_total() {
        assert_eq!(indexing_label(0, 0), "Indexing… 0 files found");
        assert_eq!(indexing_label(1, 0), "Indexing… 1 file found");
        assert_eq!(indexing_label(12345, 0), "Indexing… 12345 files found");
        assert_eq!(indexing_label(1200, 42700), "Indexing… 1200/42700 files");
    }

    #[test]
    fn a_copy_says_how_far_it_has_got_once_bytes_move() {
        let bytes = Bytes::default();
        assert_eq!(bytes.line("Downloading paper.pdf"), None);
        bytes.set(12 * 1024 * 1024 + 300 * 1024, 80 * 1024 * 1024);
        assert_eq!(
            bytes.line("Downloading paper.pdf").as_deref(),
            Some("Downloading paper.pdf… 12.3/80.0 MB")
        );
    }

    #[test]
    fn page_label_counts_from_one_and_says_nothing_until_the_pages_are_known() {
        assert_eq!(page_label(0, 12).as_deref(), Some("Page 1 of 12"));
        assert_eq!(page_label(11, 12).as_deref(), Some("Page 12 of 12"));
        assert_eq!(page_label(0, 1).as_deref(), Some("Page 1 of 1"));
        assert_eq!(page_label(0, 0), None);
    }

    #[test]
    fn code_label_names_plain_text_when_there_is_no_language() {
        assert_eq!(code_label(Some("Rust"), "UTF-8 · LF"), "Rust · UTF-8 · LF");
        assert_eq!(
            code_label(None, "Not UTF-8 · CRLF"),
            "Plain Text · Not UTF-8 · CRLF"
        );
    }
}
