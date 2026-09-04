//! accent desktop app: GTK4 + libadwaita shell.
//!
//! `accent [vault-dir] [note.md]`. Without a path the start screen picks a vault; with one,
//! [`accent_api::Vault`] opens the index and the tree is filled straight from it, while the
//! worker thread reconciles and watches in the background. The window is never blocked, and every
//! change the vault reports arrives here as an [`Event`].

mod completion;
mod diff;
mod editor;
mod fileops;
mod find;
mod highlight;
mod multicaret;
mod palette;
mod paned;
mod panes;
mod preview;
mod settings;
mod sidebar;
mod start;
mod theme;
mod tree;
mod typing;

use accent_api::{Config, Etag, Event, SaveError, Session, Vault};
use accent_core::index::Phase;
use accent_core::markdown::{self, Link, LinkKind};
use adw::prelude::*;
use editor::{Alert, Tab};
use gtk::{gdk, gio, glib, pango};
use panes::{Pane, Side, Zone};
use std::cell::{Cell, OnceCell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

const APP_ID: &str = "io.github.stroblme.Accent";

/// What the palette lists before the user types anything.
const RECENT_NOTES: usize = 50;
/// Commands kept in the session's recently-used list. There are only about forty of them, so a
/// shorter list is still every command the user actually reaches for.
const RECENT_COMMANDS: usize = 20;
/// Full-text hits the sidebar shows; beyond this the list stops being scannable.
const SEARCH_LIMIT: usize = 100;
/// Rows in a `[[wikilink]]` or `#tag` completion popup.
const COMPLETIONS: usize = 20;
/// DESIGN.md, Motion: the preview re-renders 300 ms after the last edit.
const RENDER: Duration = Duration::from_millis(300);
/// Session state is cheap to lose and noisy to write, so it follows a change by a second.
const SESSION: Duration = Duration::from_secs(1);
/// The vault worker is polled instead of woken; 120 ms is below what a progress label needs.
const POLL: Duration = Duration::from_millis(120);
/// One press of Zoom In or Zoom Out, a tenth of the document font.
const ZOOM_STEP: f64 = 0.1;
/// How often the tree may be re-read while the first index is still running, in microseconds:
/// often enough that a cold start fills in as it goes, rarely enough to stay off the main loop.
const TREE_REPAINT: i64 = 250_000;

/// Every user-facing action: the name it answers to, the label the menu and the palette show, and
/// its accelerators. One table, so an action cannot exist without being reachable and findable
/// (DESIGN.md, Keyboard). Tab switching is `AdwTabView`'s own set of shortcuts.
const ACTIONS: &[(&str, &str, &[&str])] = &[
    ("win.save", "Save", &["<Control>s"]),
    ("win.new-note", "New Note", &["<Control>n"]),
    ("win.new-folder", "New Folder", &["<Control><Shift>n"]),
    ("win.close-tab", "Close Tab", &["<Control>w"]),
    // Split Right takes VS Code's chord; the other three are menu and palette only, because
    // three more accelerators for the same idea is three more chords nobody has to spare.
    ("win.split-right", "Split Right", &["<Control>backslash"]),
    ("win.split-left", "Split Left", &[]),
    ("win.split-up", "Split Up", &[]),
    ("win.split-down", "Split Down", &[]),
    ("app.open-vault", "Open Folder…", &["<Control><Shift>o"]),
    ("app.close-vault", "Close Vault", &[]),
    ("app.quit", "Quit", &["<Control>q"]),
    ("win.palette-files", "Open Note…", &["<Control>e"]),
    (
        "win.palette-commands",
        "Run a Command…",
        &["<Control>p", "<Control><Shift>p"],
    ),
    ("win.find", "Find", &["<Control>f"]),
    ("win.replace", "Replace", &["<Control>h"]),
    (
        "win.replace-in-files",
        "Replace in Notes",
        &["<Control><Shift>h"],
    ),
    ("win.find-next", "Find Next", &["F3"]),
    ("win.find-previous", "Find Previous", &["<Shift>F3"]),
    ("win.goto-line", "Go to Line", &["<Control>g"]),
    ("win.duplicate-line", "Duplicate Line", &["<Control>d"]),
    ("win.delete-line", "Delete Line", &["<Control>l"]),
    ("win.scroll-up", "Scroll Up", &["<Control>Up"]),
    ("win.scroll-down", "Scroll Down", &["<Control>Down"]),
    ("win.caret-above", "Add Caret Above", &["<Shift><Alt>Up"]),
    ("win.caret-below", "Add Caret Below", &["<Shift><Alt>Down"]),
    (
        "win.zoom-in",
        "Zoom In",
        &["<Control>plus", "<Control>equal", "<Control>KP_Add"],
    ),
    (
        "win.zoom-out",
        "Zoom Out",
        &["<Control>minus", "<Control>KP_Subtract"],
    ),
    (
        "win.zoom-reset",
        "Reset Zoom",
        &["<Control>0", "<Control>KP_0"],
    ),
    ("win.sidebar", "Toggle Sidebar", &["F9"]),
    ("win.pane-files", "Files Pane", &["<Control><Shift>e"]),
    ("win.pane-search", "Search Pane", &["<Control><Shift>f"]),
    ("win.pane-tags", "Tags Pane", &["<Control><Shift>t"]),
    ("win.backlinks", "Backlinks Pane", &["<Control><Shift>b"]),
    ("win.view-mode", "Toggle Split View", &["<Control>m"]),
    ("win.minimap", "Toggle Minimap", &[]),
    ("win.copy-relative-path", "Copy Relative Path", &[]),
    ("win.copy-absolute-path", "Copy Absolute Path", &[]),
    ("win.show-in-files", "Show in Files", &[]),
    ("win.reveal-in-sidebar", "Reveal in Sidebar", &[]),
    ("win.follow-link", "Follow Link", &["<Control>Return"]),
    ("win.rename", "Rename", &["F2"]),
    ("win.daily-note", "Daily Note", &["<Control><Shift>d"]),
    ("win.present", "Presentation Mode", &["F5"]),
    ("win.fullscreen", "Fullscreen", &["F11"]),
    ("win.preferences", "Preferences", &["<Control>comma"]),
    ("win.menu", "Primary Menu", &["F10"]),
    ("win.about", "About accent", &[]),
];

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
    });
    shell.install_app_actions(&app);
    app.connect_command_line({
        let shell = shell.clone();
        move |gtk_app, command_line| shell.command_line(gtk_app, command_line)
    });
    app.run()
}

// -------------------------------------------------------------------------------------- shell

/// One process, one config, one window per vault.
struct Shell {
    config: Rc<RefCell<Config>>,
    /// The open vaults, and the only strong reference to each window's state: an entry is dropped
    /// in `forget` when the window closes, which is what releases the vault and its worker thread.
    windows: RefCell<Vec<(PathBuf, Rc<App>)>>,
    /// The start screen while one is up, so Open Folder… presents it again instead of stacking a
    /// second copy. Weak: the window belongs to GTK, and closing it is how it goes away.
    start: glib::WeakRef<adw::ApplicationWindow>,
}

impl Shell {
    /// The two actions that outlive the window firing them: Open Folder… lands on the start
    /// screen, and Close Vault takes the current window away, so neither can live on a window the
    /// way the `win.` actions do. Registered once on the application, where the shell is in scope.
    fn install_app_actions(self: &Rc<Self>, gtk_app: &adw::Application) {
        let open = gio::SimpleAction::new("open-vault", None);
        open.connect_activate({
            let (shell, gtk_app) = (self.clone(), gtk_app.clone());
            move |_, _| shell.start_screen(&gtk_app)
        });
        gtk_app.add_action(&open);

        let close = gio::SimpleAction::new("close-vault", None);
        close.connect_activate({
            let (shell, gtk_app) = (self.clone(), gtk_app.clone());
            move |_, _| shell.close_vault(&gtk_app)
        });
        gtk_app.add_action(&close);
    }

    /// Close Vault: hand this window's vault back and land on the start screen.
    ///
    /// The window goes rather than being emptied out. The vault, its worker thread and its WebKit
    /// process all hang off the window's [`App`], so closing the window is what releases them, and
    /// it is the path a user closing the window already takes. The cost is the window's geometry,
    /// which the next vault takes from the defaults again. The start screen is presented first, so
    /// the application never stands at zero windows and quits out from under us.
    fn close_vault(self: &Rc<Self>, gtk_app: &adw::Application) {
        let Some(window) = gtk_app.active_window() else {
            return;
        };
        // Only a vault window has a vault to close; from the start screen this leads nowhere.
        let opened = self
            .windows
            .borrow()
            .iter()
            .any(|(_, app)| app.window.upcast_ref::<gtk::Window>() == &window);
        if !opened {
            return;
        }
        self.start_screen(gtk_app);
        window.close();
    }

    /// Let go of a window's [`App`] once the close is certain. This is the only strong reference
    /// to it, so the vault, its worker thread and its WebKit process all go with it.
    ///
    /// ponytail: the `App` goes here, the vault a moment later — the tree and the sidebar hold
    /// their own `Rc<Vault>` inside widgets, so the last one drops when GTK destroys the window,
    /// and `Vault`'s `Drop` joins the worker there. Closing a second into `testvault`'s 2.7 s cold
    /// reconcile blocked the main loop for 2.3 s, with the window already off screen. `Vault::drop`
    /// names the fix (a cancellation flag on `reconcile`); until a vault is opened and closed often
    /// enough for that pause to be felt, one stalled close is cheaper than the flag.
    fn forget(&self, window: &adw::ApplicationWindow) {
        let mut windows = self.windows.borrow_mut();
        let Some(i) = windows.iter().position(|(_, app)| &app.window == window) else {
            return;
        };
        let app = windows.remove(i);
        // Out of the borrow before the drop: `App` reaches a long way as it goes.
        drop(windows);
        drop(app);
    }

    fn command_line(
        self: &Rc<Self>,
        gtk_app: &adw::Application,
        command_line: &gio::ApplicationCommandLine,
    ) -> glib::ExitCode {
        let args = command_line.arguments();
        let Some(arg) = args.get(1) else {
            // Launched with no folder: pick up the vault this window was last opened on, and only
            // fall back to the start screen when there is none or it has gone away.
            let last = self.config.borrow().recent_vaults.first().cloned();
            match last.filter(|path| path.is_dir()) {
                Some(root) => self.open_vault(gtk_app, root, None),
                None => self.start_screen(gtk_app),
            }
            return glib::ExitCode::SUCCESS;
        };
        let path = PathBuf::from(arg);
        let root = match path.canonicalize() {
            Ok(root) if root.is_dir() => root,
            _ => {
                // `printerr_literal` needs glib 2.80, which this build does not enable; a
                // local invocation is the only one that has a terminal to print to anyway.
                eprintln!("not a directory: {}", path.display());
                return glib::ExitCode::FAILURE;
            }
        };
        let note = args.get(2).and_then(|a| a.to_str()).map(str::to_string);
        self.open_vault(gtk_app, root, note);
        glib::ExitCode::SUCCESS
    }

    fn start_screen(self: &Rc<Self>, gtk_app: &adw::Application) {
        if let Some(window) = self.start.upgrade() {
            window.present();
            return;
        }
        let window = start::present(gtk_app, self.config.clone(), {
            let (shell, gtk_app) = (self.clone(), gtk_app.clone());
            move |root| {
                shell.open_vault(&gtk_app, root, None);
                // The start window has done its job. It is reached through the shell rather than
                // captured, which is what keeps the closure it lives in out of its own cycle.
                if let Some(window) = shell.start.upgrade() {
                    window.close();
                }
            }
        });
        self.start.set(Some(&window));
    }

    fn open_vault(
        self: &Rc<Self>,
        gtk_app: &adw::Application,
        root: PathBuf,
        note: Option<String>,
    ) {
        if let Some(window) = self.window_for(&root) {
            window.present();
            return;
        }
        let Some(app) = build_window(gtk_app, self, root.clone(), note) else {
            return;
        };
        // A second `close-request` handler. `wire_window`'s is connected first and can still stop
        // the close (an unsaved buffer that will not write), and GTK stops emitting as soon as one
        // handler does, so this one only ever sees a close that is really happening.
        app.window.connect_close_request({
            let shell = Rc::downgrade(self);
            move |window| {
                if let Some(shell) = shell.upgrade() {
                    shell.forget(window);
                }
                glib::Propagation::Proceed
            }
        });
        self.windows.borrow_mut().push((root, app));
    }

    fn window_for(&self, root: &Path) -> Option<adw::ApplicationWindow> {
        let windows = self.windows.borrow();
        let (_, app) = windows.iter().find(|(path, _)| path == root)?;
        Some(app.window.clone())
    }
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

struct App {
    /// `Arc`, not `Rc`: the sidebar's search runs its queries on a worker thread.
    vault: Arc<Vault>,
    config: Rc<RefCell<Config>>,
    window: adw::ApplicationWindow,
    /// Every open pane, in the order they were created. The arrangement itself lives in the
    /// widget tree under `root`; this is only what has to be iterated over.
    panes: RefCell<Vec<Rc<Pane>>>,
    /// The pane a note opens into and the one the find bar and the preview follow.
    active_pane: RefCell<Rc<Pane>>,
    title: adw::WindowTitle,
    toasts: adw::ToastOverlay,
    /// Find, replace and go to line, one bar for the window rather than one per tab.
    find: Rc<find::Bar>,
    status: gtk::Label,
    /// A `Vec`, not a map: a rename retargets an open tab, so `rel` is not a stable key.
    open: RefCell<Vec<Rc<Tab>>>,
    /// View-only image tabs, which have no buffer, no etag and no place in the session.
    images: RefCell<Vec<(String, adw::TabPage)>>,
    /// Set once, after `App` exists, by the sidebar the tree lives in.
    tree: OnceCell<tree::Tree>,
    sidebar: OnceCell<sidebar::Sidebar>,
    ops: OnceCell<Rc<fileops::Ops>>,
    /// Built on the first Split or Preview: a WebKit process per window is not worth paying for
    /// at startup by someone who only ever writes.
    preview: RefCell<Option<preview::Preview>>,
    /// Sidebar on the left, editor column on the right; drag the handle to resize.
    split: gtk::Paned,
    /// The sidebar column itself: hiding the sidebar is hiding this widget.
    sidebar_column: adw::ToolbarView,
    sidebar_header: adw::HeaderBar,
    /// The editor column: presentation mode unreveals its top bars, which is the header. The tab
    /// bars belong to the panes and go with `content`.
    toolbar: adw::ToolbarView,
    header: adw::HeaderBar,
    modes: gtk::ToggleButton,
    menu: gtk::MenuButton,
    paned: gtk::Paned,
    /// Swaps the pane tree for a placeholder while no note is open (DESIGN.md, States).
    content: gtk::Stack,
    mode: Cell<Mode>,
    /// Document zoom, applied to every tab and to the preview, never to the chrome.
    zoom: Cell<f64>,
    /// The zoom readout floating over the document, shown only while the zoom is not 100 %.
    zoom_pill: gtk::Box,
    zoom_label: gtk::Label,
    /// `Some` while presenting, holding what to restore on the way out.
    presenting: Cell<Option<Presenting>>,
    chrome_hidden: Cell<bool>,
    /// Whether a reconcile has finished, so the index can be trusted for backlinks. A real flag
    /// rather than the status label, which is also hidden before the first `Progress` arrives.
    reconciled: Cell<bool>,
    /// The tab `setup-menu` named, so the tab context menu acts on the page that was
    /// right-clicked rather than on the selected one. `None` once the popup is gone, which is
    /// what makes the same actions work from the palette.
    menu_page: RefCell<Option<adw::TabPage>>,
    /// When the tree was last re-read during the first index, from `glib::monotonic_time`.
    tree_painted: Cell<i64>,
    render: RefCell<Option<glib::SourceId>>,
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
    fn toast(&self, text: &str) {
        self.toasts.add_toast(adw::Toast::new(text));
    }

    fn ops(&self) -> &Rc<fileops::Ops> {
        self.ops
            .get()
            .expect("file operations are set up in build_window")
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
        let from = self.pane_of(page);
        if from.as_ref().is_some_and(|f| Rc::ptr_eq(f, at)) && at.tabs.n_pages() <= 1 {
            return self.toast("This pane has only one note.");
        }
        let pane = self.split_beside(at, side);
        if let Some(from) = from {
            from.tabs.transfer_page(page, &pane.tabs, 0);
        }
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
            None => self.open_note(rel),
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

    /// A tab or a vault path let go over `pane`. `true` when it was taken.
    fn dropped(self: &Rc<Self>, pane: &Rc<Pane>, zone: Zone, value: &glib::Value) -> bool {
        if let Ok(page) = value.get::<adw::TabPage>() {
            return match zone {
                Zone::Split(side) => {
                    self.split_page(pane, side, &page);
                    true
                }
                // Already here: libadwaita would have reordered it, and there is nothing to move.
                Zone::Here if self.pane_of(&page).is_some_and(|p| Rc::ptr_eq(&p, pane)) => false,
                Zone::Here => {
                    let Some(from) = self.pane_of(&page) else {
                        return false;
                    };
                    from.tabs
                        .transfer_page(&page, &pane.tabs, pane.tabs.n_pages());
                    pane.tabs.set_selected_page(&page);
                    true
                }
            };
        }
        let Ok(rel) = value.get::<String>() else {
            return false;
        };
        match zone {
            Zone::Split(side) => self.open_beside(pane, side, &rel),
            Zone::Here => {
                self.set_active_pane(pane);
                self.open_note(&rel);
            }
        }
        true
    }

    fn active(&self) -> Option<Rc<Tab>> {
        let page = self.tabs().selected_page()?;
        self.open.borrow().iter().find(|t| t.page == page).cloned()
    }

    fn tab_for(&self, rel: &str) -> Option<Rc<Tab>> {
        self.open.borrow().iter().find(|t| t.rel() == rel).cloned()
    }

    fn is_active(&self, tab: &Rc<Tab>) -> bool {
        self.active().is_some_and(|a| Rc::ptr_eq(&a, tab))
    }

    /// Every open tab, cloned out: the callbacks below reach back into `open`, and a live borrow
    /// across them would be a panic waiting to happen.
    fn open_tabs(&self) -> Vec<Rc<Tab>> {
        self.open.borrow().clone()
    }

    // --- opening -------------------------------------------------------------------------

    fn open_note(self: &Rc<Self>, rel: &str) {
        let Some(rel) = self.safe_rel(rel) else {
            // A session pointing at a note that has since been deleted lands here too, and
            // "outside this vault" would be the wrong thing to say about it.
            return match self.vault.root().join(rel).exists() {
                true => self.toast(&format!("{rel} is outside this vault")),
                false => self.toast(&format!("Cannot open {rel}: no such note")),
            };
        };
        if let Some(tab) = self.tab_for(&rel) {
            return self.reveal_page(&tab.page);
        }
        let (spellcheck, font, minimap, line_numbers) = {
            let config = self.config.borrow();
            (
                config.spellcheck,
                config.editor_font.clone(),
                config.minimap,
                config.line_numbers,
            )
        };
        let opened = editor::open(
            self.vault.root(),
            &rel,
            &self.tabs(),
            {
                let vault = self.vault.clone();
                move |prefix| {
                    vault
                        .complete_notes(prefix, COMPLETIONS)
                        .unwrap_or_default()
                }
            },
            {
                let vault = self.vault.clone();
                move |prefix| vault.complete_tags(prefix, COMPLETIONS).unwrap_or_default()
            },
            spellcheck,
            font.as_deref(),
            self.zoom.get(),
        );
        match opened {
            Ok(tab) => {
                tab.set_minimap(minimap);
                tab.set_line_numbers(line_numbers);
                self.adopt(tab);
                self.sync_conflict_banner(&rel);
            }
            Err(e) => self.toast(&format!("Cannot open {rel}: {e}")),
        }
    }

    /// Open a note with the caret on a byte offset, which is how a sidebar search result opens the
    /// exact match rather than the top of the note.
    ///
    /// ponytail: the offset is turned into a character offset by counting the text in front of it,
    /// because `GtkTextBuffer` addresses characters. Fine for a note; a real byte-to-iter map
    /// belongs on `Tab` if anything ever needs it per keystroke.
    fn open_note_at(self: &Rc<Self>, rel: &str, offset: Option<usize>) {
        self.open_note(rel);
        let (Some(offset), Some(tab)) = (offset, self.tab_for(rel)) else {
            return;
        };
        let text = tab.text();
        let Some(head) = text.get(..offset.min(text.len())) else {
            return;
        };
        let iter = tab.buffer.iter_at_offset(head.chars().count() as i32);
        tab.buffer.place_cursor(&iter);
        tab.view
            .scroll_to_mark(&tab.buffer.get_insert(), 0.0, true, 0.0, 0.3);
        tab.view.grab_focus();
    }

    /// Rewrite every match of `re` in the vault, from the sidebar's Replace All.
    ///
    /// Open tabs are saved first: the vault writes through the etag gate, so an unsaved buffer
    /// would come back as a changed-on-disk banner instead of a replacement.
    fn replace_in_notes(self: &Rc<Self>, re: &accent_api::Regex, replacement: &str, literal: bool) {
        let open: Vec<String> = self.open_tabs().iter().map(|tab| tab.rel()).collect();
        (self.ops().flush)(&open);
        match self.vault.replace_all(re, replacement, literal) {
            Ok(report) => {
                let unsaved = (self.ops().reload)(&report.rewritten);
                self.toast(&replace_message(
                    report.matches,
                    report.rewritten.len(),
                    report.failed.len(),
                    unsaved,
                ));
            }
            Err(e) => self.toast(&format!("Cannot replace: {e:#}")),
        }
    }

    /// An image from the tree, in a tab that only looks at it.
    ///
    /// ponytail: the file goes straight into a `GtkPicture` at full resolution, the tab is not
    /// retargeted by a rename, it is left out of the session, and in split view the preview keeps
    /// showing the last note. All three want a real tab type, which is what Phase 2's PDF viewer
    /// has to build anyway.
    fn open_image(&self, rel: &str) {
        let Some(rel) = self.safe_rel(rel) else {
            return self.toast(&format!("{rel} is outside this vault"));
        };
        // Cloned out of the borrow: selecting a page runs the handlers that read this list.
        let open = self
            .images
            .borrow()
            .iter()
            .find(|(r, _)| *r == rel)
            .map(|(_, page)| page.clone());
        if let Some(page) = open {
            return self.reveal_page(&page);
        }
        let picture = gtk::Picture::for_filename(self.vault.root().join(&rel));
        picture.set_content_fit(gtk::ContentFit::ScaleDown);
        picture.set_can_shrink(true);
        let scroller = gtk::ScrolledWindow::builder()
            .hexpand(true)
            .vexpand(true)
            .child(&picture)
            .build();
        let page = self.tabs().append(&scroller);
        page.set_title(rel.rsplit('/').next().unwrap_or(&rel));
        page.set_tooltip(&fileops::display_path(self.vault.root(), &rel));
        page.set_icon(Some(&gio::ThemedIcon::new("image-x-generic-symbolic")));
        self.images.borrow_mut().push((rel, page.clone()));
        self.tabs().set_selected_page(&page);
    }

    /// A link target as written, resolved the way a wikilink resolves: by name, shortest path.
    fn open_target(self: &Rc<Self>, target: &str) {
        match self.vault.resolve_link(target) {
            Ok(Some(rel)) => self.open_note(&rel),
            Ok(None) => self.toast(&format!("No note called {target}")),
            Err(e) => self.toast(&format!("Cannot resolve {target}: {e:#}")),
        }
    }

    /// A vault-relative path that really is inside the vault, or `None`.
    ///
    /// Wikilink targets come out of note content, so `![[../../../../etc/passwd]]` reaches
    /// `open_note` from the preview and has to be stopped here rather than by the reader.
    ///
    /// ponytail: a note reached through a directory symlink canonicalises outside the root and is
    /// refused with it. Compare against `Index::symlink_dirs` as well the day linked-in code
    /// trees have to be openable from the preview.
    fn safe_rel(&self, rel: &str) -> Option<String> {
        let root = self.vault.root();
        let canonical = root.join(rel).canonicalize().ok()?;
        let inside = canonical.strip_prefix(root).ok()?;
        inside.to_str().map(str::to_string)
    }

    /// Wire a freshly opened tab into the window.
    fn adopt(self: &Rc<Self>, tab: Rc<Tab>) {
        tab.connect_autosave(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |tab| app.save_tab(tab, false)
        ));
        tab.connect_edited(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |tab| app.queue_render(tab)
        ));
        tab.connect_banner(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |tab| app.answer_banner(tab)
        ));
        tab.connect_follow(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |_, link| app.follow(link)
        ));
        tab.connect_cursor(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |tab| app.sync_scroll(tab)
        ));
        // The chrome hides on the keystroke itself, not on the debounce that follows it.
        tab.buffer.connect_changed(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |_| app.on_edit()
        ));

        let page = tab.page.clone();
        self.open.borrow_mut().push(tab);
        self.tabs().set_selected_page(&page);
        self.sync_active();
        self.save_session_soon();
    }

    /// Keep the window subtitle, the backlinks pane and the preview in step with the active tab.
    fn sync_active(self: &Rc<Self>) {
        self.find.retarget(self.active());
        let Some(tab) = self.active() else {
            self.title.set_subtitle("");
            if let Some(sidebar) = self.sidebar.get() {
                sidebar.set_backlinks(&[]);
            }
            return;
        };
        let rel = tab.rel();
        self.note_used(&rel);
        self.title.set_subtitle(&rel);
        if let Some(sidebar) = self.sidebar.get() {
            let mut sources: Vec<String> = Vec::new();
            for link in self.vault.backlinks(&rel).unwrap_or_default() {
                if !sources.contains(&link.src_rel_path) {
                    sources.push(link.src_rel_path);
                }
            }
            sidebar.set_backlinks(&sources);
        }
        self.render(&tab);
    }

    // --- saving --------------------------------------------------------------------------

    fn save_active(self: &Rc<Self>) {
        if let Some(tab) = self.active() {
            self.save_tab(&tab, true);
        }
    }

    /// `explicit` is a Ctrl+S, which may raise a dialog. An autosave never can: interrupting
    /// someone mid-sentence with a modal is exactly what autosave exists to avoid.
    fn save_tab(self: &Rc<Self>, tab: &Rc<Tab>, explicit: bool) {
        let text = tab.text();
        match self.write_tab(tab, &text, tab.etag.get()) {
            Ok(()) => {
                if explicit {
                    self.toast("Saved");
                }
            }
            Err(SaveError::ChangedOnDisk { .. }) if explicit => self.ask_overwrite(tab, text),
            Err(SaveError::ChangedOnDisk { .. }) => {
                tab.disk_changed.set(true);
                tab.show_alert(Alert::Compare);
            }
            Err(e) => self.toast(&format!("Save failed: {e}")),
        }
    }

    /// Write the buffer and hand the error back instead of reporting it: a caller that is about
    /// to make the buffer unreachable has to know whether the bytes landed.
    fn write_tab(
        &self,
        tab: &Rc<Tab>,
        text: &str,
        expected: Option<Etag>,
    ) -> Result<(), SaveError> {
        let etag = self.vault.save(&tab.rel(), text, expected)?;
        tab.mark_clean(etag);
        tab.clear_disk_alert();
        Ok(())
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
        true
    }

    fn ask_overwrite(self: &Rc<Self>, tab: &Rc<Tab>, text: String) {
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
                "overwrite" => match app.write_tab(&tab, &text, None) {
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
                    "overwrite" => match app.write_tab(&tab, &tab.text(), None) {
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
        self.open.borrow_mut().retain(|t| &t.page != page);
        self.images.borrow_mut().retain(|(_, p)| p != page);
        self.sync_active();
        self.save_session_soon();
    }

    /// The banner's button, doing what its label says. Which is which is decided when the banner
    /// goes up, not read off the file system when the button is pressed.
    fn answer_banner(self: &Rc<Self>, tab: &Rc<Tab>) {
        match tab.alert() {
            // Both sides hold work, so neither is thrown away on one click: the diff shows what
            // differs and the user picks (DESIGN.md: a choice that can lose data is a dialog).
            Some(Alert::Compare) => self.compare_with_disk(tab),
            Some(Alert::Restore) => match self.write_tab(tab, &tab.text(), None) {
                Ok(()) => self.toast("Saved"),
                Err(e) => self.toast(&format!("Save failed: {e}")),
            },
            // Looked up again rather than remembered: the copy may have been resolved from
            // another window, or by Syncthing, since the banner went up.
            Some(Alert::Conflict) => {
                let rel = tab.rel();
                match self.vault.conflicts_of(&rel).unwrap_or_default().first() {
                    Some(conflict) => self.resolve_conflict(&rel, conflict),
                    None => {
                        tab.hide_banner();
                        self.toast("The conflict copy is gone");
                    }
                }
            }
            None => tab.hide_banner(),
        }
    }

    /// The unsaved buffer against the file underneath it, in the conflict resolver.
    fn compare_with_disk(self: &Rc<Self>, tab: &Rc<Tab>) {
        let rel = tab.rel();
        let Ok((disk, _)) = self.vault.read(&rel) else {
            return self.toast(&format!("Cannot read {rel} from disk"));
        };
        let mine = tab.text();
        let resolve = {
            let (app, tab) = (self.clone(), tab.clone());
            move |choice| match choice {
                // Keeping mine forces the buffer over the file; keeping theirs drops the buffer,
                // which is a loss the user has now seen spelled out line by line. A pane edited
                // in the dialog replaces the buffer first, so what was compared is what is saved.
                diff::Choice::KeepMine { edited } => {
                    if let Some(text) = edited {
                        tab.set_text(&text);
                    }
                    match app.write_tab(&tab, &tab.text(), None) {
                        Ok(()) => app.toast("Saved"),
                        Err(e) => app.toast(&format!("Save failed: {e}")),
                    }
                }
                diff::Choice::KeepTheirs => {
                    tab.discard();
                    app.refresh_tab(&tab);
                }
            }
        };
        diff::present_conflict(
            &self.window,
            "Changed on disk",
            (&format!("{rel} (unsaved)"), &mine),
            (&format!("{rel} (on disk)"), &disk),
            resolve,
        );
    }

    // --- vault events --------------------------------------------------------------------

    fn on_event(self: &Rc<Self>, event: Event) {
        match event {
            Event::Progress(p) => {
                self.status
                    .set_label(&format!("Indexing… {}/{} files", p.done, p.total));
                self.status.set_visible(true);
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
            Event::Reconciled(stats) => {
                tracing::debug!(
                    t_ms = ms(),
                    scanned = stats.scanned,
                    unchanged = stats.unchanged,
                    "reconcile done"
                );
                self.status.set_visible(false);
                self.reconciled.set(true);
                if let Some(tree) = self.tree.get() {
                    tree.refresh();
                }
                if let Some(sidebar) = self.sidebar.get() {
                    sidebar.mark_tags_dirty();
                }
                self.sync_active();
                // Conflicts on notes nobody has open have no banner to appear on, so the toast
                // that is already there says how many are waiting in the vault.
                let mut message = format!(
                    "Indexed {} files ({} new, {} updated)",
                    stats.scanned, stats.added, stats.updated
                );
                match self.vault.conflicts().unwrap_or_default().len() {
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
                let Some(tab) = self.tab_for(&rel) else {
                    return;
                };
                self.refresh_tab(&tab);
                if self.is_active(&tab) {
                    self.sync_active();
                }
            }
            Event::FileRemoved(rel) => {
                // A conflict copy is never a tab of its own; what its removal changes is the
                // banner on the note it was a copy of.
                if let Some(original) = accent_api::conflict_original_rel(&rel) {
                    self.sync_conflict_banner(&original);
                }
                let Some(tab) = self.tab_for(&rel) else {
                    return;
                };
                if tab.modified.get() {
                    tab.disk_changed.set(true);
                    tab.show_alert(Alert::Restore);
                } else {
                    self.close_page(&tab.page);
                }
            }
            Event::FileRenamed { from, to } => {
                let prefix = format!("{from}/");
                for tab in self.open_tabs() {
                    let rel = tab.rel();
                    if rel == from {
                        tab.retarget(self.vault.root(), &to);
                    } else if let Some(rest) = rel.strip_prefix(&prefix) {
                        tab.retarget(self.vault.root(), &format!("{to}/{rest}"));
                    }
                }
                accent_core::config::rename_in(&mut self.recent_notes.borrow_mut(), &from, &to);
                self.sync_active();
            }
            Event::Conflict { original, .. } => self.sync_conflict_banner(&original),
            Event::Error(message) => self.toast(&message),
        }
    }

    /// Raise or drop the conflict banner on the tab showing `rel`, from what is on disk now.
    ///
    /// DESIGN.md, States: a conflict copy is a state that persists and needs a decision, so it is
    /// a banner on the note it concerns rather than a toast that scrolls past. It displaces a
    /// "changed on disk" banner if one is up, which loses no work: `disk_changed` still holds
    /// autosave back and Ctrl+S still raises the overwrite dialog.
    fn sync_conflict_banner(&self, rel: &str) {
        let Some(tab) = self.tab_for(rel) else {
            return;
        };
        match self.vault.conflicts_of(rel).unwrap_or_default().is_empty() {
            false => tab.show_alert(Alert::Conflict),
            true if tab.alert() == Some(Alert::Conflict) => tab.hide_banner(),
            true => {}
        }
    }

    fn resolve_conflict(self: &Rc<Self>, original: &str, conflict: &str) {
        let (Ok((mine, _)), Ok((theirs, _))) =
            (self.vault.read(original), self.vault.read(conflict))
        else {
            return self.toast("Cannot read the conflicting notes");
        };
        let resolve = {
            let (app, original, conflict) =
                (self.clone(), original.to_string(), conflict.to_string());
            move |choice| {
                // Keeping mine is only the copy going away, unless the dialog was edited: then
                // the merged text is written first. Keeping theirs adopts the copy.
                let rewritten = match choice {
                    diff::Choice::KeepTheirs => {
                        if let Err(e) = app.vault.adopt_conflict(&original, &conflict) {
                            return app.toast(&format!("Cannot resolve: {e:#}"));
                        }
                        true
                    }
                    diff::Choice::KeepMine { edited: Some(text) } => {
                        if let Err(e) = app.vault.save(&original, &text, None) {
                            return app.toast(&format!("Cannot resolve: {e}"));
                        }
                        true
                    }
                    diff::Choice::KeepMine { edited: None } => false,
                };
                // The note on disk is new text now, but a tab with unsaved edits still holds the
                // only copy of them: it gets the banner, not a silent overwrite.
                if rewritten && let Some(tab) = app.tab_for(&original) {
                    app.refresh_tab(&tab);
                }
                fileops::trash(app.ops(), &conflict);
                app.sync_conflict_banner(&original);
            }
        };
        diff::present_conflict(
            &self.window,
            "Sync conflict",
            (original, &mine),
            (conflict, &theirs),
            resolve,
        );
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
    /// shows the preview alone, whatever mode the user will come back to.
    fn apply_layout(self: &Rc<Self>) {
        let presenting = self.presenting.get().is_some();
        if self.shows_preview() {
            self.ensure_preview();
        }
        self.content.set_visible(!presenting);
        if let Some(preview) = self.preview.borrow().as_ref() {
            preview.widget().set_visible(self.shows_preview());
        }
        if self.mode.get() == Mode::Split && !presenting {
            self.even_split();
        }
        if let Some(tab) = self.active() {
            self.render(&tab);
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

    /// F5: the note alone and rendered, with the sidebar, the tab bar and both header bars gone.
    /// A state of the window rather than a [`Mode`], because it is a way of looking at the current
    /// note instead of a layout to work in, and it is deliberately not part of the session: a
    /// window restored chromeless would be hard to get out of.
    ///
    /// ponytail: markdown only. A PDF tab keeps showing its own view here; route it through the
    /// same preview switch once the PDF viewer lands.
    fn set_presenting(self: &Rc<Self>, on: bool) {
        match (on, self.presenting.get()) {
            (true, None) => {
                self.presenting.set(Some(Presenting {
                    mode: self.mode.get(),
                    sidebar: self.sidebar_column.is_visible(),
                }));
                self.sidebar_column.set_visible(false);
                self.toolbar.set_reveal_top_bars(false);
                self.apply_layout();
            }
            (false, Some(before)) => {
                self.presenting.set(None);
                self.sidebar_column.set_visible(before.sidebar);
                self.toolbar.set_reveal_top_bars(true);
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
        let preview = preview::Preview::new(
            self.vault.root().to_path_buf(),
            glib::clone!(
                #[weak(rename_to = app)]
                self,
                move |target: &str| app.open_target(target)
            ),
        );
        self.paned.set_end_child(Some(preview.widget()));
        preview.set_zoom(self.zoom.get());
        preview.connect_found(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |count| app.find.set_matches(count)
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

    fn queue_render(self: &Rc<Self>, tab: &Rc<Tab>) {
        if !self.shows_preview() || !self.is_active(tab) {
            return;
        }
        if let Some(id) = self.render.borrow_mut().take() {
            id.remove();
        }
        let id = glib::timeout_add_local_once(
            RENDER,
            glib::clone!(
                #[weak(rename_to = app)]
                self,
                move || {
                    *app.render.borrow_mut() = None;
                    if let Some(tab) = app.active() {
                        app.render(&tab);
                    }
                }
            ),
        );
        *self.render.borrow_mut() = Some(id);
    }

    /// The find bar addressing the rendered preview, which is what it does while presenting.
    fn preview_find(&self, op: find::PreviewOp) {
        let preview = self.preview.borrow();
        let Some(preview) = preview.as_ref() else {
            return;
        };
        match op {
            find::PreviewOp::Find(text) => preview.find(&text),
            find::PreviewOp::Next => preview.find_next(),
            find::PreviewOp::Previous => preview.find_previous(),
            find::PreviewOp::Clear => preview.find_clear(),
            find::PreviewOp::Line(line) => preview.scroll_to_line(line),
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

    /// The point of the app (DESIGN.md): the chrome fades while the user types.
    fn on_edit(&self) {
        // Only a keystroke into a focused editor hides it; a reload writing into a background
        // buffer is not the user typing.
        if self.focused().is_some_and(|w| w.is::<sourceview5::View>()) {
            self.hide_chrome();
        }
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
        for pane in self.panes.borrow().iter() {
            pane.bar.add_css_class("chrome-hidden");
        }
        self.zoom_pill.add_css_class("chrome-hidden");
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
        for pane in self.panes.borrow().iter() {
            pane.bar.remove_css_class("chrome-hidden");
        }
        self.zoom_pill.remove_css_class("chrome-hidden");
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
            || self.find.is_open()
            || self.active().is_some_and(|tab| tab.banner.is_revealed())
    }

    // --- actions -------------------------------------------------------------------------

    fn run_action(self: &Rc<Self>, name: &str) {
        match name {
            "save" => self.save_active(),
            "new-note" => {
                let dir = self
                    .selected_dir()
                    .unwrap_or_else(|| self.vault.config().new_note_dir);
                fileops::new_note(self.ops(), &dir);
            }
            "new-folder" => {
                fileops::new_folder(self.ops(), &self.selected_dir().unwrap_or_default())
            }
            "close-tab" => {
                if let Some(page) = self.tabs().selected_page() {
                    self.tabs().close_page(&page);
                }
            }
            "split-left" => self.split_active(Side::Left),
            "split-right" => self.split_active(Side::Right),
            "split-up" => self.split_active(Side::Up),
            "split-down" => self.split_active(Side::Down),
            "palette-files" => self.palette(palette::Mode::Files),
            "palette-commands" => self.palette(palette::Mode::Commands),
            "find" => self.find.open(find::Mode::Find),
            "replace" => self.find.open(find::Mode::Replace),
            "goto-line" => self.find.open(find::Mode::Goto),
            "find-next" => self.find.step(true),
            "find-previous" => self.find.step(false),
            "duplicate-line" => {
                if let Some(tab) = self.active() {
                    tab.duplicate_line();
                }
            }
            "delete-line" => {
                if let Some(tab) = self.active() {
                    tab.delete_line();
                }
            }
            "scroll-up" => {
                if let Some(tab) = self.active() {
                    tab.scroll_lines(-1);
                }
            }
            "scroll-down" => {
                if let Some(tab) = self.active() {
                    tab.scroll_lines(1);
                }
            }
            "caret-above" => {
                if let Some(tab) = self.active() {
                    tab.add_caret(false);
                }
            }
            "caret-below" => {
                if let Some(tab) = self.active() {
                    tab.add_caret(true);
                }
            }
            "zoom-in" => self.set_zoom(self.zoom.get() + ZOOM_STEP),
            "zoom-out" => self.set_zoom(self.zoom.get() - ZOOM_STEP),
            "zoom-reset" => self.set_zoom(1.0),
            "minimap" => self.toggle_minimap(),
            "copy-relative-path" => {
                if let Some(rel) = self.menu_rel() {
                    fileops::copy_relative_path(self.ops(), &rel);
                }
            }
            "copy-absolute-path" => {
                if let Some(rel) = self.menu_rel() {
                    fileops::copy_absolute_path(self.ops(), &rel);
                }
            }
            "show-in-files" => {
                if let Some(rel) = self.menu_rel() {
                    fileops::show_in_files(self.ops(), &rel);
                }
            }
            "reveal-in-sidebar" => self.reveal_in_sidebar(),
            "sidebar" => self
                .sidebar_column
                .set_visible(!self.sidebar_column.is_visible()),
            "pane-files" => self.show_pane("files"),
            "pane-search" => self.show_pane("search"),
            "replace-in-files" => {
                self.sidebar_column.set_visible(true);
                if let Some(sidebar) = self.sidebar.get() {
                    sidebar.show_replace();
                }
            }
            "pane-tags" => self.show_pane("tags"),
            "backlinks" => self.show_pane("backlinks"),
            "view-mode" => self.set_mode(self.mode.get().next()),
            "follow-link" => {
                if let Some(link) = self.active().and_then(|tab| tab.link_at_cursor()) {
                    self.follow(&link);
                }
            }
            "rename" => {
                let target = self
                    .selected_row()
                    .map(|(_, rel)| rel)
                    .or_else(|| self.active().map(|tab| tab.rel()));
                if let Some(rel) = target {
                    fileops::rename(self.ops(), &rel);
                }
            }
            "daily-note" => match self.vault.daily_note() {
                Ok((rel, _)) => self.open_note(&rel),
                Err(e) => self.toast(&format!("Cannot open today's note: {e:#}")),
            },
            "present" => self.set_presenting(self.presenting.get().is_none()),
            "fullscreen" => self.window.set_fullscreened(!self.window.is_fullscreen()),
            "preferences" => self.preferences(),
            "menu" => self.menu.popup(),
            "about" => self.about(),
            _ => tracing::warn!("no handler for action {name}"),
        }
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
        if let Some(preview) = self.preview.borrow().as_ref() {
            preview.set_zoom(zoom);
        }
        // 100 % is the state that needs no readout, so Reset makes the pill disappear rather than
        // leaving a badge saying nothing is going on.
        self.zoom_label
            .set_label(&format!("{} %", (zoom * 100.0).round() as i32));
        self.zoom_pill.set_visible(zoom != 1.0);
        self.save_session_soon();
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

    fn follow(self: &Rc<Self>, link: &Link) {
        if link.kind == LinkKind::External {
            gtk::UriLauncher::new(&link.target).launch(
                Some(&self.window),
                gio::Cancellable::NONE,
                |result| {
                    if let Err(e) = result {
                        tracing::warn!("cannot open link in browser: {e}");
                    }
                },
            );
            return;
        }
        self.open_target(&link.target);
    }

    fn show_pane(&self, name: &str) {
        self.sidebar_column.set_visible(true);
        if let Some(sidebar) = self.sidebar.get() {
            sidebar.show_pane(name);
        }
    }

    /// The vault path the tab context menu acts on: the page that was right-clicked, or the
    /// active tab when the same action is fired from the palette.
    fn menu_rel(&self) -> Option<String> {
        let Some(page) = self.menu_page.borrow().clone() else {
            return self.active().map(|tab| tab.rel());
        };
        let note = self
            .open
            .borrow()
            .iter()
            .find(|t| t.page == page)
            .map(|t| t.rel());
        note.or_else(|| {
            self.images
                .borrow()
                .iter()
                .find(|(_, p)| *p == page)
                .map(|(rel, _)| rel.clone())
        })
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

    fn selected_row(&self) -> Option<(char, String)> {
        self.tree.get()?.selected()
    }

    /// The directory the tree selection points at: the folder itself, or the one a file sits in.
    fn selected_dir(&self) -> Option<String> {
        let (kind, rel) = self.selected_row()?;
        match kind {
            'd' => Some(rel),
            _ => Some(
                rel.rsplit_once('/')
                    .map(|(dir, _)| dir)
                    .unwrap_or("")
                    .to_string(),
            ),
        }
    }

    fn palette(self: &Rc<Self>, initial: palette::Mode) {
        // Two answers to "recent": what this window opened, and what changed on disk. The first
        // is what the user means, so it leads and the index's mtime list fills the page below it.
        let mut recent = self.recent_notes.borrow().clone();
        for rel in self.vault.recent_notes(RECENT_NOTES).unwrap_or_default() {
            if !recent.contains(&rel) {
                recent.push(rel);
            }
        }
        let used = self.recent_commands.borrow();
        let config = self.config.borrow();
        let sources = palette::Sources {
            recent,
            load_notes: Box::new({
                let vault = self.vault.clone();
                move || vault.note_paths().unwrap_or_default()
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
                let vault = self.vault.clone();
                move || {
                    vault
                        .tags()
                        .unwrap_or_default()
                        .into_iter()
                        .map(|(tag, _)| tag)
                        .collect()
                }
            }),
            // Weak, like the pick callback below: this closure outlives the call and a strong
            // handle here would keep the window alive through the dialog.
            on_rebind: Box::new({
                let app = Rc::downgrade(self);
                move |action: &str, accels: Option<Vec<String>>| match app.upgrade() {
                    Some(app) => app.rebind(action, accels),
                    None => Vec::new(),
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
                    palette::Item::Note(rel) => app.open_note(rel),
                    palette::Item::Command { action, .. } => {
                        let _ = WidgetExt::activate_action(&app.window, action, None);
                    }
                    palette::Item::Tag(tag) => {
                        app.sidebar_column.set_visible(true);
                        if let Some(sidebar) = app.sidebar.get() {
                            sidebar.show_tag(tag);
                        }
                    }
                }
            ),
        );
    }

    /// Push the accelerators in force into the application and rebuild the four captured chords.
    /// Done wholesale: forty `set_accels_for_action` calls are cheaper than working out which of
    /// them a config change touched.
    fn apply_accels(&self) {
        let Some(gtk_app) = self.window.application() else {
            return;
        };
        let config = self.config.borrow();
        for (action, _, _) in ACTIONS {
            let accels = accels_for(&config, action);
            let accels: Vec<&str> = accels.iter().map(String::as_str).collect();
            gtk_app.set_accels_for_action(action, &accels);
        }
        fill_captured(&self.captured, &config);
    }

    /// Store an accelerator override for `action` and put it into effect at once. `None` drops the
    /// override, so the action goes back to what [`ACTIONS`] says. Returns what is in force after.
    fn rebind(&self, action: &str, accels: Option<Vec<String>>) -> Vec<String> {
        {
            let mut config = self.config.borrow_mut();
            match accels {
                Some(accels) => config.shortcuts.insert(action.to_string(), accels),
                None => config.shortcuts.remove(action),
            };
            if let Err(e) = config.save() {
                tracing::warn!("saving config: {e:#}");
            }
        }
        self.apply_accels();
        accels_for(&self.config.borrow(), action)
    }

    /// Put a config into effect: everything an edit in the preferences dialog, a Restore Defaults
    /// or a re-read from disk can have changed.
    fn apply_config(self: &Rc<Self>, config: &Config) {
        self.vault.set_config(config.vault(self.vault.root()));
        // Switching to or away from Solarized does not change the system's dark state, so the
        // notify handler that usually restyles never fires here.
        theme::apply(config.theme);
        self.apply_accels();
        for tab in self.open_tabs() {
            tab.set_font(config.editor_font.as_deref(), self.zoom.get());
            tab.set_spellcheck(config.spellcheck);
            tab.set_minimap(config.minimap);
            tab.set_line_numbers(config.line_numbers);
            tab.restyle();
        }
        if let Some(preview) = self.preview.borrow().as_ref() {
            preview.restyle();
        }
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
            self.vault.root().to_path_buf(),
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
            open: self.open.borrow().iter().map(|tab| tab.rel()).collect(),
            active: self.active().map(|tab| tab.rel()),
            // Presentation is not a session state, so the sidebar it hid is saved as it was.
            sidebar: match self.presenting.get() {
                Some(before) => before.sidebar,
                None => self.sidebar_column.is_visible(),
            },
            sidebar_width: sidebar_width(self.split.position()),
            view: self.mode.get().name().to_string(),
            pane: self
                .sidebar
                .get()
                .map(|s| s.pane())
                .unwrap_or_else(|| Session::default().pane),
            zoom: self.zoom.get(),
            recent_notes: self.recent_notes.borrow().clone(),
            recent_commands: self.recent_commands.borrow().clone(),
        };
        if let Err(e) = self.vault.save_session(&session) {
            tracing::warn!("saving the session: {e:#}");
        }
    }

    /// Restored after the window is on screen, so nothing here is on the path to the first frame.
    fn restore_session(self: &Rc<Self>) {
        let session = self.vault.session();
        // Before the tabs, so each one is built at the right size instead of being restyled
        // afterwards. A state file written before zoom existed defaults to 1.0.
        self.set_zoom(session.zoom);
        // ponytail: every note comes back into one pane, because the session does not record the
        // pane layout. Add a tree of splits to `Session` the day restoring into one column stops
        // being what someone who left four panes open expects.
        for rel in &session.open {
            self.open_note(rel);
        }
        if let Some(tab) = session.active.as_deref().and_then(|rel| self.tab_for(rel)) {
            self.tabs().set_selected_page(&tab.page);
        }
        // A state file written before panes were saved leaves the name empty; that keeps
        // whichever pane the sidebar was built showing.
        if let Some(sidebar) = self.sidebar.get().filter(|_| !session.pane.is_empty()) {
            sidebar.show_pane(&session.pane);
        }
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

/// Zoom in tenths, between half size and triple. Rounded as well as clamped, so stepping does
/// not drift into 0.7999999999999999 and a hand-edited state file cannot ask for 0.
fn clamp_zoom(zoom: f64) -> f64 {
    ((zoom * 10.0).round() / 10.0).clamp(0.5, 3.0)
}

/// A sidebar width in pixels, falling back to the default for anything a sidebar would never
/// be: a hidden column's zero position, or the fraction an older session file may still hold.
fn sidebar_width(stored: i32) -> i32 {
    match stored >= 50 {
        true => stored,
        false => Session::default().sidebar_width,
    }
}

// ------------------------------------------------------------------------------ construction

fn build_window(
    gtk_app: &adw::Application,
    shell: &Rc<Shell>,
    root: PathBuf,
    note: Option<String>,
) -> Option<Rc<App>> {
    install_document_font();
    install_chrome_css();
    theme::apply(shell.config.borrow().theme);

    let vault_config = shell.config.borrow().vault(&root);
    let (vault, events) = match Vault::open(&root, vault_config) {
        Ok(opened) => opened,
        Err(e) => {
            eprintln!("cannot open {}: {e:#}", root.display());
            return None;
        }
    };
    let vault = Arc::new(vault);
    {
        let mut config = shell.config.borrow_mut();
        config.touch_recent(&root);
        if let Err(e) = config.save() {
            tracing::warn!("saving config: {e:#}");
        }
    }

    let vault_name = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| root.display().to_string());
    let title = adw::WindowTitle::new(&vault_name, "");
    let first = Pane::new(&tab_menu());
    let toasts = adw::ToastOverlay::new();
    let status = gtk::Label::builder().label("Indexing…").build();
    status.add_css_class("dim-label");

    // An empty vault window should say so rather than showing a blank rectangle.
    let placeholder = adw::StatusPage::builder()
        .icon_name("text-x-generic-symbolic")
        .title("No Note Open")
        .description("Pick one in the sidebar, or press Ctrl+P to search.")
        .build();
    // The panes hang off a bin, so a split can swap the whole arrangement for a `GtkPaned` the
    // same way it swaps one branch of it (`panes::split`).
    let panes_root = adw::Bin::builder().child(first.widget()).build();
    let content = gtk::Stack::new();
    content.add_named(&panes_root, Some("tabs"));
    content.add_named(&placeholder, Some("empty"));
    content.set_visible_child_name("empty");

    // Editor on the left, preview on the right; the mode decides which of the two is visible.
    let paned = gtk::Paned::builder()
        .orientation(gtk::Orientation::Horizontal)
        .start_child(&content)
        .resize_start_child(true)
        .resize_end_child(true)
        .shrink_start_child(false)
        .shrink_end_child(false)
        .build();

    // Zoom had no visual feedback at all: the note simply grew. The readout floats over the
    // document in an overlay rather than sitting in a bar, so it costs the column no width and
    // appearing never moves a line of text. It carries `chrome-fade` like the header and tab
    // bars, so typing fades it out with the rest of the chrome instead of leaving a fourth thing
    // on screen.
    let zoom_label = gtk::Label::new(Some("100 %"));
    zoom_label.add_css_class("numeric");
    let zoom_reset = gtk::Button::builder()
        .label("Reset")
        .action_name("win.zoom-reset")
        .build();
    zoom_reset.add_css_class("flat");
    let zoom_pill = gtk::Box::builder()
        .spacing(6)
        .halign(gtk::Align::End)
        .valign(gtk::Align::Start)
        .margin_top(12)
        .margin_end(12)
        .visible(false)
        .build();
    zoom_pill.append(&zoom_label);
    zoom_pill.append(&zoom_reset);
    zoom_pill.add_css_class("osd");
    zoom_pill.add_css_class("accent-pill");
    zoom_pill.add_css_class("chrome-fade");

    let document = gtk::Overlay::builder().child(&paned).build();
    document.add_overlay(&zoom_pill);
    toasts.set_child(Some(&document));

    // Split headers, as GNOME Files and VS Code have them: the sidebar is a full-height column
    // with a header of its own, and the tab bar belongs to the editor column. The two header
    // bars share the window controls so they still read as one titlebar.
    let sidebar_header = adw::HeaderBar::new();
    sidebar_header.set_show_start_title_buttons(true);
    sidebar_header.set_show_end_title_buttons(false);
    // `build_sidebar` makes the pane switcher this header's title widget. Until then a label keeps
    // the header from falling back to the window title, which here is the application name next to
    // the vault name on the right.
    sidebar_header.set_title_widget(Some(&gtk::Label::new(None)));
    // libadwaita gives a header that shares its toolbar area with another bar 3 px of padding and
    // the bar area another 3, but a lone header keeps the default 6 above and 7 below. This one is
    // alone in its column while the main header sits above the tab bar, so without the correction
    // in `install_chrome_css` the two headers hold their contents in bands of different heights.
    sidebar_header.add_css_class("accent-lone-header");

    let sidebar_column = adw::ToolbarView::builder().width_request(200).build();
    sidebar_column.add_top_bar(&sidebar_header);

    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&title));
    // The toggle belongs to the header that never goes away, as in Files and Text Editor: in the
    // sidebar's own header, hiding the sidebar takes the way back with it. An icon toggle in a
    // header bar is already flat, so it reads as one family with the view-mode group at the other
    // end of the bar without a style class of its own.
    let toggle = gtk::ToggleButton::builder()
        .icon_name("sidebar-show-symbolic")
        .tooltip_text("Toggle Sidebar")
        .valign(gtk::Align::Center)
        .build();
    header.pack_start(&toggle);
    toggle
        .bind_property("active", &sidebar_column, "visible")
        .bidirectional()
        .sync_create()
        .build();
    toggle.set_active(true);
    // The start window controls sit in the sidebar header, so the main header takes them over
    // while the sidebar is hidden. Nothing moves on the default GNOME layout, where that side is
    // empty; a user who keeps buttons on the left does not lose them.
    sidebar_column
        .bind_property("visible", &header, "show-start-title-buttons")
        .invert_boolean()
        .sync_create()
        .build();
    let modes = mode_switcher();
    let menu = menu_button();
    header.pack_end(&menu);
    header.pack_end(&status);
    header.pack_end(&modes);

    // The two headers must end at the same height or the switcher row and the tab bar under them
    // cannot line up. They do at the default font (both 40 px), but the sidebar header is empty
    // and stays at Adwaita's minimum while this one grows with the window title: measured at
    // 20 pt, the switcher row starts 14 px above the tab bar without this. The widgets keep the
    // group alive.
    let headers = gtk::SizeGroup::new(gtk::SizeGroupMode::Vertical);
    headers.add_widget(&sidebar_header);
    headers.add_widget(&header);

    // The panes' tab bars carry the fade class too, so the chrome still hides as one
    // (DESIGN.md); `Pane::new` adds it to each of them.
    sidebar_header.add_css_class("chrome-fade");
    header.add_css_class("chrome-fade");

    // The find bar goes in the toolbar's content rather than among its top bars: presentation
    // mode unreveals those *and* hides the tab stack, and Ctrl+F has to outlive both.
    let find = find::Bar::new();
    let editor_column = gtk::Box::new(gtk::Orientation::Vertical, 0);
    editor_column.append(find.widget());
    editor_column.append(&toasts);

    // Only the header is a top bar now: the tab bars belong to the panes, so they sit inside
    // `content` and presentation mode takes them away with it rather than unrevealing them.
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&editor_column));

    // One flat background across sidebar, chrome and document (DESIGN.md, Colour): without it
    // the two columns sit on `--window-bg-color` and band against the note. The header bars and
    // the tab bar need nothing of their own; `AdwToolbarView` draws its top bars flat by default,
    // so they take the colour set here.
    sidebar_column.add_css_class("accent-flat");
    toolbar.add_css_class("accent-flat");

    // ponytail: a plain `GtkPaned` so the sidebar can be dragged, which `AdwOverlaySplitView`
    // cannot do. The cost is its adaptive collapse on a narrow window; go back to it if that
    // ever matters more than resizing.
    let split = gtk::Paned::builder()
        .orientation(gtk::Orientation::Horizontal)
        .start_child(&sidebar_column)
        .end_child(&toolbar)
        .resize_start_child(false)
        .shrink_start_child(false)
        .resize_end_child(true)
        .position(Session::default().sidebar_width)
        .build();

    let window = adw::ApplicationWindow::builder()
        .application(gtk_app)
        .default_width(1100)
        .default_height(760)
        .content(&split)
        .build();

    let app = Rc::new(App {
        vault: vault.clone(),
        config: shell.config.clone(),
        window: window.clone(),
        panes: RefCell::new(vec![first.clone()]),
        active_pane: RefCell::new(first.clone()),
        title,
        toasts,
        find,
        status,
        open: RefCell::new(Vec::new()),
        images: RefCell::new(Vec::new()),
        tree: OnceCell::new(),
        sidebar: OnceCell::new(),
        ops: OnceCell::new(),
        preview: RefCell::new(None),
        split,
        sidebar_column,
        sidebar_header,
        toolbar,
        header,
        modes: modes.clone(),
        menu,
        paned,
        content: content.clone(),
        mode: Cell::new(Mode::Editor),
        zoom: Cell::new(1.0),
        zoom_pill,
        zoom_label,
        presenting: Cell::new(None),
        chrome_hidden: Cell::new(false),
        reconciled: Cell::new(false),
        menu_page: RefCell::new(None),
        tree_painted: Cell::new(0),
        render: RefCell::new(None),
        session: RefCell::new(None),
        recent_notes: RefCell::new(Vec::new()),
        recent_commands: RefCell::new(Vec::new()),
        captured: gtk::ShortcutController::new(),
    });
    let _ = app.ops.set(build_ops(&app));

    // Populate straight from the index: the window must be up before reconcile finishes.
    let rows = gio::ListStore::new::<gtk::StringObject>();
    tree::fill(&rows, &vault, "");
    tracing::debug!(t_ms = ms(), rows = rows.n_items(), "tree populated");
    build_sidebar(&app, &rows);

    wire_pane(&app, &first);

    install_actions(gtk_app, &app);
    wire_window(&app, &modes);
    wire_tree(&app);

    window.connect_map(|_| tracing::debug!(t_ms = ms(), "window mapped"));
    window.present();
    tracing::debug!(t_ms = ms(), "window presented");

    glib::idle_add_local_once(glib::clone!(
        #[weak]
        app,
        move || {
            app.restore_session();
            if let Some(rel) = note {
                app.open_note(&rel);
            }
        }
    ));
    install_bench_hooks(&app);
    start_events(&app, events);
    Some(app)
}

/// Files / Search / Tags / Backlinks over the vault tree.
fn build_sidebar(app: &Rc<App>, rows: &gio::ListStore) {
    let tree = tree::build(
        app.vault.clone(),
        rows,
        glib::clone!(
            #[weak]
            app,
            move |kind, rel: &str| match kind {
                'm' => app.open_note(rel),
                _ if markdown::is_image(rel) => app.open_image(rel),
                _ => app.toast("Only markdown notes and images open in this phase"),
            }
        ),
    );
    // The tree owns its scroller now, wrapped in a box the context menu can parent itself to.
    let files = tree.widget().clone();
    let _ = app.tree.set(tree);

    let data =
        sidebar::Data {
            // The one closure the sidebar calls off the main loop, which is why the vault is an `Arc`.
            search: Arc::new({
                let vault = app.vault.clone();
                move |query| match query {
                    sidebar::Query::Fts(text) => {
                        sidebar::Answer::Fts(vault.search(&text, SEARCH_LIMIT).unwrap_or_default())
                    }
                    sidebar::Query::Grep(re) => {
                        let (hits, total) = vault.grep(&re, SEARCH_LIMIT).unwrap_or_default();
                        sidebar::Answer::Grep(hits, total)
                    }
                }
            }),
            replace_all: Box::new(glib::clone!(
                #[weak]
                app,
                move |re: &accent_api::Regex, replacement: &str, literal: bool| app
                    .replace_in_notes(re, replacement, literal)
            )),
            tags: Box::new({
                let vault = app.vault.clone();
                move || vault.tags().unwrap_or_default()
            }),
            files_with_tag: Box::new({
                let vault = app.vault.clone();
                move |tag| {
                    vault
                        .files_with_tag(tag)
                        .unwrap_or_default()
                        .into_iter()
                        .map(|f| f.rel_path)
                        .collect()
                }
            }),
        };
    let pane = sidebar::Sidebar::new(
        files,
        data,
        glib::clone!(
            #[weak]
            app,
            move |rel: &str, offset: Option<usize>| app.open_note_at(rel, offset)
        ),
    );
    // The switcher is the sidebar header's title widget rather than a top bar of its own, so the
    // sidebar spends no band on an empty header: the pane icons sit level with the collapse toggle
    // in the main header, and the tree starts level with the tab bar. `AdwHeaderBar` centres a
    // title widget, and the header-to-header size group already keeps the two bands equal, so the
    // switcher needs neither a box around it nor a size group of its own.
    app.sidebar_header.set_title_widget(Some(pane.switcher()));
    // The panes dim rather than hide while the user types, on the same transition as the bars.
    pane.widget().add_css_class("chrome-fade");
    app.sidebar_column.set_content(Some(pane.widget()));
    let _ = app.sidebar.set(pane);
}

/// Everything `fileops` needs from the window, as closures. Weak throughout: the operations
/// outlive nothing, and a strong capture here would keep a closed window's vault open.
fn build_ops(app: &Rc<App>) -> Rc<fileops::Ops> {
    let toast = Rc::downgrade(app);
    let open = Rc::downgrade(app);
    let split = Rc::downgrade(app);
    let flush = Rc::downgrade(app);
    let reload = Rc::downgrade(app);
    let close = Rc::downgrade(app);
    let reconciled = Rc::downgrade(app);
    Rc::new(fileops::Ops {
        vault: app.vault.clone(),
        window: app.window.clone(),
        toast: Box::new(move |message| {
            if let Some(app) = toast.upgrade() {
                app.toast(message);
            }
        }),
        open: Box::new(move |rel| {
            if let Some(app) = open.upgrade() {
                app.open_note(rel);
            }
        }),
        split: Box::new(move |rel, side| {
            if let Some(app) = split.upgrade() {
                let at = app.pane();
                app.open_beside(&at, side, rel);
            }
        }),
        reconciled: Box::new(move || reconciled.upgrade().is_some_and(|app| app.reconciled.get())),
        flush: Box::new(move |rels| {
            let Some(app) = flush.upgrade() else { return };
            for rel in rels {
                if let Some(tab) = app.tab_for(rel).filter(|tab| tab.modified.get()) {
                    app.save_tab(&tab, false);
                }
            }
        }),
        reload: Box::new(move |rels| {
            let Some(app) = reload.upgrade() else {
                return 0;
            };
            rels.iter()
                .filter_map(|rel| app.tab_for(rel))
                .filter(|tab| !app.refresh_tab(tab))
                .count()
        }),
        close: Box::new(move |rel| {
            let Some(app) = close.upgrade() else { return };
            if let Some(tab) = app.tab_for(rel) {
                // The file is in the trash: there is nothing left to save the buffer into, so the
                // tab goes without the close asking to write it back out again.
                tab.discard();
                app.close_page(&tab.page);
            }
        }),
    })
}

/// Everything one pane's tab view has to answer for. Called for the pane the window is built with
/// and for every pane a split adds, so a new pane behaves exactly like the first one.
fn wire_pane(app: &Rc<App>, pane: &Rc<Pane>) {
    // A tab may only go once its buffer is on disk. When the save fails the close stops here and
    // `AdwTabView` waits for `close_page_finish`, which the dialog calls with the user's answer.
    pane.tabs.connect_close_page(glib::clone!(
        #[weak]
        app,
        #[upgrade_or]
        glib::Propagation::Proceed,
        move |tabs, page| {
            let dirty = app
                .open_tabs()
                .into_iter()
                .find(|t| &t.page == page)
                .filter(|t| t.modified.get());
            if let Some(tab) = dirty
                && let Err(e) = app.write_tab(&tab, &tab.text(), tab.etag.get())
            {
                let (tabs, page) = (tabs.clone(), page.clone());
                app.ask_unsaved(&tab, &e, move |app, close| {
                    if close {
                        app.forget_page(&page);
                    }
                    tabs.close_page_finish(&page, close);
                });
                return glib::Propagation::Stop;
            }
            app.forget_page(page);
            glib::Propagation::Proceed
        }
    ));
    // `setup-menu` fires with the page just before the popup and with `None` from an idle after
    // it hides. A model button activates its action before the popdown, so the page is still here
    // when the action runs, and afterwards `menu_rel` falls back to the active tab.
    pane.tabs.connect_setup_menu(glib::clone!(
        #[weak]
        app,
        move |_, page| *app.menu_page.borrow_mut() = page.cloned()
    ));
    pane.tabs.connect_selected_page_notify(glib::clone!(
        #[weak]
        app,
        #[weak]
        pane,
        move |_| {
            app.set_active_pane(&pane);
            app.sync_active();
            app.save_session_soon();
        }
    ));
    // The placeholder is a property of the window, not of one pane: it shows only when no pane
    // has anything left to show, which with panes that close themselves means the last one.
    pane.tabs.connect_n_pages_notify(glib::clone!(
        #[weak]
        app,
        move |_| app.sync_panes()
    ));
    // A pane that has just lost its last page has nothing left to be. Closing it from an idle
    // rather than here, because this also fires in the middle of `transfer_page`, which is still
    // holding the page when the source view reports it gone.
    pane.tabs.connect_page_detached(glib::clone!(
        #[weak]
        app,
        #[weak]
        pane,
        move |tabs, _, _| {
            if tabs.n_pages() > 0 {
                return;
            }
            let app = Rc::downgrade(&app);
            glib::idle_add_local_once(move || {
                let Some(app) = app.upgrade() else { return };
                if pane.tabs.n_pages() == 0 {
                    app.close_pane(&pane);
                }
            });
        }
    ));
    // libadwaita sets this on *every* tab view when a tab drag starts anywhere, which is the only
    // notice we get that a drag is in flight and the drop sheets should go up.
    pane.tabs.connect_is_transferring_page_notify(glib::clone!(
        #[weak]
        app,
        move |tabs| app.set_drop_active(tabs.is_transferring_page())
    ));
    // Clicking into a pane's editor makes it the one a note opens into, the same as picking one
    // of its tabs would.
    let focus = gtk::EventControllerFocus::new();
    focus.connect_enter(glib::clone!(
        #[weak]
        app,
        #[weak]
        pane,
        move |_| {
            if app.set_active_pane(&pane) {
                app.sync_active();
            }
        }
    ));
    pane.widget().add_controller(focus);
    wire_pane_drops(app, pane);
}

/// The drop zones on one pane: the pointer picks an edge or the middle, and letting go there
/// either splits the pane or drops into it.
fn wire_pane_drops(app: &Rc<App>, pane: &Rc<Pane>) {
    pane.drop.connect_motion(glib::clone!(
        #[weak]
        pane,
        #[upgrade_or]
        gdk::DragAction::empty(),
        move |target, x, y| {
            let (w, h) = pane.size();
            pane.show_zone(Some(panes::zone(x, y, w, h)));
            // A tab moves, a path from the tree is only read; offer whichever the drag allows.
            let offered = target
                .current_drop()
                .map(|drop| drop.actions())
                .unwrap_or_else(gdk::DragAction::empty);
            match offered.contains(gdk::DragAction::MOVE) {
                true => gdk::DragAction::MOVE,
                false => gdk::DragAction::COPY,
            }
        }
    ));
    pane.drop.connect_leave(glib::clone!(
        #[weak]
        pane,
        move |_| pane.show_zone(None)
    ));
    pane.drop.connect_drop(glib::clone!(
        #[weak]
        app,
        #[weak]
        pane,
        #[upgrade_or]
        false,
        move |_, value, x, y| {
            let (w, h) = pane.size();
            let zone = panes::zone(x, y, w, h);
            pane.show_zone(None);
            app.dropped(&pane, zone, value)
        }
    ));
}

fn wire_window(app: &Rc<App>, modes: &gtk::ToggleButton) {
    // While presenting there is no editor on screen, so find and go to line address the rendered
    // preview instead. Two closures rather than a back-reference, so `find.rs` never sees `App`.
    app.find.wire(find::Wiring {
        presenting: Box::new(glib::clone!(
            #[weak]
            app,
            #[upgrade_or]
            false,
            move || app.presenting.get().is_some()
        )),
        preview: Box::new(glib::clone!(
            #[weak]
            app,
            move |op| app.preview_find(op)
        )),
    });

    modes.connect_toggled(glib::clone!(
        #[weak]
        app,
        move |button| {
            let picked = if button.is_active() {
                Mode::Split
            } else {
                Mode::Editor
            };
            if picked != app.mode.get() {
                app.set_mode(picked);
            }
        }
    ));
    app.sidebar_column.connect_visible_notify(glib::clone!(
        #[weak]
        app,
        move |_| app.save_session_soon()
    ));

    // Every divider in the window: double-click resets it, and it thickens while dragged.
    paned::watch(
        app.window.upcast_ref(),
        glib::clone!(
            #[weak]
            app,
            move |divider: &gtk::Paned| {
                if divider == &app.split {
                    divider.set_position(Session::default().sidebar_width);
                } else if divider == &app.paned {
                    app.centre_handle();
                } else if !app
                    .sidebar
                    .get()
                    .is_some_and(|sidebar| sidebar.reset_divider(divider))
                {
                    // A divider nobody claims (the pane splitters to come) has no remembered
                    // default, so half of its own extent is the reset.
                    let extent = match divider.orientation() {
                        gtk::Orientation::Vertical => divider.height(),
                        _ => divider.width(),
                    };
                    divider.set_position(extent / 2);
                }
            }
        ),
    );

    // Same rule on the way out of the window: the first buffer that cannot be written stops the
    // close and asks. Answering Discard or Overwrite closes the window again, which picks up
    // where this left off.
    app.window.connect_close_request(glib::clone!(
        #[weak]
        app,
        #[upgrade_or]
        glib::Propagation::Proceed,
        move |_| {
            for tab in app.open_tabs().iter().filter(|t| t.modified.get()) {
                let Err(e) = app.write_tab(tab, &tab.text(), tab.etag.get()) else {
                    continue;
                };
                app.ask_unsaved(tab, &e, |app, close| {
                    if close {
                        app.window.close();
                    }
                });
                return glib::Propagation::Stop;
            }
            app.save_session();
            glib::Propagation::Proceed
        }
    ));

    // Chrome comes back on pointer motion, on Escape and whenever focus moves; hover alone must
    // never be the way back, or a keyboard-only user is stuck (DESIGN.md).
    //
    // GTK also emits `motion` when the widget under a *stationary* pointer changes, which typing
    // does every time the text reflows past it, Return most of all. So compare against the last
    // position and ignore an event that did not actually move the pointer, or the chrome pops
    // back on the first newline.
    let motion = gtk::EventControllerMotion::new();
    let last: Cell<Option<(f64, f64)>> = Cell::new(None);
    motion.connect_motion(glib::clone!(
        #[weak]
        app,
        move |_, x, y| {
            if last.replace(Some((x, y))) != Some((x, y)) {
                app.show_chrome();
            }
        }
    ));
    app.window.add_controller(motion);

    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    keys.connect_key_pressed(glib::clone!(
        #[weak]
        app,
        #[upgrade_or]
        glib::Propagation::Proceed,
        move |_, key, _, _| {
            if key == gdk::Key::Escape {
                // The way out of presentation, where there is no chrome to bring back.
                match app.presenting.get().is_some() {
                    true => app.set_presenting(false),
                    false => app.show_chrome(),
                }
            }
            glib::Propagation::Proceed
        }
    ));
    app.window.add_controller(keys);
    app.window.connect_notify_local(
        Some("focus-widget"),
        glib::clone!(
            #[weak]
            app,
            move |_, _| app.show_chrome()
        ),
    );

    // Dark mode, the accent colour and the document font are pure GNOME settings; we only
    // re-colour what we drew ourselves.
    let style = adw::StyleManager::default();
    for property in ["accent-color", "dark"] {
        style.connect_notify_local(
            Some(property),
            glib::clone!(
                #[weak]
                app,
                move |_, _| {
                    // Solarized has one palette per system state, so which half is installed is
                    // decided here, before anything reads the resulting colours back out.
                    theme::refresh();
                    for tab in app.open_tabs() {
                        tab.restyle();
                    }
                    if let Some(preview) = app.preview.borrow().as_ref() {
                        preview.restyle();
                    }
                }
            ),
        );
    }
    style.connect_document_font_name_notify(glib::clone!(
        #[weak]
        app,
        move |_| {
            install_document_font();
            // A different document font is a different marker width, so the hanging headings
            // have to be measured again.
            for tab in app.open_tabs() {
                tab.rehang();
            }
            if let Some(preview) = app.preview.borrow().as_ref() {
                preview.restyle();
            }
        }
    ));
}

/// Right-click and Menu open the file-operations menu; Delete trashes. A key controller on the
/// tree, not a global accelerator, so Delete cannot fire while the user is typing.
fn wire_tree(app: &Rc<App>) {
    let Some(list) = app.tree.get().map(|tree| tree.view().clone()) else {
        return;
    };

    let click = gtk::GestureClick::builder()
        .button(gdk::BUTTON_SECONDARY)
        .build();
    click.connect_pressed(glib::clone!(
        #[weak]
        app,
        move |gesture, _, x, y| {
            let Some(tree) = app.tree.get() else { return };
            let Some((kind, rel)) = tree.row_at(x, y) else {
                return;
            };
            gesture.set_state(gtk::EventSequenceState::Claimed);
            // The menu hangs off the host box, so the click has to be translated out of the
            // list's coordinates or it would point at the wrong row once the list is scrolled.
            let Some(at) = tree.view().compute_point(
                tree.widget(),
                &gtk::graphene::Point::new(x as f32, y as f32),
            ) else {
                return;
            };
            let anchor = gdk::Rectangle::new(at.x() as i32, at.y() as i32, 1, 1);
            fileops::context_menu(app.ops(), tree.widget(), &rel, kind == 'd', anchor);
        }
    ));
    list.add_controller(click);

    let keys = gtk::EventControllerKey::new();
    keys.connect_key_pressed(glib::clone!(
        #[weak]
        app,
        #[upgrade_or]
        glib::Propagation::Proceed,
        move |_, key, _, _| {
            let Some(tree) = app.tree.get() else {
                return glib::Propagation::Proceed;
            };
            let Some((kind, rel)) = tree.selected() else {
                return glib::Propagation::Proceed;
            };
            match key {
                gdk::Key::Delete => fileops::trash(app.ops(), &rel),
                gdk::Key::Menu => fileops::context_menu(
                    app.ops(),
                    tree.widget(),
                    &rel,
                    kind == 'd',
                    row_anchor(tree.view(), tree.widget()),
                ),
                _ => return glib::Propagation::Proceed,
            }
            glib::Propagation::Stop
        }
    ));
    list.add_controller(keys);
}

/// Where a Menu-key popover points: the focused row, or the top of the list. In `host`'s
/// coordinates, since that is what the popover is parented to.
fn row_anchor(list: &gtk::ListView, host: &gtk::Widget) -> gdk::Rectangle {
    let bounds = list.focus_child().and_then(|row| row.compute_bounds(host));
    match bounds {
        Some(r) => gdk::Rectangle::new(
            r.x() as i32,
            r.y() as i32,
            r.width() as i32,
            r.height() as i32,
        ),
        None => gdk::Rectangle::new(0, 0, 1, 1),
    }
}

/// What `action` is bound to right now: the user's override from the config if there is one, the
/// built-in table otherwise. An override that is an empty list leaves the action unbound, which is
/// a binding too — it still lists in the palette, just without a chord.
fn accels_for(config: &Config, action: &str) -> Vec<String> {
    if let Some(accels) = config.shortcuts.get(action) {
        return accels.clone();
    }
    ACTIONS
        .iter()
        .find(|(name, _, _)| *name == action)
        .map(|(_, _, accels)| accels.iter().map(|a| a.to_string()).collect())
        .unwrap_or_default()
}

// GtkTextView binds Ctrl+Up/Down to paragraph movement and GtkSourceView binds Shift+Alt+Up/Down
// to move-viewport. Both are class shortcuts, which run in the bubble phase at the focused view
// and so get the key before the window's application accelerators ever see it. Claiming these four
// actions in the capture phase at the window is the way past that; whichever chords they carry.
const CAPTURED: &[&str] = &[
    "win.scroll-up",
    "win.scroll-down",
    "win.caret-above",
    "win.caret-below",
];

/// Refill the capture controller from the accelerators in force. Cleared first, so a rebind that
/// moves a chord away from one of the four does not leave the old one claimed.
fn fill_captured(controller: &gtk::ShortcutController, config: &Config) {
    let old: Vec<gtk::Shortcut> = (0..controller.n_items())
        .filter_map(|i| controller.item(i).and_downcast::<gtk::Shortcut>())
        .collect();
    for shortcut in old {
        controller.remove_shortcut(&shortcut);
    }
    for action in CAPTURED {
        for accel in accels_for(config, action) {
            if let Some(trigger) = gtk::ShortcutTrigger::parse_string(&accel) {
                controller.add_shortcut(gtk::Shortcut::new(
                    Some(trigger),
                    Some(gtk::NamedAction::new(action)),
                ));
            }
        }
    }
}

fn install_actions(gtk_app: &adw::Application, app: &Rc<App>) {
    for (full, _, _) in ACTIONS {
        if let Some(name) = full.strip_prefix("win.") {
            let action = gio::SimpleAction::new(name, None);
            action.connect_activate(glib::clone!(
                #[weak]
                app,
                move |_, _| {
                    // Any action fired is attention leaving the text (DESIGN.md).
                    app.show_chrome();
                    app.command_used(full);
                    app.run_action(name);
                }
            ));
            app.window.add_action(&action);
        }
    }

    app.captured
        .set_propagation_phase(gtk::PropagationPhase::Capture);
    app.window.add_controller(app.captured.clone());
    app.apply_accels();

    // Close the windows rather than calling `quit()`: `GtkApplication::quit` tears the process
    // down without emitting `close-request`, which is where unsaved buffers get written and where
    // a failed save gets to stop the exit. The application ends on its own once the last window
    // is gone, so a window that refuses to close also refuses to quit.
    let quit = gio::SimpleAction::new("quit", None);
    quit.connect_activate(glib::clone!(
        #[weak]
        gtk_app,
        move |_, _| {
            for window in gtk_app.windows() {
                window.close();
            }
        }
    ));
    gtk_app.add_action(&quit);
}

fn label_of(action: &'static str) -> &'static str {
    label_of_owned(action).unwrap_or(action)
}

/// The same lookup for a name built at run time, where there is no static string to fall back to.
fn label_of_owned(action: &str) -> Option<&'static str> {
    ACTIONS
        .iter()
        .find(|(name, _, _)| *name == action)
        .map(|(_, label, _)| *label)
}

fn menu_button() -> gtk::MenuButton {
    let menu = gio::Menu::new();
    for group in [
        ["win.new-note", "win.new-folder", "win.save"].as_slice(),
        ["win.find", "win.view-mode", "win.present"].as_slice(),
        ["win.preferences", "win.about"].as_slice(),
        // What leaves the vault, in the order of how much it takes with it.
        ["app.open-vault", "app.close-vault", "app.quit"].as_slice(),
    ] {
        let section = gio::Menu::new();
        for action in group {
            section.append(Some(label_of(action)), Some(action));
        }
        menu.append_section(None, &section);
    }
    gtk::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .tooltip_text("Main Menu")
        .menu_model(&menu)
        .valign(gtk::Align::Center)
        .build()
}

/// The tab's own context menu: what can be done with the file behind a tab without touching it.
/// Splitting leads, because it opens rather than copies; Reveal sits in a section of its own
/// because it moves the sidebar rather than the clipboard.
fn tab_menu() -> gio::Menu {
    let menu = gio::Menu::new();
    let split = gio::Menu::new();
    for side in [Side::Left, Side::Right, Side::Up, Side::Down] {
        let action = format!("win.split-{}", side.action());
        split.append(label_of_owned(&action), Some(&action));
    }
    menu.append_section(None, &split);
    for action in [
        "win.copy-relative-path",
        "win.copy-absolute-path",
        "win.show-in-files",
    ] {
        menu.append(Some(label_of(action)), Some(action));
    }
    let reveal = gio::Menu::new();
    reveal.append(
        Some(label_of("win.reveal-in-sidebar")),
        Some("win.reveal-in-sidebar"),
    );
    menu.append_section(None, &reveal);
    menu
}

/// One button rather than a two-item group: there are only two states, so the pressed look plus
/// an icon that names the current one says everything a second toggle would have.
fn mode_switcher() -> gtk::ToggleButton {
    let button = gtk::ToggleButton::builder()
        .icon_name(Mode::Editor.icon())
        .tooltip_text("Toggle Split View")
        .valign(gtk::Align::Center)
        .build();
    button.add_css_class("flat");
    button
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

// ----------------------------------------------------------------------------------- benchmarks

/// `ACCENT_BENCH_EXPAND=<rel_path>` and `ACCENT_BENCH_SWITCHER=<query>` time the two interactions
/// that used to stall the main loop, print the numbers to stdout and quit. Both run headless under
/// Xvfb, so "expanding a big directory is still fast" stays a command anyone can re-run rather
/// than a claim in a commit message. `RUST_LOG=accent=debug` adds the per-query breakdown.
fn install_bench_hooks(app: &Rc<App>) {
    let expand = std::env::var("ACCENT_BENCH_EXPAND").ok();
    let switcher = std::env::var("ACCENT_BENCH_SWITCHER").ok();
    if expand.is_none() && switcher.is_none() {
        return;
    }
    let app = app.clone();
    // After the first frame, so widget realisation is not counted in the numbers.
    glib::timeout_add_local_once(Duration::from_millis(400), move || {
        if let Some(rel) = expand {
            bench_expand(&app, &rel);
        }
        let Some(query) = switcher else {
            bench_quit(&app);
            return;
        };
        let t0 = Instant::now();
        let _ = WidgetExt::activate_action(&app.window, "win.palette-files", None);
        println!("bench switcher_open_ms {:.1}", ms_since(t0));

        // A query of "1" just means "open it"; anything else is typed into the entry so the
        // debounce, the lazy corpus load and the match all get exercised.
        let entry = (query != "1")
            .then(|| {
                app.window
                    .visible_dialog()
                    .and_then(|d| find_search_entry(d.upcast_ref()))
            })
            .flatten();
        let Some(entry) = entry else {
            bench_quit(&app);
            return;
        };
        let t1 = Instant::now();
        entry.set_text(&query);
        // Debounced, so the keystroke itself must return immediately.
        println!("bench switcher_keystroke_ms {:.1}", ms_since(t1));
        // Long enough for GtkSearchEntry's own ~150 ms delay plus our 50 ms debounce.
        glib::timeout_add_local_once(Duration::from_millis(1500), move || bench_quit(&app));
    });
}

/// First `GtkSearchEntry` in `w`'s subtree, which the bench drives directly because the headless
/// image has no xdotool.
///
/// The caller must pass the palette dialog, not the window: a window holds the sidebar's search
/// entry too, and it comes first in tree order, so searching from the window typed the benchmark's
/// query into the sidebar and measured nothing.
fn find_search_entry(w: &gtk::Widget) -> Option<gtk::SearchEntry> {
    if let Ok(e) = w.clone().downcast::<gtk::SearchEntry>() {
        return Some(e);
    }
    let mut child = w.first_child();
    while let Some(c) = child {
        if let Some(found) = find_search_entry(&c) {
            return Some(found);
        }
        child = c.next_sibling();
    }
    None
}

/// Closing the window is not enough to end the process while a dialog is up: quit the
/// application so the bench always terminates.
fn bench_quit(app: &Rc<App>) {
    match app.window.application() {
        Some(gtk_app) => gtk_app.quit(),
        None => app.window.close(),
    }
}

fn ms_since(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1e3
}

fn bench_expand(app: &Rc<App>, rel: &str) {
    let Some(tree) = app.tree.get() else { return };
    let model = tree.model();
    let mut path = String::new();
    for seg in rel.split('/') {
        if !path.is_empty() {
            path.push('/');
        }
        path.push_str(seg);
        let Some(row) = tree::find_row(model, &path) else {
            println!("bench expand {path} NOT-FOUND");
            return;
        };
        let before = model.n_items();
        let t0 = Instant::now();
        row.set_expanded(true);
        println!(
            "bench expand {path} revealed {} rows in {:.1} ms",
            model.n_items().saturating_sub(before),
            ms_since(t0)
        );
    }
    // `is_expandable` is what `GtkTreeExpander::set_list_row` calls for every row the ListView
    // binds, i.e. the per-row cost paid while scrolling.
    let n = model.n_items();
    let t0 = Instant::now();
    for i in 0..n {
        if let Some(row) = model.item(i).and_downcast::<gtk::TreeListRow>() {
            let _ = row.is_expandable();
        }
    }
    println!("bench bind_probe {n} rows in {:.1} ms", ms_since(t0));
}

// --------------------------------------------------------------------------------- appearance

/// The editor uses GNOME's *document* font, not the monospace one: notes are prose.
fn install_document_font() {
    let Some(display) = gdk::Display::default() else {
        return;
    };
    let desc = pango::FontDescription::from_string(&editor::default_font());
    let family = desc
        .family()
        .map(|f| f.to_string())
        .unwrap_or_else(|| "Monospace".to_string());
    let size = match desc.size() as f64 / pango::SCALE as f64 {
        s if s > 0.0 => s,
        _ => 11.0,
    };
    let provider = gtk::CssProvider::new();
    provider.load_from_string(&format!(
        "textview.accent-doc {{ font-family: \"{family}\"; font-size: {size}pt; }}"
    ));
    // Replaced rather than stacked, the way `theme::apply` handles its own provider: this runs
    // once per window as well as on every font change, so adding would grow the display's
    // provider list for the life of the process.
    FONT.with_borrow_mut(|slot| {
        if let Some(old) = slot.replace(provider.clone()) {
            gtk::style_context_remove_provider_for_display(&display, &old);
        }
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    });
}

thread_local! {
    /// The document-font provider currently on the display, so the next call can take it off.
    static FONT: RefCell<Option<gtk::CssProvider>> = const { RefCell::new(None) };
}

/// The app's own rules. The chrome fade (DESIGN.md) is opacity only, so the layout never
/// shifts and neither the focus order nor accessibility notices; with `gtk-enable-animations` off
/// the class still toggles but there is no transition, so the chrome snaps instead of fading and
/// nothing becomes unreachable. `.accent-flat` puts the two columns on the note's own background
/// so nothing bands against it, on a class of ours rather than on `headerbar` globally.
/// `.accent-lone-header` drops the bottom padding of the sidebar header, the one header in the
/// window that does not sit above a second bar: libadwaita pads a stacked header 3 px top and
/// bottom and its bar area another 3, so with 6 above and none below both headers hold their
/// contents in the same band whatever the interface font makes of their height.
///
/// The last rules are corrections to GtkSourceView, which styles itself from its style scheme
/// (a widget-level provider at priority 598) and from its own CSS (599). A display provider at
/// `STYLE_PROVIDER_PRIORITY_APPLICATION` outranks both per property, so the document takes the
/// theme's view colours instead of the scheme's grey, and the completion popup takes the
/// popover's. The scheme itself stays: dropping it takes the find bar's match highlight with it.
/// On the `text` node only `color` is ours, because GtkSourceView pins that node's background to
/// transparent at maximum priority; the background therefore goes on the `textview` node.
// ponytail: the header rule leans on libadwaita's own header padding (6 above a lone header,
// 3 + 3 above a stacked one) adding up to the same offset. Reach for `AdwToolbarView`'s spacing
// API instead if one ever appears; today the class is the only handle on it.
//
// ponytail: `paned.dragging` widens the handle from 1 px to 3 px, which moves the pane beside it
// by 2 px for the length of the drag. Drawing outside the 1 px allocation instead, with an
// outline or a negative margin, was measured: it only ever reaches the side rendered before the
// handle, because the pane after it paints over the other. A 2 px shift while a divider is being
// dragged is invisible, so it is the cheaper of the two.
fn install_chrome_css() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let Some(display) = gdk::Display::default() else {
            return;
        };
        let fade = match gtk::Settings::for_display(&display).is_gtk_enable_animations() {
            true => ".chrome-fade { transition: opacity 250ms ease; } ",
            false => "",
        };
        let provider = gtk::CssProvider::new();
        provider.load_from_string(&format!(
            "{fade}.chrome-hidden {{ opacity: 0; }} \
             .chrome-dimmed {{ opacity: 0.5; }} \
             .accent-pill {{ padding: 6px; border-radius: 12px; }} \
             .accent-drop-zone {{ background-color: var(--accent-bg-color); opacity: 0.25; }} \
             paned.dragging > separator {{ min-width: 3px; min-height: 3px; \
               background-color: var(--border-color); }} \
             .accent-flat, .accent-flat:backdrop {{ background-color: var(--view-bg-color); }} \
             .accent-lone-header > windowhandle > box {{ padding-bottom: 0; }} \
             textview.accent-doc {{ color: var(--view-fg-color); \
               background-color: var(--view-bg-color); }} \
             textview.accent-doc text {{ color: var(--view-fg-color); }} \
             GtkSourceAssistant.completion {{ background-color: var(--popover-bg-color); \
               color: var(--popover-fg-color); min-width: 240px; \
               box-shadow: 0 1px 4px var(--shade-color), 0 0 0 1px var(--shade-color); }} \
             GtkSourceAssistant.completion list row {{ padding: 3px 6px; }} \
             GtkSourceAssistant.completion list row cell.typed-text {{ margin-left: 12px; \
               margin-right: 12px; min-height: 30px; }} \
             textview.GtkSourceMap {{ font-size: 2.5pt; line-height: 6px; }}"
        ));
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    });
}

/// What the toast says after a Replace All: what it wrote, what it could not, and what is still
/// showing the old text because its tab has unsaved edits. Same shape as `fileops::rename_message`.
fn replace_message(matches: usize, notes: usize, failed: usize, unsaved: usize) -> String {
    let plural = |n: usize, one: &str, many: &str| match n {
        1 => format!("1 {one}"),
        n => format!("{n} {many}"),
    };
    let mut message = match matches {
        0 => "Nothing to replace".to_string(),
        _ => format!(
            "Replaced {} in {}",
            plural(matches, "match", "matches"),
            plural(notes, "note", "notes")
        ),
    };
    if failed > 0 {
        message.push_str(&format!("; {failed} could not be written"));
    }
    if unsaved > 0 {
        message.push_str(&format!(
            "; {unsaved} have unsaved changes and were not reloaded"
        ));
    }
    message
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replace_toast_counts_matches_notes_and_what_went_wrong() {
        assert_eq!(replace_message(0, 0, 0, 0), "Nothing to replace");
        assert_eq!(replace_message(1, 1, 0, 0), "Replaced 1 match in 1 note");
        assert_eq!(replace_message(7, 3, 0, 0), "Replaced 7 matches in 3 notes");
        assert_eq!(
            replace_message(7, 3, 1, 2),
            "Replaced 7 matches in 3 notes; 1 could not be written; 2 have unsaved changes and were not reloaded"
        );
    }

    #[test]
    fn zoom_steps_in_tenths_and_stops_at_the_ends() {
        assert_eq!(clamp_zoom(1.0 + ZOOM_STEP), 1.1);
        assert_eq!(clamp_zoom(1.0 - ZOOM_STEP), 0.9);
        assert_eq!(clamp_zoom(0.1), 0.5, "no zooming down to nothing");
        assert_eq!(clamp_zoom(9.0), 3.0, "nor up past legibility");
        assert_eq!(clamp_zoom(1.24), 1.2, "a hand-edited state file is rounded");
    }

    #[test]
    fn accels_for_prefers_the_override() {
        let mut config = Config::default();
        assert_eq!(accels_for(&config, "win.save"), ["<Control>s"]);
        assert!(accels_for(&config, "win.about").is_empty());
        assert!(accels_for(&config, "win.nonexistent").is_empty());

        config.shortcuts.insert(
            "win.save".to_string(),
            vec!["<Control><Shift>s".to_string()],
        );
        // An override replaces the whole list rather than adding to it.
        assert_eq!(accels_for(&config, "win.save"), ["<Control><Shift>s"]);
        // An empty override is "unbound", not "fall back to the default".
        config.shortcuts.insert("win.find".to_string(), Vec::new());
        assert!(accels_for(&config, "win.find").is_empty());
    }

    /// Every action the capture controller claims has to be in the table it reads its chords from,
    /// or a rebind would silently drop it.
    #[test]
    fn captured_actions_are_in_the_action_table() {
        for action in CAPTURED {
            assert!(
                ACTIONS.iter().any(|(name, _, _)| name == action),
                "{action} is captured but not in ACTIONS"
            );
        }
    }

    /// `set_accels_for_action` is last-writer-wins, so a chord claimed twice silently unbinds the
    /// action listed first. The table is the only place that can go wrong, and it is pure data.
    #[test]
    fn no_two_actions_claim_the_same_accelerator() {
        let mut seen = std::collections::HashMap::new();
        for (name, _, accels) in ACTIONS {
            for accel in *accels {
                if let Some(other) = seen.insert(*accel, *name) {
                    panic!("{accel} is bound to both {other} and {name}");
                }
            }
        }
    }
}
