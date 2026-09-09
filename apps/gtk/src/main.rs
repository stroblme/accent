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
mod pdftab;
mod pdfview;
mod preview;
mod ring;
mod save;
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
use gtk::{gdk, gio, glib, graphene};
use layout::{Mode, Presenting};
use open::Opened;
use panes::{Pane, Place, Side, Spot, Zone};
use shell::Shell;
use sourceview5::prelude::ViewExt as _;
use std::cell::{Cell, OnceCell, RefCell};
use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};
use wire::{choice_row, wire_pane, wire_tree, wire_window, written_at};

const APP_ID: &str = "io.github.stroblme.Accent";

/// What the palette lists before the user types anything.
const RECENT_NOTES: usize = 50;
/// Commands kept in the session's recently-used list. There are only about forty of them, so a
/// shorter list is still every command the user actually reaches for.
const RECENT_COMMANDS: usize = 20;
/// Full-text hits the sidebar shows; beyond this the list stops being scannable.
const SEARCH_LIMIT: usize = 100;
/// DESIGN.md, Motion: the preview re-renders 300 ms after the last edit, and the status bar's
/// word count is read again on the same beat.
const RENDER: Duration = Duration::from_millis(300);
/// Session state is cheap to lose and noisy to write, so it follows a change by a second.
const SESSION: Duration = Duration::from_secs(1);
/// The vault worker is polled instead of woken; 120 ms is below what a progress label needs.
const POLL: Duration = Duration::from_millis(120);
/// A jump into a file that is not open yet waits for the read: how often it looks for the tab,
/// and how many times before it gives up. 300 ms in all, which is five times the ~60 ms a read
/// from the remote vault this was developed against costs.
const OPEN_POLL: Duration = Duration::from_millis(30);
const OPEN_TRIES: usize = 10;
/// DESIGN.md, Motion: the References pane follows the caret by 300 ms.
const REFERENCES: Duration = Duration::from_millis(300);
/// One press of Zoom In or Zoom Out, a tenth of the document font.
const ZOOM_STEP: f64 = 0.1;
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

/// What the palette lists, kept warm so the dialog never waits on the vault.
#[derive(Default)]
struct Corpus {
    files: Rc<Vec<String>>,
    tags: Rc<Vec<String>>,
}

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

    /// Say why something needs a folder open, for the actions that do.
    fn needs_vault(&self, what: &str) {
        self.toast(&format!("Open a folder to {what}"));
    }

    fn toast(&self, text: &str) {
        self.toasts.add_toast(adw::Toast::new(text));
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
            let (title, body) = match tab.lang.support().and_then(|s| s.missing) {
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

    /// Fill the References pane for the active tab: a note's backlinks, or what refers to the
    /// symbol under the caret.
    ///
    /// Debounced and cancellable, because on a code tab it follows the caret: the previous
    /// request is dropped, which is what cancels it at the server rather than leaving it to be
    /// answered and thrown away.
    fn refresh_references(self: &Rc<Self>) {
        if let Some(handle) = self.references.borrow_mut().take() {
            handle.abort();
        }
        let Some(sidebar) = self.sidebar.get() else {
            return;
        };
        let tab = self.active();
        let empty = references_empty(tab.as_ref());
        // Emptied at once, so the pane never shows the last file's answer while this one's is
        // still coming.
        sidebar.set_references(&[], empty);
        let (Some(tab), Some(vault)) = (tab.clone(), tab.as_ref().and_then(|tab| tab.lang.vault()))
        else {
            return;
        };
        let (key, note) = (tab.rel(), tab.flavour().is_note());
        let pos = lang::pos_of(&tab.buffer.iter_at_mark(&tab.buffer.get_insert()));
        let weak = Rc::downgrade(self);
        let handle = glib::spawn_future_local(async move {
            glib::timeout_future(REFERENCES).await;
            lang::flush(tab.clone()).await;
            let found = vault.references(&key, pos).await.unwrap_or_default();
            let Some(app) = weak.upgrade() else { return };
            // The user may have moved on while we were asking; a stale answer must not replace
            // the pane the current tab put there.
            if app.active_key().as_deref() != Some(&key) {
                return;
            }
            if let Some(sidebar) = app.sidebar.get() {
                sidebar.set_references(&reference_rows(&found, note), empty);
            }
        });
        *self.references.borrow_mut() = Some(handle);
    }

    /// Put a list of locations in the References pane and show it. What a definition with more
    /// than one answer does, rather than the window picking one of them.
    fn show_locations(self: &Rc<Self>, found: &[Location]) {
        if let Some(handle) = self.references.borrow_mut().take() {
            handle.abort();
        }
        if let Some(sidebar) = self.sidebar.get() {
            sidebar.set_references(&reference_rows(found, false), references_empty(None));
        }
        self.show_pane("references");
    }

    // --- vault events --------------------------------------------------------------------

    fn on_event(self: &Rc<Self>, event: Event) {
        // Anything that touched a file may have changed what git says about it. The pane
        // debounces, so a burst of watcher events still costs one `git status`.
        if matches!(
            event,
            Event::Reconciled(_)
                | Event::DirsChanged(_)
                | Event::FileChanged(_)
                | Event::FileRemoved(_)
                | Event::FileRenamed { .. }
        ) {
            if let Some(git) = self.git.get() {
                git.schedule_refresh();
            }
            // The same events mean a note may have gained or lost a link into an open PDF.
            self.sync_pdf_links_soon();
        }
        match event {
            Event::Progress(p) => {
                self.statusbar.set_progress(Some(&match p.total {
                    0 => "Indexing…".to_string(),
                    total => format!("Indexing… {}/{total} files", p.done),
                }));
                // The indexer commits rows in batches and the walk hands it files depth-first,
                // so the root level is queryable long before the reconcile ends. Without this the
                // tree of a cold vault stays empty for the whole two seconds. Throttled, and
                // deliberately not marking the tags pane dirty: that is a whole-pane rebuild and
                // it can wait for `Reconciled`.
                let now = glib::monotonic_time();
                if p.phase == Phase::Index && now - self.tree_painted.get() >= TREE_REPAINT {
                    self.tree_painted.set(now);
                    if let Some(tree) = self.tree.get() {
                        tree.refresh();
                        tracing::debug!(t_ms = ms(), rows = tree.model().n_items(), "tree painted");
                    }
                }
            }
            Event::Busy { what, busy } => {
                self.statusbar
                    .set_provider_busy(busy.then_some(what.as_str()));
            }
            Event::Reconciled(stats) => {
                tracing::debug!(
                    t_ms = ms(),
                    scanned = stats.scanned,
                    unchanged = stats.unchanged,
                    "reconcile done"
                );
                self.statusbar.set_progress(None);
                self.reconciled.set(true);
                if let Some(tree) = self.tree.get() {
                    tree.refresh();
                }
                if let Some(sidebar) = self.sidebar.get() {
                    sidebar.mark_tags_dirty();
                }
                self.refresh_corpus();
                self.sync_active();
                // Conflicts on notes nobody has open have no banner to appear on, so the toast
                // that is already there says how many are waiting in the vault.
                let mut message = format!(
                    "Indexed {} files ({} new, {} updated)",
                    stats.scanned, stats.added, stats.updated
                );
                match self
                    .vault()
                    .and_then(|v| v.conflicts().ok())
                    .unwrap_or_default()
                    .len()
                {
                    0 => {}
                    n => message.push_str(&format!(", {n} with sync conflicts")),
                }
                self.toast(&message);
            }
            Event::DirsChanged(dirs) => {
                if let Some(tree) = self.tree.get() {
                    tree.invalidate(&dirs);
                }
                if let Some(sidebar) = self.sidebar.get() {
                    sidebar.mark_tags_dirty();
                }
            }
            Event::FileChanged(rel) => {
                let Some(doc) = self.doc_for(&rel) else {
                    return;
                };
                match &doc {
                    Doc::Text(tab) => {
                        self.file_changed(tab);
                        if self.is_active(tab) {
                            self.sync_active();
                        }
                    }
                    // Re-point the picture at the same file: the texture it holds is of the old
                    // contents, so redrawing alone would show them again.
                    Doc::Image(_) => {
                        if let Some(picture) = picture_of(doc.page()) {
                            picture.set_file(gio::File::NONE);
                            picture.set_filename(Some(self.root().join(&rel)));
                        }
                    }
                    // A rebuilt PDF, which is what a LaTeX loop produces: re-read it in place
                    // rather than sending the reader back to page one.
                    Doc::Pdf(pdf) => pdf.refresh(),
                    // Neither a diff nor a shell is keyed by a path, so a file changing under one
                    // reaches none of these.
                    Doc::Status(_) | Doc::Diff(_) | Doc::Terminal(_) => {}
                }
            }
            Event::FileRemoved(rel) => {
                // A conflict copy is never a tab of its own; what its removal changes is the
                // banner on the note it was a copy of.
                if let Some(original) = accent_api::conflict_original_rel(&rel) {
                    self.sync_conflict_banner(&original, None);
                }
                // A trashed folder arrives as one removal, so everything under it goes too:
                // a tab whose file is inside a folder that no longer exists has nothing left.
                let prefix = format!("{rel}/");
                for doc in self.docs() {
                    let key = doc.key();
                    if key != rel && !key.starts_with(&prefix) {
                        continue;
                    }
                    // Only a buffer holds work the file no longer does; everything else has
                    // nothing left to show, so its tab goes with the file.
                    match doc.tab().filter(|tab| tab.modified.get()) {
                        Some(tab) => {
                            tab.disk_changed.set(true);
                            tab.show_alert(Alert::Restore);
                        }
                        None => self.close_page(doc.page()),
                    }
                }
            }
            Event::FileRenamed { from, to } => {
                let prefix = format!("{from}/");
                for doc in self.docs() {
                    let key = doc.key();
                    if key == from {
                        doc.retarget(&self.root(), &to);
                    } else if let Some(rest) = key.strip_prefix(&prefix) {
                        doc.retarget(&self.root(), &format!("{to}/{rest}"));
                    }
                }
                accent_core::config::rename_in(&mut self.recent_notes.borrow_mut(), &from, &to);
                self.sync_active();
            }
            Event::Conflict { original, .. } => self.sync_conflict_banner(&original, None),
            // A repository moved under us: a commit in a shell, a checkout, a rebase. The pane
            // asks git what changed; nothing else in the window is affected.
            Event::GitChanged => {
                if let Some(git) = self.git.get() {
                    git.schedule_refresh();
                }
            }
            // A remote vault is still coming up. It reads as the same wait as indexing, because
            // that is what it is: the window is open and the files are not there yet.
            // The bar carries the one step that can measure itself, the upload, and pulses
            // through the rest; the text says which step it is.
            Event::Connecting { what, fraction } => {
                self.statusbar.set_progress(Some(&format!("{what}…")));
                self.connect.show(fraction);
            }
            Event::Connected => {
                self.statusbar.set_progress(None);
                self.connect.hide();
                self.hide_connection_banner();
            }
            Event::Disconnected(why) => {
                self.statusbar.set_progress(None);
                self.connect.hide();
                self.show_connection_banner(&why);
            }
            Event::Error(message) => self.toast(&message),
            Event::Diagnostics { rel, items } => {
                if let Some(tab) = self.tab_for(&rel) {
                    tab.set_diagnostics(items);
                    // The count lives in the status bar, which only speaks for the active tab.
                    if self.is_active(&tab) {
                        self.sync_status();
                    }
                }
            }
        }
    }

    /// Step an image's zoom, or, with `None`, put it back to fitting the window.
    ///
    /// Reset is the fit, which is how the tab opened, and is what a PDF's Fit Width is. The step
    /// is taken from the tab's own zoom rather than from the size it asked the picture for: a
    /// pixel width is a whole number, and a zoom read back out of one lands short of the tenth it
    /// was, which is enough for the next step to be the zoom the image is already at.
    fn zoom_image(self: &Rc<Self>, image: &doc::Viewer, out: Option<bool>) {
        let Some(picture) = picture_of(&image.page) else {
            return;
        };
        let zoom = out.zip(image_zoom(image, &picture)).map(|(out, from)| {
            stepped_zoom(from, out).clamp(pdfview::MIN_SCALE, pdfview::MAX_SCALE)
        });
        image.zoom.set(zoom);
        set_image_zoom(&picture, zoom);
        self.refresh_zoom();
    }

    /// Zoom is the document's, never the chrome's: DESIGN.md leaves the interface font to the
    /// system, and this is the reading size of one note. Presentation mode is the same WebView,
    /// so it is zoomed along with the preview.
    fn set_zoom(self: &Rc<Self>, zoom: f64) {
        let zoom = clamp_zoom(zoom);
        self.zoom.set(zoom);
        let font = self.config.borrow().editor_font.clone();
        for tab in self.open_tabs() {
            tab.set_font(font.as_deref(), zoom);
        }
        for doc in self.docs() {
            if let Some(diff) = doc.diff() {
                diff.set_font(font.as_deref(), zoom);
            }
        }
        if let Some(preview) = self.preview.borrow().as_ref() {
            preview.set_zoom(zoom);
        }
        self.refresh_zoom();
        self.save_session_soon();
    }

    /// Show the header's progress bar while the active tab is a PDF still being opened.
    ///
    /// The same thin bar indexing uses, for the same reason: something is being read and the
    /// window is usable meanwhile. GTK4 has no indeterminate mode, so it is stepped by a timer
    /// that exists only while an open is in flight.
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
        for doc in self.docs() {
            if let Some(term) = doc.terminal() {
                term.restyle();
            }
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
                move |out| {
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
        self.tabs().set_selected_page(&page);
        // The terminal itself, not the scroller around it: focus on the wrapper leaves the shell
        // unable to hear a keystroke, which is a terminal you have to click before you can type
        // in. From an idle, because the page has only just been selected and the widget it holds
        // is not on screen to take focus until the frame it was added in is done.
        let view = term.view.clone();
        glib::idle_add_local_once(move || {
            view.grab_focus();
        });
        self.docs.borrow_mut().push(Doc::Terminal(term));
        self.sync_active();
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
        if let Some(tree) = self.tree.get() {
            tree.set_ignored(excluded.clone());
        }
        // The same set the tree dims its rows with, handed to the index so every query can leave
        // it out. It is a few thousand `UPDATE`s on a large vault, so it goes to a worker thread;
        // a search already on screen is asked again once it lands, because its answer changed
        // without the box being touched.
        if let Some(vault) = self.vault.clone() {
            let ignored: Vec<String> = excluded.into_iter().collect();
            let weak = Rc::downgrade(self);
            glib::spawn_future_local(async move {
                let written = gio::spawn_blocking(move || vault.set_excluded(&ignored)).await;
                match written {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => return tracing::warn!("recording the exclusion set: {e}"),
                    Err(_) => return tracing::warn!("the exclusion-set writer panicked"),
                }
                if let Some(app) = weak.upgrade()
                    && let Some(sidebar) = app.sidebar.get()
                {
                    sidebar.requery_search();
                }
            });
        }
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

    /// The zoom readout in the status bar: the document zoom for a text tab, the shell's own for a
    /// terminal, and the PDF's own for a PDF, which fits to the window rather than counting
    /// percentages.
    ///
    /// A document at 100 % has nothing to say, so the readout goes rather than leaving a control
    /// saying nothing is going on; the same for a shell at its own size. A PDF always shows one:
    /// fitting is a zoom too, and it is what clicking the readout goes back to. So does an image,
    /// for the same reason. A status page and a diff show nothing at all, because no zoom reaches
    /// them — the readout used to fall through to the window's document zoom and say "120 %" over
    /// a picture drawn at its own size. It matches the same six variants [`App::zoom_action`]
    /// does, so the readout and the chords cannot disagree.
    fn refresh_zoom(&self) {
        let label = match self.active_doc() {
            Some(Doc::Pdf(pdf)) => pdf.zoom_label(),
            Some(Doc::Terminal(term)) => term.zoom_label(),
            Some(Doc::Text(_)) | Some(Doc::Diff(_)) => {
                let zoom = self.zoom.get();
                (zoom != 1.0).then(|| format!("{} %", (zoom * 100.0).round() as i32))
            }
            Some(Doc::Image(image)) => Some(image_zoom_label(&image)),
            Some(Doc::Status(_)) | None => None,
        };
        self.statusbar.set_zoom(label.as_deref());
    }

    /// The minimap is a global preference with no accelerator, so the palette and the preferences
    /// dialog are the two ways to it. Both end up here.
    fn toggle_minimap(self: &Rc<Self>) {
        let on = {
            let mut config = self.config.borrow_mut();
            config.minimap = !config.minimap;
            if let Err(e) = config.save() {
                tracing::warn!("saving config: {e:#}");
            }
            config.minimap
        };
        for tab in self.open_tabs() {
            tab.set_minimap(on);
        }
    }

    /// Go to Definition: the chord, `F12` and a Ctrl+click in the view all end up here.
    ///
    /// An external link under the caret is followed as a link, because that is what the reader
    /// pointed at; everything else is a question for the language server, whether the tab holds
    /// a note or a source file.
    fn go_to_definition(self: &Rc<Self>) {
        let Some(tab) = self.active() else {
            return;
        };
        if let Some(link) = tab
            .link_at_cursor()
            .filter(|link| link.kind == LinkKind::External)
        {
            return self.launch(&link.target);
        }
        let Some(vault) = tab.lang.vault() else {
            return self.needs_vault("go to a definition");
        };
        // Said once per tab: a file whose server is not installed would otherwise toast on every
        // Ctrl+click, and the answer does not change while the tab is open.
        if let Some(server) = tab.lang.support().and_then(|s| s.missing) {
            if tab.lang.claim_toast() {
                let language = tab.language().unwrap_or_else(|| "this file".to_string());
                self.toast(&format!(
                    "No language server for {language} ({server} not found)"
                ));
            }
            return;
        }
        let pos = lang::pos_of(&tab.buffer.iter_at_mark(&tab.buffer.get_insert()));
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            lang::flush(tab.clone()).await;
            let found = vault.definition(&tab.rel(), pos).await;
            tracing::debug!("definition for {} at {pos:?}: {found:?}", tab.rel());
            let Some(app) = weak.upgrade() else { return };
            match found.unwrap_or_default().as_slice() {
                [] => app.toast("No definition found"),
                [one] => app.open_at(one),
                // More than one place answers to the name — an overload, a trait method, a note
                // title two files share — so the pane lists them instead of the window guessing.
                many => app.show_locations(many),
            }
        });
    }

    /// Open a location and put the caret on it: a URL in the browser, a path in a tab.
    fn open_at(self: &Rc<Self>, loc: &Location) {
        if loc.is_url() {
            return self.launch(&loc.path);
        }
        // A definition into a PDF carries its anchor in the path, so that a wikilink into a
        // page reaches the page (`language/notes.rs`, `definition`).
        let (path, anchor) = split_pdf_anchor(&loc.path);
        let (key, at) = (path.to_string(), loc.range.start);
        self.mark();
        match doc::is_loose_key(&key) {
            // Outside the vault: the same door a file dropped on the window comes through, and
            // the tab it opens gets no language server of its own.
            true => self.open_path(&key),
            false => self.open_preview(&key),
        }
        if anchor.is_some() {
            return self.show_pdf_anchor(&key, anchor);
        }
        self.on_tab(key, move |tab| tab.goto_pos(at));
    }

    /// Do something to the tab holding `key`, once there is one.
    ///
    /// The one door for every jump that follows an open. A file that is not open yet is read on a
    /// worker thread, so its tab arrives a turn or two later; this waits for it rather than
    /// dropping the jump on the floor, and gives up rather than waiting on a file that will not
    /// open at all.
    fn on_tab(self: &Rc<Self>, key: String, f: impl Fn(&Rc<Tab>) + 'static) {
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            for _ in 0..OPEN_TRIES {
                let Some(app) = weak.upgrade() else { return };
                if let Some(tab) = app.tab_for(&key) {
                    return f(&tab);
                }
                drop(app);
                glib::timeout_future(OPEN_POLL).await;
            }
        });
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

    /// Re-read what the palette lists, off the main loop. Cheap enough to do on every reconcile
    /// and every time the dialog opens, which is what keeps the answer both instant and current.
    fn refresh_corpus(self: &Rc<Self>) {
        let Some(vault) = self.vault().cloned() else {
            return;
        };
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let loaded = gio::spawn_blocking(move || {
                (
                    // Never widened: Go to File has no All toggle, and the tree is where an
                    // ignored file is reached, dimmed but listed.
                    vault.file_paths(false).unwrap_or_default(),
                    vault
                        .tags()
                        .unwrap_or_default()
                        .into_iter()
                        .map(|(name, _)| name)
                        .collect::<Vec<_>>(),
                )
            })
            .await;
            if let (Some(app), Ok((files, tags))) = (weak.upgrade(), loaded) {
                *app.corpus.borrow_mut() = Corpus {
                    files: Rc::new(files),
                    tags: Rc::new(tags),
                };
            }
        });
    }

    fn palette(self: &Rc<Self>, initial: palette::Mode) {
        // For the next time it opens; this one uses what is already there.
        self.refresh_corpus();
        // Two answers to "recent": what this window opened, and what changed on disk. The first
        // is what the user means, so it leads and the index's mtime list fills the page below it.
        let mru = self.recent_notes.borrow().clone();
        let mut recent = mru.clone();
        for rel in self
            .vault()
            .and_then(|v| v.recent_notes(RECENT_NOTES).ok())
            .unwrap_or_default()
        {
            if !recent.contains(&rel) {
                recent.push(rel);
            }
        }
        let used = self.recent_commands.borrow();
        let config = self.config.borrow();
        let sources = palette::Sources {
            recent,
            mru,
            // Every file, not only the notes: a source file has to be reachable by name too.
            load_files: Box::new({
                let corpus = self.corpus.borrow().files.clone();
                move || corpus.as_ref().clone()
            }),
            commands: ACTIONS
                .iter()
                .map(|(action, label, _)| palette::Item::Command {
                    action: action.to_string(),
                    label: label.to_string(),
                    accels: accels_for(&config, action),
                    recent: used.iter().position(|a| a == action),
                })
                .collect(),
            load_tags: Box::new({
                let corpus = self.corpus.borrow().tags.clone();
                move || corpus.as_ref().clone()
            }),
            // Filtered here rather than in the dialog: the window is the only thing that knows
            // which vault it is already on, and a row that raises the window it was picked from
            // would be the one row in the list that does nothing.
            vaults: start::other_vaults(&config.recent_vaults, self.vault().map(|v| v.key())),
            // The tab bar's own chords: no command runs them, so they are not rows, but a
            // rebind that took one would be shadowed by a controller the dialog cannot see.
            taken: panes::widget_chords()
                .into_iter()
                .map(|(accel, what)| (accel.to_string(), what.to_string()))
                .collect(),
            // Weak, like the pick callback below: this closure outlives the call and a strong
            // handle here would keep the window alive through the dialog.
            on_rebind: Box::new({
                let app = Rc::downgrade(self);
                move |action: &str, accels: Option<Vec<String>>| match app.upgrade() {
                    Some(app) => app.rebind(action, accels),
                    None => Vec::new(),
                }
            }),
            // The start screen's own removal, so one list is written one way.
            on_forget: Box::new({
                let app = Rc::downgrade(self);
                move |key: &str| {
                    if let Some(app) = app.upgrade() {
                        start::forget_vault(&app.config, Path::new(key));
                    }
                }
            }),
        };
        drop(config);
        drop(used);
        palette::present(
            &self.window,
            initial,
            sources,
            glib::clone!(
                #[weak(rename_to = app)]
                self,
                move |item: &palette::Item| match item {
                    palette::Item::File(rel) => app.open_path(rel),
                    palette::Item::Command { action, .. } => {
                        let _ = WidgetExt::activate_action(&app.window, action, None);
                    }
                    palette::Item::Tag(tag) => {
                        app.sidebar_column.set_visible(true);
                        if let Some(sidebar) = app.sidebar.get() {
                            sidebar.show_tag(tag);
                        }
                    }
                    // Through the shell, which raises the window that vault already has rather
                    // than opening a second one on the same index, session and watcher.
                    palette::Item::Vault(key) => {
                        if let (Some(shell), Some(gtk_app)) = (
                            app.shell.upgrade(),
                            app.window.application().and_downcast::<adw::Application>(),
                        ) {
                            shell.open_vault(&gtk_app, PathBuf::from(key), None);
                        }
                    }
                }
            ),
        );
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
        for doc in self.docs() {
            if let Some(diff) = doc.diff() {
                diff.set_font(config.editor_font.as_deref(), self.zoom.get());
                diff.restyle();
            }
        }
        // A PDF is rendered in the theme's colours, so Solarized to Adwaita is a re-render even
        // though the system's dark state, and with it the notify handler, never moved.
        for doc in self.docs() {
            if let Some(pdf) = doc.pdf() {
                pdf.restyle();
                pdf.set_drawing_config(config.drawing.clone());
            }
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

    // --- session -------------------------------------------------------------------------

    /// Remember that this note was just looked at. Called from `sync_active`, so it covers
    /// opening a note, switching to its tab and coming back to the window.
    fn note_used(self: &Rc<Self>, rel: &str) {
        if self.recent_notes.borrow().first().is_some_and(|r| r == rel) {
            return;
        }
        accent_core::config::touch(&mut self.recent_notes.borrow_mut(), rel, RECENT_NOTES);
        self.save_session_soon();
    }

    /// Remember a command by full action name, whichever surface fired it.
    fn command_used(self: &Rc<Self>, action: &str) {
        accent_core::config::touch(
            &mut self.recent_commands.borrow_mut(),
            action,
            RECENT_COMMANDS,
        );
        self.save_session_soon();
    }

    fn save_session_soon(self: &Rc<Self>) {
        if self.session.borrow().is_some() {
            return;
        }
        let id = glib::timeout_add_local_once(
            SESSION,
            glib::clone!(
                #[weak(rename_to = app)]
                self,
                move || {
                    *app.session.borrow_mut() = None;
                    app.save_session();
                }
            ),
        );
        *self.session.borrow_mut() = Some(id);
    }

    fn save_session(&self) {
        let session = Session {
            open: self
                .docs
                .borrow()
                .iter()
                .filter(|d| !d.is_transient())
                .map(|d| d.key())
                .collect(),
            active: self
                .active_doc()
                .filter(|d| !d.is_transient())
                .map(|d| d.key()),
            // Presentation is not a session state, so the sidebar it hid is saved as it was.
            sidebar: match self.presenting.get() {
                Some(before) => before.sidebar,
                None => self.sidebar_column.is_visible(),
            },
            sidebar_width: sidebar_width(self.split.position()),
            view: self.mode.get().name().to_string(),
            zoom: self.zoom.get(),
            recent_notes: self.recent_notes.borrow().clone(),
            recent_commands: self.recent_commands.borrow().clone(),
            // Merged rather than replaced: a PDF closed earlier in this session keeps the place
            // it was left at, which is the whole point of remembering it.
            pdf: {
                let mut places = self.vault().map(|v| v.session().pdf).unwrap_or_default();
                for doc in self.docs() {
                    if let Some(pdf) = doc.pdf() {
                        places.insert(doc.key(), pdf.place());
                    }
                }
                places
            },
        };
        let Some(vault) = self.vault() else {
            // Nothing to key a session file on, and nothing worth restoring: a window opened on
            // one file is opened again the same way.
            return;
        };
        if let Err(e) = vault.save_session(&session) {
            tracing::warn!("saving the session: {e:#}");
        }
    }

    /// Restored after the window is on screen, so nothing here is on the path to the first frame.
    fn restore_session(self: &Rc<Self>) {
        let Some(vault) = self.vault() else {
            return;
        };
        let session = vault.session();
        // Before the tabs, so each one is built at the right size instead of being restyled
        // afterwards. A state file written before zoom existed defaults to 1.0.
        self.set_zoom(session.zoom);
        // ponytail: every note comes back into one pane, because the session does not record the
        // pane layout. Add a tree of splits to `Session` the day restoring into one column stops
        // being what someone who left four panes open expects.
        for key in &session.open {
            self.open_path(key);
        }
        if let Some(doc) = session.active.as_deref().and_then(|key| self.doc_for(key)) {
            self.tabs().set_selected_page(doc.page());
        }
        // Restoring tabs selects each in turn, and none of that is somewhere the reader went, so
        // the pane starts with an empty history rather than with the order the restore happened in.
        for pane in self.panes.borrow().iter() {
            pane.nav.replace(panes::Nav::default());
        }
        // Which pane was showing is deliberately not restored: Files is where a vault is opened,
        // every time. A window that came back on Search or Git left the reader looking at the
        // answer to a question they asked in another sitting.
        self.sidebar_column.set_visible(session.sidebar);
        self.split
            .set_position(sidebar_width(session.sidebar_width));
        self.set_mode(Mode::from_name(&session.view));
        // Last, and merged rather than assigned: opening the tabs above ran `note_used` for each
        // of them, and the order they happened to restore in says nothing about how they were
        // used. Touching the stored list back to front puts it in front of those, and a note the
        // restore opened that the stored list does not know about still keeps its place at the end.
        for rel in session.recent_notes.iter().rev() {
            accent_core::config::touch(&mut self.recent_notes.borrow_mut(), rel, RECENT_NOTES);
        }
        for action in session.recent_commands.iter().rev() {
            accent_core::config::touch(
                &mut self.recent_commands.borrow_mut(),
                action,
                RECENT_COMMANDS,
            );
        }
    }
}

/// What an image is drawn at: its own zoom, or the scale the window fitted it to.
fn image_zoom(image: &doc::Viewer, picture: &gtk::Picture) -> Option<f64> {
    if let Some(zoom) = image.zoom.get() {
        return Some(zoom);
    }
    let paintable = picture.paintable()?;
    let (w, h) = (paintable.intrinsic_width(), paintable.intrinsic_height());
    if w <= 0 || h <= 0 {
        return None;
    }
    // Fitted: `ScaleDown` takes whichever axis binds and never enlarges.
    let fitted = (f64::from(picture.width()) / f64::from(w))
        .min(f64::from(picture.height()) / f64::from(h))
        .min(1.0);
    Some(fitted)
}

/// The status bar's readout for an image, in the shape a PDF's is: what it is fitted to, or the
/// percentage it is at.
fn image_zoom_label(image: &doc::Viewer) -> String {
    match image.zoom.get() {
        Some(zoom) => format!("{} %", (zoom * 100.0).round() as i32),
        None => "Fit".to_string(),
    }
}

/// Draw an image at `zoom`, or fitted to the window when there is none.
///
/// A zoomed picture is centred and asks for its exact size, so the scroller scrolls it once it
/// is larger than the viewport and does not stretch it while it is smaller.
fn set_image_zoom(picture: &gtk::Picture, zoom: Option<f64>) {
    let size = zoom.and_then(|zoom| {
        let paintable = picture.paintable()?;
        let (w, h) = (paintable.intrinsic_width(), paintable.intrinsic_height());
        (w > 0 && h > 0).then(|| ((f64::from(w) * zoom) as i32, (f64::from(h) * zoom) as i32))
    });
    match size {
        Some((w, h)) => {
            picture.set_content_fit(gtk::ContentFit::Contain);
            picture.set_halign(gtk::Align::Center);
            picture.set_valign(gtk::Align::Center);
            picture.set_size_request(w, h);
        }
        None => {
            picture.set_content_fit(gtk::ContentFit::ScaleDown);
            picture.set_halign(gtk::Align::Fill);
            picture.set_valign(gtk::Align::Fill);
            picture.set_size_request(-1, -1);
        }
    }
}

/// The `GtkPicture` inside a page built by [`App::open_image`].
///
/// Through the viewport: a picture is not a `GtkScrollable`, so the scroller puts one in between,
/// and the `child` property hands that back rather than what was put in it.
fn picture_of(page: &adw::TabPage) -> Option<gtk::Picture> {
    let child = page
        .child()
        .downcast::<gtk::ScrolledWindow>()
        .ok()?
        .child()?;
    match child.downcast::<gtk::Viewport>() {
        Ok(viewport) => viewport.child().and_downcast(),
        Err(child) => child.downcast().ok(),
    }
}

/// Zoom in tenths, between half size and triple. Rounded as well as clamped, so stepping does
/// not drift into 0.7999999999999999 and a hand-edited state file cannot ask for 0.
fn clamp_zoom(zoom: f64) -> f64 {
    ((zoom * 10.0).round() / 10.0).clamp(0.5, 3.0)
}

/// One step in or out from `zoom`: the next multiple of [`ZOOM_STEP`], so a PDF fitted to the
/// window at 137 % lands on 140 % rather than 147 %. Shared with `pdfview`, so a chord, a wheel
/// notch and a pinch mean the same amount of zoom whichever kind of tab is in front.
///
/// The epsilon is what keeps an exact multiple from stepping to itself once the division has
/// drifted; the rounding is what keeps the result out of 1.4000000000000001.
fn stepped_zoom(zoom: f64, out: bool) -> f64 {
    let steps = zoom / ZOOM_STEP;
    let next = match out {
        true => (steps - 1e-6).ceil() - 1.0,
        false => (steps + 1e-6).floor() + 1.0,
    };
    (next * ZOOM_STEP * 100.0).round() / 100.0
}

/// How many whole steps `dy` completes, given the fraction earlier deltas left over. A
/// smooth-scroll device sends one wheel notch as several fractional deltas, and one notch is one
/// step wherever the wheel zooms.
fn wheel_steps(accum: &Cell<f64>, dy: f64) -> i32 {
    let total = accum.get() + dy;
    accum.set(total.fract());
    total.trunc() as i32
}

/// Ctrl+scroll on `widget` steps whatever it is that zooms there: `step(true)` is one step out,
/// `step(false)` one step in. One notch is one step, the same amount the chords move.
///
/// Each controller owns its own accumulator, because a smooth-scroll device sends one notch as
/// several fractional deltas and two widgets sharing the remainder would zoom each other. Without
/// Control the event is passed on untouched, so a plain scroll still scrolls whatever it scrolled.
///
/// The phase is the caller's. A text view wants `Bubble`, ahead of the scrolled window around it;
/// WebKit and VTE answer a Ctrl+scroll themselves, with a zoom of their own that neither the
/// readout nor the session would know about, so those two have to be beaten to it in `Capture`.
fn zoom_on_wheel(
    widget: &impl IsA<gtk::Widget>,
    phase: gtk::PropagationPhase,
    step: impl Fn(bool) + 'static,
) {
    let accum = Cell::new(0.0);
    let wheel = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
    wheel.set_propagation_phase(phase);
    wheel.connect_scroll(move |controller, _, dy| {
        if !controller
            .current_event_state()
            .contains(gdk::ModifierType::CONTROL_MASK)
        {
            return glib::Propagation::Proceed;
        }
        let steps = wheel_steps(&accum, dy);
        for _ in 0..steps.abs() {
            step(steps > 0);
        }
        glib::Propagation::Stop
    });
    widget.add_controller(wheel);
}

/// A sidebar width in pixels, falling back to the default for anything a sidebar would never
/// be: a hidden column's zero position, or the fraction an older session file may still hold.
fn sidebar_width(stored: i32) -> i32 {
    match stored >= 50 {
        true => stored,
        false => Session::default().sidebar_width,
    }
}

// ---------------------------------------------------------------------------------- indexing

/// Drain whatever the vault worker has said since the last tick.
///
/// This source is also what *owns* the window's state: every other closure holds `App` weakly, so
/// that a closed tab, a finished dialog or a dropped controller cannot keep it alive by accident.
///
/// ponytail: a 120 ms poll instead of wiring an `async-channel` into the GLib context. One timeout
/// source, no extra dependency, and the latency is below what a progress label needs.
fn start_events(app: &Rc<App>, events: Receiver<Event>) {
    // Weak, and the source ends with the window: `Shell.windows` holds the only strong `App`, so
    // closing a window drops it along with its vault, its worker thread and its WebKit process.
    let app = Rc::downgrade(app);
    glib::timeout_add_local(POLL, move || {
        let Some(app) = app.upgrade() else {
            return glib::ControlFlow::Break;
        };
        for event in events.try_iter() {
            app.on_event(event);
        }
        glib::ControlFlow::Continue
    });
}

/// The References pane's rows: `path:line`, one-based, in the order the server answered.
///
/// `per_path` keeps one row per file, which is what a note's backlinks have always been — a note
/// that links to the open one three times is one backlink, not three. A code tab wants every
/// occurrence, so it asks for none of that.
fn reference_rows(found: &[Location], per_path: bool) -> Vec<String> {
    let mut rows: Vec<String> = Vec::new();
    let mut paths: Vec<&str> = Vec::new();
    for loc in found {
        if per_path {
            if paths.contains(&loc.path.as_str()) {
                continue;
            }
            paths.push(&loc.path);
        }
        let row = format!("{}:{}", loc.path, loc.range.start.line + 1);
        if !rows.contains(&row) {
            rows.push(row);
        }
    }
    rows
}

/// A References row read back: the path and the line it names.
fn reference_target(row: &str) -> Option<Location> {
    let (path, line) = row.rsplit_once(':')?;
    let line: u32 = line.parse().ok()?;
    let at = accent_api::Pos {
        line: line.saturating_sub(1),
        character: 0,
    };
    Some(Location {
        path: path.to_string(),
        range: accent_api::Range { start: at, end: at },
    })
}

/// What the References pane says when it has nothing to list. A note has backlinks; a source file
/// has references to whatever the caret is on.
fn references_empty(tab: Option<&Rc<Tab>>) -> (&'static str, &'static str) {
    match tab.map(|tab| tab.flavour().is_note()) {
        Some(false) => (
            "No References",
            "Nothing refers to the symbol under the caret.",
        ),
        _ => ("No Backlinks", "No note links to the open one."),
    }
}

/// The character range `bytes` names in `text`, or `None` when it names no range this text has.
///
/// The index reports byte offsets and `GtkTextBuffer` addresses characters, so a search hit has to
/// be counted across before it can be pointed at. Out of bounds and mid-character are both `None`
/// rather than a guess: the file on disk has moved on from what was indexed, and a caret dropped
/// somewhere near the old place is worse than one left where it was.
///
/// ponytail: counting the text in front of the match is fine for a note opened by a click; a real
/// byte-to-iter map belongs on `Tab` if anything ever needs one per keystroke.
/// A place in a PDF a link names: the page, and the selection on it if it names one.
type PdfAnchor = (usize, Option<[usize; 4]>);

/// Split a link target into the path and the PDF anchor it carries, if it carries one.
///
/// `paper.pdf#page=3&selection=4,0,4,11` is a path *and* a place in it; a heading anchor is not
/// this function's business and stays with the path it came in on.
fn split_pdf_anchor(target: &str) -> (&str, Option<PdfAnchor>) {
    match target.split_once('#') {
        Some((path, anchor)) => match accent_core::markdown::pdf_anchor(anchor) {
            Some(at) => (path, Some(at)),
            None => (target, None),
        },
        None => (target, None),
    }
}

fn char_range(text: &str, bytes: Range<usize>) -> Option<Range<usize>> {
    let start = text.get(..bytes.start)?.chars().count();
    Some(start..start + text.get(bytes)?.chars().count())
}

#[cfg(test)]
mod reference_tests {
    use super::*;

    fn at(path: &str, line: u32) -> Location {
        let pos = accent_api::Pos { line, character: 0 };
        Location {
            path: path.to_string(),
            range: accent_api::Range {
                start: pos,
                end: pos,
            },
        }
    }

    /// A note's pane lists the notes that link to it, once each; a code tab's lists every place
    /// the symbol turns up.
    #[test]
    fn a_notes_rows_are_one_per_file_and_a_code_tabs_are_one_per_use() {
        let found = [at("a.md", 0), at("a.md", 4), at("b.md", 2)];
        assert_eq!(reference_rows(&found, true), ["a.md:1", "b.md:3"]);
        assert_eq!(
            reference_rows(&found, false),
            ["a.md:1", "a.md:5", "b.md:3"]
        );
    }

    /// The row is the only thing the pane hands back, so it has to read as a location again.
    #[test]
    fn a_row_reads_back_as_the_place_it_names() {
        let target = reference_target("src/main.rs:12").unwrap();
        assert_eq!(target.path, "src/main.rs");
        assert_eq!(target.range.start.line, 11);
        assert!(reference_target("no-line-here").is_none());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_search_hit_counts_across_to_the_characters_the_buffer_addresses() {
        // Two bytes a character, so the byte range and the character range differ.
        let text = "αβγ match δε";
        assert_eq!(&text[7..12], "match");
        assert_eq!(char_range(text, 7..12), Some(4..9));
        // ASCII is the identity.
        assert_eq!(char_range("hello world", 6..11), Some(6..11));
        // The file has changed since it was indexed: past the end, or mid-character.
        assert_eq!(char_range("short", 4..99), None);
        assert_eq!(char_range("αβγ", 1..3), None);
    }

    #[test]
    fn zoom_steps_in_tenths_and_stops_at_the_ends() {
        assert_eq!(clamp_zoom(stepped_zoom(1.0, false)), 1.1);
        assert_eq!(clamp_zoom(stepped_zoom(1.0, true)), 0.9);
        assert_eq!(clamp_zoom(0.1), 0.5, "no zooming down to nothing");
        assert_eq!(clamp_zoom(9.0), 3.0, "nor up past legibility");
        assert_eq!(clamp_zoom(1.24), 1.2, "a hand-edited state file is rounded");
    }

    #[test]
    fn stepped_zoom_moves_to_the_next_tenth() {
        assert_eq!(stepped_zoom(1.0, false), 1.1);
        assert_eq!(stepped_zoom(1.0, true), 0.9);
        // Off a tenth, which is where a PDF fitted to the window sits: the next tenth, not a
        // tenth further.
        assert_eq!(stepped_zoom(1.37, false), 1.4);
        assert_eq!(stepped_zoom(1.37, true), 1.3);
        assert_eq!(stepped_zoom(1.1, false), 1.2);
    }

    #[test]
    fn a_wheel_notch_is_one_step() {
        let accum = Cell::new(0.0);
        assert_eq!(
            wheel_steps(&accum, 0.5),
            0,
            "half a notch is not a step yet"
        );
        assert_eq!(wheel_steps(&accum, 0.5), 1, "the other half completes it");
        assert_eq!(wheel_steps(&accum, 1.0), 1);
        assert_eq!(wheel_steps(&accum, -2.0), -2);
    }
}
