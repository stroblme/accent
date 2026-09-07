//! accent desktop app: GTK4 + libadwaita shell.
//!
//! `accent [vault-dir] [note.md]`. Without a path the start screen picks a vault; with one,
//! [`accent_api::Vault`] opens the index and the tree is filled straight from it, while the
//! worker thread reconciles and watches in the background. The window is never blocked, and every
//! change the vault reports arrives here as an [`Event`].

mod askpass;
mod comment;
mod completion;
mod connect;
mod diagnostics;
mod diff;
mod doc;
mod editor;
mod fileops;
mod find;
mod git;
mod highlight;
mod hover;
mod lang;
mod marks;
mod multicaret;
mod palette;
mod paned;
mod panes;
mod pdftab;
mod pdfview;
mod preview;
mod settings;
mod sidebar;
mod start;
mod statusbar;
mod terminal;
mod theme;
mod tree;
mod typing;

use accent_api::{Config, Etag, Event, SaveError, Session, Vault, ssh};
use accent_core::config::PdfZoom;
use accent_core::index::Phase;
use accent_core::markdown::{Link, LinkKind};
use adw::prelude::*;
use doc::{Doc, Kind};
use editor::{Alert, Flavour, Prefs, Tab};
use gtk::{gdk, gio, glib};
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
/// Tracing target for the save/etag decisions, so a conflict reported in a real session can be
/// read back afterwards: `RUST_LOG=accent::saves=debug accent <vault>` records every write with
/// the etag it expected and the one it wrote, and every watcher report with the etag the tab
/// holds against the one on disk. Its own target, because the answer is a handful of lines and
/// `accent=debug` is a wall of them.
const SAVES: &str = "accent::saves";

/// Every user-facing action: the name it answers to, the label the menu and the palette show, and
/// its accelerators. One table, so an action cannot exist without being reachable and findable
/// (DESIGN.md, Keyboard). Stepping along the bar and picking a tab by number stay `AdwTabView`'s
/// own shortcuts; the two that walk the tabs in the order they were last used are ours, because
/// libadwaita has no notion of that order.
const ACTIONS: &[(&str, &str, &[&str])] = &[
    ("win.save", "Save", &["<Control>s"]),
    ("win.open-file", "Open File…", &["<Control>o"]),
    ("win.new-note", "New Note", &["<Control>n"]),
    ("win.new-folder", "New Folder", &["<Control><Shift>n"]),
    ("win.upload", "Upload Files…", &[]),
    ("win.close-tab", "Close Tab", &["<Control>w"]),
    // Most-recently-used order, so one press is the note before this one. Both spellings of the
    // backwards chord, because X11 delivers Shift+Tab as `ISO_Left_Tab` and which of the two a
    // GTK trigger matches is a question of the keymap rather than of the table.
    ("win.next-tab", "Next Tab", &["<Control>Tab"]),
    (
        "win.previous-tab",
        "Previous Tab",
        &["<Control><Shift>Tab", "<Control><Shift>ISO_Left_Tab"],
    ),
    ("win.terminal", "New Terminal", &["<Control>j"]),
    // Actions rather than callbacks on the shell itself, so they rebind, list in the palette and
    // can be named by the terminal's own context menu. Both spellings carry Control and Shift, so
    // `forwarded` hands them back from a focused shell without being told to.
    (
        "win.terminal-copy",
        "Copy in Terminal",
        &["<Control><Shift>c"],
    ),
    (
        "win.terminal-paste",
        "Paste in Terminal",
        &["<Control><Shift>v"],
    ),
    // Split Right takes VS Code's chord; the other three are menu and palette only, because
    // three more accelerators for the same idea is three more chords nobody has to spare.
    ("win.split-right", "Split Right", &["<Control>backslash"]),
    ("win.split-left", "Split Left", &[]),
    ("win.split-up", "Split Up", &[]),
    ("win.split-down", "Split Down", &[]),
    ("app.new-window", "New Window", &[]),
    ("app.open-vault", "Open Folder…", &["<Control><Shift>o"]),
    ("app.open-remote", "Open Remote…", &[]),
    ("win.open-recent", "Open Recent…", &["<Control>r"]),
    ("app.close-vault", "Close Vault", &[]),
    ("app.quit", "Quit", &["<Control>q"]),
    ("win.palette-files", "Go to File…", &["<Control>e"]),
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
    (
        "win.newline-below",
        "Insert Line Below",
        &["<Control>Return"],
    ),
    ("win.toggle-comment", "Toggle Comment", &["<Control>k"]),
    ("win.toggle-wrap", "Toggle Word Wrap", &["<Alt>z"]),
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
    // No chord: it is the Search pane's own All button, and the palette is how a command with
    // no chord is found.
    ("win.search-all", "Search Ignored Files", &[]),
    ("win.pane-tags", "Tags Pane", &["<Control><Shift>t"]),
    ("win.pane-git", "Git Pane", &["<Control><Shift>g"]),
    ("win.git-sync", "Sync", &[]),
    ("win.pane-outline", "Outline Pane", &["<Control><Shift>l"]),
    // The PDF reader. Back and forward take the chords a browser uses for the same idea;
    // the rest live in the palette, where they are found by name rather than by chord.
    ("win.pdf-back", "Back", &["<Alt>Left"]),
    ("win.pdf-forward", "Forward", &["<Alt>Right"]),
    ("win.pdf-fit-width", "Fit Width", &[]),
    ("win.pdf-fit-page", "Fit Page", &[]),
    ("win.pdf-invert", "Invert PDF Colours", &[]),
    ("win.backlinks", "Backlinks Pane", &["<Control><Shift>b"]),
    ("win.view-mode", "Toggle Split View", &["<Control>m"]),
    ("win.minimap", "Toggle Minimap", &[]),
    ("win.copy-relative-path", "Copy Relative Path", &[]),
    ("win.copy-absolute-path", "Copy Absolute Path", &[]),
    ("win.show-in-files", "Show in Files", &[]),
    ("win.reveal-in-sidebar", "Reveal in Sidebar", &[]),
    (
        "win.follow-link",
        "Follow Link",
        &["<Control><Shift>Return"],
    ),
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
    /// `None` for the one window opened on files rather than on a folder.
    windows: RefCell<Vec<(Option<PathBuf>, Rc<App>)>>,
    /// The start screen while one is up, so Open Folder… presents it again instead of stacking a
    /// second copy. Weak: the window belongs to GTK, and closing it is how it goes away.
    start: glib::WeakRef<adw::ApplicationWindow>,
    /// Where a dragged tab was let go, between our drop zone seeing it and libadwaita asking for
    /// somewhere to put it. See [`Landing`].
    landing: RefCell<Option<Landing>>,
}

/// A tab let go over a pane, waiting for `AdwTabView::create-window` to spend it.
///
/// libadwaita detaches a dragged page from its view for the length of the drag, and neither
/// `attach_page` nor the page's own view is public, so nothing can give a page a view back except
/// the `create-window` handler, which libadwaita calls on the source view the moment a drop is
/// declined. Our drop zones therefore record where the drop landed and decline it; the handler
/// hands back the tab view named here and libadwaita does the attaching.
struct Landing {
    app: Rc<App>,
    pane: Rc<Pane>,
    zone: Zone,
}

impl Shell {
    /// The actions that outlive the window firing them: Open Folder… and Open Remote… both land a
    /// vault that may not be this window's, Close Vault takes the current window away, and Quit
    /// takes them all, so none of them can live on a window the way the `win.` actions do.
    /// Registered once on the application, where the shell is in scope.
    ///
    /// Each one records itself in the active window's recently-run list on the way through, which
    /// is what the `win.` trampoline in [`install_actions`] does for everything else: an action
    /// the palette lists has to be an action the palette can learn.
    fn install_app_actions(self: &Rc<Self>, gtk_app: &adw::Application) {
        let open = gio::SimpleAction::new("open-vault", None);
        open.connect_activate({
            let (shell, gtk_app) = (self.clone(), gtk_app.clone());
            move |_, _| {
                shell.record(&gtk_app, "app.open-vault");
                shell.choose_vault(&gtk_app);
            }
        });
        gtk_app.add_action(&open);

        let remote = gio::SimpleAction::new("open-remote", None);
        remote.connect_activate({
            let (shell, gtk_app) = (self.clone(), gtk_app.clone());
            move |_, _| {
                shell.record(&gtk_app, "app.open-remote");
                shell.choose_remote(&gtk_app);
            }
        });
        gtk_app.add_action(&remote);

        // GNOME Shell offers New Window in the launcher's context menu only when it finds an
        // `app.new-window` action, the `new-window` desktop action, or one of the SingleWindow
        // keys. Both of the first two are provided: this is the one DESIGN.md's Keyboard rule
        // asks for, and the desktop file carries the other for a shell that reads it first.
        let new_window = gio::SimpleAction::new("new-window", None);
        new_window.connect_activate({
            let (shell, gtk_app) = (self.clone(), gtk_app.clone());
            move |_, _| {
                shell.record(&gtk_app, "app.new-window");
                shell.start_screen(&gtk_app);
            }
        });
        gtk_app.add_action(&new_window);

        let close = gio::SimpleAction::new("close-vault", None);
        close.connect_activate({
            let (shell, gtk_app) = (self.clone(), gtk_app.clone());
            move |_, _| {
                shell.record(&gtk_app, "app.close-vault");
                shell.close_vault(&gtk_app);
            }
        });
        gtk_app.add_action(&close);

        // Close the windows rather than calling `quit()`: `GtkApplication::quit` tears the process
        // down without emitting `close-request`, which is where unsaved buffers get written and
        // where a failed save gets to stop the exit. The application ends on its own once the last
        // window is gone, so a window that refuses to close also refuses to quit.
        let quit = gio::SimpleAction::new("quit", None);
        quit.connect_activate({
            let (shell, gtk_app) = (self.clone(), gtk_app.clone());
            move |_, _| {
                // Recorded before the windows go: the session is written by each window's own
                // `close-request`, which runs after this and carries the entry with it.
                shell.record(&gtk_app, "app.quit");
                for window in gtk_app.windows() {
                    window.close();
                }
            }
        });
        gtk_app.add_action(&quit);
    }

    /// The [`App`] behind a window, for the app-scoped actions: they are fired at the application
    /// and have to find the window that asked before they can record anything on it.
    fn app_at(&self, window: &gtk::Window) -> Option<Rc<App>> {
        self.windows
            .borrow()
            .iter()
            .find(|(_, app)| app.window.upcast_ref::<gtk::Window>() == window)
            .map(|(_, app)| app.clone())
    }

    /// Record an `app.` action in the active window's recently-run commands. Nothing happens from
    /// the start screen, which has no session to remember it in.
    fn record(&self, gtk_app: &adw::Application, action: &str) {
        if let Some(app) = gtk_app.active_window().and_then(|w| self.app_at(&w)) {
            app.command_used(action);
        }
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
        // `accent --terminal [dir]` is accent as a terminal: a window with no vault holding one
        // shell. A second one joins that window as another tab, the way a second loose file does.
        if args.iter().any(|a| a == "--terminal" || a == "-t") {
            let cwd = terminal_cwd(&args).and_then(|arg| {
                // Resolved against the invoking process's directory, as a vault path is.
                let path = command_line.create_file_for_arg(arg).path()?;
                match path.canonicalize() {
                    Ok(dir) if dir.is_dir() => Some(dir),
                    // A file is not taken as its parent: which directory was meant is a guess,
                    // and a shell in the wrong one is worse than a shell at home that says so.
                    Ok(other) => {
                        eprintln!("not a folder, opening at home: {}", other.display());
                        None
                    }
                    Err(e) => {
                        eprintln!("cannot open {}: {e}", path.display());
                        None
                    }
                }
            });
            if let Some(app) = self.loose_window(gtk_app) {
                app.window.present();
                app.open_terminal_at(cwd);
            }
            return glib::ExitCode::SUCCESS;
        }
        // `accent --new-window` is the launcher's New Window action, and a second process is how
        // it arrives. The start screen rather than a vault: opening a vault that already has a
        // window would only raise it, and no vault ever gets a second one.
        if args.iter().any(|a| a == "--new-window") {
            self.start_screen(gtk_app);
            return glib::ExitCode::SUCCESS;
        }
        let Some(arg) = args.get(1) else {
            // Launched with no folder: pick up the vault this window was last opened on, and only
            // fall back to the start screen when there is none or it has gone away.
            let last = self.config.borrow().recent_vaults.first().cloned();
            match last.filter(|path| path.is_dir() || ssh::is_remote_path(path)) {
                Some(root) => self.open_vault(gtk_app, root, None),
                None => self.start_screen(gtk_app),
            }
            return glib::ExitCode::SUCCESS;
        };
        // An address rather than a path, and `create_file_for_arg` would answer a URI whose
        // `path()` is `None` — "cannot resolve" for something perfectly openable.
        if let Some(address) = arg.to_str().filter(|a| ssh::is_remote(a)) {
            let note = args.get(2).and_then(|a| a.to_str()).map(str::to_string);
            // Read back through the parser rather than taken as typed: `Vault::key` is the
            // address as `ssh::Url` spells it, and that is what the recent list and the
            // one-window-per-vault check compare against. `ssh://box/srv/vault/` stored as typed
            // matches neither, so it would open a second window on a vault already open.
            let root = match ssh::parse(address) {
                Ok(url) => url.to_string(),
                Err(e) => {
                    eprintln!("cannot open {address}: {e}");
                    return glib::ExitCode::FAILURE;
                }
            };
            self.open_vault(gtk_app, PathBuf::from(root), note);
            return glib::ExitCode::SUCCESS;
        }
        // Resolved against the *invoking* process's directory, not this one's: a second
        // `accent notes/x.md` is forwarded here by the single instance, whose cwd is its own.
        let path = match command_line.create_file_for_arg(arg).path() {
            Some(path) => path,
            None => {
                // `printerr_literal` needs glib 2.80, which this build does not enable; a
                // local invocation is the only one that has a terminal to print to anyway.
                eprintln!("cannot resolve: {}", arg.to_string_lossy());
                return glib::ExitCode::FAILURE;
            }
        };
        match path.canonicalize() {
            Ok(root) if root.is_dir() => {
                let note = args.get(2).and_then(|a| a.to_str()).map(str::to_string);
                self.open_vault(gtk_app, root, note);
            }
            // A file rather than a folder: opened where it belongs, which is how accent works as
            // the system's PDF viewer and text editor.
            Ok(file) => self.open_file(gtk_app, file),
            Err(e) => {
                eprintln!("cannot open {}: {e}", path.display());
                return glib::ExitCode::FAILURE;
            }
        }
        glib::ExitCode::SUCCESS
    }

    /// Open Folder…: the picker, straight away.
    ///
    /// It used to land on the start screen, which then showed a button that opened this dialog —
    /// a screen in the way of the thing it was asking for. The start screen is still what a bare
    /// launch with no vault lands on, where it also lists the recent ones.
    fn choose_vault(self: &Rc<Self>, gtk_app: &adw::Application) {
        let dialog = gtk::FileDialog::builder().title("Open Vault").build();
        let parent = gtk_app.active_window();
        let (shell, gtk_app) = (self.clone(), gtk_app.clone());
        dialog.select_folder(parent.as_ref(), gio::Cancellable::NONE, move |result| {
            // A dismissed chooser is an error here, and not one worth saying anything about.
            let Some(path) = result.ok().and_then(|folder| folder.path()) else {
                return;
            };
            shell.open_vault(&gtk_app, path, None);
            // The start screen has done its job if it was what asked.
            if let Some(window) = shell.start.upgrade() {
                window.close();
            }
        });
    }

    /// Open Remote…: the same host-and-path form the start screen asks with, over whichever
    /// window is in front. A remote vault is opened, keyed and remembered exactly as a local one
    /// is, so there is nothing here but the address.
    fn choose_remote(self: &Rc<Self>, gtk_app: &adw::Application) {
        let Some(window) = gtk_app.active_window() else {
            return;
        };
        let (shell, gtk_app) = (self.clone(), gtk_app.clone());
        start::connect_dialog(&window, move |address| {
            shell.open_vault(&gtk_app, PathBuf::from(address), None);
            // The start screen has done its job if it was what asked.
            if let Some(window) = shell.start.upgrade() {
                window.close();
            }
        });
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

    /// One vault, one window (VS Code's rule): a vault that already has a window raises it rather
    /// than opening a second one on the same index, session and watcher. New Window lands on the
    /// start screen instead, where a vault without a window yet is picked.
    ///
    /// `root` is what the vault is keyed by: a directory, or an `ssh://` address for one on
    /// another machine. The two are one list, one rule and one window each.
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
        self.add_window(gtk_app, Some(root), note);
    }

    /// Build a window on `root` — a vault, or `None` for the one with no vault — and take charge
    /// of it. The only place a window joins `windows`, so the handler that takes it out again is
    /// written once.
    fn add_window(
        self: &Rc<Self>,
        gtk_app: &adw::Application,
        root: Option<PathBuf>,
        note: Option<String>,
    ) -> Option<Rc<App>> {
        let app = build_window(gtk_app, self, root.clone(), note)?;
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
        self.windows.borrow_mut().push((root, app.clone()));
        Some(app)
    }

    /// The window a page belongs to, and what it holds. libadwaita's tab drag hands a page to any
    /// window in the process, so this is how the receiving one finds out where it came from.
    fn owner_of(&self, page: &adw::TabPage) -> Option<(Rc<App>, Doc)> {
        self.windows
            .borrow()
            .iter()
            .find_map(|(_, app)| Some((app.clone(), app.doc_for_page(page)?)))
    }

    /// A page has landed in `into`'s tab view that `into` knows nothing about: either a tab
    /// dragged out of another window, or one of its own a moment before it is registered.
    ///
    /// From an idle rather than here, because `page-attached` fires inside libadwaita's own drop
    /// handling, which is still holding the page — and because a page this window has just built
    /// is attached before it reaches `docs`, so the second look is what tells the two apart.
    fn adopt_soon(self: &Rc<Self>, into: &Rc<App>, page: &adw::TabPage) {
        let (shell, into, page) = (Rc::downgrade(self), Rc::downgrade(into), page.clone());
        glib::idle_add_local_once(move || {
            if let (Some(shell), Some(into)) = (shell.upgrade(), into.upgrade()) {
                shell.adopt_page(&into, &page);
            }
        });
    }

    /// A dragged tab was let go over `pane`. Recorded rather than moved: see [`Landing`].
    fn aim(self: &Rc<Self>, app: &Rc<App>, pane: &Rc<Pane>, zone: Zone) {
        *self.landing.borrow_mut() = Some(Landing {
            app: app.clone(),
            pane: pane.clone(),
            zone,
        });
        // Spent by `create-window` in the same turn of the main loop; this is only so a drop that
        // never reaches one cannot misdirect the next drag.
        let shell = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            if let Some(shell) = shell.upgrade() {
                *shell.landing.borrow_mut() = None;
            }
        });
    }

    /// The tab view a page let go over one of our drop zones belongs in, or `None` when the drop
    /// was nowhere of ours.
    fn where_to_land(&self) -> Option<adw::TabView> {
        // Left in place rather than taken: `landed` spends it, once the page is somewhere.
        let (app, pane) = self
            .landing
            .borrow()
            .as_ref()
            .map(|l| (l.app.clone(), l.pane.clone()))?;
        // The pane may have closed itself behind the drag, having held nothing else.
        let at = match app.panes.borrow().iter().any(|p| Rc::ptr_eq(p, &pane)) {
            true => pane,
            false => app.pane(),
        };
        app.window.present();
        Some(at.tabs.clone())
    }

    /// A page has been attached to `pane`, which is where a drag ends. Two things may be owed:
    /// the split the drop asked for, and — for a page out of another window — the move into this
    /// window's bookkeeping.
    ///
    /// The split waits for an idle because this runs inside libadwaita's drag handling, and
    /// re-parenting the pane it is emitting from fails GTK's own assertion.
    fn landed(self: &Rc<Self>, app: &Rc<App>, pane: &Rc<Pane>, page: &adw::TabPage) {
        let side = self
            .landing
            .borrow_mut()
            .take()
            .filter(|l| Rc::ptr_eq(&l.pane, pane))
            .and_then(|l| match l.zone {
                Zone::Split(side) => Some(side),
                Zone::Here => None,
            });
        if let Some(side) = side {
            let (app, pane, page) = (app.clone(), pane.clone(), page.clone());
            glib::idle_add_local_once(move || app.split_page(&pane, side, &page));
        }
        // Before the adoption, which is queued behind it: the note is reopened in whichever pane
        // the window is working in, and a split has just made that the new one.
        if app.doc_for_page(page).is_none() {
            self.adopt_soon(app, page);
        }
    }

    /// Move a tab from the window it was dragged out of into the window it was dropped on.
    ///
    /// One vault never has two windows, so the note always comes from another vault: it is
    /// adopted as an absolute-path tab, the files-outside-a-vault model (DESIGN.md, Window
    /// without a vault). It edits and saves; it gets no index, backlinks or wikilinks here. The
    /// buffer is written out first, because the receiving window opens the *file* — a drag is a
    /// focus change, and a focus change always saves.
    fn adopt_page(self: &Rc<Self>, into: &Rc<App>, page: &adw::TabPage) {
        // Gone again, or one of `into`'s own that had not reached `docs` when it was attached.
        if into.pane_of(page).is_none() || into.doc_for_page(page).is_some() {
            return;
        }
        let Some((from, doc)) = self.owner_of(page) else {
            return tracing::debug!("a tab in no window's bookkeeping; left where it is");
        };
        // A shell is a running process and a diff is a view of two texts: neither is a file the
        // other window could open, so the drag goes back where it came from.
        if doc.is_transient() {
            return return_page(into, &from, page, "This tab cannot move between windows.");
        }
        let key = doc.key();
        let path = match doc::is_loose_key(&key) {
            true => PathBuf::from(&key),
            false => from.root().join(&key),
        };
        if let Some(tab) = doc.tab().filter(|tab| tab.modified.get())
            && let Err(e) = from.write_tab(tab, tab.etag.get())
        {
            // Refused rather than dropped: a drag must never be the thing that loses an edit.
            return return_page(into, &from, page, &format!("Save failed: {e}"));
        }
        // Opened before the old page goes, so a pane that the drop has just split off never
        // stands empty and closes itself out from under the note arriving in it.
        into.open_path(&into.key_for(&path));
        from.forget_page(page);
        into.close_page(page);
    }

    fn window_for(&self, root: &Path) -> Option<adw::ApplicationWindow> {
        let windows = self.windows.borrow();
        let (_, app) = windows
            .iter()
            .find(|(path, _)| path.as_deref() == Some(root))?;
        Some(app.window.clone())
    }

    /// Open `path` wherever it belongs: in the window whose vault contains it, or in the one
    /// window this process keeps for files that are in no vault.
    ///
    /// ponytail: one vault-less window per process, so a second loose file joins it as a tab.
    /// Give it a window each the day two of them need to sit side by side.
    fn open_file(self: &Rc<Self>, gtk_app: &adw::Application, path: PathBuf) {
        let inside = self.windows.borrow().iter().find_map(|(root, app)| {
            let rel = path.strip_prefix(root.as_ref()?).ok()?;
            Some((app.clone(), rel.to_string_lossy().into_owned()))
        });
        if let Some((app, rel)) = inside {
            app.window.present();
            app.open_path(&rel);
            return;
        }
        let Some(app) = self.loose_window(gtk_app) else {
            return;
        };
        app.window.present();
        app.open_path(&path.to_string_lossy());
    }

    /// The window with no vault, built if this is the first thing to want one. One per process, so
    /// a second loose file — or a second shell — joins it as a tab.
    fn loose_window(self: &Rc<Self>, gtk_app: &adw::Application) -> Option<Rc<App>> {
        let loose = self
            .windows
            .borrow()
            .iter()
            .find(|(root, _)| root.is_none())
            .map(|(_, app)| app.clone());
        if let Some(app) = loose {
            return Some(app);
        }
        self.add_window(gtk_app, None, None)
    }
}

/// Where `accent --terminal` was pointed: the first argument that is not one of the flags, or
/// `None` for the bare form. Pure, so the parsing is a test rather than a manual run; whether the
/// path is a directory is the caller's question, because only it can resolve one.
fn terminal_cwd(args: &[std::ffi::OsString]) -> Option<&std::ffi::OsStr> {
    args.iter()
        .skip(1)
        .map(|arg| arg.as_os_str())
        .find(|arg| !matches!(arg.to_str(), Some("--terminal" | "-t" | "--new-window")))
}

/// Hand a page back to the window it was dragged out of, and say there why.
///
/// The pane it left may have closed itself behind it, so it goes to whichever pane that window is
/// working in rather than to the one it came from.
fn return_page(into: &Rc<App>, from: &Rc<App>, page: &adw::TabPage, why: &str) {
    let Some(here) = into.pane_of(page) else {
        return;
    };
    here.tabs.transfer_page(page, &from.pane().tabs, 0);
    from.window.present();
    from.toast(why);
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

/// Why a tab was opened, which decides whether it stays.
///
/// A tab opened by browsing — a click in the sidebar tree, a search hit, a Git row, a wikilink
/// followed — is a `Preview`: the next such open closes it and takes its place, so clicking down
/// a list of notes to see what is in them leaves one tab rather than twenty. Anything the reader
/// named is `Kept`: the palette, Open File…, a drop, a rename, the command line and the session,
/// where the file was asked for by name and the tab is meant to stay. A preview tab becomes a
/// kept one the moment it is edited, its own tab is double-clicked, or it is moved to another
/// pane, all three being the reader saying they want to keep it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Opened {
    Kept,
    Preview,
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
    /// Find, replace and go to line, one bar for the window rather than one per tab.
    find: Rc<find::Bar>,
    /// The bar along the bottom of the editor column: progress, branch, file type, word count.
    statusbar: statusbar::Bar,
    /// Every open tab, whatever it holds. A `Vec`, not a map: a rename retargets an open tab,
    /// so its key is not a stable one.
    docs: RefCell<Vec<Doc>>,
    /// Set once, after `App` exists, by the sidebar the tree lives in.
    tree: OnceCell<tree::Tree>,
    sidebar: OnceCell<sidebar::Sidebar>,
    /// The Git pane, in a vault window whose sidebar has one. Set once, with the sidebar.
    git: OnceCell<Rc<git::Panel>>,
    /// The pane the session asked for and the sidebar could not show yet. Only the Git page is
    /// ever missing at restore time — it does not exist until the first refresh finds a
    /// repository — so the first refresh reads this and then clears it for good.
    pane_wanted: RefCell<String>,
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
    header: adw::HeaderBar,
    modes: gtk::ToggleButton,
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
    /// Whether the accelerator table is currently narrowed to [`reserved`] for a focused shell.
    /// Only a change is worth acting on: focus moves on every click, and the rebuild is sixty
    /// `set_accels_for_action` calls.
    shell_keys: Cell<bool>,
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
        let page = self.tabs().selected_page()?;
        self.doc_for_page(&page)
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

    // --- opening -------------------------------------------------------------------------

    /// Open anything, from the tree, the palette, a link, a drop, the session or the command
    /// line. This is the only door into a tab.
    ///
    /// What a file opens as is decided by its name first, and then by its bytes when the name
    /// says "text": a `.png` that is really random bytes is still an image tab, but a `.py` full
    /// of NULs is a status page rather than a screen of garbage.
    fn open_path(self: &Rc<Self>, key: &str) {
        self.open_as(key, Opened::Kept);
    }

    /// Open `key` for a look: the tab is this pane's preview, and the next such open takes its
    /// place instead of leaving it behind. What a click in the sidebar and a followed link do.
    fn open_preview(self: &Rc<Self>, key: &str) {
        self.open_as(key, Opened::Preview);
    }

    fn open_as(self: &Rc<Self>, key: &str, how: Opened) {
        let Some((key, path)) = self.locate(key) else {
            // A session pointing at a file that has since been deleted lands here too, and
            // "outside this vault" would be the wrong thing to say about it.
            return match self.root().join(key).exists() {
                true => self.toast(&format!("{key} is outside this vault")),
                false => self.toast(&format!("Cannot open {key}: no such file")),
            };
        };
        // A note that is already open keeps whatever it is: looking at a real tab again does not
        // demote it, and looking at the preview again does not promote it.
        if let Some(doc) = self.doc_for(&key) {
            return self.reveal_page(doc.page());
        }
        match doc::kind_of(&key) {
            Kind::Note => self.open_text(&key, &path, Flavour::Note, how),
            Kind::Image => self.open_image(&key, &path, how),
            Kind::Pdf => self.open_pdf(&key, &path, how),
            Kind::Text => self.open_text(&key, &path, flavour_of(&key), how),
        }
    }

    /// Open a note. Kept as its own name because most callers mean exactly this, and it says so.
    fn open_note(self: &Rc<Self>, rel: &str) {
        self.open_path(rel);
    }

    /// A preview tab has arrived: it replaces whichever tab this pane was previewing before.
    ///
    /// The old tab goes after the new one is in place, so the pane never stands empty and closes
    /// itself out from under the note arriving in it.
    fn mark_opened(&self, page: &adw::TabPage, how: Opened) {
        if how != Opened::Preview {
            return;
        }
        let Some(pane) = self.pane_of(page) else {
            return;
        };
        if let Some(old) = pane.set_preview(page) {
            pane.tabs.close_page(&old);
        }
    }

    /// The preferences every text tab is built with.
    fn prefs(&self) -> Prefs {
        let config = self.config.borrow();
        Prefs {
            spellcheck: config.spellcheck,
            font: config.editor_font.clone(),
            zoom: self.zoom.get(),
            column_width: config.column_width,
            minimap: config.minimap,
            line_numbers: config.line_numbers,
        }
    }

    /// A text file in an editor tab, unless its bytes say it is not one after all.
    ///
    /// The read happens on a worker thread, so opening a note on a remote vault does not hold the
    /// window for the round trip — measured at ~60 ms to the host this was developed against,
    /// which is four frames. Locally it lands in the same turn of the loop and nothing changes.
    fn open_text(self: &Rc<Self>, key: &str, path: &Path, flavour: Flavour, how: Opened) {
        // A loose file has no vault to ask, so it still reads its own absolute path.
        let vault = self.vault().filter(|_| !doc::is_loose_key(key)).cloned();
        let (key, path) = (key.to_string(), path.to_path_buf());
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let read = gio::spawn_blocking({
                let key = key.clone();
                move || match vault {
                    Some(vault) => vault.read_text(&key),
                    None => accent_core::fs::read_text(&path),
                }
            })
            .await;
            let Some(app) = weak.upgrade() else { return };
            // Two clicks on the same row while the first read was in flight: the tab exists now.
            if app.doc_for(&key).is_some() {
                return;
            }
            match read {
                Ok(read) => app.adopt_text(&key, read, flavour, how),
                Err(_) => tracing::warn!("the reader panicked on {key}"),
            }
        });
    }

    /// What [`open_text`](Self::open_text) does once the bytes are in hand.
    fn adopt_text(
        self: &Rc<Self>,
        key: &str,
        read: std::io::Result<accent_core::fs::Read>,
        flavour: Flavour,
        how: Opened,
    ) {
        let text = match read {
            Ok(accent_core::fs::Read::Text(text)) => text,
            Ok(accent_core::fs::Read::Binary { size }) => {
                return self.open_status(
                    key,
                    "Binary File",
                    &format!("{} is not text, so there is nothing to edit.", human(size)),
                    how,
                );
            }
            Ok(accent_core::fs::Read::TooLarge { size }) => {
                return self.open_status(
                    key,
                    "File Too Large",
                    // The file's size goes through `human`, which is decimal because that is what
                    // GNOME shows in Files. The cap does not: it is `16 * 1024 * 1024`, and
                    // decimal units render that as "16.8 MB", which is not the number anyone set.
                    &format!(
                        "{} is over the {} MiB accent will read into an editor.",
                        human(size),
                        accent_core::fs::MAX_TEXT / (1024 * 1024)
                    ),
                    how,
                );
            }
            Err(e) => return self.toast(&format!("Cannot open {key}: {e}")),
        };
        let prefs = self.prefs();
        let tab = editor::open(
            &self.root(),
            key,
            text,
            flavour,
            &self.tabs(),
            &prefs,
            (
                // Without a vault there is nothing to complete against, and the closures are
                // only ever installed on a note anyway.
                {
                    let vault = self.vault.clone();
                    move |prefix: &str| {
                        vault
                            .as_ref()
                            .and_then(|v| v.complete_notes(prefix, COMPLETIONS).ok())
                            .unwrap_or_default()
                    }
                },
                {
                    let vault = self.vault.clone();
                    move |prefix: &str| {
                        vault
                            .as_ref()
                            .and_then(|v| v.complete_tags(prefix, COMPLETIONS).ok())
                            .unwrap_or_default()
                    }
                },
            ),
        );
        self.adopt(tab, how);
        if flavour.is_note() {
            self.sync_conflict_banner(key, None);
        }
    }

    /// A PDF, in the reader.
    fn open_pdf(self: &Rc<Self>, key: &str, path: &Path, how: Opened) {
        let place = self
            .vault()
            .and_then(|v| v.session().pdf.get(key).copied())
            .unwrap_or_default();
        let path = self.local_copy(key).unwrap_or_else(|| path.to_path_buf());
        let pdf = pdftab::open(
            &path,
            key,
            doc::file_name(key),
            &fileops::display_path(&self.root(), key),
            &self.tabs(),
            place,
        );
        pdf.connect_zoom(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |_| {
                app.refresh_zoom();
                app.save_session_soon();
            }
        ));
        pdf.connect_page(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |_| app.save_session_soon()
        ));
        pdf.connect_outline(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |_| app.sync_outline()
        ));
        pdf.connect_opened(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |_| {
                app.sync_opening();
                app.sync_outline();
            }
        ));
        pdf.connect_matches(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |pdf| app.find.set_matches_text(&pdf.matches_label())
        ));
        pdf.connect_uri(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |uri| {
                let launcher = gtk::UriLauncher::new(uri);
                launcher.launch(Some(&app.window), gio::Cancellable::NONE, |_| {});
            }
        ));
        let page = pdf.page.clone();
        self.mark_loose(&page, key);
        self.docs.borrow_mut().push(Doc::Pdf(pdf));
        self.tabs().set_selected_page(&page);
        self.mark_opened(&page, how);
        self.sync_active();
        self.save_session_soon();
    }

    /// The PDF in the active tab, for the actions that only mean something in one.
    fn active_pdf(&self) -> Option<Rc<pdftab::PdfTab>> {
        self.active_doc()?.pdf().cloned()
    }

    /// An image, in a tab that only looks at it.
    fn open_image(self: &Rc<Self>, key: &str, path: &Path, how: Opened) {
        // `fetch` is the file itself locally and a cached copy from the host remotely: a picture
        // widget needs real bytes, and the protocol deliberately carries none.
        let path = self.local_copy(key).unwrap_or_else(|| path.to_path_buf());
        let picture = gtk::Picture::for_filename(&path);
        picture.set_content_fit(gtk::ContentFit::ScaleDown);
        picture.set_can_shrink(true);
        let scroller = gtk::ScrolledWindow::builder()
            .hexpand(true)
            .vexpand(true)
            .child(&picture)
            .build();
        let image = self.adopt_viewer(Doc::Image, key, &scroller, "image-x-generic-symbolic", how);
        // On the scroller rather than the picture: while the image is fitted it is smaller than
        // the viewport, and a wheel over the empty space around it has to zoom too. Bubble
        // phase, ahead of the scroller's own controller, as everywhere else.
        let viewer = Rc::downgrade(&image);
        zoom_on_wheel(
            &scroller,
            gtk::PropagationPhase::Bubble,
            glib::clone!(
                #[weak(rename_to = app)]
                self,
                move |out| {
                    if let Some(image) = viewer.upgrade() {
                        app.zoom_image(&image, Some(out));
                    }
                }
            ),
        );
    }

    /// Where `key`'s bytes are on *this* machine, for the readers that cannot work with anything
    /// else: the PDF engine, an image, the preview's assets.
    ///
    /// ponytail: on a remote vault this downloads on the main thread, so a large PDF holds the
    /// window for as long as the transfer takes. Move it to a worker thread with the opening
    /// status the PDF tab already shows if that ever bites.
    fn local_copy(&self, key: &str) -> Option<PathBuf> {
        let vault = self.vault()?;
        match vault.fetch(key) {
            Ok(path) => Some(path),
            Err(e) => {
                tracing::warn!("fetching {key}: {e}");
                None
            }
        }
    }

    /// A file we decline to open, as a page saying why (DESIGN.md, States: a status page with one
    /// sentence and at most one button).
    fn open_status(self: &Rc<Self>, key: &str, title: &str, body: &str, how: Opened) {
        let key = key.to_string();
        let status = adw::StatusPage::builder()
            .icon_name("dialog-warning-symbolic")
            .title(title)
            .description(body)
            .build();
        let button = gtk::Button::builder()
            .label("Show in Files")
            .halign(gtk::Align::Center)
            .css_classes(["pill"])
            .build();
        button.connect_clicked(glib::clone!(
            #[weak(rename_to = app)]
            self,
            #[strong]
            key,
            move |_| {
                let path = app.root().join(&key);
                let toast = app.clone();
                fileops::reveal(&app.window, &path, move |m| toast.toast(m));
            }
        ));
        status.set_child(Some(&button));
        self.adopt_viewer(Doc::Status, &key, &status, "dialog-warning-symbolic", how);
    }

    /// Put a tab with no buffer into the window: the shared half of [`App::open_image`] and
    /// [`App::open_status`].
    /// A comparison as a tab. `key` says which comparison it is, so asking for the same one twice
    /// reveals the tab already showing it rather than stacking a second copy; `title` is what the
    /// tab is called, since the key is not a path and would not read as one.
    fn open_diff(self: &Rc<Self>, key: &str, title: &str, body: &impl IsA<gtk::Widget>) {
        if let Some(doc) = self.doc_for(key) {
            return self.reveal_page(doc.page());
        }
        let page = self.tabs().append(body);
        page.set_title(title);
        page.set_icon(Some(&gio::ThemedIcon::new("view-dual-symbolic")));
        self.docs
            .borrow_mut()
            .push(Doc::Diff(doc::Viewer::new(key, page.clone())));
        self.tabs().set_selected_page(&page);
        self.sync_active();
    }

    fn adopt_viewer(
        self: &Rc<Self>,
        wrap: fn(Rc<doc::Viewer>) -> Doc,
        key: &str,
        child: &impl IsA<gtk::Widget>,
        icon: &str,
        how: Opened,
    ) -> Rc<doc::Viewer> {
        let page = self.tabs().append(child);
        page.set_title(doc::file_name(key));
        page.set_tooltip(&fileops::display_path(&self.root(), key));
        page.set_icon(Some(&gio::ThemedIcon::new(icon)));
        self.mark_loose(&page, key);
        let viewer = doc::Viewer::new(key, page.clone());
        self.docs.borrow_mut().push(wrap(viewer.clone()));
        self.tabs().set_selected_page(&page);
        self.mark_opened(&page, how);
        self.sync_active();
        self.save_session_soon();
        viewer
    }

    /// Open a note with the caret on a byte offset, which is how a sidebar search result opens the
    /// exact match rather than the top of the note.
    ///
    /// Every row that leads here — a search hit, a tag, a backlink, an outline heading — is a
    /// single click in the sidebar, so the note opens as a preview.
    ///
    /// ponytail: the offset is turned into a character offset by counting the text in front of it,
    /// because `GtkTextBuffer` addresses characters. Fine for a note; a real byte-to-iter map
    /// belongs on `Tab` if anything ever needs it per keystroke.
    fn open_note_at(self: &Rc<Self>, rel: &str, offset: Option<usize>) {
        self.open_preview(rel);
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
    /// would come back as a changed-on-disk banner instead of a replacement. That part is the
    /// main loop's, and so is the reload afterwards; the rewrite between them is not. It is a
    /// read, a substitution and an fsync per note — 1.9 s across 245 notes and 35 s across 3.3k
    /// of them, measured on the generated vault — so it goes to a worker thread and `done` hands
    /// the sidebar back its pane when it lands.
    fn replace_in_notes(
        self: &Rc<Self>,
        query: String,
        options: accent_api::Options,
        replacement: String,
        literal: bool,
        done: Box<dyn FnOnce()>,
    ) {
        let Some(vault) = self.vault().cloned() else {
            done();
            return self.needs_vault("replace across notes");
        };
        let Some(ops) = self.ops().cloned() else {
            done();
            return;
        };
        let open: Vec<String> = self.open_tabs().iter().map(|tab| tab.rel()).collect();
        (ops.flush)(&open);
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let outcome = gio::spawn_blocking(move || {
                vault.replace_all(&query, options, &replacement, literal)
            })
            .await;
            if let Some(app) = weak.upgrade() {
                match outcome {
                    Ok(Ok(report)) => {
                        let unsaved = (ops.reload)(&report.rewritten);
                        app.toast(&replace_message(
                            report.matches,
                            report.rewritten.len(),
                            report.failed.len(),
                            unsaved,
                        ));
                    }
                    Ok(Err(e)) => app.toast(&format!("Cannot replace: {e:#}")),
                    Err(_) => tracing::warn!("the replace worker panicked"),
                }
            }
            done();
        });
    }

    /// A link target as written, resolved the way a wikilink resolves: by name, shortest path.
    fn open_target(self: &Rc<Self>, target: &str) {
        let Some(vault) = self.vault() else {
            return self.needs_vault("follow a link");
        };
        match vault.resolve_link(target) {
            Ok(Some(rel)) => self.open_preview(&rel),
            Ok(None) => self.toast(&format!("No note called {target}")),
            Err(e) => self.toast(&format!("Cannot resolve {target}: {e:#}")),
        }
    }

    /// Where `key` really is: the key to open it under, and the path to read.
    ///
    /// Wikilink targets come out of note content, so `![[../../../../etc/passwd]]` reaches
    /// `open_path` from the preview and has to be stopped here rather than by the reader. The
    /// check is the vault's own lexical one: canonicalising would refuse a note reached through
    /// one of the directory symlinks a vault links in on purpose, which the walk indexed and the
    /// tree is already showing.
    fn locate(&self, key: &str) -> Option<(String, PathBuf)> {
        if doc::is_loose_key(key) {
            let path = PathBuf::from(key);
            return path.is_file().then(|| (key.to_string(), path));
        }
        // A window with no vault has nothing to be relative to, so only absolute keys open.
        let vault = self.vault()?;
        let path = vault.resolve(key).ok()?;
        // Asked of the vault rather than of this machine: on a remote one the path is the host's
        // and `exists()` here would answer about a file that was never meant to be here.
        let key = path.strip_prefix(self.root()).ok()?.to_str()?.to_string();
        if !vault.exists(&key) {
            return None;
        }
        // Normalised, so `./a.md` and `a.md` are one tab rather than two.
        Some((key, path))
    }

    /// Open File…: anything, from anywhere. A file inside this vault opens as a vault tab; one
    /// from outside opens as a loose tab in this window, marked as being from outside it.
    fn open_file_dialog(self: &Rc<Self>) {
        let dialog = gtk::FileDialog::builder().title("Open File").build();
        if let Some(vault) = self.vault() {
            dialog.set_initial_folder(Some(&gio::File::for_path(vault.root())));
        }
        dialog.open(
            Some(&self.window),
            gio::Cancellable::NONE,
            glib::clone!(
                #[weak(rename_to = app)]
                self,
                move |result| {
                    // A dismissed chooser is an error here, and not one worth a toast.
                    let Some(path) = result.ok().and_then(|file| file.path()) else {
                        return;
                    };
                    let key = match app.vault().and_then(|v| path.strip_prefix(v.root()).ok()) {
                        Some(rel) => rel.to_string_lossy().into_owned(),
                        None => path.to_string_lossy().into_owned(),
                    };
                    app.open_path(&key);
                }
            ),
        );
    }

    /// A tab on a file from outside this window's vault says so on its own tab, so saving it is
    /// never a surprise and it is obvious why it has no backlinks.
    fn mark_loose(&self, page: &adw::TabPage, key: &str) {
        if self.vault.is_some() && doc::is_loose_key(key) {
            page.set_indicator_icon(Some(&gio::ThemedIcon::new("document-open-symbolic")));
            page.set_indicator_tooltip("Outside this vault");
        }
    }

    /// Wire a freshly opened tab into the window.
    fn adopt(self: &Rc<Self>, tab: Rc<Tab>, how: Opened) {
        // Ctrl+scroll zooms the document, as it zooms a PDF page, through the same step and the
        // same readout. On the view rather than on the window: a window-level controller would
        // have to work out which tab the pointer is over and would race the PDF's own, while this
        // one only ever sees a text tab. Bubble phase, ahead of the scrolled window's controller,
        // which is the order `pdfview` relies on for the same reason.
        zoom_on_wheel(
            &tab.view,
            gtk::PropagationPhase::Bubble,
            glib::clone!(
                #[weak(rename_to = app)]
                self,
                move |out| app.set_zoom(stepped_zoom(app.zoom.get(), out))
            ),
        );

        tab.connect_autosave(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |tab| app.save_tab(tab, false)
        ));
        tab.connect_edited(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |tab| {
                app.queue_render(tab);
                if app.is_active(tab) {
                    app.sync_outline();
                }
            }
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
            #[weak]
            tab,
            move |_| app.on_edit(&tab)
        ));

        // Nothing else watches a loose file: the vault's worker only reports on its own tree.
        if doc::is_loose_key(&tab.rel()) {
            tab.watch_file(glib::clone!(
                #[weak(rename_to = app)]
                self,
                move |tab| app.file_changed(tab)
            ));
        }
        self.mark_loose(&tab.page, &tab.rel());
        let page = tab.page.clone();
        self.fetch_head(&tab);
        self.docs.borrow_mut().push(Doc::Text(tab));
        self.tabs().set_selected_page(&page);
        self.mark_opened(&page, how);
        self.sync_active();
        self.save_session_soon();
    }

    /// Keep the window subtitle, the backlinks pane and the preview in step with the active tab.
    fn sync_active(self: &Rc<Self>) {
        self.find.retarget(self.active());
        let Some(doc) = self.active_doc() else {
            self.title.set_subtitle(&self.host());
            if let Some(sidebar) = self.sidebar.get() {
                sidebar.set_backlinks(&[]);
            }
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
        // Backlinks and the preview are about notes. A source file, an image or a status page
        // leaves both empty rather than showing the last note's.
        let note = doc.tab().filter(|t| t.flavour().is_note()).cloned();
        // Off the main loop: one round trip on a remote vault is ~60 ms here, and this runs on
        // every tab switch. The pane is emptied at once so it never shows the last note's
        // backlinks while the new note's are still coming.
        if let Some(sidebar) = self.sidebar.get() {
            sidebar.set_backlinks(&[]);
        }
        if let Some(vault) = self.vault().filter(|_| note.is_some()).cloned() {
            let (key, weak) = (key.clone(), Rc::downgrade(self));
            glib::spawn_future_local(async move {
                let found = gio::spawn_blocking({
                    let key = key.clone();
                    move || vault.backlinks(&key).unwrap_or_default()
                })
                .await;
                let Some(app) = weak.upgrade() else { return };
                // The user may have moved on while we were asking; a stale answer must not
                // replace the pane the current tab put there.
                if app.active_key().as_deref() != Some(&key) {
                    return;
                }
                let mut sources: Vec<String> = Vec::new();
                for link in found.unwrap_or_default() {
                    if !sources.contains(&link.src_rel_path) {
                        sources.push(link.src_rel_path);
                    }
                }
                if let Some(sidebar) = app.sidebar.get() {
                    sidebar.set_backlinks(&sources);
                }
            });
        }
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
        if !tab.flavour().is_note() {
            // ponytail: an outline of code is a symbol list, which is the language server's job.
            return sidebar.set_outline(Some(&sidebar::outline_note(
                "No Outline",
                "Symbols arrive with language server support.",
            )));
        }
        let headings: Vec<(u8, String, usize)> = tab
            .headings()
            .into_iter()
            .map(|h| (h.level, h.text, h.range.start))
            .collect();
        if headings.is_empty() {
            return sidebar.set_outline(Some(&sidebar::outline_note(
                "No Headings",
                "This note has no headings yet.",
            )));
        }
        let key = doc.key();
        sidebar.set_outline(Some(&sidebar::outline_list(
            &headings,
            glib::clone!(
                #[weak(rename_to = app)]
                self,
                move |at| app.open_note_at(&key, Some(at))
            ),
        )));
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

    /// The unsaved buffer against the file underneath it, in the conflict resolver.
    fn compare_with_disk(self: &Rc<Self>, tab: &Rc<Tab>) {
        let rel = tab.rel();
        let read = match self.vault().filter(|_| !doc::is_loose_key(&rel)) {
            Some(vault) => vault.read(&rel).map(|(text, _)| text),
            None => accent_core::fs::read_note(&tab.path()).map(|(text, _)| text),
        };
        let Ok(disk) = read else {
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
                    match app.write_tab(&tab, None) {
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
        let key = format!("conflict:disk:{rel}");
        let body = diff::conflict(
            (&format!("{rel} (unsaved)"), &mine),
            (&format!("{rel} (on disk)"), &disk),
            glib::clone!(
                #[weak(rename_to = app)]
                self,
                #[strong]
                key,
                move |choice| {
                    resolve(choice);
                    app.close_diff(&key);
                }
            ),
        );
        self.open_diff(
            &key,
            &format!("{} (Changed on Disk)", doc::file_name(&rel)),
            &body,
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
        ) && let Some(git) = self.git.get()
        {
            git.schedule_refresh();
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

    fn resolve_conflict(self: &Rc<Self>, original: &str, conflict: &str) {
        let Some(vault) = self.vault() else {
            return;
        };
        let (Ok((mine, mine_etag)), Ok((theirs, theirs_etag))) =
            (vault.read(original), vault.read(conflict))
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
                        let Some(vault) = app.vault() else { return };
                        if let Err(e) = vault.adopt_conflict(&original, &conflict) {
                            return app.toast(&format!("Cannot resolve: {e:#}"));
                        }
                        true
                    }
                    diff::Choice::KeepMine { edited: Some(text) } => {
                        let Some(vault) = app.vault() else { return };
                        // Gated on the version the resolver was built from, not forced. This is
                        // a tab and not a modal: it can sit open while the note is typed into
                        // and autosaved, and a merge decided against an older Mine must not
                        // undo what has been written since.
                        if let Err(e) = vault.save(&original, &text, Some(mine_etag)) {
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
                if let Some(ops) = app.ops() {
                    fileops::trash(ops, &conflict);
                }
                // The copy is named here because the index has not seen it go yet.
                app.sync_conflict_banner(&original, Some(&conflict));
            }
        };
        let key = format!("conflict:sync:{original}");
        let body = diff::conflict(
            (&written_at(original, &mine_etag), &mine),
            (&written_at(conflict, &theirs_etag), &theirs),
            glib::clone!(
                #[weak(rename_to = app)]
                self,
                #[strong]
                key,
                move |choice| {
                    resolve(choice);
                    app.close_diff(&key);
                }
            ),
        );
        self.open_diff(
            &key,
            &format!("{} (Sync Conflict)", doc::file_name(original)),
            &body,
        );
    }

    /// Close a diff tab once its question has been answered.
    fn close_diff(self: &Rc<Self>, key: &str) {
        if let Some(doc) = self.doc_for(key) {
            self.close_page(doc.page());
        }
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
            move |label| app.find.set_matches_text(label)
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
        // A PDF gets first refusal: it is what the user is looking at, and it counts its own
        // matches rather than letting the bar count them.
        if let Some(pdf) = self.active_pdf() {
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
            || self.find.is_open()
            || self.active().is_some_and(|tab| tab.banner.is_revealed())
    }

    // --- actions -------------------------------------------------------------------------

    fn run_action(self: &Rc<Self>, name: &str) {
        match name {
            "save" => self.save_active(),
            "open-file" => self.open_file_dialog(),
            "new-note" => {
                let Some(vault) = self.vault() else {
                    return self.needs_vault("create a note");
                };
                let dir = self
                    .selected_dir()
                    .unwrap_or_else(|| vault.config().new_note_dir);
                if let Some(ops) = self.ops() {
                    fileops::new_note(ops, &dir);
                }
            }
            "new-folder" => {
                if let Some(ops) = self.need_ops("create a folder") {
                    fileops::new_folder(ops, &self.selected_dir().unwrap_or_default())
                }
            }
            // The one way to upload into the vault root: the tree has no row for it, so the
            // folder's own context menu cannot offer it and this reads the selection the way
            // New Folder does.
            "upload" => {
                if let Some(ops) = self.need_ops("upload files") {
                    match ops.vault.is_remote() {
                        true => fileops::upload(ops, &self.selected_dir().unwrap_or_default()),
                        // Listed for every vault, because the palette shows all of ACTIONS, so
                        // the local one says why nothing opened rather than doing nothing.
                        false => self.toast("This vault is already on this machine"),
                    }
                }
            }
            "terminal" => self.open_terminal(),
            // Nothing to do over any other tab: the editor and the PDF have their own copy, and a
            // paste into a document is GtkTextView's.
            "terminal-copy" => {
                if let Some(Doc::Terminal(term)) = self.active_doc() {
                    term.copy();
                }
            }
            "terminal-paste" => {
                if let Some(Doc::Terminal(term)) = self.active_doc() {
                    term.paste();
                }
            }
            "close-tab" => {
                if let Some(page) = self.tabs().selected_page() {
                    self.tabs().close_page(&page);
                }
            }
            "next-tab" => self.cycle_tab(true),
            "previous-tab" => self.cycle_tab(false),
            "split-left" => self.split_active(Side::Left),
            "split-right" => self.split_active(Side::Right),
            "split-up" => self.split_active(Side::Up),
            "split-down" => self.split_active(Side::Down),
            "palette-files" => self.palette(palette::Mode::Files),
            "palette-commands" => self.palette(palette::Mode::Commands),
            "open-recent" => self.palette(palette::Mode::Vaults),
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
            "toggle-comment" => {
                if let Some(tab) = self.active() {
                    tab.toggle_comment();
                }
            }
            "toggle-wrap" => {
                if let Some(tab) = self.active() {
                    tab.toggle_wrap();
                }
            }
            "delete-line" => {
                if let Some(tab) = self.active() {
                    tab.delete_line();
                }
            }
            "newline-below" => {
                // `Ctrl+Return` belongs to the git commit box while the keyboard is in it
                // (DESIGN.md, Git pane). A window accelerator is dispatched ahead of any
                // controller on the focused widget, so the box cannot claim the chord itself.
                let committed = self.git.get().is_some_and(|git| git.commit_if_focused());
                if !committed && let Some(tab) = self.active() {
                    tab.newline_below();
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
            "zoom-in" | "zoom-out" | "zoom-reset" => self.zoom_action(name),
            "pdf-back" => {
                if let Some(pdf) = self.active_pdf() {
                    pdf.back();
                }
            }
            "pdf-forward" => {
                if let Some(pdf) = self.active_pdf() {
                    pdf.forward();
                }
            }
            "pdf-invert" => {
                if let Some(pdf) = self.active_pdf() {
                    pdf.toggle_invert();
                }
            }
            "pdf-fit-width" | "pdf-fit-page" => {
                if let Some(pdf) = self.active_pdf() {
                    pdf.set_zoom(match name {
                        "pdf-fit-page" => PdfZoom::FitPage,
                        _ => PdfZoom::FitWidth,
                    });
                }
            }
            "minimap" => self.toggle_minimap(),
            "copy-relative-path" => {
                if let (Some(rel), Some(ops)) =
                    (self.menu_rel(), self.need_ops("copy a vault path"))
                {
                    fileops::copy_relative_path(ops, &rel);
                }
            }
            "copy-absolute-path" => {
                if let Some(rel) = self.menu_rel() {
                    match self.ops() {
                        Some(ops) => fileops::copy_absolute_path(ops, &rel),
                        // No vault, so the key already is the absolute path.
                        None => self.window.clipboard().set_text(&rel),
                    }
                }
            }
            "show-in-files" => {
                if let Some(rel) = self.menu_rel() {
                    let path = self.root().join(&rel);
                    let toast = self.clone();
                    fileops::reveal(&self.window, &path, move |m| toast.toast(m));
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
            "search-all" => {
                self.sidebar_column.set_visible(true);
                if let Some(sidebar) = self.sidebar.get() {
                    sidebar.toggle_search_all();
                }
            }
            "pane-tags" => self.show_pane("tags"),
            "pane-git" => {
                self.show_pane("git");
                // The chord is how the keyboard reaches the commit box; the pane on its own
                // leaves the caret in the note.
                if let Some(git) = self.git.get() {
                    git.focus_commit();
                }
            }
            // The pane's own button and the status bar's branch are this one action, so whichever
            // is pressed, the repository synced is the one the active document sits in and the
            // pane's selection ends up on it.
            "git-sync" => {
                if let Some(git) = self.git.get() {
                    let key = self
                        .active_doc()
                        .filter(|d| !d.is_transient())
                        .map(|d| d.key());
                    git.sync(key.as_deref());
                }
            }
            "pane-outline" => self.show_pane("outline"),
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
                if let (Some(rel), Some(ops)) = (target, self.need_ops("rename a file")) {
                    fileops::rename(ops, &rel);
                }
            }
            "daily-note" => match self.vault().map(|v| v.daily_note()) {
                Some(Ok((rel, _))) => self.open_note(&rel),
                Some(Err(e)) => self.toast(&format!("Cannot open today's note: {e:#}")),
                None => self.needs_vault("open today's note"),
            },
            "present" => self.set_presenting(self.presenting.get().is_none()),
            "fullscreen" => self.window.set_fullscreened(!self.window.is_fullscreen()),
            "preferences" => self.preferences(),
            "menu" => self.menu.popup(),
            "about" => self.about(),
            _ => tracing::warn!("no handler for action {name}"),
        }
    }

    /// One of the three zoom chords, dispatched to whatever the active tab is.
    ///
    /// A PDF fits its pages, an image is given a size of its own, a shell scales its own font and
    /// a document scales the display-wide one; a status page and a diff draw at a size nobody
    /// chose, so the chords do nothing there. It is matched in the same shape as
    /// [`App::sync_status`] and [`App::refresh_zoom`] on purpose: what the chords reach and what
    /// the readout says have to be the same list, or the bar says 120 % over something drawn at
    /// its own size.
    fn zoom_action(self: &Rc<Self>, name: &str) {
        // Reset is 100 % for anything counted in percentages, and Fit Width for a PDF, which is
        // what a page was fitted to before anyone zoomed it.
        let stepped = |from: f64| match name {
            "zoom-in" => stepped_zoom(from, false),
            "zoom-out" => stepped_zoom(from, true),
            _ => 1.0,
        };
        match self.active_doc() {
            Some(Doc::Pdf(pdf)) => match name {
                "zoom-in" => pdf.zoom_step(false),
                "zoom-out" => pdf.zoom_step(true),
                _ => pdf.set_zoom(PdfZoom::FitWidth),
            },
            Some(Doc::Terminal(term)) => {
                term.set_zoom(stepped(term.zoom()));
                self.refresh_zoom();
            }
            Some(Doc::Text(_)) => self.set_zoom(stepped(self.zoom.get())),
            Some(Doc::Image(image)) => self.zoom_image(
                &image,
                match name {
                    "zoom-in" => Some(false),
                    "zoom-out" => Some(true),
                    _ => None,
                },
            ),
            Some(Doc::Status(_)) | Some(Doc::Diff(_)) | None => {}
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
    /// The file's own facts in the status bar: what it is, and for a note how long it is.
    fn sync_status(&self) {
        let (kind, facts) = match self.active_doc() {
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
            Some(Doc::Pdf(_)) => (Some("PDF".to_string()), None),
            Some(Doc::Image(_)) => (Some("Image".to_string()), None),
            Some(Doc::Terminal(_)) => (Some("Terminal".to_string()), None),
            Some(Doc::Status(_)) | Some(Doc::Diff(_)) | None => (None, None),
        };
        self.statusbar.set_kind(kind.as_deref());
        self.statusbar.set_facts(facts.as_deref());
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
        // The session ended on Git and the page has only just appeared. Taken whatever it says,
        // so a later refresh cannot pull the user back to a pane they have since left.
        if self.pane_wanted.take() == "git" {
            self.show_pane("git");
        }
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
            Some(Doc::Text(_)) => {
                let zoom = self.zoom.get();
                (zoom != 1.0).then(|| format!("{} %", (zoom * 100.0).round() as i32))
            }
            Some(Doc::Image(image)) => Some(image_zoom_label(&image)),
            Some(Doc::Status(_)) | Some(Doc::Diff(_)) | None => None,
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

    /// Push the accelerators in force into the application and rebuild the four captured chords.
    /// Done wholesale: forty `set_accels_for_action` calls are cheaper than working out which of
    /// them a config change touched.
    ///
    /// A focused shell narrows the table to [`reserved`], because an application accelerator is
    /// dispatched at the window ahead of the VTE and unbinding it is the only thing that lets the
    /// key reach the shell. The filter reads the accelerators in force, so a rebound chord follows
    /// the same rule as the default it replaced.
    fn apply_accels(&self) {
        let Some(gtk_app) = self.window.application() else {
            return;
        };
        let config = self.config.borrow();
        let shell = terminal::has_focus(&self.window);
        for (action, _, _) in ACTIONS {
            let accels = accels_for(&config, action);
            let accels: Vec<&str> = accels
                .iter()
                .map(String::as_str)
                .filter(|accel| !shell || reserved(action, accel))
                .collect();
            gtk_app.set_accels_for_action(action, &accels);
        }
        let captured: Vec<(&str, String)> = CAPTURED
            .iter()
            .flat_map(|action| {
                accels_for(&config, action)
                    .into_iter()
                    .map(move |accel| (*action, accel))
            })
            .collect();
        fill_captured(&self.captured, &captured);
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
        if let Some(vault) = self.vault() {
            vault.set_config(config.vault(&self.root()));
        }
        // Switching to or away from Solarized does not change the system's dark state, so the
        // notify handler that usually restyles never fires here.
        theme::apply(config.theme);
        self.apply_accels();
        for tab in self.open_tabs() {
            tab.set_font(config.editor_font.as_deref(), self.zoom.get());
            tab.set_spellcheck(config.spellcheck);
            tab.set_minimap(config.minimap);
            tab.set_line_numbers(config.line_numbers);
            tab.set_column_width(config.column_width);
            tab.restyle();
        }
        // A PDF is rendered in the theme's colours, so Solarized to Adwaita is a re-render even
        // though the system's dark state, and with it the notify handler, never moved.
        for doc in self.docs() {
            if let Some(pdf) = doc.pdf() {
                pdf.restyle();
            }
        }
        if let Some(preview) = self.preview.borrow().as_ref() {
            preview.restyle();
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
            pane: self
                .sidebar
                .get()
                .map(|s| s.pane())
                .unwrap_or_else(|| Session::default().pane),
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
        // A state file written before panes were saved leaves the name empty; that keeps
        // whichever pane the sidebar was built showing.
        //
        // Remembered as well as shown: the Git page is still hidden here, so asking for it is a
        // no-op until the first refresh finds a repository (`on_git_changed`).
        self.pane_wanted.replace(session.pane.clone());
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

/// Which editor a text file gets. Only CSV is special: its columns are coloured instead of it
/// being handed to a language, because `csv.lang` would tint numbers and strings underneath.
fn flavour_of(key: &str) -> Flavour {
    match doc::file_name(key).rsplit_once('.') {
        Some((_, ext)) if ext.eq_ignore_ascii_case("csv") => Flavour::Csv,
        _ => Flavour::Code,
    }
}

/// A byte count as a person reads it, in the decimal units GNOME shows in Files.
fn human(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["bytes", "kB", "MB", "GB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1000.0 && unit + 1 < UNITS.len() {
        size /= 1000.0;
        unit += 1;
    }
    match unit {
        0 => format!("{bytes} bytes"),
        _ => format!("{size:.1} {}", UNITS[unit]),
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

// ------------------------------------------------------------------------------ construction

fn build_window(
    gtk_app: &adw::Application,
    shell: &Rc<Shell>,
    root: Option<PathBuf>,
    note: Option<String>,
) -> Option<Rc<App>> {
    install_document_font();
    install_icons();
    install_chrome_css();
    theme::apply(shell.config.borrow().theme);

    // No root is a window opened on a file: no index to build, no watcher to run, and nothing
    // to add to the recent-vaults list. A root that is an `ssh://` address is a vault on another
    // machine — it opens the same way and returns just as fast, because the connection is made on
    // a thread and reports itself through the events like the indexing does.
    let (vault, events) = match &root {
        Some(root) => {
            let vault_config = shell.config.borrow().vault(root);
            let opened = match ssh::is_remote_path(root) {
                true => Vault::open_remote(&root.to_string_lossy(), vault_config),
                false => Vault::open(root, vault_config),
            };
            match opened {
                Ok((vault, events)) => (Some(Arc::new(vault)), Some(events)),
                Err(e) => {
                    eprintln!("cannot open {}: {e:#}", root.display());
                    return None;
                }
            }
        }
        None => (None, None),
    };
    // Touched now, so the window title and any picker opened in this window read the list the
    // way it will be written; the write itself waits for the post-present idle below, an fsync
    // being no part of building a widget tree.
    if let Some(root) = &root {
        shell.config.borrow_mut().touch_recent(root);
    }

    let vault_name = match &root {
        Some(root) => root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| root.display().to_string()),
        None => "Accent".to_string(),
    };
    // A remote window says which machine it is on, under the vault's name. Nothing else in the
    // chrome differs: it is the same vault, and the point is that it behaves like one.
    let host = root
        .as_deref()
        .and_then(|r| ssh::parse(&r.to_string_lossy()).ok())
        .map(|url| url.host)
        .unwrap_or_default();
    let title = adw::WindowTitle::new(&vault_name, &host);
    let first = Pane::new(&tab_menu());
    let toasts = adw::ToastOverlay::new();
    // Hidden until something goes wrong with a connection, which for a local vault is never.
    let connection = adw::Banner::builder().button_label("Reconnect").build();
    // Hidden until the first `Progress`, so a warm start that never reports one never shows it.
    // Going visible costs the content 4 px once, at the moment indexing ends; a `GtkRevealer`
    // would slide it away instead if that ever reads as a jump.
    let statusbar = statusbar::Bar::new();

    // An empty vault window should say so rather than showing a blank rectangle.
    let placeholder = adw::StatusPage::builder()
        .icon_name("text-x-generic-symbolic")
        .title("No Note Open")
        .description("Pick one in the sidebar, or press Ctrl+E to go to a file.")
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

    toasts.set_child(Some(&paned));

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
    // A first connection to a remote host is the window becoming usable, not a list being
    // replaced, so its bar spans the document column rather than sitting in the status bar
    // beside the text (DESIGN.md, Loading). The text stays in the status bar either way. Only a
    // remote vault puts the widget in the layout: a local one is connected from the moment it
    // opens, so there is nothing to draw and no height to reserve.
    let connect = connect::Bar::new();
    let editor_column = gtk::Box::new(gtk::Orientation::Vertical, 0);
    if vault.as_ref().is_some_and(|v| v.is_remote()) {
        editor_column.append(connect.widget());
    }
    editor_column.append(find.widget());
    editor_column.append(&toasts);

    // Only the header is a top bar now: the tab bars belong to the panes, so they sit inside
    // `content` and presentation mode takes them away with it rather than unrevealing them.
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.add_top_bar(&connection);
    // A bottom bar rather than a row inside the content: presentation mode takes it away with the
    // header for one line, and the find bar and the terminal panel stack above it.
    toolbar.add_bottom_bar(statusbar.widget());
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
        // (`vault` is an `Option` here: `None` is a window opened on a file, with no folder.)
        shell: Rc::downgrade(shell),
        config: shell.config.clone(),
        window: window.clone(),
        panes: RefCell::new(vec![first.clone()]),
        active_pane: RefCell::new(first.clone()),
        title,
        toasts,
        connection,
        connect,
        corpus: RefCell::new(Corpus::default()),
        find,
        statusbar,
        docs: RefCell::new(Vec::new()),
        tree: OnceCell::new(),
        sidebar: OnceCell::new(),
        git: OnceCell::new(),
        pane_wanted: RefCell::new(String::new()),
        ops: OnceCell::new(),
        preview: RefCell::new(None),
        terminals: Cell::new(0),
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
        shell_keys: Cell::new(false),
    });
    if let Some(vault) = &vault {
        let _ = app.ops.set(build_ops(&app, vault));
    }

    // The sidebar is the vault: a tree, a search over the index, the tags in it, the backlinks
    // between its notes. A window without one is tabs and nothing else.
    match &vault {
        Some(vault) => {
            // Populate straight from the index: the window must be up before reconcile finishes.
            let rows = gio::ListStore::new::<gtk::StringObject>();
            tree::fill(&rows, vault, "");
            tracing::debug!(t_ms = ms(), rows = rows.n_items(), "tree populated");
            build_sidebar(&app, &rows, vault);
        }
        // Outline only, and collapsed: a window opened on one file is that file, and the
        // sidebar is there for when it is asked for with F9 or Ctrl+Shift+L.
        None => {
            build_outline_sidebar(&app);
            app.sidebar_column.set_visible(false);
        }
    }

    wire_pane(&app, &first);

    install_actions(&app);
    wire_window(&app, &modes);
    if vault.is_some() {
        wire_tree(&app);
    }

    window.connect_map(|_| tracing::debug!(t_ms = ms(), "window mapped"));
    window.present();
    tracing::debug!(t_ms = ms(), "window presented");

    glib::idle_add_local_once(glib::clone!(
        #[weak]
        app,
        move || {
            // The recent list, written once the window the user asked for is on screen.
            if app.vault().is_some()
                && let Err(e) = app.config.borrow().save()
            {
                tracing::warn!("saving config: {e:#}");
            }
            app.restore_session();
            if let Some(rel) = note {
                app.open_path(&rel);
            }
        }
    ));
    install_bench_hooks(&app);
    if let Some(events) = events {
        start_events(&app, events);
    }
    Some(app)
}

/// Files / Search / Tags / Backlinks over the vault tree.
/// The sidebar for a window with a vault: the tree, the index panes and the outline.
fn build_sidebar(app: &Rc<App>, rows: &gio::ListStore, vault: &Arc<Vault>) {
    let tree = tree::build(
        vault.clone(),
        rows,
        // The row's kind used to decide what opened. `open_path` reads the name itself, so the
        // tree no longer has to agree with it about what a file is. A row opens as a preview:
        // one click is looking, not keeping.
        glib::clone!(
            #[weak]
            app,
            move |_kind, rel: &str| app.open_preview(rel)
        ),
        // A drag out of the tree is the only notice the panes get that their drop zones should
        // go up; a tab drag announces itself through `AdwTabView:is-transferring-page`.
        glib::clone!(
            #[weak]
            app,
            move |on| app.set_drop_active(on)
        ),
    );
    // The tree owns its scroller now, wrapped in a box the context menu can parent itself to.
    let files = tree.widget().clone();
    let _ = app.tree.set(tree);

    let data = sidebar::Data {
        // The one closure the sidebar calls off the main loop, which is why the vault is an `Arc`.
        search: Arc::new({
            let vault = vault.clone();
            move |query| match query {
                sidebar::Query::Fts(text, all) => {
                    sidebar::Answer::Fts(vault.search(&text, SEARCH_LIMIT, all).unwrap_or_default())
                }
                sidebar::Query::Grep { text, options, all } => {
                    // `total` is what Replace All would rewrite, not how many rows there are:
                    // the walked trees below add rows and nothing to it, and neither does a
                    // source file the index holds a body for. The button promises edits.
                    let (mut hits, total) = vault
                        .grep(&text, options, SEARCH_LIMIT, all)
                        .unwrap_or_default();
                    // What the index holds first, because that is what it can count; with
                    // All on, the trees it was never asked to hold get whatever room is left.
                    if all {
                        let room = SEARCH_LIMIT.saturating_sub(hits.len());
                        hits.extend(
                            vault
                                .grep_unindexed(&text, options, room)
                                .unwrap_or_default(),
                        );
                    }
                    sidebar::Answer::Grep(hits, total)
                }
            }
        }),
        // Port forwarding is ssh's, over the master that is already open: nothing is spawned and
        // nothing is kept but the list the pane shows.
        add_forward: Box::new({
            let vault = vault.clone();
            move |local, remote| match vault.remote() {
                Some(r) => r.forward(local, remote),
                None => Err("this vault is not remote".to_string()),
            }
        }),
        remove_forward: Box::new({
            let vault = vault.clone();
            move |local, remote| {
                if let Some(r) = vault.remote()
                    && let Err(e) = r.cancel_forward(local, remote)
                {
                    tracing::warn!("cancelling the forward {local} -> {remote}: {e}");
                }
            }
        }),
        replace_all: Box::new(glib::clone!(
            #[weak]
            app,
            move |query: String,
                  options: accent_api::Options,
                  replacement: String,
                  literal: bool,
                  done: Box<dyn FnOnce()>| {
                app.replace_in_notes(query, options, replacement, literal, done)
            }
        )),
        tags: Box::new({
            let vault = vault.clone();
            move || vault.tags().unwrap_or_default()
        }),
        files_with_tag: Box::new({
            let vault = vault.clone();
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
    let git = build_git(app, vault);
    adopt_sidebar(
        app,
        Some((files, data, git.widget().clone(), git.divider().clone())),
    );
    let _ = app.git.set(git);
    if let Some(git) = app.git.get() {
        git.schedule_refresh();
    }
}

/// The Git pane. Every hook holds the window weakly: the pane lives in the sidebar, which the
/// window owns, so a strong capture here is a cycle that keeps a closed window's vault open.
fn build_git(app: &Rc<App>, vault: &Arc<Vault>) -> Rc<git::Panel> {
    let (toast, open, diff, trash, changed, syncing) = (
        Rc::downgrade(app),
        Rc::downgrade(app),
        Rc::downgrade(app),
        Rc::downgrade(app),
        Rc::downgrade(app),
        Rc::downgrade(app),
    );
    git::Panel::new(git::Hooks {
        vault: vault.clone(),
        window: app.window.clone(),
        toast: Box::new(move |text| {
            if let Some(app) = toast.upgrade() {
                app.toast(text);
            }
        }),
        open: Box::new(move |key| {
            if let Some(app) = open.upgrade() {
                // A single click, the same as a tree row, so the same preview tab.
                app.open_preview(key);
            }
        }),
        open_diff: Box::new(move |key, title, body| {
            if let Some(app) = diff.upgrade() {
                app.open_diff(key, title, body);
            }
        }),
        trash: Box::new(move |key| {
            if let Some(ops) = trash.upgrade().and_then(|app| app.ops().cloned()) {
                fileops::trash(&ops, key);
            }
        }),
        changed: Box::new(move || {
            if let Some(app) = changed.upgrade() {
                app.on_git_changed();
            }
        }),
        syncing: Box::new(move |on| {
            if let Some(app) = syncing.upgrade() {
                app.statusbar.set_syncing(on);
            }
        }),
    })
}

/// A sidebar with the Outline pane alone, for a window opened on a file rather than a folder.
/// There is no index behind it, so Files, Search, Tags and Backlinks have nothing to show; an
/// outline does not need one, and a PDF's bookmarks are the reason such a window has a sidebar.
fn build_outline_sidebar(app: &Rc<App>) {
    adopt_sidebar(app, None);
}

fn adopt_sidebar(
    app: &Rc<App>,
    vault: Option<(gtk::Widget, sidebar::Data, gtk::Widget, gtk::Paned)>,
) {
    let pane = sidebar::Sidebar::new(
        vault,
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
    // A remote vault is the only one with ports to forward, and that is settled when the window
    // is built rather than discovered later, so unlike the Git pane this needs no refresh to say.
    pane.set_ports_visible(app.vault().is_some_and(|v| v.is_remote()));
    app.sidebar_header.set_title_widget(Some(pane.switcher()));
    // The panes dim rather than hide while the user types, on the same transition as the bars.
    pane.widget().add_css_class("chrome-fade");
    app.sidebar_column.set_content(Some(pane.widget()));
    let _ = app.sidebar.set(pane);
}

/// Everything `fileops` needs from the window, as closures. Weak throughout: the operations
/// outlive nothing, and a strong capture here would keep a closed window's vault open.
/// The file operations the tree, the tab menus and the palette share. `None` without a vault:
/// creating, renaming and trashing are all things done to a vault, not to a lone open file.
fn build_ops(app: &Rc<App>, vault: &Arc<Vault>) -> Rc<fileops::Ops> {
    let toast = Rc::downgrade(app);
    let open = Rc::downgrade(app);
    let split = Rc::downgrade(app);
    let flush = Rc::downgrade(app);
    let reload = Rc::downgrade(app);
    let close = Rc::downgrade(app);
    let reconciled = Rc::downgrade(app);
    Rc::new(fileops::Ops {
        vault: vault.clone(),
        window: app.window.clone(),
        toast: Box::new(move |message| {
            if let Some(app) = toast.upgrade() {
                app.toast(message);
            }
        }),
        open: Box::new(move |rel| {
            if let Some(app) = open.upgrade() {
                app.open_path(rel);
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
                && let Err(e) = app.write_tab(&tab, tab.etag.get())
            {
                let (tabs, page) = (tabs.clone(), page.clone());
                app.ask_unsaved(&tab, &e, move |app, close| {
                    if close {
                        // Only once the answer is in: a cancelled close must not have moved the
                        // selection off the tab it kept.
                        app.select_survivor(&page);
                        app.forget_page(&page);
                    }
                    tabs.close_page_finish(&page, close);
                });
                return glib::Propagation::Stop;
            }
            app.select_survivor(page);
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
        move |tabs| {
            if let Some(page) = tabs.selected_page() {
                pane.touch(&page);
            }
            app.set_active_pane(&pane);
            app.sync_active();
            app.save_session_soon();
        }
    ));
    // Double-clicking a tab is what makes a preview tab a real one, which is VS Code's gesture
    // for it. A pane holding one tab hides its bar, so there is nothing to double-click there —
    // and nothing to protect either, since a preview tab is only ever replaced by the next one.
    pane.on_tab_double_click(glib::clone!(
        #[weak]
        app,
        move |page| app.promote(page)
    ));
    // The placeholder is a property of the window, not of one pane: it shows only when no pane
    // has anything left to show, which with panes that close themselves means the last one.
    pane.tabs.connect_n_pages_notify(glib::clone!(
        #[weak]
        app,
        move |_| app.sync_panes()
    ));
    // A tab let go outside every tab bar. libadwaita reads that as "detach into a window of its
    // own" and this is the only public way to give a dragged page a view again, so a drop on one
    // of our pane zones — which `dropped` declines for exactly this reason — arrives here too.
    pane.tabs.connect_create_window(glib::clone!(
        #[weak]
        app,
        #[upgrade_or]
        None,
        move |view| {
            let aimed = app.shell.upgrade().and_then(|shell| shell.where_to_land());
            // Let go on nothing of ours: back where it came from. A window per detached tab is a
            // gesture one-window-per-vault has no answer for, and `None` is an error here.
            Some(aimed.unwrap_or_else(|| view.clone()))
        }
    ));
    // Where a drag ends: the split the drop asked for, and the move into this window's
    // bookkeeping when the page came out of another one — libadwaita's tab bars take a foreign
    // page natively, so a note of vault A would otherwise land under window B's `docs`, session
    // and backlinks.
    pane.tabs.connect_page_attached(glib::clone!(
        #[weak]
        app,
        #[weak]
        pane,
        move |_, page, _| {
            if let Some(shell) = app.shell.upgrade() {
                shell.landed(&app, &pane, page);
            }
        }
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
    // A tree row let go on the bar itself opens in that pane, which is the shortest way to say
    // "over there" and the one libadwaita already draws an insertion point for.
    pane.bar
        .setup_extra_drop_target(gdk::DragAction::COPY, &[String::static_type()]);
    pane.bar.connect_extra_drag_drop(glib::clone!(
        #[weak]
        app,
        #[weak]
        pane,
        #[upgrade_or]
        false,
        move |_, _, value| {
            let Ok(rel) = value.get::<String>() else {
                return false;
            };
            app.set_active_pane(&pane);
            app.open_path(&rel);
            true
        }
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
            // Belt and braces: the drag is over whatever the source has to say about it, and a
            // sheet left up would swallow every click meant for the editor under it.
            app.set_drop_active(false);
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
            // A PDF answers find and go-to itself, whether or not anything is being presented.
            move || app.presenting.get().is_some() || app.active_pdf().is_some()
        )),
        preview: Box::new(glib::clone!(
            #[weak]
            app,
            move |op| app.preview_find(op)
        )),
        pages: Box::new(glib::clone!(
            #[weak]
            app,
            #[upgrade_or]
            None,
            move || app.active_pdf().map(|pdf| pdf.page_count())
        )),
    });

    // The bottom bar of an `AdwToolbarView` is a `GtkWindowHandle`, so a secondary press anywhere
    // in it asks the shell for the window menu — Restore / Minimize / Maximize / Close under a
    // footer that is one line of the document's own facts. Claim the press and do nothing with it.
    // Only button 3: dragging the window by the bar is button 1 and is left alone.
    let quiet = gtk::GestureClick::new();
    quiet.set_button(gdk::BUTTON_SECONDARY);
    quiet.connect_pressed(|gesture, _, _, _| {
        gesture.set_state(gtk::EventSequenceState::Claimed);
    });
    app.statusbar.widget().add_controller(quiet);

    // Right-click over the zoom readout: a PDF's two fitting modes, which otherwise live only in
    // the palette. Parented on the status bar's own button rather than in a header bar, so the
    // popover has a plain widget to hang off.
    //
    // The claim comes before anything else and happens whatever the tab is. `GtkButton`'s own
    // gesture is primary-only, so without it the press bubbled past the readout into the window
    // handle above and the shell's window menu took the pointer over our popover.
    let fit = gtk::GestureClick::new();
    fit.set_button(gdk::BUTTON_SECONDARY);
    fit.connect_pressed(glib::clone!(
        #[weak]
        app,
        move |gesture, _, _, _| {
            gesture.set_state(gtk::EventSequenceState::Claimed);
            if app.active_pdf().is_none() {
                return;
            }
            let menu = gio::Menu::new();
            for action in ["win.pdf-fit-width", "win.pdf-fit-page"] {
                menu.append(Some(label_of(action)), Some(action));
            }
            let popover = gtk::PopoverMenu::from_model(Some(&menu));
            popover.set_parent(app.statusbar.zoom());
            popover.set_has_arrow(false);
            // A popover parented by hand stays parented until it is unparented by hand — but not
            // while it is closing. `closed` is emitted from inside the item's own `clicked`, and
            // an unparented widget has no path to the window's action muxer, so unparenting there
            // dropped the action the click had just asked for: the menu appeared, Fit Page did
            // nothing, and the page stayed fitted to the width. The idle runs once the click is
            // over.
            popover.connect_closed(|p| {
                let p = p.clone();
                glib::idle_add_local_once(move || p.unparent());
            });
            popover.popup();
        }
    ));
    app.statusbar.zoom().add_controller(fit);

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

    // The mouse's back and forward buttons. GTK's own gestures stop at button 3, and a
    // `GtkGestureClick` beside a widget that claims the sequence never sees the press at all
    // (paned.rs says why), so one capture-phase legacy controller on the window is where these
    // can be seen. It goes through the GAction rather than calling the reader directly, which is
    // what gives a mouse click the chrome reveal and the palette bookkeeping a chord gets.
    let nav = gtk::EventControllerLegacy::new();
    nav.set_propagation_phase(gtk::PropagationPhase::Capture);
    nav.connect_event(glib::clone!(
        #[weak]
        app,
        #[upgrade_or]
        glib::Propagation::Proceed,
        move |_, event| {
            let pressed = match event.event_type() {
                gdk::EventType::ButtonPress => true,
                gdk::EventType::ButtonRelease => false,
                _ => return glib::Propagation::Proceed,
            };
            let button = event
                .downcast_ref::<gdk::ButtonEvent>()
                .map(|event| event.button());
            let Some(action) = button.and_then(nav_action) else {
                return glib::Propagation::Proceed;
            };
            if pressed {
                let _ = WidgetExt::activate_action(&app.window, action, None);
            }
            // The release goes with the press, or whatever is under the pointer sees half a click.
            glib::Propagation::Stop
        }
    ));
    app.window.add_controller(nav);

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
                let Err(e) = app.write_tab(tab, tab.etag.get()) else {
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
                    // A PDF is rendered light or dark rather than recoloured, so the theme
                    // change is a re-render of whatever is on screen.
                    for doc in app.docs() {
                        if let Some(pdf) = doc.pdf() {
                            pdf.restyle();
                        }
                    }
                    if let Some(preview) = app.preview.borrow().as_ref() {
                        preview.restyle();
                    }
                    app.restyle_terminals();
                }
            ),
        );
    }
    // A terminal is code, so it follows the monospace font rather than the document one.
    style.connect_monospace_font_name_notify(glib::clone!(
        #[weak]
        app,
        move |_| {
            for doc in app.docs() {
                if let Some(term) = doc.terminal() {
                    term.refont();
                }
            }
        }
    ));
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
            if let Some(ops) = app.ops() {
                fileops::context_menu(ops, tree.widget(), &rel, kind == 'd', anchor);
            }
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
                gdk::Key::Delete => {
                    if let Some(ops) = app.ops() {
                        fileops::trash(ops, &rel);
                    }
                }
                gdk::Key::Menu => {
                    let Some(ops) = app.ops() else {
                        return glib::Propagation::Proceed;
                    };
                    fileops::context_menu(
                        ops,
                        tree.widget(),
                        &rel,
                        kind == 'd',
                        row_anchor(tree.view(), tree.widget()),
                    );
                }
                _ => return glib::Propagation::Proceed,
            }
            glib::Propagation::Stop
        }
    ));
    list.add_controller(keys);
}

/// A conflict pane's label: the file, and when it was last written.
///
/// The two sides of a sync conflict are one note twice, and which of them is called Mine is
/// decided by which one kept the original name — that is Syncthing's decision, not ours, and the
/// copy it renames can be the newer of the two. The time is the only thing here that says so.
fn written_at(rel: &str, etag: &Etag) -> String {
    match glib::DateTime::from_unix_local(etag.mtime_ns / 1_000_000_000)
        .and_then(|when| when.format("%d %b %H:%M"))
    {
        Ok(when) => format!("{rel} · {when}"),
        Err(_) => rel.to_string(),
    }
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
    // GtkTextView binds Ctrl+K to deleting to the end of the line, which is not something anyone
    // reaches for in an editor that has Ctrl+L for the whole line.
    "win.toggle-comment",
];

/// The chords the window keeps while a shell has the keyboard. Everything else in [`ACTIONS`]
/// goes to the shell: a terminal that answers only half of readline is not a terminal.
///
/// GTK dispatches a window's application accelerators at the window in the capture phase, ahead
/// of the focused VTE, so a chord in the table is eaten whatever the terminal does with it —
/// unbinding it in `App::apply_accels` is what lets the key through. The reserved set is small
/// and each entry earns its place:
///
/// * `win.close-tab` (`Ctrl+W`) — Close Tab has to mean the same thing over every tab. This is
///   the one budgeted cost: readline loses delete-word, and `Alt+Backspace` still does it.
/// * `win.next-tab` / `win.previous-tab` (`Ctrl+Tab`) — the same rule as Close Tab, and these
///   were `AdwTabView`'s own capture-phase chords before they were actions, so a shell never had
///   them to lose. No readline meaning either: `Ctrl+I` is the completion key, not `Ctrl+Tab`.
/// * `win.terminal` (`Ctrl+J`) and the three zoom actions — the chords that open a shell and
///   scale one have to be reachable from inside one.
/// * `win.fullscreen` (`F11`) — no readline or curses meaning, and GNOME Terminal keeps the same
///   key for the same reason: a fullscreen window has to be leavable from a focused shell.
/// * every chord whose spelling carries both `<Control>` and `<Shift>` — the existing convention,
///   which no shell claims, and which already covers Copy and Paste in Terminal, the pane chords,
///   the palette's second spelling and Replace in Notes.
///
/// ponytail: matched on the accelerator's spelling. A `<Primary>` or `<Ctrl>` written by hand into
/// the config is not recognised; `gtk::accelerator_parse` would settle it but needs an initialised
/// GTK, which the tests do not have.
fn reserved(action: &str, accel: &str) -> bool {
    matches!(
        action,
        "win.close-tab"
            | "win.next-tab"
            | "win.previous-tab"
            | "win.terminal"
            | "win.zoom-in"
            | "win.zoom-out"
            | "win.zoom-reset"
            | "win.fullscreen"
    ) || (accel.contains("<Control>") && accel.contains("<Shift>"))
}

fn clear(controller: &gtk::ShortcutController) {
    let old: Vec<gtk::Shortcut> = (0..controller.n_items())
        .filter_map(|i| controller.item(i).and_downcast::<gtk::Shortcut>())
        .collect();
    for shortcut in old {
        controller.remove_shortcut(&shortcut);
    }
}

/// The window's capture controller, which takes chords the text widgets would otherwise claim.
///
/// It runs before everything, so it has to ask who has the keyboard first: these are editor
/// chords, and a shell wants `Ctrl+K` to kill to the end of the line rather than to toggle a
/// comment in a note nobody is looking at.
fn fill_captured(controller: &gtk::ShortcutController, bindings: &[(&str, String)]) {
    clear(controller);
    for (action, accel) in bindings {
        let Some(trigger) = gtk::ShortcutTrigger::parse_string(accel) else {
            continue;
        };
        let action = action.to_string();
        controller.add_shortcut(gtk::Shortcut::new(
            Some(trigger),
            Some(gtk::CallbackAction::new(move |widget, _| {
                let editing = widget
                    .root()
                    .and_downcast::<gtk::Window>()
                    .and_then(|w| gtk::prelude::GtkWindowExt::focus(&w))
                    .is_some_and(|f| f.is::<sourceview5::View>());
                if !editing {
                    return glib::Propagation::Proceed;
                }
                widget.activate_action(&action, None).is_ok().into()
            })),
        ));
    }
}

fn install_actions(app: &Rc<App>) {
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

    // A focused shell keeps the keyboard, which means the table has to be rebuilt whenever it
    // crosses into or out of a terminal. `focus-widget` is the one signal that hears every way
    // that happens: a click, a tab switch, a dialog, `Ctrl+J` itself.
    app.window.connect_focus_widget_notify(glib::clone!(
        #[weak]
        app,
        move |window| {
            let shell = terminal::has_focus(window);
            if shell != app.shell_keys.replace(shell) {
                app.apply_accels();
            }
        }
    ));
}

/// Which action a mouse button asks for, for the two GTK has no name for. GDK names only the
/// first three buttons; 8 and 9 are the side pair every mouse that has one ships, and browsers
/// have meant back and forward by them for twenty years.
fn nav_action(button: u32) -> Option<&'static str> {
    match button {
        8 => Some("win.pdf-back"),
        9 => Some("win.pdf-forward"),
        _ => None,
    }
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
        [
            "win.new-note",
            "win.new-folder",
            "win.open-file",
            "win.save",
        ]
        .as_slice(),
        // What changes which vault this window is on: the three ways in, then the way out.
        [
            "app.open-vault",
            "app.open-remote",
            "win.open-recent",
            "app.close-vault",
        ]
        .as_slice(),
        ["win.find", "win.view-mode", "win.terminal", "win.present"].as_slice(),
        ["win.preferences", "win.about", "app.quit"].as_slice(),
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

/// The display-wide rule every editor starts from: Adwaita Mono at the size of GNOME's *document*
/// font, which is [`editor::default_font`]. A vault is prose with code fences, tables and
/// wikilinks in it, and none of those line up in a proportional face, so the family is ours and
/// only the size follows the system.
///
/// It goes through [`editor::font_css`], the same function a tab's own zoom rule is written with,
/// so the family and the size are decided in one place and a zoomed note cannot end up in a
/// different face from an unzoomed one.
fn install_document_font() {
    let Some(display) = gdk::Display::default() else {
        return;
    };
    let provider = gtk::CssProvider::new();
    provider.load_from_string(&editor::font_css(
        &editor::default_font(),
        // The label too: the editor's sticky block title is a line of the document, and a tab at
        // the default zoom has no `#accent-doc-N` rule of its own for it to pick the face up from.
        "textview.accent-doc, label.accent-doc",
        1.0,
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
/// `.accent-bar-button` does the same job for the status bar's two controls, the branch readout
/// and the zoom one: Adwaita gives a button a 24 px minimum and 5 px of padding either side, a box
/// is as tall as its tallest child however that child is aligned, and so either of them appearing
/// lifted the bar from 29 px to 46 px. Dropping the minimum and the vertical padding puts them on
/// the caption's own line height, and they stay buttons rather than becoming labels, so the click,
/// the focus ring and the tooltip stay.
///
/// The last rules are corrections to GtkSourceView, which styles itself from its style scheme
/// (a widget-level provider at priority 598) and from its own CSS (599). A display provider at
/// `STYLE_PROVIDER_PRIORITY_APPLICATION` outranks both per property, so the document takes the
/// theme's view colours instead of the scheme's grey, and the completion popup takes the
/// popover's. The scheme itself stays: dropping it takes the find bar's match highlight with it.
/// On the `text` node only `color` is ours, because GtkSourceView pins that node's background to
/// transparent at maximum priority; the background therefore goes on the `textview` node. The
/// gutter is the same correction one node over: a scheme's `line-numbers` style carries a
/// background of its own, and Solarized's is a shade off its text background (`base2` on `base3`,
/// `base02` on `base03`), so the line numbers sat in a stripe beside the page. Adwaita happens to
/// paint the two the same, which is why only Solarized showed it.
// ponytail: the header rule leans on libadwaita's own header padding (6 above a lone header,
// 3 + 3 above a stacked one) adding up to the same offset. Reach for `AdwToolbarView`'s spacing
// API instead if one ever appears; today the class is the only handle on it.
//
// A handle under the pointer takes the accent colour without changing size, so it says it can be
// dragged before it is. `box-shadow: none` is what makes it visible at all: Adwaita draws the line
// as a 1 px inset shadow over a transparent background, and on a 1 px handle that shadow covers
// the whole allocation, so a background colour alone would never show. The dragging rule above
// paints 3 px, of which the shadow still covers one; the hover rule follows it, so a handle being
// dragged is the same colour as one being aimed at and only the width changes.
//
// ponytail: `paned.dragging` widens the handle from 1 px to 3 px, which moves the pane beside it
// by 2 px for the length of the drag. Drawing outside the 1 px allocation instead, with an
// outline or a negative margin, was measured: it only ever reaches the side rendered before the
// handle, because the pane after it paints over the other. A 2 px shift while a divider is being
// dragged is invisible, so it is the cheaper of the two.
/// Registers the icons compiled into the binary and points the theme at them.
///
/// A GResource rather than hicolor: the completion list needs the kind icons long before anyone
/// runs `make install`, and the theme keeps answering for every Adwaita name as it did.
fn install_icons() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        if let Err(e) = gio::resources_register_include!("accent.gresource") {
            tracing::warn!("icons: {e}");
            return;
        }
        let Some(display) = gdk::Display::default() else {
            return;
        };
        gtk::IconTheme::for_display(&display).add_resource_path("/io/github/stroblme/Accent/icons");
    });
}

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
             .accent-drop-zone {{ background-color: var(--accent-bg-color); opacity: 0.3; }} \
             .git-actions {{ opacity: 0; }} \
             row:hover .git-actions, row:focus-within .git-actions {{ opacity: 1; }} \
             .git-log > row {{ margin-top: 0; margin-bottom: 0; }} \
             paned.dragging > separator {{ min-width: 3px; min-height: 3px; \
               background-color: var(--border-color); }} \
             paned > separator:hover {{ box-shadow: none; \
               background-color: var(--accent-bg-color); }} \
             .accent-flat, .accent-flat:backdrop {{ background-color: var(--view-bg-color); }} \
             .accent-bar-button {{ min-height: 0; padding: 0 6px; border-radius: 6px; }} \
             .accent-lone-header > windowhandle > box {{ padding-bottom: 0; }} \
             textview.accent-doc {{ color: var(--view-fg-color); \
               background-color: var(--view-bg-color); }} \
             textview.accent-doc text {{ color: var(--view-fg-color); }} \
             textview border gutter {{ background-color: var(--view-bg-color); }} \
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

    /// Same guard for the mouse: a side button fires an action by name, so the name has to be one
    /// the window actually has.
    #[test]
    fn the_side_buttons_name_actions_that_exist() {
        assert_eq!(nav_action(8), Some("win.pdf-back"));
        assert_eq!(nav_action(9), Some("win.pdf-forward"));
        // The three GTK does name are everyone else's: click, paste, context menu.
        for button in [1, 2, 3] {
            assert_eq!(nav_action(button), None);
        }
        for action in [8, 9].into_iter().filter_map(nav_action) {
            assert!(
                ACTIONS.iter().any(|(name, _, _)| *name == action),
                "{action} is on a mouse button but not in ACTIONS"
            );
        }
    }

    /// The reserved set is what a focused shell does not get, and it is pure data: everything
    /// else in the table is unbound for as long as a terminal has the keyboard.
    #[test]
    fn a_focused_shell_keeps_everything_but_the_reserved_set() {
        // Kept: closing a tab, opening a shell, scaling one, leaving fullscreen, and every
        // Ctrl+Shift chord in the table — Copy and Paste in Terminal among them.
        assert!(reserved("win.close-tab", "<Control>w"));
        assert!(reserved("win.terminal", "<Control>j"));
        assert!(reserved("win.fullscreen", "F11"));
        assert!(reserved("win.new-folder", "<Control><Shift>n"));
        assert!(reserved("win.terminal-copy", "<Control><Shift>c"));
        assert!(reserved("win.terminal-paste", "<Control><Shift>v"));
        // Every spelling of the zoom chords, or Ctrl+= would zoom the shell while Ctrl+plus went
        // to readline.
        for accel in ["<Control>plus", "<Control>equal", "<Control>KP_Add"] {
            assert!(reserved("win.zoom-in", accel));
        }
        assert!(reserved("win.zoom-out", "<Control>minus"));
        assert!(reserved("win.zoom-reset", "<Control>0"));
        // The shell's: plain Ctrl, function keys, and the chords readline reaches for most.
        for (action, accel) in [
            ("win.save", "<Control>s"),
            ("win.duplicate-line", "<Control>d"),
            ("win.toggle-comment", "<Control>k"),
            ("win.delete-line", "<Control>l"),
            ("win.palette-files", "<Control>e"),
            ("win.palette-commands", "<Control>p"),
            ("win.find-previous", "<Shift>F3"),
            ("win.menu", "F10"),
        ] {
            assert!(!reserved(action, accel), "{action} eats {accel}");
        }
    }

    /// A rebound chord follows the same rule as the default it replaced, because the filter reads
    /// the spelling in force rather than the table's.
    #[test]
    fn a_rebound_chord_follows_the_same_rule() {
        let mut config = Config::default();
        config.shortcuts.insert(
            "win.save".to_string(),
            vec!["<Control><Shift>s".to_string()],
        );
        for accel in accels_for(&config, "win.save") {
            assert!(reserved("win.save", &accel));
        }
        // The other direction: Close Tab moved off Ctrl+W is still Close Tab, and still reserved.
        config
            .shortcuts
            .insert("win.close-tab".to_string(), vec!["<Control>y".to_string()]);
        for accel in accels_for(&config, "win.close-tab") {
            assert!(reserved("win.close-tab", &accel));
        }
    }

    #[test]
    fn the_terminal_flag_takes_the_first_argument_that_is_not_a_flag() {
        let args = |v: &[&str]| -> Vec<std::ffi::OsString> {
            v.iter().map(std::ffi::OsString::from).collect()
        };
        let cwd = |v: &[&str]| terminal_cwd(&args(v)).map(|p| p.to_string_lossy().into_owned());

        assert_eq!(
            cwd(&["accent", "--terminal", "/tmp"]).as_deref(),
            Some("/tmp")
        );
        // Order does not matter, and the short spelling is the same flag.
        assert_eq!(cwd(&["accent", "/tmp", "-t"]).as_deref(), Some("/tmp"));
        // The bare form has no directory to offer, so the window decides.
        assert_eq!(cwd(&["accent", "--terminal"]), None);
        // argv[0] is the program, never the path.
        assert_eq!(cwd(&["accent"]), None);
        // The other flag a command line can carry is not a path either.
        assert_eq!(cwd(&["accent", "--new-window", "--terminal"]), None);
    }

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
