//! Show progress and document facts below the editor: kind, length, zoom and repository.
//!
//! Keeping these below the document avoids competing with its title and vault name. The
//! zoom readout resets zoom. The entire bar, including the unsaved dot, fades with the chrome.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use accent_api::link::Stop;
use gtk::glib;
use gtk::prelude::*;

/// The vault's line while a stopped walk leaves the index partial.
pub const PAUSED: &str = "Indexing paused";
/// The line while the tab in front is a PDF still being opened.
const OPENING: &str = "Opening the document…";

/// Vault indexing state and the action offered beside its progress.
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
    /// Independent vault, transfer and language-provider progress sources.
    /// Retain overlapping transfers separately so one completion cannot clear another.
    vault_busy: RefCell<Option<String>>,
    /// A PDF still being opened. A source of its own, beneath the vault's: an open that ends while
    /// the vault indexes leaves the indexing line, where one shared slot kept "Opening the
    /// document…" until the walk next reported, 22 s on the Mac.
    opening: Cell<bool>,
    /// Each copy's line, newest last, and its Stop where the reader can stop it.
    transfers: RefCell<Vec<(String, Option<Stop>)>>,
    /// Delayed language-server startup and subsequent checking progress for the active tab.
    starting: RefCell<Option<String>>,
    checking: RefCell<Option<String>>,
    /// How far the reading of the vault's PDFs' text has got, while it runs.
    pdf_text: RefCell<Option<String>>,
    provider_busy: RefCell<Option<String>>,
    /// Indexing Stop/Resume control beside progress. Suggestion indexing cannot be stopped here.
    index: gtk::Button,
    index_label: gtk::Label,
    indexing: Cell<Indexing>,
    /// Stop for the copy whose line is shown, and that copy's [`Stop`].
    transfer_stop: gtk::Button,
    shown_stop: Rc<RefCell<Option<Stop>>>,
    /// Stop beside the PDF text line, which the window wires to the vault.
    pdf_stop: gtk::Button,
    /// Branch readout and Sync control.
    branch: gtk::Button,
    branch_label: gtk::Label,
    kind: gtk::Label,
    /// The dot a dirty tab wears, so one symbol means "unsaved" wherever it appears.
    unsaved: gtk::Label,
    /// Document count and optional control: diagnostics toggle or PDF page menu.
    /// Word counts remain plain readouts.
    words: gtk::Label,
    facts: gtk::Button,
    /// Zoom readout and reset control.
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
        // A copy's Stop needs nothing from the window, so it is wired here.
        let stop_label = label(false);
        stop_label.set_label("Stop");
        stop_label.set_visible(true);
        let transfer_stop =
            bar_button(&stop_label, None, "Stop copying; files already copied stay");
        let shown_stop: Rc<RefCell<Option<Stop>>> = Rc::default();
        let pdf_stop_label = label(false);
        pdf_stop_label.set_label("Stop");
        pdf_stop_label.set_visible(true);
        let pdf_stop = bar_button(
            &pdf_stop_label,
            None,
            "Stop indexing PDF text until the vault is opened again",
        );
        transfer_stop.connect_clicked({
            let shown = shown_stop.clone();
            move |button| {
                if let Some(stop) = shown.borrow().as_ref() {
                    stop.stop();
                }
                button.set_visible(false);
            }
        });
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
        row.append(&transfer_stop);
        row.append(&pdf_stop);
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
            opening: Cell::new(false),
            transfers: RefCell::new(Vec::new()),
            starting: RefCell::new(None),
            checking: RefCell::new(None),
            pdf_text: RefCell::new(None),
            provider_busy: RefCell::new(None),
            index,
            index_label,
            indexing: Cell::new(Indexing::Idle),
            transfer_stop,
            shown_stop,
            pdf_stop,
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

    /// What the vault is busy with: "Indexing… 1200/42700 files", "Connecting…".
    pub fn set_progress(&self, text: Option<&str>) {
        *self.vault_busy.borrow_mut() = text.map(str::to_string);
        self.show_busy();
    }

    /// Whether the tab in front is a PDF still being opened, which the line says unless the
    /// vault's own work is under way: that is the slower of the two, and its Stop sits beside it.
    pub fn set_opening(&self, opening: bool) {
        self.opening.set(opening);
        self.show_busy();
    }

    /// Set vault indexing state independently from the shared progress text, since other jobs cannot be stopped.
    ///
    /// [`set_progress`]: Self::set_progress
    pub fn set_indexing(&self, state: Indexing) {
        self.indexing.set(state);
        let (label, tooltip) = match state {
            Indexing::Running => ("Stop", "Stop indexing; indexed files remain available"),
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

    /// A language provider is busy with something worth waiting for, named, and how far it says
    /// it has got: "suggestions" while the ghost-text index is rebuilt, "1200 files, sorting".
    /// Nothing is shown when it is idle.
    pub fn set_provider_busy(&self, what: Option<&str>, progress: Option<&str>) {
        *self.provider_busy.borrow_mut() = what.map(|w| busy_label(w, progress));
        self.show_busy();
    }

    /// The language server named `server` is starting for the tab in front, or is not (`None`):
    /// "Starting lake…".
    pub fn set_starting(&self, server: Option<&str>) {
        *self.starting.borrow_mut() = server.map(|server| format!("Starting {server}…"));
        self.show_busy();
    }

    /// The language server is checking the tab in front from 0-based `line` on, or is done with
    /// it (`None`): "Checking… line 12".
    pub fn set_checking(&self, line: Option<u32>) {
        *self.checking.borrow_mut() = line.map(|line| format!("Checking… line {}", line + 1));
        self.show_busy();
    }

    /// How far the reading of the vault's PDFs' text has got, `done` of `total` files, or `None`
    /// once it has ended: "Indexing PDF text… 12/80 files", after the language servers' lines and
    /// before the suggestion index, with its Stop.
    pub fn set_pdf_text(&self, progress: Option<(usize, usize)>) {
        *self.pdf_text.borrow_mut() =
            progress.map(|(done, total)| format!("Indexing PDF text… {done}/{total} files"));
        self.show_busy();
    }

    /// The Stop beside the PDF text line, which the window hangs its click on.
    pub fn pdf_text_control(&self) -> &gtk::Button {
        &self.pdf_stop
    }

    /// Start or end a transfer readout, also used while closing waits for Git.
    /// Single files report activity without stat calls; batches count files through [`Bar::retell_transfer`].
    pub fn set_transfer(&self, text: &str, running: bool) {
        self.transfer(text, running, None);
    }

    /// [`set_transfer`](Self::set_transfer) for a copy the reader started, which the Stop beside
    /// its line stops.
    pub fn set_stoppable(&self, text: &str, running: bool, stop: &Stop) {
        self.transfer(text, running, Some(stop));
    }

    fn transfer(&self, text: &str, running: bool, stop: Option<&Stop>) {
        {
            let mut transfers = self.transfers.borrow_mut();
            match running {
                true => transfers.push((text.to_string(), stop.cloned())),
                false => {
                    if let Some(at) = transfers.iter().position(|(t, _)| t == text) {
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
        if let Some((line, _)) = self
            .transfers
            .borrow_mut()
            .iter_mut()
            .find(|(t, _)| t == from)
        {
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

    /// The Stop beside a copy's line, for the drills.
    #[cfg(feature = "bench")]
    pub fn transfer_stop(&self) -> &gtk::Button {
        &self.transfer_stop
    }

    /// Choose progress text by priority: vault work, an open, newest transfer, active server
    /// startup/checking, the PDF text, then suggestions.
    /// Keep user-requested work ahead of background conveniences.
    fn show_busy(&self) {
        let vault = self.vault_busy.borrow();
        let transfers = self.transfers.borrow();
        let starting = self.starting.borrow();
        let checking = self.checking.borrow();
        let pdf_text = self.pdf_text.borrow();
        let provider = self.provider_busy.borrow();
        let ahead = vault.is_some() || self.opening.get();
        let transfer = transfers.last().filter(|_| !ahead);
        let before = vault
            .as_deref()
            .or(self.opening.get().then_some(OPENING))
            .or(transfer.map(|(line, _)| line.as_str()))
            .or(starting.as_deref())
            .or(checking.as_deref());
        let pdf_text = pdf_text.as_deref().filter(|_| before.is_none());
        set(&self.progress, before.or(pdf_text).or(provider.as_deref()));
        self.pdf_stop.set_visible(pdf_text.is_some());
        // The Stop goes with the line it stops, and once pressed stays gone.
        let stop = transfer.and_then(|(_, stop)| stop.clone());
        self.transfer_stop
            .set_visible(stop.as_ref().is_some_and(|stop| !stop.stopped()));
        *self.shown_stop.borrow_mut() = stop;
    }

    /// Show the active document repository's branch, upstream counts and uncommitted-work dot.
    /// [`crate::git::Panel::branch_label`] owns composition and shares the unsaved dot symbol.
    pub fn set_branch(&self, branch: Option<&str>) {
        set(&self.branch_label, branch);
        self.branch.set_visible(branch.is_some());
    }

    /// Dim the branch control while syncing and explain its disabled state in the tooltip.
    /// The Git pane owns the spinner.
    pub fn set_syncing(&self, on: bool) {
        self.branch.set_sensitive(!on);
        self.branch
            .set_tooltip_text(Some(if on { "Syncing…" } else { "Sync" }));
    }

    /// What the file is: "Markdown", "PDF", or "Rust · UTF-8 · LF" for code.
    pub fn set_kind(&self, text: Option<&str>) {
        set(&self.kind, text);
    }

    /// Show the document's count: words, diagnostics or PDF page, in the shared count slot.
    ///
    /// `press` supplies its action and tooltip, or `None` for a readout. Readouts disable input
    /// without dimming, since insensitive styling would imply a stale count.
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

    /// Show the unsaved dot only while the document differs from disk.
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

    /// Whether a menu hung off one of the bar's controls is up.
    pub fn menu_open(&self) -> bool {
        holds_popover(&self.row)
    }
}

/// Whether a popover parented anywhere under `widget` is showing.
fn holds_popover(widget: &impl IsA<gtk::Widget>) -> bool {
    std::iter::successors(widget.first_child(), |w| w.next_sibling())
        .any(|w| (w.is::<gtk::Popover>() && w.is_visible()) || holds_popover(&w))
}

/// Build a flat label button matching the status bar's readouts.
///
/// `.accent-bar-button` fixes the caption line height so normal button minimums cannot
/// stretch the bar. Button focus, role and tooltip remain intact. `action` may be `None`
/// when the window connects the handler directly.
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
    label.add_css_class("dimmed");
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

/// Count whitespace-separated runs for a draft-growth readout.
/// Standalone list dashes count; heading markers attached to words do not.
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

/// A language provider's line while it is busy: what it is indexing, then its own words on how
/// far it has got, as it sends them.
pub fn busy_label(what: &str, progress: Option<&str>) -> String {
    match progress {
        Some(progress) => format!("Indexing {what}… {progress}"),
        None => format!("Indexing {what}…"),
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

    /// Format `what` and transferred bytes, or `None` before any bytes move.
    /// Current cached files therefore produce no transfer readout.
    pub fn line(&self, what: &str) -> Option<String> {
        let (done, total) = (
            self.done.load(Ordering::Relaxed),
            self.total.load(Ordering::Relaxed),
        );
        let mb = |bytes: u64| format!("{:.1}", bytes as f64 / (1024.0 * 1024.0));
        (total > 0).then(|| format!("{what}… {}/{} MB", mb(done), mb(total)))
    }
}

/// Await `work` while updating its status line every [`TICK`].
/// Start and finish use `say(text, running)`; `retell` updates in place, and completion stops the timer.
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

/// Format 0-based `page` as a 1-based PDF readout.
/// Return `None` before page counts arrive to avoid displaying Page 1 of 0.
pub fn page_label(page: usize, count: usize) -> Option<String> {
    (count > 0).then(|| format!("Page {} of {count}", page + 1))
}

/// Format code language, encoding and line endings.
/// Notes omit encoding and endings because they always use UTF-8 and LF.
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
    fn a_providers_line_carries_its_own_words_on_how_far_it_is() {
        assert_eq!(busy_label("suggestions", None), "Indexing suggestions…");
        assert_eq!(
            busy_label("suggestions", Some("1200 files, sorting")),
            "Indexing suggestions… 1200 files, sorting"
        );
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
