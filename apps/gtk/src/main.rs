//! accent desktop app: GTK4 + libadwaita shell.
//!
//! `accent [vault-dir] [note.md]`. Without a path the start screen picks a vault; with one,
//! [`accent_api::Vault`] opens the index and the tree is filled straight from it, while the
//! worker thread reconciles and watches in the background. The window is never blocked, and every
//! change the vault reports arrives here as an [`Event`].

mod actions;
mod askpass;
mod bench;
mod build;
mod comment;
mod completion;
mod connect;
mod diagnostics;
mod diff;
mod doc;
mod editor;
mod events;
mod fileops;
mod find;
mod fold;
mod ghost;
mod git;
mod highlight;
mod hover;
mod lang;
mod layout;
mod marks;
mod multicaret;
mod nav;
mod open;
mod palette;
mod paned;
mod panes;
mod pdf;
mod preview;
mod references;
mod ring;
mod save;
mod session;
mod settings;
mod shell;
mod sidebar;
mod signature;
mod start;
mod statusbar;
mod terminal;
mod theme;
mod tree;
mod typing;
mod wire;
mod zoom;

use accent_api::{Config, Etag, Event, Location, SaveError, Session, Vault, ssh};
use accent_core::config::PdfZoom;
use accent_core::index::Phase;
use accent_core::markdown::LinkKind;
use actions::{
    ACTIONS, accels_for, install_actions, label_of, menu_button, mode_switcher, nav_action,
    pane_rect, tab_menu,
};
use adw::prelude::*;
use build::{build_window, install_document_font};
use doc::{Doc, Kind};
use editor::{Alert, Flavour, Prefs, Tab};
use events::start_events;
use gtk::{gdk, gio, glib, graphene};
use layout::{Mode, Presenting};
use open::Opened;
use panes::{Pane, Place, Side, Spot, Zone};
// The widget and the tab kept the names the rest of the window calls them by when
// they moved into `pdf/`.
pub(crate) use pdf as pdfview;
pub(crate) use pdf::tab as pdftab;
use references::{PdfAnchor, char_range, reference_target, split_pdf_anchor};
use session::Corpus;
use shell::Shell;
use sourceview5::prelude::ViewExt as _;
use std::cell::{Cell, OnceCell, RefCell};
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};
use wire::{choice_row, wire_pane, wire_tree, wire_window, written_at};
use zoom::{picture_of, stepped_zoom, zoom_on_wheel};

const APP_ID: &str = "io.github.stroblme.Accent";

/// Full-text hits the sidebar shows; beyond this the list stops being scannable.
const SEARCH_LIMIT: usize = 100;
/// DESIGN.md, Motion: the preview re-renders 300 ms after the last edit, and the status bar's
/// word count is read again on the same beat.
const RENDER: Duration = Duration::from_millis(300);
/// How often the tree may be re-read while the first index is still running, in microseconds:
/// often enough that a cold start fills in as it goes, rarely enough to stay off the main loop.
const TREE_REPAINT: i64 = 250_000;
/// Tracing target for the save/etag decisions, so a conflict reported in a real session can be
/// read back afterwards: `RUST_LOG=accent::saves=debug accent <vault>` records every write with
/// the etag it expected and the one it wrote, and every watcher report with the etag the tab
/// holds against the one on disk. Its own target, because the answer is a handful of lines and
/// `accent=debug` is a wall of them.
const SAVES: &str = "accent::saves";

/// Startup milestones: `RUST_LOG=accent=debug accent <vault>` prints ms since process start at
/// "main", "tree populated", "window mapped"/"presented" and "reconcile done". Keeping these makes
/// a regression in time-to-window visible without reaching for a profiler.
static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

fn ms() -> u128 {
    START
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_millis()
}

fn main() -> glib::ExitCode {
    // ssh spawns accent as its own askpass helper; that process only answers the question.
    if let Some(code) = askpass::maybe_run() {
        return code;
    }
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    tracing::debug!(t_ms = ms(), "main");

    let app = adw::Application::builder()
        .application_id(APP_ID)
        // The vault comes from argv, which GApplication would otherwise try to parse itself.
        .flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();
    let shell = Rc::new(Shell {
        config: Rc::new(RefCell::new(Config::load())),
        windows: RefCell::new(Vec::new()),
        start: glib::WeakRef::new(),
        landing: RefCell::new(None),
        shell_keys: Cell::new(false),
    });
    // A `[shortcuts]` key naming no action binds nothing, silently — an action that was renamed
    // leaves exactly that behind. Said once per process; there is no migration.
    for name in shell.config.borrow().shortcuts.keys() {
        if !ACTIONS.iter().any(|(action, _, _)| action == name) {
            tracing::warn!("shortcut for unknown action {name:?} in config.toml");
        }
    }
    shell.install_app_actions(&app);
    // The keyboard moving to another window changes no `focus-widget` — each window keeps its
    // own — so which shell has it is asked again here, of the window that has it now.
    app.connect_active_window_notify({
        let shell = shell.clone();
        move |gtk_app| shell.sync_accels(gtk_app.upcast_ref())
    });
    app.connect_command_line({
        let shell = shell.clone();
        move |gtk_app, command_line| shell.command_line(gtk_app, command_line)
    });
    app.run()
}

// ----------------------------------------------------------------------------------- app state

/// Something to do with a tab once it is open: see [`App::with_tab`].
type Waiting = Box<dyn FnOnce(&Rc<App>, &Rc<Tab>)>;

struct App {
    /// The vault this window is on, or `None` for a window opened on a file instead of a folder:
    /// no index, no watcher, no session, and every tab keyed by an absolute path.
    ///
    /// `Arc`, not `Rc`: the sidebar's search runs its queries on a worker thread.
    vault: Option<Arc<Vault>>,
    /// The process's other windows, for the one thing a window cannot answer alone: a tab dragged
    /// in from another one. Weak, because the shell owns this `App`.
    shell: std::rc::Weak<Shell>,
    config: Rc<RefCell<Config>>,
    window: adw::ApplicationWindow,
    /// Every open pane, in the order they were created. The arrangement itself lives in the
    /// widget tree under `root`; this is only what has to be iterated over.
    panes: RefCell<Vec<Rc<Pane>>>,
    /// The pane a note opens into and the one the find bar and the preview follow.
    active_pane: RefCell<Rc<Pane>>,
    title: adw::WindowTitle,
    toasts: adw::ToastOverlay,
    /// Raised across the window when a remote vault stops answering, with a way back. A banner
    /// rather than a toast because it is a state that persists and needs a decision, and one
    /// across the window rather than per tab because it is every tab that is affected.
    connection: adw::Banner,
    /// How far a remote vault has got in coming up, across the top of the document column. Only
    /// a remote vault's window puts it in the layout at all.
    connect: connect::Bar,
    /// The bar along the bottom of the editor column: progress, branch, file type, word count.
    statusbar: statusbar::Bar,
    /// Every open tab, whatever it holds. A `Vec`, not a map: a rename retargets an open tab,
    /// so its key is not a stable one.
    docs: RefCell<Vec<Doc>>,
    /// Work waiting for a tab that is still being opened, by key: see [`App::with_tab`].
    awaiting: RefCell<HashMap<String, Waiting>>,
    /// Set once, after `App` exists, by the sidebar the tree lives in.
    tree: OnceCell<tree::Tree>,
    /// Files / Search / Tags / References over the vault tree.
    sidebar: OnceCell<sidebar::Sidebar>,
    /// The Git pane, in a vault window whose sidebar has one. Set once, with the sidebar.
    git: OnceCell<Rc<git::Panel>>,
    /// The exclusion set the index was last given — git's ignored paths and the `[search]
    /// exclude` list as one — and `None` until a refresh has written one. Kept so that the write,
    /// which is a few thousand `UPDATE`s, happens when the set moves rather than on every save.
    excluded: RefCell<Option<HashSet<String>>>,
    /// The References request in flight. Replaced rather than queued: the caret moves faster
    /// than a server answers.
    references: RefCell<Option<glib::JoinHandle<()>>>,
    ops: OnceCell<Rc<fileops::Ops>>,
    /// Built on the first Split or Preview: a WebKit process per window is not worth paying for
    /// at startup by someone who only ever writes.
    preview: RefCell<Option<preview::Preview>>,
    /// Numbers the shells this window has opened, so each tab has a key of its own.
    terminals: Cell<usize>,
    /// Every file and every tag in the vault, as the palette lists them. Kept warm in the
    /// background rather than asked for when the dialog opens: on a remote vault that question
    /// costs a round trip, and the palette is a thing that has to appear instantly.
    corpus: RefCell<Corpus>,
    /// Sidebar on the left, editor column on the right; drag the handle to resize.
    split: gtk::Paned,
    /// The sidebar column itself: hiding the sidebar is hiding this widget.
    sidebar_column: adw::ToolbarView,
    sidebar_header: adw::HeaderBar,
    /// The editor column: presentation mode unreveals its top bars, which is the header. The tab
    /// bars belong to the panes and go with `content`.
    toolbar: adw::ToolbarView,
    /// What `toolbar` holds: the connection bar, the toasts, and — only while presentation mode
    /// has the panes off screen — the find bar of the pane being presented.
    editor_column: gtk::Box,
    header: adw::HeaderBar,
    modes: gtk::ToggleButton,
    /// The header's Drawing toggle, shown only over a PDF.
    drawing_button: gtk::ToggleButton,
    /// Whether the ring of tools is out, which is the window's state and not the tab's.
    drawing: Cell<bool>,
    /// The tool the ring offers when it comes back.
    tool: Cell<pdfview::Mode>,
    /// Where this window last left the ring, once the reader has moved it. `None` until then,
    /// which leaves each ring free to open in its own corner.
    ring_at: Cell<Option<(f64, f64)>>,
    menu: gtk::MenuButton,
    paned: gtk::Paned,
    /// Swaps the pane tree for a placeholder while no note is open (DESIGN.md, States).
    content: gtk::Stack,
    mode: Cell<Mode>,
    /// Document zoom, applied to every tab and to the preview, never to the chrome.
    zoom: Cell<f64>,
    /// `Some` while presenting, holding what to restore on the way out.
    presenting: Cell<Option<Presenting>>,
    chrome_hidden: Cell<bool>,
    /// True while Back or Forward is walking a pane's history, so the selection change and the
    /// caret move it makes are not recorded as places of their own — which would clear the
    /// forward side the moment Back used it.
    navigating: Cell<bool>,
    /// Whether a reconcile has finished, so the index can be trusted for backlinks. A real flag
    /// rather than the status label, which is also hidden before the first `Progress` arrives.
    reconciled: Cell<bool>,
    /// The tab `setup-menu` named, so the tab context menu acts on the page that was
    /// right-clicked rather than on the selected one. `None` once the popup is gone, which is
    /// what makes the same actions work from the palette.
    menu_page: RefCell<Option<adw::TabPage>>,
    /// When the tree was last re-read during the first index, from `glib::monotonic_time`.
    tree_painted: Cell<i64>,
    /// The pending post-edit refresh: the preview's re-render and the status bar's word count.
    refresh: RefCell<Option<glib::SourceId>>,
    /// The pending re-query of the note links every open PDF highlights.
    pdf_links: RefCell<Option<glib::SourceId>>,
    session: RefCell<Option<glib::SourceId>>,
    /// Notes this window showed and commands it ran, most recent first. The palette leads with
    /// them, so opening a note is remembered as well as editing it; the index only knows mtime.
    recent_notes: RefCell<Vec<String>>,
    recent_commands: RefCell<Vec<String>>,
    /// The four chords the editor would otherwise eat, claimed at the window. Kept because a
    /// rebind has to rebuild it: see [`fill_captured`].
    captured: gtk::ShortcutController,
}

impl App {
    /// The vault, for everything that needs one. A window without one still edits and saves;
    /// what it cannot do is index, search, link or remember a session.
    fn vault(&self) -> Option<&Arc<Vault>> {
        self.vault.as_ref()
    }

    /// The vault root that keys are relative to.
    ///
    /// ponytail: a window with no vault answers `/`, which is never seen: every key such a window
    /// holds is absolute, and joining an absolute path onto any root gives the path back.
    fn root(&self) -> PathBuf {
        self.vault
            .as_ref()
            .map_or_else(|| PathBuf::from("/"), |v| v.root())
    }

    /// The machine this window's vault is on, or "" when it is this one. In the subtitle
    /// whatever is open, because "which machine am I editing on" is not a question a window
    /// should ever leave to the tab that happens to be selected.
    fn host(&self) -> String {
        self.vault()
            .and_then(|v| v.remote().map(|r| r.url().host.clone()))
            .unwrap_or_default()
    }

    /// The connection to a remote vault went away. Every tab keeps what it holds — the buffer is
    /// the only copy of an unsaved edit — and saving fails with a toast until this clears.
    fn show_connection_banner(&self, why: &str) {
        self.connection.set_title(why);
        self.connection.set_revealed(true);
    }

    fn hide_connection_banner(&self) {
        self.connection.set_revealed(false);
    }

    /// Say why something needs a folder open, for the actions that do — a folder on this machine,
    /// for the few that write next to a file rather than through the vault.
    fn needs_vault(&self, what: &str) {
        self.toast(&format!("Open a local folder to {what}"));
    }

    fn toast(&self, text: &str) {
        self.toasts.add_toast(adw::Toast::new(text));
    }

    /// A failure, in the one shape every failure toast takes: "Cannot <what>: <why>". A reason
    /// that runs to several lines is a dialog's, not a toast's.
    fn cannot(&self, what: &str, why: impl std::fmt::Display) {
        self.toast(&format!("Cannot {what}: {why:#}"));
    }

    /// The file operations the tree, the tab menus and the palette share. `None` without a vault:
    /// creating, renaming and trashing are all things done to a vault, not to a lone open file.
    fn ops(&self) -> Option<&Rc<fileops::Ops>> {
        self.ops.get()
    }

    /// The file operations, or a toast saying why there are none.
    fn need_ops(&self, what: &str) -> Option<&Rc<fileops::Ops>> {
        let ops = self.ops();
        if ops.is_none() {
            self.needs_vault(what);
        }
        ops
    }

    /// The active tab, if it is one with a buffer. Everything that edits, saves, finds or
    /// previews goes through here, so an image or a PDF simply makes those actions no-ops.
    fn active(&self) -> Option<Rc<Tab>> {
        self.active_doc()?.tab().cloned()
    }

    /// What the active tab is showing, for the background answers that must not land on a tab the
    /// user has since moved away from.
    fn active_key(&self) -> Option<String> {
        Some(self.active_doc()?.key())
    }

    fn active_doc(&self) -> Option<Doc> {
        self.doc_of(&self.pane())
    }

    /// What `pane` is showing, active or not. A pane's find bar asks all four of these questions
    /// about its own pane, so none of them may go through the active one.
    fn doc_of(&self, pane: &Pane) -> Option<Doc> {
        self.doc_for_page(&pane.tabs.selected_page()?)
    }

    fn tab_of(&self, pane: &Pane) -> Option<Rc<Tab>> {
        self.doc_of(pane)?.tab().cloned()
    }

    fn pdf_of(&self, pane: &Pane) -> Option<Rc<pdftab::PdfTab>> {
        self.doc_of(pane)?.pdf().cloned()
    }

    /// Point a pane's find bar at whatever it is showing now, and put it away where that is a
    /// shell: there is no buffer to point at, so the bar would sit over a terminal holding a
    /// query nothing answers. It comes back the way any other tab gets it, with the chord.
    fn retarget_find(&self, pane: &Pane) {
        pane.find.retarget(self.tab_of(pane));
        if self.shows_shell(pane) {
            pane.find.close();
        }
    }

    /// Whether what `pane` is showing is a shell, which is the one document neither find nor go
    /// to line can address: vte keeps its own scrollback and counts no lines of ours.
    fn shows_shell(&self, pane: &Pane) -> bool {
        self.doc_of(pane)
            .is_some_and(|doc| doc.terminal().is_some())
    }

    /// `Ctrl+F` and its two neighbours. The bar belongs to the pane, so the chord opens the one in
    /// the pane the reader is in and leaves the other pane's query and open state alone — and
    /// over a shell it opens nothing at all, there being nothing of ours to search there.
    fn open_find(&self, mode: find::Mode) {
        let pane = self.pane();
        if self.shows_shell(&pane) {
            return;
        }
        pane.find.open(mode);
    }

    fn doc_for_page(&self, page: &adw::TabPage) -> Option<Doc> {
        self.docs
            .borrow()
            .iter()
            .find(|d| d.page() == page)
            .cloned()
    }

    fn doc_for(&self, key: &str) -> Option<Doc> {
        self.docs.borrow().iter().find(|d| d.key() == key).cloned()
    }

    /// The key `path` opens under in this window: vault-relative inside the vault, absolute
    /// outside it, which is what a file from another window's vault always is.
    fn key_for(&self, path: &Path) -> String {
        self.vault()
            .and_then(|vault| path.strip_prefix(vault.root()).ok())
            .and_then(Path::to_str)
            .map(str::to_string)
            .unwrap_or_else(|| path.to_string_lossy().into_owned())
    }

    fn tab_for(&self, rel: &str) -> Option<Rc<Tab>> {
        self.doc_for(rel)?.tab().cloned()
    }

    fn is_active(&self, tab: &Rc<Tab>) -> bool {
        self.active().is_some_and(|a| Rc::ptr_eq(&a, tab))
    }

    /// Every open tab, cloned out: the callbacks below reach back into `open`, and a live borrow
    /// across them would be a panic waiting to happen.
    fn open_tabs(&self) -> Vec<Rc<Tab>> {
        self.docs
            .borrow()
            .iter()
            .filter_map(|d| d.tab().cloned())
            .collect()
    }

    /// Every open document, cloned out for the same reason as [`App::open_tabs`].
    fn docs(&self) -> Vec<Doc> {
        self.docs.borrow().clone()
    }

    /// The open documents of one kind, cloned out likewise.
    fn pdfs(&self) -> Vec<Rc<pdftab::PdfTab>> {
        self.docs
            .borrow()
            .iter()
            .filter_map(|d| d.pdf().cloned())
            .collect()
    }

    fn diffs(&self) -> Vec<Rc<diff::DiffTab>> {
        self.docs
            .borrow()
            .iter()
            .filter_map(|d| d.diff().cloned())
            .collect()
    }

    fn terminals(&self) -> Vec<Rc<terminal::Term>> {
        self.docs
            .borrow()
            .iter()
            .filter_map(|d| d.terminal().cloned())
            .collect()
    }

    /// Keep the window subtitle, the References pane and the preview in step with the active tab.
    fn sync_active(self: &Rc<Self>) {
        self.retarget_find(&self.pane());
        // The tools belong to the window, so they follow the tab in front.
        self.sync_drawing();
        // The tree's selection follows the tab in front, so the sidebar says which file is open
        // rather than which row the pointer last crossed. A diff, a terminal and a file from
        // outside the vault have no row to point at, and clear it.
        if let Some(tree) = self.tree.get() {
            let open = self
                .active_doc()
                .filter(|doc| !doc.is_transient() && !doc.is_loose())
                .map(|doc| doc.key());
            tree.set_active(open.as_deref());
        }
        let Some(doc) = self.active_doc() else {
            self.title.set_subtitle(&self.host());
            self.refresh_references();
            // The bar speaks for the tab in front, so with none it says nothing: the last
            // document's "Markdown · 2 words" used to stay under an empty document column,
            // because this path returned before either readout was asked again. The Outline
            // pane is the same: a closed PDF left its "No Bookmarks" page and its thumbnail
            // strip in the sidebar, and `sync_outline` already says "No Outline" for no
            // document at all.
            self.sync_status();
            self.sync_outline();
            self.refresh_zoom();
            return;
        };
        let key = doc.key();
        // A diff is not a file: it is no note anyone opened, and its key names a comparison
        // rather than a path, so the subtitle says what the tab is called instead.
        match doc.is_transient() {
            true => self.title.set_subtitle(&doc.page().title()),
            false => {
                self.note_used(&key);
                let where_ = match doc.is_loose() {
                    true => fileops::display_path(&self.root(), &key),
                    false => key.clone(),
                };
                // On a remote vault the path alone is ambiguous — the same note path exists on
                // this machine too — so the host is named with it, every time.
                self.title.set_subtitle(&match self.host().is_empty() {
                    true => where_,
                    false => format!("{where_} — {}", self.host()),
                });
            }
        }
        // The preview is about notes. A source file, an image or a status page leaves it empty
        // rather than showing the last note's.
        let note = doc.tab().filter(|t| t.flavour().is_note()).cloned();
        self.refresh_references();
        self.sync_status();
        self.sync_outline();
        self.sync_opening();
        self.refresh_zoom();
        match note {
            Some(tab) => self.render(&tab),
            // Nothing here is markdown, so the preview shows nothing rather than the last note
            // it happened to be given.
            None => {
                if let Some(preview) = self.preview.borrow().as_ref() {
                    preview.render("", "");
                }
            }
        }
    }

    /// Fill the Outline pane from the active tab: a note's headings, or a sentence saying why
    /// there are none.
    fn sync_outline(self: &Rc<Self>) {
        let Some(sidebar) = self.sidebar.get() else {
            return;
        };
        let Some(doc) = self.active_doc() else {
            return sidebar.set_outline(None);
        };
        // A PDF's outline is its bookmarks, with the page thumbnails under them.
        if let Some(pdf) = doc.pdf() {
            // Still being opened on the render thread, so there is nothing to say yet and
            // "No Bookmarks" would be a guess.
            if pdf.page_count() == 0 {
                return sidebar.set_outline(Some(&sidebar::outline_note(
                    "Opening…",
                    "Reading the document.",
                )));
            }
            let outline = pdf.outline();
            let content = gtk::Paned::builder()
                .orientation(gtk::Orientation::Vertical)
                .resize_start_child(true)
                .shrink_start_child(false)
                .shrink_end_child(false)
                .build();
            let rows: Vec<(u8, String, usize)> = outline
                .iter()
                .map(|entry| {
                    (
                        entry.depth as u8 + 1,
                        entry.title.clone(),
                        entry.page.unwrap_or(0),
                    )
                })
                .collect();
            let top = match rows.is_empty() {
                true => sidebar::outline_note("No Bookmarks", "This PDF has no outline."),
                false => sidebar::outline_list(
                    &rows,
                    glib::clone!(
                        #[weak]
                        pdf,
                        move |page| pdf.goto_page(page)
                    ),
                ),
            };
            content.set_start_child(Some(&top));
            content.set_end_child(Some(&pdf.thumbnails()));
            return sidebar.set_outline(Some(content.upcast_ref()));
        }
        let Some(tab) = doc.tab() else {
            return sidebar.set_outline(None);
        };
        // The same list whatever the tab holds: a note's headings and a source file's functions
        // are both what the language layer calls symbols.
        let rows = lang::flatten(&tab.lang.symbols());
        if rows.is_empty() {
            let language = tab.language().unwrap_or_else(|| "this file".to_string());
            let (title, body) = match tab.lang.support().and_then(|s| s.missing.clone()) {
                Some(_) => (
                    "No Language Server",
                    format!("Symbols need a language server for {language}."),
                ),
                None if tab.flavour().is_note() => {
                    ("No Headings", "This note has no headings yet.".to_string())
                }
                None => (
                    "No Symbols",
                    "Nothing in this file has a name to list.".to_string(),
                ),
            };
            return sidebar.set_outline(Some(&sidebar::outline_note(title, &body)));
        }
        sidebar.set_outline(Some(&sidebar::outline_list(
            &rows,
            glib::clone!(
                #[weak]
                tab,
                move |at| tab.goto_pos(at)
            ),
        )));
    }

    /// The file's own facts in the status bar: what it is, whether it is saved, and its one
    /// count — a note's words, a code tab's diagnostics, a PDF's page.
    fn sync_status(&self) {
        let doc = self.active_doc();
        let (kind, facts) = match &doc {
            Some(Doc::Text(tab)) => match tab.flavour() {
                editor::Flavour::Note => (
                    Some("Markdown".to_string()),
                    Some(statusbar::words_label(statusbar::word_count(&tab.text()))),
                ),
                // A code tab counts what is wrong with it instead: words are a prose fact.
                editor::Flavour::Code => (
                    Some(statusbar::code_label(
                        tab.language().as_deref(),
                        &tab.encoding_label(),
                    )),
                    diagnostics::counts(&tab.diagnostics()),
                ),
                editor::Flavour::Csv => (
                    Some(statusbar::code_label(Some("CSV"), &tab.encoding_label())),
                    None,
                ),
            },
            // Where the reader is, in the slot a note fills with its word count — and what the
            // pointer is doing to the page, while it is doing anything but reading.
            Some(Doc::Pdf(pdf)) => {
                let facts = match (pdf.page_label(), pdf.mode_label()) {
                    (Some(page), Some(mode)) => Some(format!("{page} · {mode}")),
                    (page, mode) => page.or_else(|| mode.map(str::to_string)),
                };
                (Some("PDF".to_string()), facts)
            }
            Some(Doc::Image(_)) => (Some("Image".to_string()), None),
            Some(Doc::Terminal(_)) => (Some("Terminal".to_string()), None),
            Some(Doc::Status(_)) | Some(Doc::Diff(_)) | None => (None, None),
        };
        self.statusbar.set_kind(kind.as_deref());
        self.statusbar.set_facts(facts.as_deref());
        // Only a text tab has a buffer that can be ahead of the disk; the dot is the tab's own,
        // so one symbol means "unsaved" in both places.
        self.statusbar.set_unsaved(
            doc.as_ref()
                .and_then(Doc::tab)
                .is_some_and(|t| t.modified.get()),
        );
        self.sync_branch();
    }

    fn restyle_terminals(&self) {
        for term in self.terminals() {
            term.restyle();
        }
    }

    /// A shell in a new tab of the active pane, at the vault root — the directory everything else
    /// in the window is measured from. A window with no vault opens one at home.
    fn open_terminal(self: &Rc<Self>) {
        self.open_terminal_at(None);
    }

    /// The same, at a directory of the caller's choosing: `accent --terminal <dir>` is the only
    /// one that has one, so `win.terminal` keeps going through `open_terminal` and the action,
    /// the menu and the palette entry are all untouched.
    fn open_terminal_at(self: &Rc<Self>, cwd: Option<PathBuf>) {
        let n = self.terminals.get() + 1;
        self.terminals.set(n);
        // A remote vault's shell opens on the remote, unless the caller named a directory here:
        // `accent --terminal <dir>` means this machine whatever window it lands in.
        let shell = match (&cwd, self.vault().and_then(|v| v.remote().cloned())) {
            (None, Some(remote)) => terminal::Shell::Remote {
                argv: accent_api::ssh::shell(remote.url(), remote.control_path()),
                host: remote.url().host.clone(),
            },
            _ => terminal::Shell::Local(cwd.unwrap_or_else(|| match self.vault() {
                Some(vault) => vault.root(),
                None => glib::home_dir(),
            })),
        };
        let term = terminal::open(&self.tabs(), &shell, terminal::key(n));
        // The shell's own zoom, not the document's. Capture phase: VTE binds Ctrl+scroll to a font
        // scale of its own, which would move the terminal without the readout ever hearing of it.
        zoom_on_wheel(
            &term.view,
            gtk::PropagationPhase::Capture,
            glib::clone!(
                #[weak(rename_to = app)]
                self,
                #[weak]
                term,
                move |out, _| {
                    term.set_zoom(stepped_zoom(term.zoom(), out));
                    app.refresh_zoom();
                }
            ),
        );
        terminal::on_exit(
            &term,
            glib::clone!(
                #[weak(rename_to = app)]
                self,
                move |term| app.close_page(&term.page)
            ),
        );
        let page = term.page.clone();
        let view = term.view.clone();
        self.docs.borrow_mut().push(Doc::Terminal(term));
        self.select_new_page(&page);
        // The terminal itself, not the scroller around it: focus on the wrapper leaves the shell
        // unable to hear a keystroke, which is a terminal you have to click before you can type
        // in. From an idle, because the page has only just been selected and the widget it holds
        // is not on screen to take focus until the frame it was added in is done.
        glib::idle_add_local_once(move || {
            view.grab_focus();
        });
    }

    /// The branch of the repository the active document sits in, which for a nested repository is
    /// not the vault's own. A comparison tab is no file, so it keeps whatever was showing.
    fn sync_branch(&self) {
        let Some(git) = self.git.get() else {
            return;
        };
        let key = self
            .active_doc()
            .filter(|d| !d.is_transient())
            .map(|d| d.key());
        self.statusbar
            .set_branch(git.branch_label(key.as_deref()).as_deref());
    }

    /// A git refresh landed. The single place the window reacts to one, so everything that has to
    /// follow the repository is added here rather than wired into the pane.
    fn on_git_changed(self: &Rc<Self>) {
        let (Some(sidebar), Some(git)) = (self.sidebar.get(), self.git.get()) else {
            return;
        };
        // A vault under no version control keeps the switcher it had (DESIGN.md, Layout map).
        sidebar.set_git_visible(git.has_repos());
        // What search leaves out: git's answer and the `[search] exclude` list, as one set. This
        // is where the two meet, and it is also what gives a vault with no repository an exclusion
        // mechanism at all — a refresh lands here whether or not it found one.
        let mut excluded = git.ignored();
        excluded.extend(self.config.borrow().search.exclude.iter().cloned());
        // Only where it really moved. A refresh lands on every save and the write below is a few
        // thousand `UPDATE`s on a large vault — the very write the ROADMAP's "Index writes" row
        // records as contending with autosave's reindex. The set that was written last is kept so
        // the question can be asked at all, and it starts as `None` rather than empty: the first
        // refresh of a window has to clear whatever the last session recorded.
        if self.excluded.borrow().as_ref() == Some(&excluded) {
            return self.after_git_changed(git);
        }
        if let Some(tree) = self.tree.get() {
            tree.set_ignored(excluded.clone());
        }
        // The same set the tree dims its rows with, handed to the index so every query can leave
        // it out. It is a few thousand `UPDATE`s on a large vault, so it goes to a worker thread;
        // a search already on screen is asked again once it lands, because its answer changed
        // without the box being touched.
        let Some(vault) = self.vault.clone() else {
            self.excluded.replace(Some(excluded));
            return self.after_git_changed(git);
        };
        let ignored: Vec<String> = excluded.iter().cloned().collect();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let written = gio::spawn_blocking(move || vault.set_excluded(&ignored)).await;
            match written {
                Ok(Ok(())) => {}
                Ok(Err(e)) => return tracing::warn!("recording the exclusion set: {e}"),
                Err(_) => return tracing::warn!("the exclusion-set writer panicked"),
            }
            if let Some(app) = weak.upgrade() {
                // Recorded only now: a write that failed — a remote one that outlasted the RPC
                // deadline, say — has to be tried again by the next refresh rather than be
                // remembered as done.
                app.excluded.replace(Some(excluded));
                if let Some(sidebar) = app.sidebar.get() {
                    sidebar.requery_search();
                }
            }
        });
        self.after_git_changed(git);
    }

    /// What follows every git refresh, whether or not the exclusion set moved with it.
    fn after_git_changed(self: &Rc<Self>, git: &Rc<git::Panel>) {
        self.sync_branch();
        // Only when HEAD actually moved: every open tab costs a `git show`, and a refresh that
        // merely noticed an edit is telling us about the very buffer the marks came from.
        if git.head_changed() {
            for tab in self.open_tabs() {
                self.fetch_head(&tab);
            }
        }
    }

    /// Give a tab the committed text its gutter draws against.
    fn fetch_head(&self, tab: &Rc<Tab>) {
        let Some(git) = self.git.get() else {
            return;
        };
        let weak = Rc::downgrade(tab);
        git.head_text(&tab.rel(), move |head| {
            if let Some(tab) = weak.upgrade() {
                tab.set_head(head);
            }
        });
    }

    /// Show the header's progress bar while the active tab is a PDF still being opened.
    ///
    /// The same thin bar indexing uses, for the same reason: something is being read and the
    /// window is usable meanwhile. GTK4 has no indeterminate mode, so it is stepped by a timer
    /// that exists only while an open is in flight.
    fn sync_opening(self: &Rc<Self>) {
        let opening = self
            .active_doc()
            .and_then(|doc| doc.pdf().cloned())
            .is_some_and(|pdf| pdf.opening());
        if opening {
            self.statusbar.set_progress(Some("Opening the document…"));
            return;
        }
        // Indexing owns the same slot and is the slower of the two: leave its text alone if it is
        // still going, and let `Reconciled` clear it.
        if self.vault.is_none() || self.reconciled.get() {
            self.statusbar.set_progress(None);
        }
    }

    /// The minimap is a global preference with no accelerator, so the palette and the preferences
    /// dialog are the two ways to it. Both end up here.
    fn toggle_minimap(self: &Rc<Self>) {
        let on = {
            let mut config = self.config.borrow_mut();
            config.minimap = !config.minimap;
            config.minimap
        };
        settings::save(&self.config.borrow());
        for tab in self.open_tabs() {
            tab.set_minimap(on);
        }
    }

    /// Hand a URL to the desktop.
    fn launch(&self, uri: &str) {
        gtk::UriLauncher::new(uri).launch(Some(&self.window), gio::Cancellable::NONE, |result| {
            if let Err(e) = result {
                tracing::warn!("cannot open link in browser: {e}");
            }
        });
    }

    /// Hand the Search pane what the editor has selected, so Ctrl+Shift+F and Ctrl+Shift+H search
    /// for it. With nothing selected the box keeps what it holds, the way VS Code does: the word
    /// under the caret is deliberately not a fallback.
    fn seed_search(&self) {
        let Some(text) = self.active().and_then(|tab| tab.selected_search()) else {
            return;
        };
        if let Some(sidebar) = self.sidebar.get() {
            sidebar.set_search_text(&text);
        }
    }

    fn show_pane(&self, name: &str) {
        // A window with no vault has only the outline, and asking for a pane it does not have
        // must not open an empty column.
        if !self.sidebar.get().is_some_and(|s| s.has_pane(name)) {
            return;
        }
        self.sidebar_column.set_visible(true);
        if let Some(sidebar) = self.sidebar.get() {
            sidebar.show_pane(name);
        }
    }

    /// The vault path the tab context menu acts on: the page that was right-clicked, or the
    /// active tab when the same action is fired from the palette.
    fn menu_rel(&self) -> Option<String> {
        let Some(page) = self.menu_page.borrow().clone() else {
            return self
                .active_doc()
                .filter(|d| !d.is_transient())
                .map(|d| d.key());
        };
        self.doc_for_page(&page)
            .filter(|d| !d.is_transient())
            .map(|d| d.key())
    }

    /// Show the open note where it lives: the Files pane, un-hidden if it was, scrolled to the row.
    fn reveal_in_sidebar(&self) {
        let Some(rel) = self.menu_rel() else { return };
        self.show_pane("files");
        if let Some(tree) = self.tree.get()
            && !tree.reveal(&rel)
        {
            self.toast(&format!("{rel} is not in the sidebar"));
        }
    }

    /// The selected tree row, where it is one the app may act on. A row inside a tree the index
    /// does not hold lists and opens but is never changed, so it is no target for a rename, a new
    /// note or a trash (`tree::Row::indexed`).
    fn selected_row(&self) -> Option<tree::Row> {
        self.tree.get()?.selected().filter(|row| row.indexed)
    }

    /// The directory the tree selection points at: the folder itself, or the one a file sits in.
    fn selected_dir(&self) -> Option<String> {
        let row = self.selected_row()?;
        match row.is_dir() {
            true => Some(row.rel),
            false => Some(
                row.rel
                    .rsplit_once('/')
                    .map(|(dir, _)| dir)
                    .unwrap_or("")
                    .to_string(),
            ),
        }
    }

    /// Put a config into effect: everything an edit in the preferences dialog, a Restore Defaults
    /// or a re-read from disk can have changed.
    fn apply_config(self: &Rc<Self>, config: &Config) {
        if let Some(vault) = self.vault() {
            vault.set_config(config.vault(&self.root()));
            vault.set_ghost(config.ghost_text);
        }
        // Switching to or away from Solarized does not change the system's dark state, so the
        // notify handler that usually restyles never fires here.
        theme::apply(config.theme);
        self.apply_accels();
        for tab in self.open_tabs() {
            tab.set_font(config.editor_font.as_deref(), self.zoom.get());
            tab.set_spellcheck(config.spellcheck);
            lang::set_ghost(&tab, config.ghost_text);
            tab.set_minimap(config.minimap);
            tab.set_line_numbers(config.line_numbers);
            tab.set_column_width(config.column_width);
            tab.restyle();
        }
        for diff in self.diffs() {
            diff.set_font(config.editor_font.as_deref(), self.zoom.get());
            diff.restyle();
        }
        // A PDF is rendered in the theme's colours, so Solarized to Adwaita is a re-render even
        // though the system's dark state, and with it the notify handler, never moved.
        for pdf in self.pdfs() {
            pdf.restyle();
            pdf.set_drawing_config(config.drawing.clone());
        }
        if let Some(preview) = self.preview.borrow().as_ref() {
            preview.restyle();
        }
        if let Some(git) = self.git.get() {
            git.set_tree(config.git_tree);
        }
        self.restyle_terminals();
    }

    fn preferences(self: &Rc<Self>) {
        // The config is read once at startup and every row here writes the whole struct back, so
        // an edit made in the file while accent runs would be undone by the next switch touched.
        // Re-reading as the dialog opens keeps the file the source of truth; a file that no
        // longer parses is left alone, exactly as at startup.
        match Config::read(&accent_core::config::config_path()) {
            Ok(fresh) => {
                *self.config.borrow_mut() = fresh;
                let config = self.config.borrow().clone();
                self.apply_config(&config);
            }
            Err(e) if accent_core::config::config_path().exists() => {
                tracing::warn!("re-reading the config: {e:#}")
            }
            Err(_) => {}
        }
        settings::present(
            &self.window,
            self.config.clone(),
            self.vault().map(|v| v.root().to_path_buf()),
            glib::clone!(
                #[weak(rename_to = app)]
                self,
                move |config: &Config| app.apply_config(config)
            ),
        );
    }

    fn about(&self) {
        let about = adw::AboutDialog::builder()
            .application_name("accent")
            .application_icon(APP_ID)
            .version(env!("CARGO_PKG_VERSION"))
            .developer_name("stroblme")
            .license_type(gtk::License::Gpl30)
            .website("https://github.com/stroblme/accent")
            .comments("Markdown and PDF knowledge editor.")
            .build();
        about.present(Some(&self.window));
    }
}
