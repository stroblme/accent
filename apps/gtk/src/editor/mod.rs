//! One editor tab: view, buffer, etag, banner and debounced editing work.
//!
//! Each pane's `find.rs` bar drives the tab's query and can also search presentation mode.
//! Callbacks carry tab events and closures supply vault access, letting tabs move and close
//! without depending on app state.

use crate::{diagnostics, diff, fold, highlight, lang, multicaret, wrap};
use accent_api::Error;
use accent_api::{Diagnostic, Fold, Vault};
use accent_core::Error as Core;
use accent_core::fs::{self, Digest, Etag};
use accent_core::markdown::Link;
use adw::prelude::*;
use gtk::{gdk, gio, glib, pango};
use sourceview5::prelude::*;
use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

mod banner;
mod compare;
mod drag;
mod follow;
mod keys;
mod lines;
mod open;
mod page;
mod paste;
mod search;
mod text;

pub use banner::Alert;
use compare::Comparing;
pub use compare::{companion, overlay_view, rehang_companion, restyle_companion, style_companion};
#[cfg(feature = "bench")]
pub(crate) use drag::ENDED as DRAG_ENDED;
#[cfg(feature = "bench")]
pub(crate) use drag::content as drag_content;
use follow::Follow;
#[cfg(feature = "bench")]
pub(crate) use keys::press;
use lines::primary_paste;
pub(crate) use lines::{caret_to_click, line_clipboard, pressed_at};
#[cfg(feature = "bench")]
pub(crate) use lines::{delete_line, duplicate_line, newline_below, paste_primary, toggle_comment};
use open::build;
pub use open::open;
pub use page::default_font;
use page::{GUTTER, line_numbers};
pub(crate) use page::{font_css, install_font, next_view_name, refont, set_margins};
use search::{match_style, matched, mute};
pub(crate) use text::{caret, line_end, line_prefix};

/// How long a long note waits after the last keystroke before it is re-analysed.
const DEBOUNCE: Duration = Duration::from_millis(150);
/// Character limit for full styling on each keystroke.
///
/// Full passes fit comfortably in a frame for short notes, but total typing costs rise before
/// analysis alone exhausts the budget. Longer notes style the caret line immediately through
/// [`highlight::apply_line`] and debounce the full pass.
const INSTANT: i32 = 16 * 1024;
/// DESIGN.md, Motion: save 1 s after the last edit. A diagram and a PDF's drawing share it.
pub(crate) const AUTOSAVE: Duration = Duration::from_secs(1);
/// The cursor callback drives the preview's scroll sync; 100 ms is below what the eye follows.
const CURSOR: Duration = Duration::from_millis(100);
type TabHook = crate::widgets::Hook<dyn Fn(&Rc<Tab>)>;
/// The find bar's hook, told whether the query itself changed ([`Tab::connect_found`]).
type FoundHook = crate::widgets::Hook<dyn Fn(bool)>;

/// Whether a tab may attempt saving its current file.
///
/// Dirty buffers whose files moved must await the banner's decision. Clean buffers can
/// recreate deleted files. `fs::write_note` still enforces the final etag gate.
pub fn may_save(modified: bool, disk_changed: bool) -> bool {
    !(modified && disk_changed)
}

/// Attach a spell checker and suggestion-menu action group to `buffer`
/// and `view`, initially disabled.
pub fn spell_adapter(
    buffer: &sourceview5::Buffer,
    view: &sourceview5::View,
) -> libspelling::TextBufferAdapter {
    let adapter = libspelling::TextBufferAdapter::new(buffer, &libspelling::Checker::default());
    view.insert_action_group("spelling", Some(&adapter));
    view.set_extra_menu(Some(&adapter.menu_model()));
    // The suggestions are the caret's word's, and a secondary click puts the caret on the word
    // clicked ([`caret_to_click`]): asked again at once rather than after the adapter's own pause
    // on a caret move, so the menu opening on this press already holds them. The caret is put
    // here too, whichever of the two presses runs first.
    let click = gtk::GestureClick::builder()
        .button(gdk::BUTTON_SECONDARY)
        .propagation_phase(gtk::PropagationPhase::Capture)
        .build();
    let weak = adapter.downgrade();
    click.connect_pressed(move |click, presses, x, y| {
        if let (Some(view), Some(adapter)) = (
            click.widget().and_downcast::<sourceview5::View>(),
            weak.upgrade(),
        ) && presses == 1
        {
            lines::place_at_click(&view, x, y);
            adapter.update_corrections();
        }
    });
    view.add_controller(click);
    adapter
}

/// One in-flight save per tab, retaining its starting state and reply channel.
pub struct Flight {
    /// [`SaveState::edits`] when the text was taken.
    pub started: u64,
    /// The etag the write was gated on.
    pub expected: Option<Etag>,
    /// Tab etag at save start, distinct from the forced-write gate when
    /// answering a disk-conflict question.
    pub held: Option<Etag>,
    /// A Ctrl+S, which says "Saved" when it lands.
    pub explicit: bool,
    /// The etag the write left, and the digest of what it wrote.
    pub answer: std::sync::mpsc::Receiver<Result<(Etag, Digest), Error>>,
}

/// What a save that has just landed means for its tab.
#[derive(Debug)]
pub enum Landing {
    /// The buffer is still what was written: clean, at the new etag.
    Clean(Etag),
    /// Save landed, but newer buffer edits remain dirty. Gate the next save
    /// on the new file etag.
    Behind(Etag),
    /// A reload replaced the buffer and etag, making this save result obsolete.
    Stale,
    /// Refused or failed, for a buffer that still holds its edits.
    Failed(Error),
}

/// Decide a [`Landing`] from the tab as the save found it (`started`, `held`) and as it is now
/// (`edits`, `holding`).
pub fn landing(
    started: u64,
    edits: u64,
    held: Option<Etag>,
    holding: Option<Etag>,
    written: Result<Etag, Error>,
) -> Landing {
    if holding != held {
        return Landing::Stale;
    }
    match written {
        Ok(etag) if started == edits => Landing::Clean(etag),
        Ok(etag) => Landing::Behind(etag),
        Err(e) => Landing::Failed(e),
    }
}

/// Shared file/save state for note and diagram tabs using `save.rs`.
#[derive(Default)]
pub struct SaveState {
    pub etag: Cell<Option<Etag>>,
    /// Digest of bytes last read or written at `etag`, identifying unchanged
    /// contents even when the disk etag moves.
    pub digest: Cell<Option<Digest>>,
    pub modified: Cell<bool>,
    /// External changes under a dirty buffer block autosave until the user
    /// answers the banner, preserving both versions.
    pub disk_changed: Cell<bool>,
    /// Count typed and loaded buffer changes to detect edits made during a save.
    pub edits: Cell<u64>,
    /// The save on its way, if one is.
    pub flight: RefCell<Option<Flight>>,
    /// Save queued behind the current write. `Some(true)` retains any explicit
    /// Ctrl+S request.
    pub save_again: Cell<Option<bool>>,
    /// Watcher event during a save; restat after landing establishes the saved etag.
    pub recheck: Cell<bool>,
}

impl SaveState {
    /// A file as it was just read, at `etag`, holding what `digest` digests.
    pub fn at(etag: Etag, digest: Digest) -> SaveState {
        SaveState {
            etag: Cell::new(Some(etag)),
            digest: Cell::new(Some(digest)),
            ..SaveState::default()
        }
    }
}

/// A tab the save path can write: its [`SaveState`], where its file is, and what the file should
/// hold.
pub trait Saves: 'static {
    fn save_state(&self) -> &SaveState;
    /// Vault-relative, or absolute for a file from outside the vault.
    fn key(&self) -> String;
    fn path(&self) -> PathBuf;
    /// What the file should hold now.
    fn for_disk(&self) -> String;
}

/// Text flavour. Shared save, find and banner mechanisms stay on one tab
/// type; flavour controls rendering and Markdown-specific behavior.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Flavour {
    /// A markdown note: our own styling spans, wikilink completion, spellcheck, a capped column.
    Note,
    /// Anything else that is text: a GtkSourceView language, monospace, the full width.
    Code,
    /// A CSV, which is code that gets its columns coloured instead of a language.
    Csv,
}

impl Flavour {
    pub fn is_note(self) -> bool {
        self == Flavour::Note
    }
}

/// Named construction preferences, avoiding a growing positional argument list.
pub struct Prefs {
    pub spellcheck: bool,
    pub ghost_text: bool,
    pub font: Option<String>,
    pub zoom: f64,
    pub column_width: u32,
    pub indent_width: u32,
    pub minimap: bool,
    pub line_numbers: bool,
}

pub struct Tab {
    /// Behind a cell because a rename retargets the tab instead of closing and reopening it.
    rel: RefCell<String>,
    path: RefCell<PathBuf>,
    pub view: sourceview5::View,
    pub buffer: sourceview5::Buffer,
    /// Committed text and diff gutter, or `None` for untracked/nonrepository files.
    head: RefCell<Option<String>>,
    marks: crate::marks::Renderer,
    /// Page content and document, retained so comparisons can insert and
    /// remove a paned around the editor ([`Tab::compare`]).
    content: gtk::Box,
    document: gtk::Box,
    comparing: RefCell<Option<Comparing>>,
    /// The buttons a comparison lays over the editor, kept across comparisons: see `diff::Pool`.
    overlays: Rc<diff::Pool>,
    /// Git's conflict markers in the text, tinted and with their Accept buttons over them.
    conflicts: Rc<crate::conflict::Conflicts>,
    /// Kept for [`Tab::scroll_lines`] and for the scrollbar the minimap replaces.
    scroller: gtk::ScrolledWindow,
    /// Scrollable document-width cap ([`Tab::set_clamp`]), keeping the view
    /// as the scroller's child rather than a viewport child.
    clamp: adw::ClampScrollable,
    /// Zoom and width percentage needed to recompute the column cap on
    /// external editor resizes.
    zoom: Cell<f64>,
    column: Cell<u32>,
    /// Ghost-text preference, mirrored to `lang.ghost` for requests and
    /// retained so already-open tabs can enable it later.
    ghost_text: Cell<bool>,
    map: crate::minimap::Minimap,
    /// The optional line-number gutter; hidden unless the preference turns it on.
    numbers: sourceview5::GutterRenderer,
    /// The sticky block title over the top of the view, and the bar it sits on. Hidden until
    /// something is scrolled out of sight above the first visible line.
    sticky: gtk::Label,
    sticky_bar: gtk::Box,
    /// Fixed text flavour, gating Markdown styling, completion, spelling,
    /// column cap and hanging markers.
    flavour: Flavour,
    /// Original CRLF endings, normalized in the buffer and restored on save
    /// to avoid rewriting every unchanged line.
    crlf: Cell<bool>,
    /// Lossy UTF-8 display flag. Disable editing to preserve undecodable bytes.
    lossy: Cell<bool>,
    pub page: adw::TabPage,
    pub banner: adw::Banner,
    pub save: SaveState,
    /// Pending file questions; show one via `banner::banner_alert` while others wait.
    alerts: RefCell<Vec<Alert>>,
    /// The find bar's query over this tab, and where it matches ([`search::Find`]).
    find: RefCell<search::Find>,
    /// What the find bar's matches are painted with, raised to the top of the table as it paints.
    find_tag: gtk::TextTag,
    /// The bar last pointed at this tab, told when the query has been matched again.
    on_found: FoundHook,
    /// A muted tag for other occurrences of the selection, below find highlighting.
    ///
    /// Two SearchContexts repeatedly raise their tags and race for overlap priority. A normal
    /// tag created before the find tag keeps occurrence hints reliably underneath.
    occurrence_tag: gtk::TextTag,
    /// What that tag is showing, so a caret move that changes nothing re-tags nothing.
    occurrence_query: RefCell<Option<String>>,
    /// Where the jump that opened this note landed, in the scheme's own match colour. See
    /// [`Tab::reveal_range`].
    reveal_tag: gtk::TextTag,
    /// Whether that tag is painting anything, so an ordinary keystroke costs no tag walk.
    revealed: Cell<bool>,
    spell: RefCell<Option<libspelling::TextBufferAdapter>>,
    /// Note links with character ranges, converted once from parse byte offsets
    /// to avoid copying prefix text on every Ctrl-pointer motion.
    links: RefCell<Vec<(Range<i32>, Link)>>,
    /// Cached note word count, invalidated on edits and loaded on first ask
    /// because status-bar queries outnumber text changes.
    words: Cell<Option<usize>>,
    /// The underline under what a Ctrl+click would follow, and what it is showing. See
    /// [`follow`].
    follow_tag: gtk::TextTag,
    follow: RefCell<Follow>,
    /// Stored diagnostics and line-message provider, read back by hover and
    /// status after painting.
    diagnostics: RefCell<Vec<Diagnostic>>,
    annotations: sourceview5::AnnotationProvider,
    /// Hide diagnostic squiggles, gutter marks and line messages while
    /// retaining hover details and status counts.
    diagnostics_hidden: Cell<bool>,
    /// Last painted line messages with their full source text. Hidden lines
    /// are omitted; the provider cannot supply the original text back.
    annotated: RefCell<diagnostics::Shown>,
    /// Line messages to relayout after diagnostic ranges move with an edit
    /// ([`Tab::shift_diagnostics`]).
    relay: RefCell<Option<diagnostics::Shown>>,
    /// Provider fold ranges and header chevrons. Actual hidden state lives
    /// in buffer tags.
    folds: RefCell<Vec<Fold>>,
    fold_renderer: fold::Renderer,
    font: RefCell<Option<gtk::CssProvider>>,
    /// A watch on the file itself, for a tab no vault watcher covers. `None` for everything
    /// inside a vault, which the worker already reports on.
    monitor: RefCell<Option<gio::FileMonitor>>,
    /// Set while we replace the buffer text ourselves, so `changed` does not mark it dirty.
    loading: Cell<bool>,
    /// Whole-buffer replacement count, identifying history marks invalidated
    /// when all marks reset to the buffer start.
    replaced: Cell<u32>,
    /// Last template snippet, whose retained buffer indicates its Tab stops
    /// are still active.
    snippet: RefCell<Option<sourceview5::Snippet>>,
    /// The paste that makes a URL over a selection a link, which `Ctrl+Shift+V` holds back.
    paste_link: glib::SignalHandlerId,
    /// The post-edit pass: the change bars, and the styling a note too long to restyle inside a
    /// frame owes the rest of its text.
    debounce: crate::widgets::Debounce,
    /// A CSV's column colouring, kept between passes: see [`highlight::Csv`].
    csv: Rc<highlight::Csv>,
    autosave: crate::widgets::Debounce,
    /// First-wins caret debounce ([`crate::widgets::Debounce::call_once`])
    /// so held arrow keys keep updating the outline during movement.
    cursor: crate::widgets::Debounce,
    /// Whether this turn of the main loop has changed the text, and whether it has moved the
    /// caret; settled into `moved` once the turn is over (see [`Tab::note_turn`]).
    turn: Cell<(bool, bool)>,
    /// The caret was last moved rather than carried by an edit: see [`Tab::caret_moved`].
    moved: Cell<bool>,
    /// The end-of-line messages laid again for a new column width, once a drag has settled.
    refit: crate::widgets::Debounce,
    on_autosave: TabHook,
    on_edited: TabHook,
    on_banner: TabHook,
    on_cursor: TabHook,
    on_follow: TabHook,
    /// This tab's document on the vault's language layer: what it can answer, what it last
    /// answered, and the refresh that is still in flight. Empty for a tab outside every vault.
    pub lang: lang::State,
}

/// Reload position: caret and top-line viewport offset, retaining the
/// same visible text even when new bytes shift its location.
#[derive(Clone, Copy)]
struct Anchor {
    offset: i32,
    top_line: i32,
    top_y: i32,
    scrolled: f64,
}

/// Detect GtkSourceView language from filename and guessed content type.
/// This supports named extensionless files (Makefile/Dockerfile) and
/// content-sniffed scripts such as `#!/bin/sh`.
fn guess_language(path: &Path, text: &str) -> Option<sourceview5::Language> {
    let (content_type, _) = gio::content_type_guess(Some(path), text.as_bytes());
    sourceview5::LanguageManager::default().guess_language(Some(path), Some(&content_type))
}

// ------------------------------------------------------------------------------------- helpers

/// Apply the theme explicitly because GtkSourceView paints its background
/// from its own style scheme.
pub fn sync_scheme(buffer: &sourceview5::Buffer) {
    let id = crate::theme::scheme_id(adw::StyleManager::default().is_dark());
    let scheme = sourceview5::StyleSchemeManager::default().scheme(id);
    buffer.set_style_scheme(scheme.as_ref());
}

/// The language a file called `key` holding `text` is coloured as.
pub fn language_for(key: &str, text: &str) -> Option<sourceview5::Language> {
    guess_language(Path::new(key), text)
}

/// Keep a file monitor calling `f` on external writes, or `None` if unavailable.
/// Only loose tabs need it: vault watchers already report writes and
/// distinguish our saves from external edits.
pub fn watch_file(path: &Path, f: impl Fn() + 'static) -> Option<gio::FileMonitor> {
    let monitor = gio::File::for_path(path)
        .monitor_file(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE)
        .ok()?;
    monitor.connect_changed(move |_, _, _, event| {
        // `ChangesDoneHint` is the settled write; `Created` is the rename an atomic save lands
        // as, ours included, which the etag check then makes a no-op.
        if matches!(
            event,
            gio::FileMonitorEvent::ChangesDoneHint | gio::FileMonitorEvent::Created
        ) {
            f();
        }
    });
    Some(monitor)
}

// ---------------------------------------------------------------------------------------- tab

impl Tab {
    pub fn rel(&self) -> String {
        self.rel.borrow().clone()
    }

    pub fn path(&self) -> PathBuf {
        self.path.borrow().clone()
    }

    pub fn flavour(&self) -> Flavour {
        self.flavour
    }

    /// Set committed text for git change bars, or clear with `None`. The Git
    /// pane supplies it because it already queries the repository.
    pub fn set_head(&self, head: Option<String>) {
        let showing = head.is_some();
        *self.head.borrow_mut() = head;
        self.marks.set_visible(showing || self.marks.checked());
        match showing {
            true => self.update_marks(),
            false => self.marks.set_marks(Vec::new()),
        }
    }

    /// Mark the lines the language server has still to check in the change bars' column, none
    /// once it is done (`Event::Checking`).
    pub fn set_checking(&self, lines: Vec<std::ops::Range<u32>>) {
        self.marks.set_checking(lines);
    }

    /// The lines the gutter marks as still being checked.
    #[cfg(feature = "bench")]
    pub fn checking(&self) -> Vec<std::ops::Range<u32>> {
        self.marks.checking()
    }

    /// The GtkSourceView language this tab was given, by its display name ("Rust", "Makefile").
    /// `None` for a file no language claimed, which the status bar calls plain text.
    pub fn language(&self) -> Option<String> {
        self.buffer.language().map(|l| l.name().to_string())
    }

    /// Watch the file behind this tab and call `f` when someone else writes it ([`watch_file`]).
    pub fn watch_file(self: &Rc<Self>, f: impl Fn(&Rc<Tab>) + 'static) {
        *self.monitor.borrow_mut() = watch_file(
            &self.path(),
            glib::clone!(
                #[weak(rename_to = tab)]
                self,
                move || f(&tab)
            ),
        );
    }

    /// Prepare save text: trim code-line trailing whitespace and restore
    /// original line endings. Preserve note whitespace, including Markdown
    /// hard breaks from two trailing spaces.
    pub fn for_disk(&self) -> String {
        fs::for_disk(&self.text(), self.crlf.get(), !self.flavour.is_note())
    }

    /// How the file is encoded and how its lines end, for the readout in the header.
    pub fn encoding_label(&self) -> String {
        format!("{} · {}", self.encoding(), self.line_ending())
    }

    pub fn encoding(&self) -> &'static str {
        match self.lossy.get() {
            true => "Not UTF-8",
            false => "UTF-8",
        }
    }

    pub fn line_ending(&self) -> &'static str {
        match self.crlf.get() {
            true => "CRLF",
            false => "LF",
        }
    }

    /// A rename landed: point the tab at the new path without losing the buffer.
    pub fn retarget(self: &Rc<Self>, root: &Path, new_rel: &str) {
        let old_rel = self.rel();
        *self.rel.borrow_mut() = new_rel.to_string();
        *self.path.borrow_mut() = root.join(new_rel);
        self.page.set_title(&self.tab_title());
        self.page
            .set_tooltip(&crate::fileops::display_path(root, new_rel));
        // A language server keys its documents by URI and knows nothing of the move, so the old
        // path is closed and the new one opened.
        lang::retarget(self, &old_rel);
    }

    pub fn text(&self) -> String {
        let (s, e) = self.buffer.bounds();
        self.buffer.text(&s, &e, true).to_string()
    }

    /// Replace buffer text without dirtying the tab during loading or a pending write.
    ///
    /// Notify the language layer explicitly because `loading` suppresses edit callbacks;
    /// otherwise symbols, folds and diagnostics would remain stale until typing resumes.
    pub fn set_text(self: &Rc<Self>, text: &str) {
        self.save.edits.set(self.save.edits.get() + 1);
        self.replaced.set(self.replaced.get() + 1);
        self.loading.set(true);
        self.buffer.set_text(text);
        self.loading.set(false);
        self.analyse();
        lang::changed(self);
    }

    /// Whole-text replacement generation, identifying marks reset by reloads.
    pub fn replaced(&self) -> u32 {
        self.replaced.get()
    }

    /// Whether programmatic text replacement is active. Insert handlers
    /// must avoid treating templates as typed brackets that open suggestions
    /// or signature help.
    pub(crate) fn is_loading(&self) -> bool {
        self.loading.get()
    }

    pub fn mark_clean(&self, etag: Etag) {
        self.save.etag.set(Some(etag));
        self.save.modified.set(false);
        self.save.disk_changed.set(false);
        self.page.set_title(&self.tab_title());
    }

    /// Reload a clean tab on a worker and call `done` once with the result.
    ///
    /// Large reads must not block watcher handling. Vault files read through `vault`, including
    /// remote paths; loose files read their own local path.
    pub fn reload_keep_cursor(
        self: &Rc<Self>,
        vault: Option<Arc<Vault>>,
        done: impl Fn(&Rc<Tab>, accent_api::Result<()>) + 'static,
    ) {
        // Where the caret and the page are, measured before the read: replacing the buffer empties
        // it, which drops the view to line one, and a scroll to the caret from there parks it
        // against whichever edge is nearer instead of putting the page back.
        let (top_iter, _) = self.view.line_at_y(self.view.visible_rect().y());
        let anchor = Anchor {
            offset: caret(&self.buffer).offset(),
            top_line: top_iter.line(),
            top_y: self.view.line_yrange(&top_iter).0,
            scrolled: self.view.vadjustment().map_or(0.0, |v| v.value()),
        };
        let (rel, path) = (self.rel(), self.path());
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let read = crate::work::off_thread("reader", move || match vault {
                Some(vault) => vault.read_text(&rel),
                None => fs::read_text(&path).map_err(Into::into),
            })
            .await;
            let Some(tab) = weak.upgrade() else { return };
            let text = match read {
                Some(Ok(fs::Read::Text(text))) => text,
                // It stopped being text while we had it open. The buffer keeps the last readable
                // version rather than showing the user a screen of replacement characters.
                Some(Ok(_)) => {
                    return done(&tab, Err(Core::Invalid("not text any more".into()).into()));
                }
                Some(Err(e)) => return done(&tab, Err(e)),
                None => return done(&tab, Err(Core::Io("the reader stopped".into()).into())),
            };
            // Typed into while the file was read: the buffer holds the only copy of that, so it
            // asks as a dirty tab does (`App::refresh_tab`) rather than being replaced.
            if tab.save.modified.get() {
                tab.save.disk_changed.set(true);
                tab.show_alert(Alert::Compare);
                return done(&tab, Ok(()));
            }
            tab.adopt_reload(text, anchor);
            done(&tab, Ok(()));
        });
    }

    /// Put a reload's text in the buffer and the reader back where they were looking.
    fn adopt_reload(self: &Rc<Self>, text: fs::Text, anchor: Anchor) {
        self.crlf.set(text.crlf);
        self.lossy.set(text.lossy);
        let etag = text.etag;
        self.save.digest.set(Some(text.digest()));
        self.set_text(&text.text);
        let iter = self
            .buffer
            .iter_at_offset(anchor.offset.min(self.buffer.char_count()));
        self.buffer.place_cursor(&iter);
        // One idle later, because the buffer has only just been replaced: a position measured
        // against lines the view has not laid out yet lands short of the line it was given.
        let (view, buffer) = (self.view.clone(), self.buffer.clone());
        glib::idle_add_local_once(move || {
            let iter = buffer
                .iter_at_line(anchor.top_line.min(buffer.line_count() - 1))
                .unwrap_or_else(|| buffer.end_iter());
            let moved = view.line_yrange(&iter).0 - anchor.top_y;
            if let Some(vadjustment) = view.vadjustment() {
                vadjustment.set_value(anchor.scrolled + f64::from(moved));
            }
        });
        self.mark_clean(etag);
        self.clear_disk_alert();
    }

    pub fn restyle(&self) {
        // The scheme is what recolours code, and it is also what a note's own tags sit on.
        sync_scheme(&self.buffer);
        // Derived from the scheme that just changed, so both have to be derived again.
        mute(&self.buffer, &self.occurrence_tag);
        matched(&self.buffer, &self.reveal_tag);
        match_style(&self.buffer, &self.find_tag);
        match self.flavour {
            Flavour::Note => {
                highlight::restyle(&self.buffer, &self.view);
                highlight::hang(&self.buffer, &self.view);
            }
            // The column hues are rotated from the accent, which the theme can change under us.
            Flavour::Csv => highlight::restyle_csv(&self.buffer),
            Flavour::Code => {}
        }
        wrap::measure(&self.view);
        self.marks.restyle(&self.view);
        diagnostics::restyle(&self.buffer, &self.view);
        self.fold_renderer.restyle(&self.view);
        self.conflicts.restyle();
        self.map.restyle();
        if let Some(hosted) = self.hosted() {
            hosted.restyle();
        }
    }

    /// How many links the note holds, as of its last analysis.
    pub fn link_count(&self) -> usize {
        self.links.borrow().len()
    }

    /// The text's words, as the status bar counts them for a note.
    pub fn words(&self) -> usize {
        let words = self
            .words
            .get()
            .unwrap_or_else(|| crate::statusbar::word_count(&self.text()));
        self.words.set(Some(words));
        words
    }

    /// Git's conflict markers in this tab, for the palette's Accept and Next Conflict commands.
    pub fn conflicts(&self) -> &crate::conflict::Conflicts {
        &self.conflicts
    }

    /// Replace stored diagnostics with the provider's full publish. Empty clears them.
    pub fn set_diagnostics(&self, items: Vec<Diagnostic>) {
        *self.diagnostics.borrow_mut() = items;
        self.paint_diagnostics();
    }

    /// Toggle visible diagnostics per tab through the status count, preserving
    /// the stored answer for this document.
    pub fn hide_diagnostics(&self, hidden: bool) {
        if self.diagnostics_hidden.replace(hidden) != hidden {
            self.paint_diagnostics();
        }
    }

    pub fn diagnostics_hidden(&self) -> bool {
        self.diagnostics_hidden.get()
    }

    /// Repaint stored diagnostics, or clear while hidden, using the same
    /// path as provider publishes to keep tags consistent.
    pub(super) fn paint_diagnostics(&self) {
        self.lay_diagnostics(diagnostics::Shown::new());
    }

    /// [`Tab::paint_diagnostics`], keeping the cut of each message in `kept` still on its line.
    fn lay_diagnostics(&self, kept: diagnostics::Shown) {
        let items = self.diagnostics.borrow();
        let painted: &[Diagnostic] = match self.diagnostics_hidden.get() {
            true => &[],
            false => &items,
        };
        *self.annotated.borrow_mut() =
            diagnostics::render(&self.view, &self.buffer, &self.annotations, painted, kept);
    }

    /// Shift diagnostics for an upcoming `from..to` edit ([`diagnostics::Edit`])
    /// so hover and repaint use current positions before the next publish.
    /// Whole-text replacement instead needs a fresh server answer.
    fn shift_diagnostics(&self, from: &gtk::TextIter, to: &gtk::TextIter, text: &str) {
        if self.loading.get() {
            return;
        }
        let edit = diagnostics::Edit::new(lang::pos_of(from), lang::pos_of(to), text);
        if edit.shift(&mut self.diagnostics.borrow_mut()) {
            *self.relay.borrow_mut() = Some(edit.reline(self.annotated.take()));
        }
    }

    /// How many end-of-line messages the last paint put up. `ACCENT_BENCH_COMPARE=diag:` only.
    #[cfg(feature = "bench")]
    pub fn annotated(&self) -> usize {
        self.annotated.borrow().len()
    }

    /// Each of them as its characters shown beside those its line has room for now.
    /// `ACCENT_BENCH_DIAG` only.
    #[cfg(feature = "bench")]
    pub fn cuts(&self) -> Vec<(usize, usize)> {
        diagnostics::cuts(&self.view, &self.annotated.borrow())
    }

    /// The lines the end-of-line messages are drawn on. `ACCENT_BENCH_DIAG` only.
    #[cfg(feature = "bench")]
    pub fn message_lines(&self) -> Vec<i32> {
        let shown = self.annotated.borrow();
        shown
            .values()
            .map(|(annotation, _)| annotation.line())
            .collect()
    }

    /// What the server said, painted or not: what the status bar counts and the hover reads back.
    pub fn diagnostics(&self) -> std::cell::Ref<'_, Vec<Diagnostic>> {
        self.diagnostics.borrow()
    }

    /// The buffer as a plain `GtkTextBuffer`, which is what `fold` works in: nothing it does
    /// needs GtkSourceView.
    fn text_buffer(&self) -> &gtk::TextBuffer {
        self.buffer.upcast_ref()
    }

    /// Update fold ranges while preserving hidden blocks whose headers survived,
    /// following their current positions.
    pub fn set_folds(&self, folds: Vec<Fold>) {
        fold::resync(self.text_buffer(), &folds);
        *self.folds.borrow_mut() = folds;
        self.show_chevrons();
    }

    /// The chevrons beside the headers of the blocks the server knows, or none while a
    /// comparison is shown, where nothing shuts (see [`Tab::shut`]).
    fn show_chevrons(&self) {
        let starts = match self.comparing.borrow().is_some() {
            true => Default::default(),
            false => self
                .folds
                .borrow()
                .iter()
                .map(|f| f.start_line as i32)
                .collect(),
        };
        self.fold_renderer.set_starts(starts);
    }

    /// Hide `f` unless a comparison is active.
    ///
    /// Combining editor folds and comparison-hidden runs can leave partially hidden lines that
    /// abort GTK 4.22 iter lookup. Comparisons open folds and restore them when leaving.
    fn shut(&self, f: Fold) {
        if self.comparing.borrow().is_some() {
            return;
        }
        let hide = || fold::fold(self.text_buffer(), f);
        match self.view.downcast_ref::<multicaret::View>() {
            Some(view) => view.keeping_carets(hide),
            None => hide(),
        }
    }

    /// Open or shut the block whose header is `line`. What the gutter chevron does.
    pub fn toggle_fold(&self, line: i32) {
        let known = self
            .folds
            .borrow()
            .iter()
            .any(|f| f.start_line as i32 == line);
        if !known {
            return;
        }
        match fold::is_folded(self.text_buffer(), line) {
            true => fold::unfold(self.text_buffer(), line),
            false => self.fold_line(line),
        }
        self.folds_changed();
    }

    /// After fold changes, redraw chevrons and diagnostics. Hidden lines lose
    /// messages; revealed lines regain them, as with comparison runs.
    fn folds_changed(&self) {
        self.fold_renderer.queue_draw();
        self.paint_diagnostics();
    }

    fn fold_line(&self, line: i32) {
        let found = self
            .folds
            .borrow()
            .iter()
            .find(|f| f.start_line as i32 == line)
            .copied();
        if let Some(f) = found {
            self.shut(f);
        }
    }

    fn caret_line(&self) -> i32 {
        caret(&self.buffer).line().max(0)
    }

    /// Fold the innermost block the caret is in.
    pub fn fold_at_caret(&self) {
        let found = fold::containing(&self.folds.borrow(), self.caret_line() as u32).copied();
        if let Some(f) = found {
            self.shut(f);
            self.folds_changed();
        }
    }

    /// Open the block the caret is on, whether the caret is on its header or inside it.
    pub fn unfold_at_caret(&self) {
        let found = fold::containing(&self.folds.borrow(), self.caret_line() as u32).copied();
        if let Some(f) = found {
            fold::unfold(self.text_buffer(), f.start_line as i32);
            self.folds_changed();
        }
    }

    pub fn fold_all(&self) {
        // Outermost first, so a nested block is already inside a hidden run and the caret only
        // has to be moved out once.
        let mut folds = self.folds.borrow().clone();
        folds.sort_by_key(|f| f.start_line);
        for f in folds {
            self.shut(f);
        }
        self.folds_changed();
    }

    pub fn unfold_all(&self) {
        fold::unfold_all(self.text_buffer());
        self.folds_changed();
    }

    /// Discard the buffer's dirty state after the user chooses to lose edits,
    /// preventing downstream close handlers from saving them.
    pub fn discard(&self) {
        self.save.modified.set(false);
        self.save.disk_changed.set(false);
        self.page.set_title(&self.tab_title());
        self.clear_disk_alert();
    }

    /// 1-based, the way an editor counts lines and the preview's `data-line` markers do.
    pub fn cursor_line(&self) -> u32 {
        caret(&self.buffer).line().max(0) as u32 + 1
    }

    fn tab_title(&self) -> String {
        let rel = self.rel();
        let name = crate::doc::file_name(&rel);
        let name = match self.comparing.borrow().as_ref() {
            Some(comparing) => format!("{name} ({})", comparing.label),
            None => name.to_string(),
        };
        match self.save.modified.get() {
            true => format!("• {name}"),
            false => name,
        }
    }

    // --- preferences ---------------------------------------------------------------------

    /// Set tab and wrap width from preferences for notes and code. Note Tab
    /// still inserts a literal tab or list indent; CommonMark interprets
    /// tabs at four-column stops.
    pub fn set_indent_width(&self, columns: u32) {
        self.view.set_tab_width(columns);
        if let Some(hosted) = self.hosted() {
            hosted.follow_editor(false);
        }
    }

    /// The spelling suggestions' menu, for `ACCENT_BENCH_KEYS=menu:`.
    #[cfg(feature = "bench")]
    pub fn spell_menu(&self) -> Option<gtk::gio::MenuModel> {
        self.spell
            .borrow()
            .as_ref()
            .map(|adapter| adapter.menu_model())
    }

    pub fn set_spellcheck(&self, on: bool) {
        // Prose only. A checker over identifiers and keywords is a wall of red squiggles.
        if !self.flavour.is_note() {
            return;
        }
        // Cloned out first: the adapter is created inside the `else`, which borrows mutably.
        let existing = self.spell.borrow().clone();
        let adapter = match existing {
            Some(adapter) => adapter,
            None if !on => return,
            None => {
                let adapter = spell_adapter(&self.buffer, &self.view);
                *self.spell.borrow_mut() = Some(adapter.clone());
                adapter
            }
        };
        adapter.set_enabled(on);
    }

    /// Add a caret above/below via the view subclass without changing the
    /// tab's plain view type. Restore diagnostics after any fold revealed
    /// for the new caret.
    pub fn add_caret(&self, below: bool) {
        if let Some(view) = self.view.downcast_ref::<multicaret::View>()
            && let Some(opened) = view.add_caret(below)
        {
            self.revealed_at(opened);
        }
    }

    /// Select the current word on the first press, then add a caret at each
    /// next occurrence (JetBrains Add Caret at Next Occurrence).
    pub fn add_next_occurrence(&self) {
        let Some(view) = self.view.downcast_ref::<multicaret::View>() else {
            return;
        };
        if self.buffer.has_selection() {
            view.add_next_occurrence();
        } else if self.select_word() {
            view.match_whole_words();
        }
    }

    /// Place carets at all selection occurrences, or all current-word
    /// occurrences without a selection (VS Code semantics).
    pub fn select_all_occurrences(&self) {
        let Some(view) = self.view.downcast_ref::<multicaret::View>() else {
            return;
        };
        if !self.buffer.has_selection() && self.select_word() {
            view.match_whole_words();
        }
        if self.buffer.has_selection() {
            view.select_all_occurrences();
        }
    }

    /// Select the current or just-completed word ([`follow::word_at`]), ending
    /// any caret column. Keep underscores and put the caret at the word end.
    /// Return whether a word existed.
    fn select_word(&self) -> bool {
        let at = caret(&self.buffer);
        let mut before = at;
        before.backward_char();
        let Some(word) = follow::word_at(&at).or_else(|| follow::word_at(&before)) else {
            return false;
        };
        let iter = |offset| self.buffer.iter_at_offset(offset);
        self.buffer.select_range(&iter(word.end), &iter(word.start));
        true
    }

    /// Focus mode's line fade, which the view subclass paints (`fade.rs`), on a comparison's
    /// other column too.
    pub fn set_fade(&self, on: bool) {
        if let Some(view) = self.view.downcast_ref::<multicaret::View>() {
            view.set_fade(on);
        }
        if let Some(compare) = self.comparison() {
            compare.set_fade(on);
        }
    }

    /// The minimap, which fades with the chrome.
    pub fn minimap(&self) -> &gtk::Widget {
        self.map.upcast_ref()
    }

    /// Insert and retain `snippet` at `at` so Tab visits active template
    /// stops before accepting suggestions.
    pub(crate) fn push_snippet(&self, snippet: &sourceview5::Snippet, at: &mut gtk::TextIter) {
        self.view.push_snippet(snippet, Some(at));
        *self.snippet.borrow_mut() = Some(snippet.clone());
    }

    /// Whether the completion popup is on screen: mapped, which nothing can leave behind.
    pub(crate) fn popup_shown(&self) -> bool {
        crate::completion::is_shown(self)
    }

    /// Check active template stops through the snippet's retained buffer,
    /// GtkSourceView 5.20's only external completion indicator.
    pub(crate) fn snippet_active(&self) -> bool {
        self.snippet
            .borrow()
            .as_ref()
            .is_some_and(|snippet| snippet.buffer().is_some())
    }

    /// Stop all active snippets by toggling `enable-snippets`, the available
    /// GtkSourceView 5.20 cancellation mechanism.
    pub(crate) fn end_snippet(&self) {
        self.view.set_enable_snippets(false);
        self.view.set_enable_snippets(true);
    }

    /// The view as the subclass that paints ghost text and holds the extra carets.
    pub fn ghost_view(&self) -> Option<&multicaret::View> {
        self.view.downcast_ref::<multicaret::View>()
    }

    /// Count selected characters across carets using GTK offsets without
    /// reading selection text.
    pub fn selected_chars(&self) -> usize {
        let spans = match self.ghost_view() {
            Some(view) => view.selections(),
            None => self.buffer.selection_bounds().into_iter().collect(),
        };
        spans
            .iter()
            .map(|(start, end)| (end.offset() - start.offset()) as usize)
            .sum()
    }

    /// Whether this tab was built with ghost text wanted; read once by `ghost::install`.
    pub fn ghost_text_wanted(&self) -> bool {
        self.ghost_text.get()
    }

    /// Apply a ghost-text preference change: off clears immediately; on
    /// reenables requests against the restarted vault session (`Vault::set_ghost`).
    pub fn set_ghost_text(self: &Rc<Self>, on: bool) {
        self.ghost_text.set(on);
        self.lang.ghost.on.set(on);
        if !on {
            crate::ghost::clear(self);
        }
    }

    // --- links ---------------------------------------------------------------------------

    pub fn link_at_cursor(&self) -> Option<Link> {
        self.link_at_iter(&caret(&self.buffer))
    }

    /// Find a bare HTTP(S) or mailto URL at the caret in notes, code or plain text.
    pub fn url_at_cursor(&self) -> Option<String> {
        follow::url_under(&caret(&self.buffer)).map(|(_, url)| url)
    }

    /// The link under a pointer position in the view's own coordinates.
    pub fn link_at(&self, x: f64, y: f64) -> Option<Link> {
        let (bx, by) =
            self.view
                .window_to_buffer_coords(gtk::TextWindowType::Widget, x as i32, y as i32);
        let iter = crate::fold::iter_at_location(&self.view, bx, by)?;
        self.link_at_iter(&iter)
    }

    /// Find the link covering `iter` in character coordinates, converted
    /// once from parse byte ranges during analysis.
    fn link_at_iter(&self, iter: &gtk::TextIter) -> Option<Link> {
        let at = iter.offset();
        self.links
            .borrow()
            .iter()
            .find(|(range, _)| range.contains(&at))
            .map(|(_, link)| link.clone())
    }

    /// Called 1 s after the last edit and when focus leaves the view. Never fires while
    /// `disk_changed` is set.
    pub fn connect_autosave(self: &Rc<Self>, f: impl Fn(&Rc<Tab>) + 'static) {
        self.on_autosave.set(Rc::new(f));
    }

    /// Called after the re-highlight debounce, for the preview.
    pub fn connect_edited(self: &Rc<Self>, f: impl Fn(&Rc<Tab>) + 'static) {
        self.on_edited.set(Rc::new(f));
    }

    /// Called when the banner's button is pressed.
    pub fn connect_banner(self: &Rc<Self>, f: impl Fn(&Rc<Tab>) + 'static) {
        self.on_banner.set(Rc::new(f));
    }

    /// Called for a Ctrl+click in the view or the Go to Definition chord.
    pub fn connect_follow(self: &Rc<Self>, f: impl Fn(&Rc<Tab>) + 'static) {
        self.on_follow.set(Rc::new(f));
    }

    /// Called at most every 100 ms while the caret moves.
    pub fn connect_cursor(self: &Rc<Self>, f: impl Fn(&Rc<Tab>) + 'static) {
        self.on_cursor.set(Rc::new(f));
    }

    // --- edits ---------------------------------------------------------------------------

    fn on_changed(self: &Rc<Self>) {
        self.note_turn(true, false);
        // Ahead of the early return: a reload changes what the query matches too, and the
        // words it has.
        self.find_edited();
        self.words.set(None);
        if self.loading.get() {
            return;
        }
        lang::edited(self);
        // The other half of the rule above: an edit that leaves the caret where it was — Delete,
        // or a replacement over a selection — still answers the jump.
        self.clear_reveal();
        self.save.edits.set(self.save.edits.get() + 1);
        if !self.save.modified.replace(true) {
            self.page.set_title(&self.tab_title());
        }
        let instant = self.buffer.char_count() <= INSTANT;
        if instant {
            self.reanalyse();
        } else if self.flavour.is_note() {
            // Too long for a full pass inside a frame, so the line under the caret is styled now
            // and everything else waits: what a typist watches change is the line they are typing.
            let line = caret(&self.buffer).line();
            highlight::apply_line(&self.buffer, line);
        }
        // GtkSourceView pins a message to its line number, where the underline and the gutter mark
        // ride along with the text: one the edit moved to another line is laid again there.
        if let Some(kept) = self.relay.take() {
            self.lay_diagnostics(kept);
        }
        // An end-of-line message is cut to the room its line leaves (`diagnostics::fit`), and
        // typing on that line takes some of it: the caret's line is measured again now, styled,
        // rather than by the next publish.
        diagnostics::refit(
            &self.view,
            &self.annotations,
            &mut self.annotated.borrow_mut(),
            caret(&self.buffer).line(),
        );
        // The change bars are a second whole-buffer copy and a line diff against the committed
        // text, which is too much to spend on a keystroke however short the note is, and nothing
        // a typist watches: they follow the debounce at either size.
        self.debounce.call(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move || {
                tab.update_marks();
                if !instant {
                    tab.reanalyse();
                }
            }
        ));
        self.schedule_autosave();
    }

    /// Read buffer text once to refresh styling and Ctrl-hover links. Notify
    /// the preview, whose own debounce defers rerendering across keystrokes.
    fn reanalyse(self: &Rc<Self>) {
        self.analyse_text();
        self.on_edited.emit(self);
    }

    /// Refresh all text-derived state, including change bars, after whole
    /// replacements such as reloads or templates.
    fn analyse(&self) {
        self.analyse_text();
        self.update_marks();
    }

    /// Apply immediate note styling and link updates within the typing budget.
    /// Code uses language-scheme highlighting and needs no corresponding pass.
    fn analyse_text(&self) {
        match self.flavour {
            Flavour::Note => {
                let (analysis, offsets) =
                    wrap::refence(&self.view, || highlight::apply(&self.buffer));
                *self.links.borrow_mut() = analysis
                    .links
                    .into_iter()
                    .map(|link| {
                        let range =
                            offsets.char_of(link.range.start)..offsets.char_of(link.range.end);
                        (range, link)
                    })
                    .collect();
            }
            Flavour::Csv => self.csv.apply(&self.buffer),
            // Code is coloured by its language through the style scheme, with nothing to derive.
            Flavour::Code => {}
        }
        // The tags the sticky title reads are the ones that were just re-applied.
        self.update_sticky();
        if let Some(hosted) = self.hosted() {
            hosted.refresh();
        }
        self.conflicts.find();
    }

    /// Debounce change-bar diffing because it copies the whole buffer.
    fn update_marks(&self) {
        let head = self.head.borrow();
        let Some(head) = head.as_ref() else {
            return;
        };
        let lines = accent_core::diff::line_ops(head, &self.text());
        self.marks.set_marks(crate::marks::marks(
            &lines,
            self.buffer.line_count() as usize,
        ));
    }

    fn schedule_autosave(self: &Rc<Self>) {
        self.autosave.cancel();
        if self.save.disk_changed.get() {
            return;
        }
        self.autosave.call(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move || tab.autosave_now()
        ));
    }

    /// Autosave unless a changed-on-disk banner still needs the user's decision.
    ///
    /// Blocked saves leave the buffer and banner intact without prompting. Do not create another
    /// recovery file that the app cannot restore; the open buffer remains the source of truth.
    fn autosave_now(self: &Rc<Self>) {
        self.autosave.cancel();
        let modified = self.save.modified.get();
        if modified && may_save(modified, self.save.disk_changed.get()) {
            self.on_autosave.emit(self);
        }
    }

    fn on_cursor_moved(self: &Rc<Self>) {
        self.note_turn(false, true);
        self.cursor.call_once(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move || tab.on_cursor.emit(&tab)
        ));
    }

    /// Record edits and caret moves within one main-loop turn. An edit marks
    /// the caret as carried immediately; settle independent moves after the
    /// turn, since editing code may move the caret before inserting text.
    fn note_turn(self: &Rc<Self>, edited: bool, caret: bool) {
        if edited {
            self.moved.set(false);
        }
        let (was_edited, was_caret) = self.turn.get();
        if !was_edited && !was_caret {
            // Above the event sources' priority, so it runs before the next key is handled.
            glib::idle_add_local_full(
                glib::Priority::HIGH,
                glib::clone!(
                    #[weak(rename_to = tab)]
                    self,
                    #[upgrade_or]
                    glib::ControlFlow::Break,
                    move || {
                        if tab.turn.take() == (false, true) {
                            tab.moved.set(true);
                        }
                        glib::ControlFlow::Break
                    }
                ),
            );
        }
        self.turn.set((was_edited || edited, was_caret || caret));
    }

    /// Whether the last caret movement was independent of edits, for outline following.
    pub fn caret_moved(&self) -> bool {
        self.moved.get()
    }
}

impl Saves for Tab {
    fn save_state(&self) -> &SaveState {
        &self.save
    }

    fn key(&self) -> String {
        self.rel()
    }

    fn path(&self) -> PathBuf {
        Tab::path(self)
    }

    fn for_disk(&self) -> String {
        Tab::for_disk(self)
    }
}

impl Drop for Tab {
    /// Release the tab's font provider and language document. Dropping the
    /// three debounces cancels pending work; [`release`] frees the view.
    fn drop(&mut self) {
        lang::detach(self);
        if let (Some(display), Some(provider)) =
            (gdk::Display::default(), self.font.borrow_mut().take())
        {
            gtk::style_context_remove_provider_for_display(&display, &provider);
        }
        release(&self.view);
    }
}

/// Dispose an unused, detached view from an idle to release GtkSourceView gutter cycles.
///
/// GtkSourceView 5.20 retains its last painted view through GtkSourceGutterLines; a closed
/// view never paints again. Unparent scrolled children before disposal, leaving attached
/// views and other parent types untouched until safe.
pub(crate) fn release(view: &sourceview5::View) {
    let view = view.clone();
    glib::idle_add_local_once(move || {
        if view.root().is_some() {
            return;
        }
        if let Some(parent) = view.parent() {
            if let Some(clamp) = parent.downcast_ref::<adw::ClampScrollable>() {
                clamp.set_child(None::<&gtk::Widget>);
            } else if let Some(scroller) = parent.downcast_ref::<gtk::ScrolledWindow>() {
                scroller.set_child(None::<&gtk::Widget>);
            } else {
                return;
            }
        }
        // SAFETY: nothing of ours holds the view any more, and it is in no window and no parent.
        unsafe { view.run_dispose() };
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify the save gate preserves unsaved writing: external file changes
    /// require a banner decision rather than implicit overwrite.
    #[test]
    fn a_dirty_buffer_over_a_file_that_moved_is_never_written() {
        assert!(may_save(true, false), "the ordinary save");
        assert!(
            !may_save(true, true),
            "edits over a file that moved: refused"
        );
        assert!(
            may_save(false, true),
            "a clean buffer has nothing to lose, which is how a deleted note is written back"
        );
        assert!(may_save(false, false));
    }

    /// Verify late save results neither mark newer typing clean nor adopt
    /// an etag after the tab has moved to another buffer state.
    #[test]
    fn a_landed_save_cleans_only_the_buffer_it_wrote() {
        let etag = |n| Etag {
            mtime_ns: n,
            size: 1,
            ino: 1,
        };
        let (was, wrote) = (Some(etag(1)), etag(2));
        assert!(
            matches!(landing(5, 5, was, was, Ok(wrote)), Landing::Clean(e) if e == wrote),
            "nothing typed since: clean at the new etag"
        );
        assert!(
            matches!(landing(5, 7, was, was, Ok(wrote)), Landing::Behind(e) if e == wrote),
            "typed into since: still dirty, gated on the new etag"
        );
        assert!(
            matches!(landing(5, 5, was, Some(etag(3)), Ok(wrote)), Landing::Stale),
            "a reload put its own etag in meanwhile: the answer is about a buffer that is gone"
        );
        assert!(matches!(
            landing(
                5,
                7,
                was,
                was,
                Err(Error::Offline("not connected".to_string()))
            ),
            Landing::Failed(Error::Offline(_))
        ));
    }
}
