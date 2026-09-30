//! One editor tab: a `sourceview5::View` on a note, plus its etag, banner and the debounced
//! work that hangs off a keystroke.
//!
//! Finding and replacing live in `find.rs`, one bar per window: the bar drives the tab's
//! `SearchContext` from the outside, so the same widgets serve every tab and stay on screen while
//! presentation mode has hidden the tab stack.
//!
//! Nothing here knows about the app. What the tab has to say goes out through a `connect_*`
//! callback and what it needs from the vault arrives as a closure, so a tab can be built, moved
//! and closed without `main` reaching inside it.

use crate::{diagnostics, diff, fold, highlight, lang, multicaret, wrap};
use accent_api::{Diagnostic, Fold, Vault};
use accent_core::fs::{self, Etag, SaveError};
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
pub(crate) use drag::content as drag_content;
use follow::Follow;
#[cfg(feature = "bench")]
pub(crate) use keys::press;
use lines::primary_paste;
#[cfg(feature = "bench")]
pub(crate) use lines::{delete_line, duplicate_line, newline_below, paste_primary, toggle_comment};
pub(crate) use lines::{line_clipboard, pressed_at};
use open::build;
pub use open::open;
pub use page::default_font;
use page::{GUTTER, line_numbers};
pub(crate) use page::{font_css, install_font, next_view_name, set_margins};
use search::{matched, mute};
pub(crate) use text::{caret, line_end, line_prefix};

/// How long a long note waits after the last keystroke before it is re-analysed.
const DEBOUNCE: Duration = Duration::from_millis(150);
/// Notes at or below this many characters get a *full* re-style on the keystroke. Measured cost
/// of a full pass, tag churn included, which dominates the parsing: 0.7 ms at 2 KB, 2.5 ms at
/// 8 KB, 10 ms at 32 KB, 21 ms at 64 KB. This size stays inside a frame and still covers the notes
/// people actually write (median 3.5 KB in the test vault). A longer note keeps the debounce for
/// the full pass, because a pass that outlasts a frame is felt as input lag — but it is not left
/// unstyled while typing: [`highlight::apply_line`] re-tags the caret's line on every keystroke,
/// so markup appears as it is typed the way Apostrophe does it either side of the threshold.
const INSTANT: i32 = 16 * 1024;
/// DESIGN.md, Motion: save 1 s after the last edit.
const AUTOSAVE: Duration = Duration::from_secs(1);
/// The cursor callback drives the preview's scroll sync; 100 ms is below what the eye follows.
const CURSOR: Duration = Duration::from_millis(100);
/// A callback the app registered. Stored behind an `Rc` so it can be cloned out of its cell
/// before it runs: a callback is free to reach back into the tab that called it.
type Hook = RefCell<Option<Rc<dyn Fn(&Rc<Tab>)>>>;

/// Whether a save may write the file under a tab.
///
/// The one rule every save path shares. A buffer whose file moved underneath it holds the only
/// copy of its edits *and* the answer to a question the banner is still asking, so nothing
/// writes until that answer is given — which is the rule VS Code follows for the same reason.
/// A clean buffer has nothing to lose and is let through: that is how the "deleted on disk"
/// banner writes the note back.
///
/// The etag gate in `fs::write_note` is still the last word; this is what keeps a save from
/// being attempted at all once the tab already knows the answer.
pub fn may_save(modified: bool, disk_changed: bool) -> bool {
    !(modified && disk_changed)
}

/// A spell checker over `buffer`, its suggestions in `view`'s context menu, switched off until
/// told otherwise. The adapter *is* the action group its own menu items resolve through.
pub fn spell_adapter(
    buffer: &sourceview5::Buffer,
    view: &sourceview5::View,
) -> libspelling::TextBufferAdapter {
    let adapter = libspelling::TextBufferAdapter::new(buffer, &libspelling::Checker::default());
    view.insert_action_group("spelling", Some(&adapter));
    view.set_extra_menu(Some(&adapter.menu_model()));
    adapter
}

/// A save on its way to the file, as its tab keeps it: what the write was started against, and
/// the channel its answer arrives on. There is at most one per tab.
pub struct Flight {
    /// [`SaveState::edits`] when the text was taken.
    pub started: u64,
    /// The etag the write was gated on.
    pub expected: Option<Etag>,
    /// A Ctrl+S, which says "Saved" when it lands.
    pub explicit: bool,
    pub answer: std::sync::mpsc::Receiver<Result<Etag, SaveError>>,
}

/// What a save that has just landed means for its tab.
#[derive(Debug)]
pub enum Landing {
    /// The buffer is still what was written: clean, at the new etag.
    Clean(Etag),
    /// The file holds what was written, but the buffer has been typed into since: still dirty,
    /// and the next save is gated on the new etag.
    Behind(Etag),
    /// A reload put other text and its own etag in the tab meanwhile, so the answer describes a
    /// buffer that is gone and changes nothing.
    Stale,
    /// Refused or failed, for a buffer that still holds its edits.
    Failed(SaveError),
}

/// Decide a [`Landing`] from the tab as the save found it (`started`, `expected`) and as it is
/// now (`edits`, `holding`).
pub fn landing(
    started: u64,
    edits: u64,
    expected: Option<Etag>,
    holding: Option<Etag>,
    written: Result<Etag, SaveError>,
) -> Landing {
    if holding != expected {
        return Landing::Stale;
    }
    match written {
        Ok(etag) if started == edits => Landing::Clean(etag),
        Ok(etag) => Landing::Behind(etag),
        Err(e) => Landing::Failed(e),
    }
}

/// What a tab keeps about the file under it for the save path (`save.rs`): a note's tab and a
/// diagram's each hold one, so the two save through the same code.
#[derive(Default)]
pub struct SaveState {
    pub etag: Cell<Option<Etag>>,
    pub modified: Cell<bool>,
    /// Someone else changed the file under a dirty tab. Autosave stops until the user has
    /// answered the banner, so a conflict is never resolved behind their back.
    pub disk_changed: Cell<bool>,
    /// Counts every change to the buffer, typed or loaded, so a save that lands can tell whether
    /// the buffer is still the text it wrote.
    pub edits: Cell<u64>,
    /// The save on its way, if one is.
    pub flight: RefCell<Option<Flight>>,
    /// A save asked for while one was on its way, run when that lands; `Some(true)` if any of the
    /// asks was a Ctrl+S.
    pub save_again: Cell<Option<bool>>,
    /// The watcher spoke while a save was on its way, so its stat is taken again once the save
    /// has landed and the tab holds the etag it wrote.
    pub recheck: Cell<bool>,
}

impl SaveState {
    /// A file as it was just read, at `etag`.
    pub fn at(etag: Etag) -> SaveState {
        SaveState {
            etag: Cell::new(Some(etag)),
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

/// What kind of text a tab holds.
///
/// Prose and code share every mechanism a tab has — the etag, autosave, find, zoom, the banner —
/// and differ only in how they are shown, so this is a field rather than a second tab type.
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

/// The preferences a tab is built with. A struct rather than five positional arguments, which is
/// what they were until code tabs needed a sixth.
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
    /// The committed text this buffer is drawn against, and the bars in the gutter that say how
    /// it differs. `None` when the file is in no repository, or is not tracked in one.
    head: RefCell<Option<String>>,
    marks: crate::marks::Renderer,
    /// The page's content and the document inside it, kept so a comparison can put a paned
    /// between the two and take it away again. See [`Tab::compare`].
    content: gtk::Box,
    document: gtk::Box,
    comparing: RefCell<Option<Comparing>>,
    /// The buttons a comparison lays over the editor, kept across comparisons: see `diff::Pool`.
    overlays: Rc<diff::Pool>,
    /// Git's conflict markers in the text, tinted and with their Accept buttons over them.
    conflicts: Rc<crate::conflict::Conflicts>,
    /// Kept for [`Tab::scroll_lines`] and for the scrollbar the minimap replaces.
    scroller: gtk::ScrolledWindow,
    /// The width cap on the document column, sized by [`Tab::set_clamp`]. Scrollable, so the
    /// view below it is the scrolled window's own scrollable child rather than a viewport's.
    clamp: adw::ClampScrollable,
    /// What the cap is computed from: the document zoom and the column's percentage of the
    /// editor's width. Kept here because the editor is also resized from the outside, and a
    /// resize has to recompute the cap without being told the other two again.
    zoom: Cell<f64>,
    column: Cell<u32>,
    /// Whether ghost text is wanted here. Mirrored onto `lang.ghost` so the request path reads
    /// one cell, and kept here so a tab built while the preference was off can be turned on.
    ghost_text: Cell<bool>,
    map: sourceview5::Map,
    /// The optional line-number gutter; hidden unless the preference turns it on.
    numbers: sourceview5::GutterRendererText,
    /// The sticky block title over the top of the view, and the bar it sits on. Hidden until
    /// something is scrolled out of sight above the first visible line.
    sticky: gtk::Label,
    sticky_bar: gtk::Box,
    /// What this tab holds, fixed when it opened. Everything markdown-specific — the styling
    /// spans, completion, spellcheck, the column cap, the hanging heading markers — asks this
    /// first, so a source file gets a source editor and a note is unchanged.
    flavour: Flavour,
    /// The file used CRLF line endings. The buffer never sees them and every save puts them back,
    /// so editing one line of a DOS file does not rewrite every line of it.
    crlf: Cell<bool>,
    /// The bytes were not valid UTF-8 and what is shown is a lossy reading of them. The view is
    /// not editable, because writing the buffer back would replace every undecodable byte with a
    /// replacement character.
    lossy: Cell<bool>,
    pub page: adw::TabPage,
    pub banner: adw::Banner,
    pub save: SaveState,
    /// Every question standing about this file. The banner shows one of them ([`banner_alert`]);
    /// the rest wait rather than being overwritten.
    alerts: RefCell<Vec<Alert>>,
    context: sourceview5::SearchContext,
    /// Every other occurrence of what is selected, muted.
    ///
    /// A tag of our own rather than a second `SearchContext` fed the selection, which is the
    /// shorter way to write it: two search contexts on one buffer each keep their tag at the top
    /// of the tag table and re-raise it as they rescan, so which of the two colours paints an
    /// overlap is a race. Measured under `ACCENT_BENCH_OCCUR`, the find bar lost it — every match
    /// rendered in the muted colour with the bar open on the selected word. An ordinary tag made
    /// before the context stays under it whatever either of them does.
    occurrence_tag: gtk::TextTag,
    /// What that tag is showing, so a caret move that changes nothing re-tags nothing.
    occurrence_query: RefCell<Option<String>>,
    /// Where the jump that opened this note landed, in the scheme's own match colour. See
    /// [`Tab::reveal_range`].
    reveal_tag: gtk::TextTag,
    /// Whether that tag is painting anything, so an ordinary keystroke costs no tag walk.
    revealed: Cell<bool>,
    spell: RefCell<Option<libspelling::TextBufferAdapter>>,
    /// The note's links, each with the character range it covers. Characters and not the bytes
    /// the parse reports them in: the pointer asks which link it is over on every motion event
    /// while Ctrl is held, and translating a byte offset there meant copying the text up to the
    /// pointer each time.
    links: RefCell<Vec<(Range<i32>, Link)>>,
    /// The underline under what a Ctrl+click would follow, and what it is showing. See
    /// [`follow`].
    follow_tag: gtk::TextTag,
    follow: RefCell<Follow>,
    /// What the language server last said about this file, and the provider that shows the loud
    /// half of it at the ends of the lines. Kept because the gutter tooltip and the status bar
    /// both read it back after the paint.
    diagnostics: RefCell<Vec<Diagnostic>>,
    annotations: sourceview5::AnnotationProvider,
    /// Whether this tab's diagnostics are kept out of the text: no squiggles, no gutter marks and
    /// no messages at the ends of the lines. What the server said is still held above, so the
    /// hover still answers for a line and the status bar still counts them.
    diagnostics_hidden: Cell<bool>,
    /// How many end-of-line messages the last paint put up, which is fewer than the lines with
    /// one whenever a comparison has collapsed some of them. Only `ACCENT_BENCH_COMPARE` reads
    /// it: the provider cannot be counted back.
    annotated: Cell<usize>,
    /// The blocks the server says can be hidden, and the chevrons beside their headers. What is
    /// hidden right now lives in the buffer's own tag, not here.
    folds: RefCell<Vec<Fold>>,
    fold_renderer: fold::Renderer,
    font: RefCell<Option<gtk::CssProvider>>,
    /// A watch on the file itself, for a tab no vault watcher covers. `None` for everything
    /// inside a vault, which the worker already reports on.
    monitor: RefCell<Option<gio::FileMonitor>>,
    /// Set while we replace the buffer text ourselves, so `changed` does not mark it dirty.
    loading: Cell<bool>,
    /// The last template pushed into the view, kept only to ask whether its stops are still being
    /// walked: a snippet drops its buffer when it finishes, so that is the question's answer.
    snippet: RefCell<Option<sourceview5::Snippet>>,
    /// The paste that makes a URL over a selection a link, which `Ctrl+Shift+V` holds back.
    paste_link: glib::SignalHandlerId,
    /// The post-edit pass: the change bars, and the styling a note too long to restyle inside a
    /// frame owes the rest of its text.
    debounce: crate::widgets::Debounce,
    autosave: crate::widgets::Debounce,
    /// First-wins ([`crate::widgets::Debounce::call_once`]): a caret held on an arrow key must
    /// still tell the outline where it is, rather than be pushed off for as long as it moves.
    cursor: crate::widgets::Debounce,
    /// Whether this turn of the main loop has changed the text, and whether it has moved the
    /// caret; settled into `moved` once the turn is over (see [`Tab::note_turn`]).
    turn: Cell<(bool, bool)>,
    /// The caret was last moved rather than carried by an edit: see [`Tab::caret_moved`].
    moved: Cell<bool>,
    /// The end-of-line messages laid again for a new column width, once a drag has settled.
    refit: crate::widgets::Debounce,
    on_autosave: Hook,
    on_edited: Hook,
    on_banner: Hook,
    on_cursor: Hook,
    on_follow: Hook,
    /// This tab's document on the vault's language layer: what it can answer, what it last
    /// answered, and the refresh that is still in flight. Empty for a tab outside every vault.
    pub lang: lang::State,
}

/// Where a reload has to put the reader back: the caret, and the line at the top of the view with
/// where that line sat, so the same text goes back under the same edge however far the new bytes
/// move it.
#[derive(Clone, Copy)]
struct Anchor {
    offset: i32,
    top_line: i32,
    top_y: i32,
    scrolled: f64,
}

/// The language for `path`, or `None` when GtkSourceView knows none for it.
///
/// The content type is guessed first and handed over with the name, which is what makes a file
/// with no extension work: gio matches `Makefile` and `Dockerfile` by name and falls back to
/// sniffing the bytes, so a `#!/bin/sh` script with no suffix still lands on `sh`.
fn guess_language(path: &Path, text: &str) -> Option<sourceview5::Language> {
    let (content_type, _) = gio::content_type_guess(Some(path), text.as_bytes());
    sourceview5::LanguageManager::default().guess_language(Some(path), Some(&content_type))
}

// ------------------------------------------------------------------------------------- helpers

/// GtkSourceView paints its background from its own style scheme, so unlike every other widget in
/// the window it has to be told about the theme explicitly.
pub fn sync_scheme(buffer: &sourceview5::Buffer) {
    let id = crate::theme::scheme_id(adw::StyleManager::default().is_dark());
    let scheme = sourceview5::StyleSchemeManager::default().scheme(id);
    buffer.set_style_scheme(scheme.as_ref());
}

/// The language a file called `key` holding `text` is coloured as.
pub fn language_for(key: &str, text: &str) -> Option<sourceview5::Language> {
    guess_language(Path::new(key), text)
}

/// Watch the file at `path` and call `f` when someone else writes it, for as long as the monitor
/// returned is kept. `None` when the file cannot be watched.
///
/// Only for a tab outside every vault: inside one, the vault's own watcher reports the change
/// and knows which writes were ours, which a bare file monitor cannot.
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

    /// Point the gutter's change bars at the committed text, or take them away with `None`. The
    /// caller is the Git pane, which is the only thing in the window that has asked git anything.
    pub fn set_head(&self, head: Option<String>) {
        let showing = head.is_some();
        *self.head.borrow_mut() = head;
        self.marks.set_visible(showing);
        match showing {
            true => self.update_marks(),
            false => self.marks.set_marks(Vec::new()),
        }
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

    /// The buffer in the shape the file should hold it: trailing whitespace off code lines, and
    /// the line endings it arrived with. A note is written exactly as typed — two trailing spaces
    /// are a hard line break in markdown.
    pub fn for_disk(&self) -> String {
        fs::for_disk(&self.text(), self.crlf.get(), !self.flavour.is_note())
    }

    /// How the file is encoded and how its lines end, for the readout in the header.
    pub fn encoding_label(&self) -> String {
        let encoding = match self.lossy.get() {
            true => "Not UTF-8",
            false => "UTF-8",
        };
        let ending = match self.crlf.get() {
            true => "CRLF",
            false => "LF",
        };
        format!("{encoding} · {ending}")
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

    /// Replace the buffer with `text` without marking the tab dirty. Callers are either loading
    /// from disk or about to write what they just put in.
    ///
    /// The language layer is told all the same. `loading` is what keeps `on_changed` out of this,
    /// and with it the edit event that would otherwise carry the news: without this a silent
    /// reload left the server, the symbols, the folds and the diagnostics describing the text the
    /// file used to hold until the next keystroke.
    pub fn set_text(self: &Rc<Self>, text: &str) {
        self.save.edits.set(self.save.edits.get() + 1);
        self.loading.set(true);
        self.buffer.set_text(text);
        self.loading.set(false);
        self.analyse();
        lang::changed(self);
    }

    /// Whether the buffer is being replaced by us rather than typed in. The handlers that watch
    /// `insert-text` ask before acting: a template pushed back through the buffer is not somebody
    /// typing an opening bracket, and should raise neither signature help nor a suggestion.
    pub(crate) fn is_loading(&self) -> bool {
        self.loading.get()
    }

    pub fn mark_clean(&self, etag: Etag) {
        self.save.etag.set(Some(etag));
        self.save.modified.set(false);
        self.save.disk_changed.set(false);
        self.page.set_title(&self.tab_title());
    }

    /// Silent reload for a clean tab: the file changed on disk and there is nothing to lose.
    ///
    /// The bytes are read on a worker thread, the way `open.rs` reads a file into a new tab: a
    /// watcher can fire this on any file, and a synchronous read of a large one held the window
    /// for as long as the disk took. They come from `vault` as they did there, since on a remote
    /// vault the tab's own path is on the host; a loose file has none and reads its path. `done`
    /// is told how it ended, once.
    pub fn reload_keep_cursor(
        self: &Rc<Self>,
        vault: Option<Arc<Vault>>,
        done: impl Fn(&Rc<Tab>, std::io::Result<()>) + 'static,
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
                None => fs::read_text(&path),
            })
            .await;
            let Some(tab) = weak.upgrade() else { return };
            let text = match read {
                Some(Ok(fs::Read::Text(text))) => text,
                // It stopped being text while we had it open. The buffer keeps the last readable
                // version rather than showing the user a screen of replacement characters.
                Some(Ok(_)) => {
                    return done(
                        &tab,
                        Err(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "not text any more",
                        )),
                    );
                }
                Some(Err(e)) => return done(&tab, Err(e)),
                None => return done(&tab, Err(std::io::Error::other("the reader stopped"))),
            };
            tab.adopt_reload(text, anchor);
            done(&tab, Ok(()));
        });
    }

    /// Put a reload's text in the buffer and the reader back where they were looking.
    fn adopt_reload(self: &Rc<Self>, text: fs::Text, anchor: Anchor) {
        self.crlf.set(text.crlf);
        self.lossy.set(text.lossy);
        let etag = text.etag;
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
        if let Some(compare) = self.comparison() {
            compare.restyle();
        }
    }

    /// Git's conflict markers in this tab, for the palette's Accept and Next Conflict commands.
    pub fn conflicts(&self) -> &crate::conflict::Conflicts {
        &self.conflicts
    }

    /// What the language server last said about this file. Replaces the previous answer whole,
    /// which is what a publish is; an empty list clears the tab.
    pub fn set_diagnostics(&self, items: Vec<Diagnostic>) {
        *self.diagnostics.borrow_mut() = items;
        self.paint_diagnostics();
    }

    /// Show what the server said in the text, or keep it out of it: the counter in the status bar
    /// is the switch. Per tab, as the count beside it is — the readout says what *this* document
    /// has, and pressing it answers for the document it is counting.
    pub fn hide_diagnostics(&self, hidden: bool) {
        if self.diagnostics_hidden.replace(hidden) != hidden {
            self.paint_diagnostics();
        }
    }

    pub fn diagnostics_hidden(&self) -> bool {
        self.diagnostics_hidden.get()
    }

    /// Lay the stored answer over the text, or nothing at all while it is hidden — which is the
    /// same pass, so hiding and showing go through the code a publish does rather than a second
    /// way of lifting the same tags.
    pub(super) fn paint_diagnostics(&self) {
        let items = self.diagnostics.borrow();
        let painted: &[Diagnostic] = match self.diagnostics_hidden.get() {
            true => &[],
            false => &items,
        };
        self.annotated.set(diagnostics::render(
            &self.view,
            &self.buffer,
            &self.annotations,
            painted,
        ));
    }

    /// How many end-of-line messages the last paint put up. `ACCENT_BENCH_COMPARE=diag:` only.
    #[cfg(feature = "bench")]
    pub fn annotated(&self) -> usize {
        self.annotated.get()
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

    /// What the server says can be folded. Whatever is hidden stays hidden if its header survived
    /// the re-analysis, at wherever the line has moved to.
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

    /// Hide `f`, unless a comparison is shown. Its collapsed runs hide lines too, and a line where
    /// a run of one kind ends inside or beside a run of the other is laid out by GTK 4.22 as a
    /// blank row: asked for the iter in that row's pixels-below-lines, as GtkSourceView asks at
    /// the top and bottom of the screen on every frame, `gtk_text_layout_get_iter_at_position`
    /// aborts ("Byte index … is off the end of the line"). So a comparison opens the folds and
    /// shuts them again when it goes (`editor/compare.rs`).
    fn shut(&self, f: Fold) {
        if self.comparing.borrow().is_none() {
            fold::fold(self.text_buffer(), f);
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

    /// What every fold command ends with: the chevrons redrawn, and the diagnostics laid again.
    /// A line that has just gone behind a header has no row of its own to put a message on, and
    /// one that has come back out wants its message back — the same rule a comparison's collapsed
    /// runs follow, and the same pass.
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

    /// The user chose to lose this buffer's unsaved edits: it stops counting as dirty, so nothing
    /// downstream tries to save it on the way out.
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

    /// How wide one indent is, from preferences: what a tab character is worth on screen and one
    /// wrap level (`wrap.rs`), in a note as in code. A note's Tab still writes a literal tab or a
    /// list item's own indent; four columns is also the tab stop CommonMark reads a note's tab at.
    pub fn set_indent_width(&self, columns: u32) {
        self.view.set_tab_width(columns);
        if let Some(compare) = self.comparison() {
            compare.follow_editor(false);
        }
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

    /// VS Code's Add Cursor Above / Below. Multi-caret lives on the view subclass; the tab keeps
    /// the plain `sourceview5::View` type so nothing else has to know about it.
    pub fn add_caret(&self, below: bool) {
        if let Some(view) = self.view.downcast_ref::<multicaret::View>() {
            view.add_caret(below);
        }
    }

    /// JetBrains' Add Caret at Next Occurrence: with nothing selected the first press selects the
    /// word under the caret, and each press after it adds a caret selecting the next occurrence.
    pub fn add_next_occurrence(&self) {
        let Some(view) = self.view.downcast_ref::<multicaret::View>() else {
            return;
        };
        if self.buffer.has_selection() {
            view.add_next_occurrence();
        } else {
            self.select_word();
        }
    }

    /// VS Code's Select All Occurrences: a caret on every occurrence of the selection, or of the
    /// word under the caret where nothing is selected.
    pub fn select_all_occurrences(&self) {
        let Some(view) = self.view.downcast_ref::<multicaret::View>() else {
            return;
        };
        if self.buffer.has_selection() || self.select_word() {
            view.select_all_occurrences();
        }
    }

    /// Select the word under the caret, or the one it has just finished, caret at its end: a word
    /// being what Ctrl+hover underlines (`follow::word_at`), underscores and all. Selecting moves
    /// the primary caret, which ends a column. Says whether there was a word.
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

    /// Focus mode's line fade, which the view subclass paints (`fade.rs`).
    pub fn set_fade(&self, on: bool) {
        if let Some(view) = self.view.downcast_ref::<multicaret::View>() {
            view.set_fade(on);
        }
    }

    /// The minimap, which fades with the chrome.
    pub fn minimap(&self) -> &gtk::Widget {
        self.map.upcast_ref()
    }

    /// Park `snippet` in the view at `at` and remember it, so a Tab pressed while its stops are
    /// still being walked goes to the template rather than to a suggestion.
    pub(crate) fn push_snippet(&self, snippet: &sourceview5::Snippet, at: &mut gtk::TextIter) {
        self.view.push_snippet(snippet, Some(at));
        *self.snippet.borrow_mut() = Some(snippet.clone());
    }

    /// Whether the completion popup is on screen, asked of the widgets on this very press.
    ///
    /// While the popup is up it owns the keyboard and nothing else in the view may answer a key,
    /// so an answer that can go stale silences the lot: this used to be a cell mirroring the
    /// completion's `show` and `hide`, and a `hide` that never arrived left every key — Return
    /// and Tab with it — to the view for the rest of this tab's life. Derived, there is nothing
    /// left to strand: see [`keys::popup_visible`] for what the widgets are asked.
    pub(crate) fn popup_shown(&self) -> bool {
        keys::popup_visible(&self.view)
    }

    /// Whether a template's stops are still being walked. A snippet lets its buffer go when it
    /// finishes, which is the only thing GtkSourceView 5.20 says about it from the outside.
    pub(crate) fn snippet_active(&self) -> bool {
        self.snippet
            .borrow()
            .as_ref()
            .is_some_and(|snippet| snippet.buffer().is_some())
    }

    /// Stop walking the template's stops, leaving the rest where they are. GtkSourceView 5.20 has
    /// no call for it but turning `enable-snippets` off, which ends every snippet in the view.
    pub(crate) fn end_snippet(&self) {
        self.view.set_enable_snippets(false);
        self.view.set_enable_snippets(true);
    }

    /// The view as the subclass that paints ghost text and holds the extra carets.
    pub fn ghost_view(&self) -> Option<&multicaret::View> {
        self.view.downcast_ref::<multicaret::View>()
    }

    /// Whether this tab was built with ghost text wanted; read once by `ghost::install`.
    pub fn ghost_text_wanted(&self) -> bool {
        self.ghost_text.get()
    }

    /// The preference changed under an open tab. Off clears whatever is on screen at once; on
    /// only arms the path, since the session behind it is decided when the document is opened.
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

    /// The bare `http(s)://` or `mailto:` URL under the caret, in any text: what a `.txt` or a
    /// code comment has for links, and what a note has outside its markdown ones.
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

    /// The link covering `iter`, compared in the buffer's own coordinates: the translation from
    /// the parse's byte ranges happened once, when the note was analysed.
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
        *self.on_autosave.borrow_mut() = Some(Rc::new(f));
    }

    /// Called after the re-highlight debounce, for the preview.
    pub fn connect_edited(self: &Rc<Self>, f: impl Fn(&Rc<Tab>) + 'static) {
        *self.on_edited.borrow_mut() = Some(Rc::new(f));
    }

    /// Called when the banner's button is pressed.
    pub fn connect_banner(self: &Rc<Self>, f: impl Fn(&Rc<Tab>) + 'static) {
        *self.on_banner.borrow_mut() = Some(Rc::new(f));
    }

    /// Called for a Ctrl+click in the view or the Go to Definition chord.
    pub fn connect_follow(self: &Rc<Self>, f: impl Fn(&Rc<Tab>) + 'static) {
        *self.on_follow.borrow_mut() = Some(Rc::new(f));
    }

    /// Called at most every 100 ms while the caret moves.
    pub fn connect_cursor(self: &Rc<Self>, f: impl Fn(&Rc<Tab>) + 'static) {
        *self.on_cursor.borrow_mut() = Some(Rc::new(f));
    }

    /// Cloned out of its cell before it runs: a callback may reach back into this tab, and a
    /// live borrow here would be a panic waiting for the first re-entrant call.
    fn emit(self: &Rc<Self>, hook: &Hook) {
        let f = hook.borrow().clone();
        if let Some(f) = f {
            f(self);
        }
    }

    // --- edits ---------------------------------------------------------------------------

    fn on_changed(self: &Rc<Self>) {
        self.note_turn(true, false);
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

    /// Re-read the buffer once and refresh everything derived from it: the styling tags, and the
    /// link table the Ctrl+hover underline reads. The preview listens on `on_edited` and
    /// debounces its own re-render, so calling this per keystroke only re-arms that timer.
    fn reanalyse(self: &Rc<Self>) {
        self.analyse_text();
        self.emit(&self.on_edited);
    }

    /// Everything this tab's text implies, the change bars included: what a reload or a template
    /// needs, where the whole document has moved at once.
    fn analyse(&self) {
        self.analyse_text();
        self.update_marks();
    }

    /// The half of [`Tab::analyse`] a keystroke can afford. A note gets its styling spans and its
    /// link table; code gets nothing, because the style scheme colours it from the language.
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
            Flavour::Csv => highlight::apply_csv(&self.buffer),
            // Code is coloured by its language through the style scheme, with nothing to derive.
            Flavour::Code => {}
        }
        // The tags the sticky title reads are the ones that were just re-applied.
        self.update_sticky();
        if let Some(compare) = self.comparison() {
            compare.refresh();
        }
        self.conflicts.find();
    }

    /// Redraw the gutter's change bars from the committed text. On the debounce, not the
    /// keystroke: it copies the whole buffer and diffs it against the committed text.
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

    /// Save now, unless the file changed underneath us: the user is looking at a banner asking
    /// what to do about it, and writing over the answer they have not given yet is not an option.
    ///
    /// A blocked autosave is a plain no-op, with the banner and the tab's dot as the only signal.
    /// It cannot ask — a modal on every focus change and every idle second is not something
    /// anyone can work through — and it deliberately does not write the buffer anywhere else
    /// either: a second copy nothing in the app ever reads back is a second source of truth, and
    /// the buffer is not going anywhere while the window is open.
    fn autosave_now(self: &Rc<Self>) {
        self.autosave.cancel();
        let modified = self.save.modified.get();
        if modified && may_save(modified, self.save.disk_changed.get()) {
            self.emit(&self.on_autosave);
        }
    }

    fn on_cursor_moved(self: &Rc<Self>) {
        self.note_turn(false, true);
        self.cursor.call_once(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move || tab.emit(&tab.on_cursor)
        ));
    }

    /// Note that this turn of the main loop changed the text or moved the caret. A caret that
    /// moves in the same turn as an edit is carried by it — a keystroke, a paste, an undo, which
    /// puts the caret back after its edit — and only one that moves without an edit is a move.
    /// An edit says so at once; a move is settled once the turn is over, since the order of the
    /// two is the editing code's: a list continuation may place the caret before it inserts.
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

    /// Whether the caret was last moved — a click, an arrow key, a jump — rather than carried
    /// along by an edit. What the Outline pane follows.
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
    /// A closed tab takes its font provider and its document on the language layer with it; the
    /// three `Debounce`s cancel whatever they are holding as they drop. The view is let go of by
    /// the minimap and then freed ([`release`]).
    fn drop(&mut self) {
        lang::detach(self);
        if let (Some(display), Some(provider)) =
            (gdk::Display::default(), self.font.borrow_mut().take())
        {
            gtk::style_context_remove_provider_for_display(&display, &provider);
        }
        self.map.set_property("view", None::<&sourceview5::View>);
        release(&self.view);
    }
}

/// Free `view`, which nothing of ours uses any more, once it has left its window.
///
/// GtkSourceView 5.20's gutter keeps the view of its last paint (`GtkSourceGutterLines`) until it
/// paints again, and a view that has left its window never does: view, gutter and buffer outlived
/// every closed tab and every comparison's read-only column. Disposing the view drops its gutters
/// and that reference with them. A widget is disposed only unparented, so the view is taken out
/// of its scrolled parent first; one still in a window, or in a parent of another kind, is left
/// alone. From an idle, because a tab goes while its page is still being closed.
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

    /// The save gate, which is the one thing in this file that costs the user their writing when
    /// it is wrong. VS Code's rule: a file that moved in the background is a question, and a save
    /// is not an answer to it.
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

    /// A background save lands on a tab that may have moved on. Getting this wrong either marks
    /// unsaved typing clean, which loses it at the next close, or trusts an etag the tab no
    /// longer holds.
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
            landing(5, 7, was, was, Err(SaveError::Offline)),
            Landing::Failed(SaveError::Offline)
        ));
    }
}
