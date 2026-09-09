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
mod marks;
mod multicaret;
mod open;
mod palette;
mod paned;
mod panes;
mod pdftab;
mod pdfview;
mod preview;
mod ring;
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

// ---------------------------------------------------------------------------------- view mode

/// The two layouts to work in. Reading the rendered note alone is presentation mode, which is
/// temporary and belongs to the window rather than here.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Editor,
    Split,
}

impl Mode {
    fn next(self) -> Mode {
        match self {
            Mode::Editor => Mode::Split,
            Mode::Split => Mode::Editor,
        }
    }

    /// The icon that names this mode in the header toggle.
    fn icon(self) -> &'static str {
        match self {
            Mode::Editor => "document-edit-symbolic",
            Mode::Split => "view-dual-symbolic",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Mode::Editor => "editor",
            Mode::Split => "split",
        }
    }

    /// Anything unrecognised is the editor: a hand-edited session file, or one written when
    /// "preview" was still a mode, must not break the window.
    fn from_name(name: &str) -> Mode {
        match name {
            "split" => Mode::Split,
            _ => Mode::Editor,
        }
    }
}

/// What leaving presentation mode has to put back. The window's size is not part of it: F5 only
/// takes the chrome away, and fullscreen stays F11's job, so the two compose freely.
#[derive(Clone, Copy)]
struct Presenting {
    mode: Mode,
    sidebar: bool,
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

    /// The pane a note opens into: the last one whose tab was selected or whose editor had focus.
    fn pane(&self) -> Rc<Pane> {
        self.active_pane.borrow().clone()
    }

    /// The active pane's tab view. Every `self.tabs` of the single-pane window went through here.
    fn tabs(&self) -> adw::TabView {
        self.pane().tabs.clone()
    }

    fn pane_of(&self, page: &adw::TabPage) -> Option<Rc<Pane>> {
        self.panes.borrow().iter().find(|p| p.has(page)).cloned()
    }

    /// Bring a page to the front of whichever pane holds it, and make that pane the active one.
    /// A note that is already open is never opened twice, so this is what "open" does for it.
    fn reveal_page(&self, page: &adw::TabPage) {
        if let Some(pane) = self.pane_of(page) {
            pane.tabs.set_selected_page(page);
            *self.active_pane.borrow_mut() = pane;
        }
    }

    /// Close a page in the pane that holds it, whichever that is.
    fn close_page(&self, page: &adw::TabPage) {
        if let Some(pane) = self.pane_of(page) {
            pane.tabs.close_page(page);
        }
    }

    /// `Ctrl+Tab` and `Ctrl+Shift+Tab`: one step through the active pane's tabs in the order they
    /// were last used. Per pane, because a pane owns its tab view and its bar is what says which
    /// notes are in it; a window-wide order would have to move the keyboard across a split, which
    /// is not what a split is for.
    fn cycle_tab(&self, forward: bool) {
        let pane = self.pane();
        if let Some(page) = pane.step(forward) {
            pane.tabs.set_selected_page(&page);
        }
    }

    /// Ctrl came up, or the window stopped being the active one mid-chord: whichever pane was
    /// cycling commits the tab it landed on. Every pane rather than the active one, because a
    /// chord started in one pane and abandoned in another must not leave a cursor behind.
    fn end_cycle(&self) {
        for pane in self.panes.borrow().iter() {
            pane.end_cycle();
        }
    }

    // --- back and forward ---------------------------------------------------------------------

    /// Where a document is being read: the caret in a text tab, the reading anchor in a PDF, and
    /// the document itself for anything with no position of its own.
    fn place_of(doc: &Doc) -> Place {
        let at = match doc {
            Doc::Text(tab) => {
                let iter = tab.buffer.iter_at_mark(&tab.buffer.get_insert());
                Spot::Caret(iter.line() + 1, iter.line_offset() + 1)
            }
            Doc::Pdf(pdf) => Spot::Page(pdf.anchor()),
            _ => Spot::Whole,
        };
        Place { key: doc.key(), at }
    }

    /// Where the reader is in `pane` right now.
    fn here(&self, pane: &Pane) -> Option<Place> {
        let page = pane.tabs.selected_page()?;
        self.doc_for_page(&page).as_ref().map(Self::place_of)
    }

    /// Record where the reader is in the active pane, so Back returns there. Every jump calls
    /// this before it moves; a place that coalesces with the last one replaces it.
    fn mark(&self) {
        let pane = self.pane();
        if let Some(here) = self.here(&pane) {
            self.record(&pane, here);
        }
    }

    /// The same, for a document that is about to stop being the selected one: the tab being left
    /// still holds its caret, so this is read before the switch has happened.
    fn mark_page(&self, page: &adw::TabPage) {
        let Some(pane) = self.pane_of(page) else {
            return;
        };
        if let Some(doc) = self.doc_for_page(page) {
            self.record(&pane, Self::place_of(&doc));
        }
    }

    fn record(&self, pane: &Pane, place: Place) {
        if self.navigating.get() {
            return;
        }
        pane.nav.borrow_mut().record(place, Instant::now());
    }

    /// `Alt+Left` / `Alt+Right` and the mouse's side buttons: one step through the active pane's
    /// history. It may switch tabs inside the pane; it never moves the keyboard to another one.
    ///
    /// Entries whose document has left the pane are stepped over rather than dropped on the
    /// floor, which is what a tab dragged into a neighbouring pane leaves behind.
    fn navigate(self: &Rc<Self>, forward: bool) {
        let pane = self.pane();
        let Some(mut here) = self.here(&pane) else {
            return;
        };
        self.navigating.set(true);
        while let Some(to) = match forward {
            true => pane.nav.borrow_mut().forward(here.clone()),
            false => pane.nav.borrow_mut().back(here.clone()),
        } {
            if self.go_to(&pane, &to) {
                break;
            }
            here = to;
        }
        self.navigating.set(false);
    }

    /// Put the reader at `to`, if the document it names is still one of this pane's.
    fn go_to(&self, pane: &Pane, to: &Place) -> bool {
        let Some(doc) = self
            .docs()
            .into_iter()
            .find(|d| d.key() == to.key && pane.has(d.page()))
        else {
            return false;
        };
        tracing::debug!("navigating to {to:?}");
        pane.tabs.set_selected_page(doc.page());
        match (&doc, to.at) {
            (Doc::Text(tab), Spot::Caret(line, column)) => tab.goto_line(line, column),
            (Doc::Pdf(pdf), Spot::Page(anchor)) => pdf.scroll_to(anchor),
            _ => {}
        }
        true
    }

    /// Put the pane on the tab its reader was on before `page`, in time for `page` to go.
    ///
    /// Here rather than after the detach, because `AdwTabView` moves the selection to the left
    /// neighbour itself the moment the selected page leaves — and by the time anything could
    /// correct that, the neighbour is the newest thing in the history and the answer is lost.
    /// Selecting first means the page that is closing is no longer the selected one, so
    /// libadwaita has nothing to pick.
    fn select_survivor(&self, page: &adw::TabPage) {
        let Some(pane) = self.pane_of(page) else {
            return;
        };
        if pane.tabs.selected_page().as_ref() != Some(page) {
            return;
        }
        if let Some(next) = pane.survivor(page) {
            pane.tabs.set_selected_page(&next);
        }
    }

    /// This tab is a real one now, not a preview: it was edited, or its tab was double-clicked.
    fn promote(&self, page: &adw::TabPage) {
        if let Some(pane) = self.pane_of(page) {
            pane.keep(page);
        }
    }

    // --- panes ---------------------------------------------------------------------------

    /// A new, empty pane beside `at`. The caller has to put something in it: an empty pane closes
    /// itself as soon as a page leaves it, but one that never held a page has nothing to react to.
    fn split_beside(self: &Rc<Self>, at: &Rc<Pane>, side: Side) -> Rc<Pane> {
        let pane = Pane::new(&tab_menu());
        wire_pane(self, &pane);
        self.panes.borrow_mut().push(pane.clone());
        panes::split(at, &pane, side);
        self.sync_panes();
        self.set_active_pane(&pane);
        pane
    }

    /// Move `page` into a new pane beside `at`. Splitting a pane's only note off it would empty
    /// the pane, which closes it again, so that one is refused rather than done and undone.
    fn split_page(self: &Rc<Self>, at: &Rc<Pane>, side: Side, page: &adw::TabPage) {
        // A page in no pane of ours: a drag still in flight, which libadwaita has detached from
        // its view. `Shell::landed` waits for the attach before asking for a split, so this is
        // only reachable from the menu and the palette, where there is nothing to split off.
        let Some(from) = self.pane_of(page) else {
            return;
        };
        if Rc::ptr_eq(&from, at) && at.tabs.n_pages() <= 1 {
            return self.toast("This pane has only one note.");
        }
        let pane = self.split_beside(at, side);
        from.tabs.transfer_page(page, &pane.tabs, 0);
        // The page is the new pane's only one, so it is selected already; what it has not got is
        // the keyboard. This is also where `move_tab` lands in a window with nowhere to move to.
        self.focus_document(&pane);
    }

    /// The tab context menu's Split Right and friends: the page that was right-clicked, split off
    /// its own pane.
    fn split_active(self: &Rc<Self>, side: Side) {
        let Some(page) = self
            .menu_page
            .borrow()
            .clone()
            .or_else(|| self.tabs().selected_page())
        else {
            return;
        };
        let at = self.pane_of(&page).unwrap_or_else(|| self.pane());
        self.split_page(&at, side, &page);
    }

    /// Move the tab into the pane on `side`, or split one off when there is none that way. The
    /// fallback is what makes the chord worth having in the common single-pane window, where
    /// there is nowhere to move to yet; `win.split-*` stays the always-split.
    fn move_tab(self: &Rc<Self>, side: Side) {
        let Some(page) = self
            .menu_page
            .borrow()
            .clone()
            .or_else(|| self.tabs().selected_page())
        else {
            return;
        };
        let Some(from) = self.pane_of(&page) else {
            return;
        };
        // Cloned out, and the borrow dropped: a transfer runs `page-detached` and `page-attached`
        // synchronously, and both reach back into `panes`.
        let panes: Vec<Rc<Pane>> = self.panes.borrow().clone();
        let root = self.window.clone().upcast::<gtk::Widget>();
        let rects: Vec<graphene::Rect> = panes.iter().map(|p| pane_rect(p, &root)).collect();
        let Some(i) = panes
            .iter()
            .position(|p| Rc::ptr_eq(p, &from))
            .and_then(|at| panes::neighbour(rects[at], &rects, side))
        else {
            return self.split_active(side);
        };
        let to = &panes[i];
        from.tabs.transfer_page(&page, &to.tabs, to.tabs.n_pages());
        // Selecting it is what makes the destination the active pane, retargets its find bar and
        // saves the session, all through the `selected-page` handler the pane already has.
        to.tabs.set_selected_page(&page);
        self.focus_document(to);
    }

    /// A note from the tree, opened in a pane of its own beside `at`. Unlike [`Self::split_page`]
    /// this always splits: the note may not be open at all, so there is something new to show.
    fn open_beside(self: &Rc<Self>, at: &Rc<Pane>, side: Side, rel: &str) {
        let pane = self.split_beside(at, side);
        match self.tab_for(rel).map(|tab| tab.page.clone()) {
            Some(page) => {
                if let Some(from) = self.pane_of(&page) {
                    from.tabs.transfer_page(&page, &pane.tabs, 0);
                }
            }
            None => self.open_path(rel),
        }
        // Nothing arrived: the path was unopenable, or it was the only note in the pane it came
        // from, which has closed itself and left this one holding the same note it already had.
        if pane.tabs.n_pages() == 0 {
            self.close_pane(&pane);
        }
    }

    /// Take a pane out of the window. The last one stays whatever happens: a window with no pane
    /// has nowhere to open a note into.
    fn close_pane(self: &Rc<Self>, pane: &Rc<Pane>) {
        if self.panes.borrow().len() <= 1 {
            return;
        }
        // Whatever presentation mode borrowed goes with the pane rather than being left behind
        // in a column that no longer has an owner for it.
        pane.hold_find();
        panes::detach(pane);
        self.panes.borrow_mut().retain(|p| !Rc::ptr_eq(p, pane));
        if Rc::ptr_eq(&self.pane(), pane)
            && let Some(next) = self.panes.borrow().first().cloned()
        {
            *self.active_pane.borrow_mut() = next;
        }
        self.sync_panes();
        self.sync_active();
    }

    /// Make `pane` the one notes open into, reporting whether that was a change. Syncing the
    /// title, the backlinks and the preview is the caller's, because the commonest caller is a
    /// page selection that has to sync whether the pane changed or not.
    fn set_active_pane(&self, pane: &Rc<Pane>) -> bool {
        if Rc::ptr_eq(&self.active_pane.borrow(), pane) {
            return false;
        }
        *self.active_pane.borrow_mut() = pane.clone();
        true
    }

    /// Give the keyboard to what `pane` is showing, so a document moved into it takes the caret
    /// with it.
    ///
    /// Without this the focus stays behind: the pane the tab came from selects its survivor while
    /// the moved child still holds the keyboard, so libadwaita hands it to *that* document, and a
    /// transfer that empties the pane drops the focus altogether. Everything a focused view draws
    /// — the caret, GtkSourceView's current-line highlight — is then in the pane the reader has
    /// just left, and the next keystroke goes there too.
    ///
    /// The document's own widget rather than the page's child: `grab_focus` on a container takes
    /// the first thing in it that will have it, which for a note is whatever its banner is showing
    /// and for a shell is the scroller around vte, which cannot hear a keystroke. An image, a
    /// status page and a two-blob comparison have no keys of their own and are left alone.
    fn focus_document(&self, pane: &Pane) {
        let widget: gtk::Widget = match self.doc_of(pane) {
            Some(Doc::Text(tab)) => tab.view.clone().upcast(),
            Some(Doc::Terminal(term)) => term.view.clone().upcast(),
            Some(Doc::Pdf(pdf)) => pdf.key_target(),
            _ => return,
        };
        // From an idle, as a new terminal's own focus is (see [`Self::open_terminal_at`]): the
        // page has only just been attached, and a widget still mid-reparenting is not one GTK
        // hands the keyboard to — measured, the grab does nothing and `GtkPaned` complains about
        // a focus child that is not its child. The idle also runs after the pane the tab left has
        // closed itself, that close being queued first.
        glib::idle_add_local_once(move || {
            widget.grab_focus();
        });
    }

    /// What changes when a pane appears or goes: whether the tab bars may hide themselves, and
    /// whether there is any note left to show at all.
    fn sync_panes(&self) {
        let panes = self.panes.borrow();
        // A single pane's bar disappears with its second tab, as it always did. Several panes have
        // to keep theirs: the bar is what says which notes are in which pane.
        let alone = panes.len() == 1;
        let pages: i32 = panes.iter().map(|p| p.tabs.n_pages()).sum();
        for pane in panes.iter() {
            pane.bar.set_autohide(alone);
        }
        let name = if pages == 0 { "empty" } else { "tabs" };
        self.content.set_visible_child_name(name);
    }

    /// Put the drop sheets in or out of the picture in every pane at once: a drag that started
    /// over one pane has to be droppable on all of them.
    fn set_drop_active(&self, on: bool) {
        for pane in self.panes.borrow().iter() {
            pane.set_drop_active(on);
        }
    }

    /// A tab or a vault path let go over `pane`. `true` when it was taken — never for a tab,
    /// which is declined on purpose so that `create-window` fires; see [`Landing`].
    fn dropped(self: &Rc<Self>, pane: &Rc<Pane>, zone: Zone, value: &glib::Value) -> bool {
        if value.get::<adw::TabPage>().is_ok() {
            // Recorded, and deliberately declined: a dragged page has left its view, and
            // declining is what summons the `create-window` that can give it one again. Whose
            // tab it is does not matter here — `Shell::adopt_page` sorts that out from
            // `page-attached` once libadwaita has attached it.
            if let Some(shell) = self.shell.upgrade() {
                shell.aim(self, pane, zone);
            }
            return false;
        }
        let Ok(rel) = value.get::<String>() else {
            return false;
        };
        match zone {
            Zone::Split(side) => self.open_beside(pane, side, &rel),
            Zone::Here => {
                self.set_active_pane(pane);
                self.open_path(&rel);
            }
        }
        true
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

    // --- saving --------------------------------------------------------------------------

    fn save_active(self: &Rc<Self>) {
        if let Some(tab) = self.active() {
            self.save_tab(&tab, true);
        }
    }

    /// `explicit` is a Ctrl+S, which may raise a dialog. An autosave never can: interrupting
    /// someone mid-sentence with a modal is exactly what autosave exists to avoid.
    ///
    /// A buffer whose file moved underneath it is not written at all, and the save is not even
    /// attempted ([`editor::may_save`]): the banner is holding a question and a save is not an
    /// answer to it. Ctrl+S raises the same dialog a refused write raises, so a reflex save is
    /// never silently dropped; an autosave says nothing beyond the banner already on screen.
    ///
    /// The two paths a tab leaves by — a tab closing, a window closing — deliberately do *not*
    /// come through here. They write and then ask when the write is refused, because a buffer on
    /// its way out has nowhere else to be kept and refusing there would lose it outright.
    fn save_tab(self: &Rc<Self>, tab: &Rc<Tab>, explicit: bool) {
        if !editor::may_save(tab.modified.get(), tab.disk_changed.get()) {
            if explicit {
                // A note deleted underneath us is asking to be written back, and its banner's
                // button already says Save, so Ctrl+S does that rather than offering to
                // overwrite a file that is not there.
                match tab.alert() {
                    Some(Alert::Restore) => self.answer_banner(tab),
                    _ => self.ask_overwrite(tab),
                }
            }
            return;
        }
        match self.write_tab(tab, tab.etag.get()) {
            Ok(()) => {
                self.sync_pdf_links_soon();
                if explicit {
                    self.toast("Saved");
                }
            }
            // The etag gate refused: the tab holds the question from now on, whatever asked. It
            // used to be recorded only for an autosave, so a Ctrl+S that was cancelled left a
            // blocked tab with no banner on it.
            Err(SaveError::ChangedOnDisk { .. }) => {
                tab.disk_changed.set(true);
                tab.show_alert(Alert::Compare);
                if explicit {
                    self.ask_overwrite(tab);
                }
            }
            Err(e) => self.toast(&format!("Save failed: {e}")),
        }
    }

    /// Write the buffer and hand the error back instead of reporting it: a caller that is about
    /// to make the buffer unreachable has to know whether the bytes landed.
    fn write_tab(&self, tab: &Rc<Tab>, expected: Option<Etag>) -> Result<(), SaveError> {
        // What the file should hold, not what the buffer holds: a code file loses its trailing
        // whitespace here and a DOS file gets its CRLFs back.
        let text = tab.for_disk();
        // A loose tab is not in any vault, so it writes through core directly. Same atomic save,
        // same etag gate; what it misses is the watcher being told the write was ours, which the
        // tab's own file monitor makes harmless.
        let written = match self.vault().filter(|_| !doc::is_loose_key(&tab.rel())) {
            Some(vault) => vault.save(&tab.rel(), &text, expected),
            None => accent_core::fs::write_note(&tab.path(), &text, expected),
        };
        match &written {
            Ok(etag) => tracing::debug!(target: SAVES, rel = %tab.rel(), ?expected, ?etag, "wrote"),
            Err(e) => {
                tracing::debug!(target: SAVES, rel = %tab.rel(), ?expected, error = %e, "refused");
            }
        }
        let etag = written?;
        tab.mark_clean(etag);
        tab.clear_disk_alert();
        // The tab is clean again, so the bar's dot goes with the one on the tab title.
        self.sync_status();
        lang::saved(tab);
        // Our own writes go through the vault, which tells the watcher they were ours, so no
        // event comes back to say the working tree moved. The pane is told here instead.
        if let Some(git) = self.git.get() {
            git.schedule_refresh();
        }
        Ok(())
    }

    /// A watcher says the file under a tab moved.
    ///
    /// Whose write it was is the first question. Every save is a rename into place, which a file
    /// monitor reports as a change like anyone else's, so the etag is the only thing that tells
    /// our own writes apart from a real one: a file still carrying the etag we wrote holds
    /// exactly what the buffer already has. Reloading it anyway threw the view at the caret a
    /// second after every keystroke, and on a buffer typed into since the save it raised a
    /// "changed on disk" banner against our own bytes.
    ///
    /// Only for a watcher. Every other caller of [`Self::refresh_tab`] is answering a question
    /// the user was asked, and has to reload whatever the etag says.
    ///
    /// A stat that failed is not an answer and must not read as one. It used to fall in with "no
    /// file there", which differs from any etag we hold and so raised the banner: on a remote
    /// vault a dropped ssh connection would report a conflict over a diff holding nothing but the
    /// user's own edits. Nothing is lost by waiting — a real change fires the watcher again, and
    /// the etag gate refuses any save that would clobber one in the meantime.
    fn file_changed(&self, tab: &Rc<Tab>) {
        let looked = match self.vault().filter(|_| !doc::is_loose_key(&tab.rel())) {
            Some(vault) => vault.stat(&tab.rel()),
            None => match Etag::of(&tab.path()) {
                Ok(etag) => Ok(Some(etag)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(e),
            },
        };
        let disk = match looked {
            Ok(disk) => disk,
            Err(e) => {
                return tracing::debug!(
                    target: SAVES, rel = %tab.rel(), error = %e, "watcher: could not stat"
                );
            }
        };
        let ours = tab.etag.get();
        tracing::debug!(
            target: SAVES,
            rel = %tab.rel(),
            ?ours,
            ?disk,
            modified = tab.modified.get(),
            "watcher"
        );
        if ours != disk {
            self.refresh_tab(tab);
        }
    }

    /// Refresh a tab from what is on disk, unless its buffer holds edits nobody has saved: that
    /// buffer is the only copy of them, so the banner asks instead of the reload deciding.
    /// Returns whether the tab was refreshed.
    fn refresh_tab(&self, tab: &Rc<Tab>) -> bool {
        if tab.modified.get() {
            tab.disk_changed.set(true);
            tab.show_alert(Alert::Compare);
            return false;
        }
        if let Err(e) = tab.reload_keep_cursor() {
            self.toast(&format!("Reload failed: {e}"));
        }
        // A reload writes the buffer without an edit event, so the count and the dot are asked
        // for here rather than waiting for the next keystroke.
        self.sync_status();
        true
    }

    fn ask_overwrite(self: &Rc<Self>, tab: &Rc<Tab>) {
        let dialog = adw::AlertDialog::new(
            Some("File Changed on Disk"),
            Some(&format!(
                "{} was modified elsewhere since you opened it.",
                tab.rel()
            )),
        );
        // Compare rather than Reload: reloading threw the buffer away on one click, and the
        // resolver shows both sides and now lets them be merged by hand.
        dialog.add_responses(&[
            ("cancel", "Cancel"),
            ("compare", "Compare"),
            ("overwrite", "Overwrite"),
        ]);
        dialog.set_response_appearance("overwrite", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");

        let (app, tab) = (self.clone(), tab.clone());
        dialog.choose(
            Some(&self.window),
            gio::Cancellable::NONE,
            move |response| match response.as_str() {
                "compare" => app.compare_with_disk(&tab),
                "overwrite" => match app.write_tab(&tab, None) {
                    Ok(()) => app.toast("Overwritten"),
                    Err(e) => app.toast(&format!("Save failed: {e}")),
                },
                _ => {}
            },
        );
    }

    /// A tab is on its way out and its buffer could not be written. Ask, then call `after` with
    /// whether the tab may go: the answer decides, never the failed save.
    ///
    /// DESIGN.md, States: the choice can lose data either way round, so it is an `AlertDialog`
    /// naming both losses rather than a toast behind a window that is already closing.
    fn ask_unsaved(
        self: &Rc<Self>,
        tab: &Rc<Tab>,
        error: &SaveError,
        after: impl Fn(&Rc<Self>, bool) + 'static,
    ) {
        let rel = tab.rel();
        let body = match error {
            SaveError::ChangedOnDisk { .. } => {
                format!("{rel} changed on disk, so your edits could not be saved.")
            }
            e => format!("{rel} could not be saved: {e}"),
        };
        let dialog = adw::AlertDialog::new(Some("Unsaved Changes"), Some(&body));
        dialog.add_responses(&[
            ("cancel", "Cancel"),
            ("discard", "Discard"),
            ("overwrite", "Overwrite"),
        ]);
        dialog.set_response_appearance("discard", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");

        let (app, tab) = (self.clone(), tab.clone());
        dialog.choose(
            Some(&self.window),
            gio::Cancellable::NONE,
            move |response| {
                let close = match response.as_str() {
                    "discard" => {
                        tab.discard();
                        true
                    }
                    "overwrite" => match app.write_tab(&tab, None) {
                        Ok(()) => true,
                        Err(e) => {
                            app.toast(&format!("Save failed: {e}"));
                            false
                        }
                    },
                    _ => false,
                };
                after(&app, close);
            },
        );
    }

    /// Forget a page that is really closing. Called on every path that closes one, because
    /// `close_page_finish` does not come back through the `close-page` handler.
    fn forget_page(self: &Rc<Self>, page: &adw::TabPage) {
        // A drawn-on document leaving: the render thread drains its channel before it ends, so
        // the write still happens after the tab is gone.
        if let Some(Doc::Pdf(pdf)) = self.doc_for_page(page) {
            pdf.flush();
            if let Some(at) = pdf.ring_at() {
                self.ring_at.set(Some(at));
            }
        }
        if let Some((pane, doc)) = self.pane_of(page).zip(self.doc_for_page(page)) {
            pane.nav.borrow_mut().forget(&doc.key());
        }
        self.docs.borrow_mut().retain(|d| d.page() != page);
        self.sync_active();
        self.save_session_soon();
    }

    /// The banner's button, doing what its label says. Which is which is decided when the banner
    /// goes up, not read off the file system when the button is pressed.
    fn answer_banner(self: &Rc<Self>, tab: &Rc<Tab>) {
        match tab.alert() {
            // Reports rather than asks, so it has no button and this cannot be reached from one.
            Some(Alert::ReadOnly) => {}
            // Both sides hold work, so neither is thrown away on one click: the diff shows what
            // differs and the user picks (DESIGN.md: a choice that can lose data is a dialog).
            Some(Alert::Compare) => self.compare_with_disk(tab),
            Some(Alert::Restore) => match self.write_tab(tab, None) {
                Ok(()) => self.toast("Saved"),
                Err(e) => self.toast(&format!("Save failed: {e}")),
            },
            // Looked up again rather than remembered: the copy may have been resolved from
            // another window, or by Syncthing, since the banner went up.
            Some(Alert::Conflict) => {
                let rel = tab.rel();
                match self
                    .vault()
                    .and_then(|v| v.conflicts_of(&rel).ok())
                    .unwrap_or_default()
                    .first()
                {
                    Some(conflict) => self.resolve_conflict(&rel, conflict),
                    None => {
                        tab.clear_alert(Alert::Conflict);
                        self.toast("The conflict copy is gone");
                    }
                }
            }
            None => tab.hide_banner(),
        }
    }

    /// The unsaved buffer against the file underneath it, in the tab itself: the editor is the
    /// Mine pane, so a merge is typed straight into the note.
    fn compare_with_disk(self: &Rc<Self>, tab: &Rc<Tab>) {
        let rel = tab.rel();
        let read = match self.vault().filter(|_| !doc::is_loose_key(&rel)) {
            Some(vault) => vault.read(&rel),
            None => accent_core::fs::read_note(&tab.path()),
        };
        let Ok((disk, disk_etag)) = read else {
            return self.toast(&format!("Cannot read {rel} from disk"));
        };
        let keep_theirs = gtk::Button::with_label("Keep Theirs");
        let keep_mine = gtk::Button::with_label("Keep Mine");
        keep_mine.add_css_class("suggested-action");
        // Keeping theirs drops the buffer, which is a loss the user has now seen spelled out
        // line by line.
        keep_theirs.connect_clicked(glib::clone!(
            #[weak(rename_to = app)]
            self,
            #[weak]
            tab,
            move |_| {
                tab.leave_compare();
                tab.discard();
                app.refresh_tab(&tab);
            }
        ));
        // Keeping mine writes the buffer over the file — gated on the version that was on screen
        // as Theirs, so a file that moved again while the panes were open is not overwritten
        // unseen: the banner stays up, and Compare shows the newer text.
        keep_mine.connect_clicked(glib::clone!(
            #[weak(rename_to = app)]
            self,
            #[weak]
            tab,
            move |_| {
                tab.leave_compare();
                match app.write_tab(&tab, Some(disk_etag)) {
                    Ok(()) => app.toast("Saved"),
                    Err(SaveError::ChangedOnDisk { .. }) => {
                        app.toast(&format!("{} changed on disk again", tab.rel()));
                    }
                    Err(e) => app.toast(&format!("Save failed: {e}")),
                }
            }
        ));
        tab.compare(
            &format!("{rel} (unsaved)"),
            (&format!("{rel} (on disk)"), &disk),
            diff::Side::Old,
            true,
            Some(choice_row(&keep_theirs, &keep_mine)),
            "Changed on Disk",
        );
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

    /// Raise or drop the conflict question on the tab showing `rel`, from what is on disk now.
    ///
    /// DESIGN.md, States: a conflict copy is a state that persists and needs a decision, so it is
    /// a banner on the note it concerns rather than a toast that scrolls past. It queues behind a
    /// "changed on disk" question rather than displacing it, and taking it down again brings that
    /// one back instead of clearing the bar.
    ///
    /// `trashed` is a copy this window has just sent to the trash. The index is a worker thread
    /// and a batch behind, so it still lists the file and the banner would otherwise linger until
    /// `FileRemoved` caught up a few hundred milliseconds later.
    fn sync_conflict_banner(&self, rel: &str, trashed: Option<&str>) {
        // Conflict copies are a vault idea: they are found by the index.
        if self.vault.is_none() {
            return;
        }
        let Some(tab) = self.tab_for(rel) else {
            return;
        };
        let standing = self
            .vault()
            .and_then(|v| v.conflicts_of(rel).ok())
            .unwrap_or_default()
            .iter()
            .any(|copy| Some(copy.as_str()) != trashed);
        match standing {
            true => tab.show_alert(Alert::Conflict),
            false => tab.clear_alert(Alert::Conflict),
        }
    }

    /// A sync conflict copy beside the note it was copied from, in the note's own tab: the editor
    /// is Mine, live, so an unsaved edit is in the comparison rather than older than it.
    fn resolve_conflict(self: &Rc<Self>, original: &str, conflict: &str) {
        let Some(vault) = self.vault() else {
            return;
        };
        let Ok((theirs, theirs_etag)) = vault.read(conflict) else {
            return self.toast("Cannot read the conflict copy");
        };
        let (original, conflict) = (original.to_string(), conflict.to_string());
        let theirs_title = written_at(&conflict, &theirs_etag);
        self.with_tab(&original.clone(), Opened::Kept, move |app, tab| {
            let mine_title = match tab.etag.get() {
                Some(etag) => written_at(&original, &etag),
                None => original.clone(),
            };
            let keep_theirs = gtk::Button::with_label("Keep Theirs");
            let keep_mine = gtk::Button::with_label("Keep Mine");
            keep_mine.add_css_class("suggested-action");
            // Keeping theirs adopts the copy and reloads the tab over it: what was Mine, unsaved
            // edits included, was on screen and is the side the user gave up.
            keep_theirs.connect_clicked(glib::clone!(
                #[weak]
                app,
                #[weak]
                tab,
                #[strong]
                original,
                #[strong]
                conflict,
                move |_| {
                    tab.leave_compare();
                    let Some(vault) = app.vault() else { return };
                    if let Err(e) = vault.adopt_conflict(&original, &conflict) {
                        return app.toast(&format!("Cannot resolve: {e:#}"));
                    }
                    tab.discard();
                    app.refresh_tab(&tab);
                    app.finish_conflict(&original, &conflict);
                }
            ));
            // Keeping mine is only the copy going away: the merge is the buffer, and the buffer
            // saves as it always does, through the tab's own etag gate.
            keep_mine.connect_clicked(glib::clone!(
                #[weak]
                app,
                #[weak]
                tab,
                #[strong]
                original,
                #[strong]
                conflict,
                move |_| {
                    tab.leave_compare();
                    if tab.modified.get() {
                        app.save_tab(&tab, false);
                    }
                    app.finish_conflict(&original, &conflict);
                }
            ));
            tab.compare(
                &mine_title,
                (&theirs_title, &theirs),
                diff::Side::Old,
                true,
                Some(choice_row(&keep_theirs, &keep_mine)),
                "Sync Conflict",
            );
        });
    }

    /// The copy goes to the trash, and the banner is told before the index has seen it go.
    fn finish_conflict(&self, original: &str, conflict: &str) {
        if let Some(ops) = self.ops() {
            fileops::trash(ops, conflict);
        }
        self.sync_conflict_banner(original, Some(conflict));
    }

    // --- view modes and preview -----------------------------------------------------------

    fn set_mode(self: &Rc<Self>, mode: Mode) {
        self.mode.set(mode);
        self.modes.set_icon_name(mode.icon());
        // Setting `active` re-enters the toggled handler, which compares against `self.mode` and
        // stops there, so this cannot loop.
        self.modes.set_active(mode == Mode::Split);
        self.show_chrome();
        self.apply_layout();
        self.save_session_soon();
    }

    /// Which of the editor column and the preview are on screen. Split shows both; presenting
    /// shows the preview alone, whatever mode the user will come back to — unless the tab renders
    /// itself, in which case it is the thing being presented and the preview stays away.
    fn apply_layout(self: &Rc<Self>) {
        let presenting = self.presenting.get().is_some();
        // A PDF, an image, a diff, a terminal: anything that is not a note in a buffer. There is
        // nothing for the preview to render, so hiding the document column would present a blank
        // window.
        let own_view = presenting && self.active().is_none();
        if self.shows_preview() && !own_view {
            self.ensure_preview();
        }
        self.content.set_visible(!presenting || own_view);
        self.hoist_find(presenting && !own_view);
        if let Some(preview) = self.preview.borrow().as_ref() {
            preview
                .widget()
                .set_visible(self.shows_preview() && !own_view);
        }
        // The tab bars go with the rest of the chrome, since the column they live in stays. The
        // restore is unconditional: `AdwTabBar` reveals and hides itself, and leaving it hidden
        // here would take that decision away from it for good.
        for pane in self.panes.borrow().iter() {
            pane.bar.set_visible(!own_view);
        }
        if self.mode.get() == Mode::Split && !presenting {
            self.even_split();
        }
        if let Some(tab) = self.active() {
            self.render(&tab);
        }
    }

    /// Lend the presented pane's find bar to the editor column, or give every bar back.
    ///
    /// A bar lives in its pane, and presentation mode takes the whole pane tree off screen — the
    /// one thing the old window-wide bar had over this. `Ctrl+F` over a rendered note still has to
    /// reach something visible, so the pane being presented lends its bar to the column above for
    /// as long as that lasts. A tab that draws its own document keeps its pane, and its bar with
    /// it, so this only ever moves one bar and only while a *note* is being presented.
    fn hoist_find(&self, up: bool) {
        let active = self.pane();
        let column: &gtk::Widget = self.editor_column.upcast_ref();
        for pane in self.panes.borrow().iter() {
            let bar = pane.find.widget();
            if !(up && Rc::ptr_eq(pane, &active)) {
                pane.hold_find();
            } else if bar.parent().as_ref() != Some(column) {
                if let Some(old) = bar.parent().and_downcast::<gtk::Box>() {
                    old.remove(bar);
                }
                self.editor_column.append(bar);
                // Above the document, not below it: appending put it after the toasts.
                self.editor_column
                    .reorder_child_after(&self.toasts, Some(bar));
            }
        }
    }

    /// Put the handle back in the middle whenever the preview comes on screen. A `GtkPaned` keeps
    /// whatever position it was left at, and one that has never been allocated has none at all, so
    /// the editor's natural width could take the whole row and the preview open with nothing to
    /// show: the "clicked the button and nothing happened" report. Presentation mode never hit it,
    /// because there the editor column is hidden outright.
    fn even_split(self: &Rc<Self>) {
        if self.centre_handle() {
            return;
        }
        // No allocation yet, which is where a session restored straight into split mode lands.
        glib::idle_add_local_once(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move || {
                app.centre_handle();
            }
        ));
    }

    /// Centres the paned handle, or reports that there is no width to centre within yet.
    fn centre_handle(&self) -> bool {
        let width = self.paned.width();
        if width > 0 {
            self.paned.set_position(width / 2);
        }
        width > 0
    }

    /// Whether the rendered note is visible at all; nothing is rendered into a hidden preview.
    fn shows_preview(&self) -> bool {
        self.mode.get() == Mode::Split || self.presenting.get().is_some()
    }

    /// F5: the document alone, with the sidebar, the tab bars and both header bars gone. A state
    /// of the window rather than a [`Mode`], because it is a way of looking at the current tab
    /// instead of a layout to work in, and it is deliberately not part of the session: a window
    /// restored chromeless would be hard to get out of.
    ///
    /// A note is presented through the preview, rendered. A tab that draws its own document — a
    /// PDF, an image, a diff, a terminal — is presented as it is: `apply_layout` keeps the
    /// document column and takes the tab bars instead.
    fn set_presenting(self: &Rc<Self>, on: bool) {
        // A PDF presents itself: one whole page, and the zoom it had back afterwards.
        if let Some(pdf) = self.active_pdf() {
            pdf.set_presenting(on);
        }
        match (on, self.presenting.get()) {
            (true, None) => {
                self.presenting.set(Some(Presenting {
                    mode: self.mode.get(),
                    sidebar: self.sidebar_column.is_visible(),
                }));
                self.sidebar_column.set_visible(false);
                self.toolbar.set_reveal_top_bars(false);
                self.toolbar.set_reveal_bottom_bars(false);
                self.apply_layout();
            }
            (false, Some(before)) => {
                self.presenting.set(None);
                self.sidebar_column.set_visible(before.sidebar);
                self.toolbar.set_reveal_top_bars(true);
                self.toolbar.set_reveal_bottom_bars(true);
                // Puts the layout back and, with presenting cleared, lets the chrome show again.
                self.set_mode(before.mode);
            }
            _ => {}
        }
    }

    fn ensure_preview(self: &Rc<Self>) {
        if self.preview.borrow().is_some() {
            return;
        }
        // The preview's assets come through the vault, so a note's images load whether the file
        // is on this disk or on a host. A window with no vault has only absolute keys, which the
        // resolver hands straight back.
        let vault = self.vault().cloned();
        let root = self.root();
        let preview = preview::Preview::new(
            move |rel: &str| match &vault {
                // `fetch` refuses a `rel` that climbs out lexically, on either backend. What it
                // cannot see is a symlink *inside* the vault pointing outside it, and the answer
                // here is handed to a WebView, so that is worth one `canonicalize`: a note
                // linking `escape.png -> ~/.ssh/id_rsa` must not render it.
                // `asset` first, because `![[img.png]]` names the file the way a wikilink does
                // and the index is what knows it lives in `Attachments/`.
                Some(vault) => vault
                    .asset(rel)
                    .and_then(|rel| vault.fetch(&rel).ok())
                    .filter(|path| {
                        match (path.canonicalize(), vault.root().canonicalize()) {
                            (Ok(real), Ok(root)) => real.starts_with(root),
                            // A remote vault's copy lives in the cache, not under the root, and the
                            // host already refused anything that escapes it there.
                            _ => vault.is_remote(),
                        }
                    }),
                None => Some(root.join(rel)),
            },
            glib::clone!(
                #[weak(rename_to = app)]
                self,
                move |target: &str| app.open_target(target)
            ),
        );
        self.paned.set_end_child(Some(preview.widget()));
        preview.set_zoom(self.zoom.get());
        // The preview follows the document zoom, so the wheel over it has to reach the same
        // setting the wheel over the editor does. Capture phase: WebKit answers a Ctrl+scroll
        // itself, with a zoom of its own that nothing else in the window knows about.
        zoom_on_wheel(
            preview.widget(),
            gtk::PropagationPhase::Capture,
            glib::clone!(
                #[weak(rename_to = app)]
                self,
                move |out| app.set_zoom(stepped_zoom(app.zoom.get(), out))
            ),
        );
        preview.connect_found(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |label| app.pane().find.set_matches_text(label)
        ));
        *self.preview.borrow_mut() = Some(preview);
    }

    fn render(self: &Rc<Self>, tab: &Rc<Tab>) {
        if !self.shows_preview() {
            return;
        }
        self.ensure_preview();
        if let Some(preview) = self.preview.borrow().as_ref() {
            preview.render(&tab.rel(), &tab.text());
            preview.scroll_to_line(tab.cursor_line());
        }
    }

    /// What follows an edit into the tab in front, [`RENDER`] after the last keystroke: the
    /// preview is re-rendered, and the status bar is asked for the word count again.
    ///
    /// One timer for both, because they are the same question — what does the buffer say now.
    /// The count used to be read only when a tab was opened or switched to, so a draft grew
    /// under a number that never moved; counting per keystroke instead would copy the whole
    /// buffer out on every key, and a number that settles a third of a second later reads the
    /// same to anyone watching it.
    fn queue_refresh(self: &Rc<Self>, tab: &Rc<Tab>) {
        if !self.is_active(tab) {
            return;
        }
        if let Some(id) = self.refresh.borrow_mut().take() {
            id.remove();
        }
        let id = glib::timeout_add_local_once(
            RENDER,
            glib::clone!(
                #[weak(rename_to = app)]
                self,
                move || {
                    *app.refresh.borrow_mut() = None;
                    app.sync_status();
                    if let Some(tab) = app.active().filter(|_| app.shows_preview()) {
                        app.render(&tab);
                    }
                }
            ),
        );
        *self.refresh.borrow_mut() = Some(id);
    }

    /// A pane's find bar addressing the rendered preview, which is what it does while presenting.
    fn preview_find(&self, pane: &Pane, op: find::PreviewOp) {
        // A PDF gets first refusal: it is what the user is looking at, and it counts its own
        // matches rather than letting the bar count them.
        if let Some(pdf) = self.pdf_of(pane) {
            match op {
                find::PreviewOp::Find(text) => pdf.find(&text),
                find::PreviewOp::Next => pdf.step_match(true),
                find::PreviewOp::Previous => pdf.step_match(false),
                find::PreviewOp::Clear => pdf.find(""),
                // Only the Return moves a PDF. A live preview under a half-typed page number
                // renders pages nobody asked to read, and it has already left the page Back is
                // supposed to return to, so the committed jump would have nothing to remember.
                find::PreviewOp::Line { line, commit: true } => {
                    pdf.goto_page((line as usize).saturating_sub(1))
                }
                find::PreviewOp::Line { commit: false, .. } => {}
            }
            return;
        }
        let preview = self.preview.borrow();
        let Some(preview) = preview.as_ref() else {
            return;
        };
        match op {
            find::PreviewOp::Find(text) => preview.find(&text),
            find::PreviewOp::Next => preview.find_next(),
            find::PreviewOp::Previous => preview.find_previous(),
            find::PreviewOp::Clear => preview.find_clear(),
            find::PreviewOp::Line { line, .. } => preview.scroll_to_line(line),
        }
    }

    fn sync_scroll(&self, tab: &Rc<Tab>) {
        if !self.shows_preview() {
            return;
        }
        if let Some(preview) = self.preview.borrow().as_ref() {
            preview.scroll_to_line(tab.cursor_line());
        }
    }

    // --- chrome --------------------------------------------------------------------------

    /// A change in `tab`'s buffer: the chrome fades while the user types, which is the point of
    /// the app (DESIGN.md), and the tab stops being a preview.
    ///
    /// Only a keystroke into *this* tab's own view counts. A reload writing into a background
    /// buffer is not the user typing, and neither is one arriving in another pane while the
    /// keyboard is here.
    fn on_edit(&self, tab: &Rc<Tab>) {
        if !tab.view.has_focus() {
            return;
        }
        // Where the edit is. Consecutive keystrokes in one paragraph coalesce into one entry, so
        // typing leaves a mark rather than hundreds (`panes::coalesces`).
        self.mark_page(&tab.page);
        self.hide_chrome();
        self.promote(&tab.page);
    }

    /// `Root` and `GtkWindow` both spell this `focus`, so the window's one is named here once.
    fn focused(&self) -> Option<gtk::Widget> {
        gtk::prelude::GtkWindowExt::focus(&self.window)
    }

    fn hide_chrome(&self) {
        if self.chrome_hidden.get() || self.chrome_busy() {
            return;
        }
        self.chrome_hidden.set(true);
        self.sidebar_header.add_css_class("chrome-hidden");
        self.header.add_css_class("chrome-hidden");
        self.statusbar.widget().add_css_class("chrome-hidden");
        for pane in self.panes.borrow().iter() {
            pane.bar.add_css_class("chrome-hidden");
        }
        // The sidebar's panes dim instead of hiding: the tree is context, and losing it while
        // typing would be losing the place in the vault (DESIGN.md, Chrome auto-hide).
        if let Some(sidebar) = self.sidebar.get() {
            sidebar.widget().add_css_class("chrome-dimmed");
        }
    }

    fn show_chrome(&self) {
        // Presentation owns the chrome while it lasts: a pointer that crosses the window must not
        // undo it, or the mode is useless.
        if self.presenting.get().is_some() || !self.chrome_hidden.replace(false) {
            return;
        }
        self.sidebar_header.remove_css_class("chrome-hidden");
        self.header.remove_css_class("chrome-hidden");
        self.statusbar.widget().remove_css_class("chrome-hidden");
        for pane in self.panes.borrow().iter() {
            pane.bar.remove_css_class("chrome-hidden");
        }
        if let Some(sidebar) = self.sidebar.get() {
            sidebar.widget().remove_css_class("chrome-dimmed");
        }
    }

    /// Never fade over something that is waiting for an answer: a dialog, a banner, an open
    /// popover or the find bar.
    fn chrome_busy(&self) -> bool {
        if self.window.visible_dialog().is_some() {
            return true;
        }
        let in_popover = self
            .focused()
            .is_some_and(|w| w.ancestor(gtk::Popover::static_type()).is_some());
        in_popover
            || self.panes.borrow().iter().any(|pane| pane.find.is_open())
            || self.active().is_some_and(|tab| tab.banner.is_revealed())
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
