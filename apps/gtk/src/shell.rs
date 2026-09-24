//! One process, one config, one window per vault: the `Shell` owns every window's `App`, answers
//! the command line and moves tabs between windows.

use super::*;

/// One process, one config, one window per vault.
pub struct Shell {
    pub config: Rc<RefCell<Config>>,
    /// The config as it was last put into effect, so the next one can tell what moved
    /// ([`Changed`]). `config` itself is edited in place before it is applied.
    pub applied: RefCell<Config>,
    /// The write [`Shell::save_config_soon`] has scheduled and not yet done.
    pub config_write: RefCell<Option<glib::SourceId>>,
    /// Watches `config.toml` for someone else's edits ([`Shell::watch_config`]); kept only so it
    /// lives as long as the shell.
    pub config_monitor: RefCell<Option<gio::FileMonitor>>,
    /// Whether the file has been found not to parse and that has been said, so it is said once
    /// until the file parses again.
    pub config_broken: Cell<bool>,
    /// The open windows, and the only strong reference to each one's state: an entry is dropped
    /// in `forget` when the window closes, which is what releases the vault and its worker thread.
    /// Each carries what it was opened on (`App::key`), so a vault-less one is found again by its
    /// kind.
    pub windows: RefCell<Vec<Rc<App>>>,
    /// The start screen while one is up, so Open Folder… presents it again instead of stacking a
    /// second copy. Weak: the window belongs to GTK, and closing it is how it goes away.
    pub start: glib::WeakRef<adw::ApplicationWindow>,
    /// Where a dragged tab was let go, between our drop zone seeing it and libadwaita asking for
    /// somewhere to put it. See [`Landing`].
    pub landing: RefCell<Option<Landing>>,
    /// Whether the accelerator table is currently narrowed to [`reserved`] for a focused shell.
    /// One flag, not one per window, because the table is the application's: a window keeping
    /// its own left every other window without its chords while a shell here had the keyboard.
    pub shell_keys: Cell<bool>,
}

/// What a window in [`Shell::windows`] was opened on: a vault, by its `Vault::key`, a terminal
/// session by its `terminal://<name>` key, or no vault at all, as one of the [`Loose`] kinds.
#[derive(Clone, PartialEq)]
pub enum WindowKey {
    Vault(PathBuf),
    /// Shells saved under a name (Save): no vault, but a session and a place in the
    /// recent list like one.
    Terminal(PathBuf),
    Loose(Loose),
}

impl WindowKey {
    /// The vault's key, or `None` for a window with no vault.
    pub fn vault(&self) -> Option<&Path> {
        match self {
            WindowKey::Vault(root) => Some(root),
            WindowKey::Terminal(_) | WindowKey::Loose(_) => None,
        }
    }

    /// The key the window is remembered by, in the recent list and in its state file. `None` for
    /// a window that is opened the same way again rather than restored.
    pub fn saved_as(&self) -> Option<&Path> {
        match self {
            WindowKey::Vault(key) | WindowKey::Terminal(key) => Some(key),
            WindowKey::Loose(_) => None,
        }
    }

    /// Whether the window is one for shells: a terminal session, or the one `--terminal` opens.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            WindowKey::Terminal(_) | WindowKey::Loose(Loose::Terminal)
        )
    }
}

/// The two windows with no vault (DESIGN.md, Window without a vault), one of each at most. The
/// launch that builds one decides which it is, for good: a shell or a file opened in it by hand
/// afterwards stays in it without making it the other kind.
#[derive(Clone, Copy, PartialEq)]
pub enum Loose {
    /// Where every `accent --terminal` opens its shell.
    Terminal,
    /// Where every file from outside the open vaults opens: `accent <file>`, or the file
    /// manager's Open With.
    Documents,
}

/// What an `app.` action does, given the shell and the application it was fired at.
type AppAction = fn(&Rc<Shell>, &adw::Application);

/// How long a preference changed outside the dialog waits to be written, so a run of ring picks is
/// one write. The session's beat.
const CONFIG_WRITE: Duration = Duration::from_secs(1);

/// A tab let go over a pane, waiting for `AdwTabView::create-window` to spend it.
///
/// libadwaita detaches a dragged page from its view for the length of the drag, and neither
/// `attach_page` nor the page's own view is public, so nothing can give a page a view back except
/// the `create-window` handler, which libadwaita calls on the source view when a drop outside its
/// own tab bars has finished. Our drop zones therefore take the drop and record where it landed;
/// the handler hands back the tab view named here and libadwaita does the attaching.
pub struct Landing {
    app: Rc<App>,
    pane: Rc<Pane>,
    zone: Zone,
}

/// The costly parts of putting a config into effect, each true only where its inputs moved. The
/// theme swaps the display's style provider and restyles every widget, a font re-measures every
/// tab, the vault settings are a `hello` to each remote host, and the shortcuts are the whole
/// accelerator table; everything else is a cheap switch that is simply set again.
#[derive(Debug, Default, PartialEq)]
pub struct Changed {
    pub theme: bool,
    pub font: bool,
    pub vault: bool,
    pub shortcuts: bool,
}

impl Changed {
    pub fn between(old: &Config, new: &Config) -> Changed {
        Changed {
            theme: old.theme != new.theme,
            font: old.editor_font != new.editor_font,
            // Every vault's entries, not this window's: a `hello` too many is harmless.
            vault: old.vaults != new.vaults || old.ghost_text != new.ghost_text,
            shortcuts: old.shortcuts != new.shortcuts,
        }
    }
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
    pub fn install_app_actions(self: &Rc<Self>, gtk_app: &adw::Application) {
        // GNOME Shell offers New Window in the launcher's context menu only when it finds an
        // `app.new-window` action, the `new-window` desktop action, or one of the SingleWindow
        // keys. Both of the first two are provided: this is the one DESIGN.md's Keyboard rule
        // asks for, and the desktop file carries the other for a shell that reads it first.
        let actions: [(&str, AppAction); 5] = [
            ("open-vault", Shell::choose_vault),
            ("open-remote", Shell::choose_remote),
            ("new-window", Shell::start_screen),
            ("close-vault", Shell::close_vault),
            ("quit", Shell::quit),
        ];
        for (name, run) in actions {
            let action = gio::SimpleAction::new(name, None);
            action.connect_activate({
                let (shell, gtk_app) = (self.clone(), gtk_app.clone());
                move |_, _| {
                    shell.record(&gtk_app, &format!("app.{name}"));
                    run(&shell, &gtk_app);
                }
            });
            gtk_app.add_action(&action);
        }
    }

    /// Close the windows rather than calling `quit()`: `GtkApplication::quit` tears the process
    /// down without emitting `close-request`, which is where unsaved buffers get written and
    /// where a failed save gets to stop the exit. The application ends on its own once the last
    /// window is gone, so a window that refuses to close also refuses to quit. The command was
    /// recorded before the windows go: the session is written by each window's own
    /// `close-request`, which runs after this and carries the entry with it.
    fn quit(self: &Rc<Self>, gtk_app: &adw::Application) {
        for window in gtk_app.windows() {
            window.close();
        }
    }

    /// The [`App`] behind a window, for the app-scoped actions: they are fired at the application
    /// and have to find the window that asked before they can record anything on it.
    fn app_at(&self, window: &gtk::Window) -> Option<Rc<App>> {
        self.windows
            .borrow()
            .iter()
            .find(|app| app.window.upcast_ref::<gtk::Window>() == window)
            .cloned()
    }

    /// Record an `app.` action in the active window's recently-run commands. Nothing happens from
    /// the start screen, which has no session to remember it in.
    fn record(&self, gtk_app: &adw::Application, action: &str) {
        if let Some(app) = gtk_app.active_window().and_then(|w| self.app_at(&w)) {
            app.command_used(action);
        }
    }

    /// Put a config into effect in every window. There is one config per process, so a preference
    /// changed in one window — in its dialog or anywhere else (`App::config_changed`) — is the
    /// same preference in all of them. Measured against the last one applied, so a switch does
    /// not restyle every widget the way a theme change has to.
    pub fn apply_config(&self, config: &Config) {
        let changed = Changed::between(&self.applied.replace(config.clone()), config);
        // The theme is the display's rather than a window's, so it goes on once.
        if changed.theme {
            theme::apply(config.theme);
        }
        // Cloned out of the borrow: applying a config reaches a long way into each window.
        let apps: Vec<Rc<App>> = self.windows.borrow().clone();
        for app in apps {
            app.apply_config(config, &changed);
        }
    }

    /// Write the config a moment from now, once for every change made before then, the way a
    /// window's session is written. The config is the process's, so the timer is too: a window
    /// that closes meanwhile takes nothing with it.
    pub fn save_config_soon(self: &Rc<Self>) {
        if self.config_write.borrow().is_some() {
            return;
        }
        let shell = Rc::downgrade(self);
        let id = glib::timeout_add_local_once(CONFIG_WRITE, move || {
            if let Some(shell) = shell.upgrade() {
                shell.config_write.take();
                settings::save(&shell.config.borrow());
            }
        });
        self.config_write.replace(Some(id));
    }

    /// Do a write [`Self::save_config_soon`] still has waiting, now: the process is ending.
    pub fn flush_config(&self) {
        if let Some(id) = self.config_write.take() {
            id.remove();
            settings::save(&self.config.borrow());
        }
    }

    /// Take in what someone else writes to `config.toml` while accent runs — a hand edit, most
    /// likely — as it lands. Accent's own writes come back through here too and are told apart by
    /// their text ([`Config::reread`]).
    pub fn watch_config(self: &Rc<Self>) {
        let file = gio::File::for_path(accent_core::config::config_path());
        let monitor = match file.monitor_file(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE) {
            Ok(monitor) => monitor,
            Err(e) => return tracing::warn!("cannot watch config.toml: {e}"),
        };
        let shell = Rc::downgrade(self);
        monitor.connect_changed(move |_, _, _, event| {
            // The settled write and the rename an atomic save lands as, as for a tab's file: an
            // in-place write is seen half done before that.
            let settled = matches!(
                event,
                gio::FileMonitorEvent::ChangesDoneHint | gio::FileMonitorEvent::Created
            );
            if let Some(shell) = shell.upgrade().filter(|_| settled) {
                shell.config_file_changed();
            }
        });
        self.config_monitor.replace(Some(monitor));
    }

    /// `config.toml` changed on disk. A change that parses is put into effect like any other
    /// (only what it moved is redone) with accent's own unwritten changes kept on top, and is not
    /// written back unless those need it. One that does not parse is left alone and said once;
    /// accent keeps its running config and writes nothing until the file parses again.
    fn config_file_changed(self: &Rc<Self>) {
        let reread = self.config.borrow().reread();
        match reread {
            Ok(None) => {}
            Ok(Some(taken)) => {
                self.config_broken.set(false);
                *self.config.borrow_mut() = taken.config.clone();
                self.apply_config(&taken.config);
                match taken.unwritten {
                    true => self.save_config_soon(),
                    // A write still waiting has nothing left of ours to carry.
                    false => {
                        if let Some(id) = self.config_write.take() {
                            id.remove();
                        }
                    }
                }
            }
            Err(e) => {
                if self.config_broken.replace(true) {
                    return;
                }
                tracing::warn!("{e:#}; keeping the running settings, and not writing the file");
                let app = gio::Application::default()
                    .and_downcast::<gtk::Application>()
                    .and_then(|gtk_app| gtk_app.active_window())
                    .and_then(|window| self.app_at(&window));
                if let Some(app) = app {
                    app.toast(
                        "config.toml does not parse. Changes made here are kept and written once it does",
                    );
                }
            }
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
            .any(|app| app.window.upcast_ref::<gtk::Window>() == &window);
        if !opened {
            return;
        }
        self.start_screen(gtk_app);
        window.close();
    }

    /// Let go of a window's [`App`] once the close is certain. This is the only strong reference
    /// to it, so the vault, its worker thread and its WebKit process all go with it.
    ///
    /// ponytail: the `App` goes here, the vault a moment later — the tree's model and the sidebar's
    /// panes hold their own `Arc<Vault>` inside widgets, so the last one drops when GTK destroys
    /// the window, or when a query still on a worker thread returns, and `Vault`'s `Drop` joins the
    /// worker there. That holds only while no handler keeps its own widget alive: one strong
    /// capture of the widget it is connected to, or of one around it, keeps a closed window's
    /// vault open until the process exits, which `ACCENT_BENCH_CLOSE=1` checks for. Closing a
    /// second into `testvault`'s 2.7 s cold reconcile blocked the main loop for 2.3 s, with the
    /// window already off screen. `Vault::drop` names the fix (a cancellation flag on
    /// `reconcile`); until a vault is opened and closed often enough for that pause to be felt,
    /// one stalled close is cheaper than the flag.
    fn forget(&self, window: &adw::ApplicationWindow) {
        let mut windows = self.windows.borrow_mut();
        let Some(i) = windows.iter().position(|app| &app.window == window) else {
            return;
        };
        let app = windows.remove(i);
        // Out of the borrow before the drop: `App` reaches a long way as it goes.
        drop(windows);
        drop(app);
    }

    pub fn command_line(
        self: &Rc<Self>,
        gtk_app: &adw::Application,
        command_line: &gio::ApplicationCommandLine,
    ) -> glib::ExitCode {
        let args = command_line.arguments();
        // `accent --terminal [PATH]` is accent as a terminal: a window with no vault holding one
        // shell. A second one joins that window as another tab, and a loose file opened meanwhile
        // goes to a window of its own rather than in among the shells.
        if args.iter().any(|a| a == "--terminal" || a == "-t") {
            let at = terminal_cwd(&args).and_then(|arg| shell_at(command_line, arg));
            if let Some(app) = self.loose_window(gtk_app, Loose::Terminal) {
                app.window.present();
                match at {
                    Some(at) => app.open_terminal_named(&at),
                    None => app.open_terminal_at(None),
                }
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
            match last.filter(|path| start::openable(path)) {
                Some(root) => self.open_vault(gtk_app, root, None),
                None => self.start_screen(gtk_app),
            }
            return glib::ExitCode::SUCCESS;
        };
        // `accent terminal://<name> [PATH]`: the session of that name, made if it is new, with a
        // shell at PATH added to it.
        if let Some(address) = arg.to_str().filter(|a| a.starts_with(terminal::SESSION)) {
            let key = PathBuf::from(address);
            if terminal::session_name(&key).is_none() {
                eprintln!("not a session name, which needs one and no '/': {address}");
                return glib::ExitCode::FAILURE;
            }
            let at = args.get(2).and_then(|arg| shell_at(command_line, arg));
            self.open_vault(gtk_app, key, at);
            return glib::ExitCode::SUCCESS;
        }
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
    ///
    /// A remote window's folders are on its host, so there the picker is the Open Remote form,
    /// filled in with this window's address: a vault opened at a mistyped path is steered from
    /// here. What it picks replaces this window — one window per vault, and the vault this one
    /// was on is the one being corrected — unless it is the same vault, which only raises it.
    fn choose_vault(self: &Rc<Self>, gtk_app: &adw::Application) {
        let parent = gtk_app.active_window();
        let asking = parent.as_ref().and_then(|w| self.app_at(w));
        if let Some(app) = asking
            && let Some(vault) = app.vault()
            && let Some(remote) = vault.remote()
        {
            let (shell, gtk_app) = (self.clone(), gtk_app.clone());
            let (from, here) = (app.window.downgrade(), vault.key().to_owned());
            start::connect_dialog(
                &app.window,
                Some(remote.url()),
                "Open Remote Vault",
                "Connect",
                move |address| {
                    let root = PathBuf::from(address);
                    let replace = root != here;
                    shell.open_from_start(&gtk_app, root);
                    // Once the new one is up, so the application never stands at zero windows.
                    if let Some(window) = from.upgrade().filter(|_| replace) {
                        window.close();
                    }
                },
            );
            return;
        }
        let dialog = gtk::FileDialog::builder().title("Open Vault").build();
        let (shell, gtk_app) = (self.clone(), gtk_app.clone());
        dialog.select_folder(parent.as_ref(), gio::Cancellable::NONE, move |result| {
            // A dismissed chooser is an error here, and not one worth saying anything about.
            if let Some(path) = result.ok().and_then(|folder| folder.path()) {
                shell.open_from_start(&gtk_app, path);
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
        start::connect_dialog(
            &window,
            None,
            "Open Remote Vault",
            "Connect",
            move |address| {
                shell.open_from_start(&gtk_app, PathBuf::from(address));
            },
        );
    }

    fn start_screen(self: &Rc<Self>, gtk_app: &adw::Application) {
        if let Some(window) = self.start.upgrade() {
            window.present();
            return;
        }
        let window = start::present(gtk_app, self.config.clone(), {
            let (shell, gtk_app) = (self.clone(), gtk_app.clone());
            move |root| shell.open_from_start(&gtk_app, root)
        });
        self.start.set(Some(&window));
    }

    /// Open a vault picked on the start screen or in one of the dialogs that stand in for it, and
    /// close the start screen if one is up: it has done its job whichever window asked. It is
    /// reached through the shell rather than captured, which keeps the start screen's own
    /// callback out of a cycle with the window it lives in.
    fn open_from_start(self: &Rc<Self>, gtk_app: &adw::Application, root: PathBuf) {
        self.open_vault(gtk_app, root, None);
        if let Some(window) = self.start.upgrade() {
            window.close();
        }
    }

    /// One vault, one window (VS Code's rule): a vault that already has a window raises it rather
    /// than opening a second one on the same index, session and watcher. New Window lands on the
    /// start screen instead, where a vault without a window yet is picked.
    ///
    /// `root` is what the vault is keyed by: a directory, or an `ssh://` address for one on
    /// another machine. The two are one list, one rule and one window each — and a terminal
    /// session's `terminal://<name>` is a third kind of key in the same list, whose `note` is a
    /// directory to add a shell at.
    pub fn open_vault(
        self: &Rc<Self>,
        gtk_app: &adw::Application,
        root: PathBuf,
        note: Option<String>,
    ) {
        if let Some(app) = self.app_for(&root) {
            app.window.present();
            if let Some(note) = note {
                app.open_named(&note);
            }
            return;
        }
        let key = match terminal::session_name(&root) {
            Some(_) => WindowKey::Terminal(root),
            None => WindowKey::Vault(root),
        };
        self.add_window(gtk_app, key, note);
    }

    /// Build a window on `key` — a vault, or one of the windows with no vault — and take charge
    /// of it. The only place a window joins `windows`, so the handler that takes it out again is
    /// written once.
    fn add_window(
        self: &Rc<Self>,
        gtk_app: &adw::Application,
        key: WindowKey,
        note: Option<String>,
    ) -> Option<Rc<App>> {
        let app = build_window(gtk_app, self, &key, note)?;
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
        self.windows.borrow_mut().push(app.clone());
        Some(app)
    }

    /// The window a page belongs to, and what it holds. libadwaita's tab drag hands a page to any
    /// window in the process, so this is how the receiving one finds out where it came from.
    fn owner_of(&self, page: &adw::TabPage) -> Option<(Rc<App>, Doc)> {
        self.windows
            .borrow()
            .iter()
            .find_map(|app| Some((app.clone(), app.doc_for_page(page)?)))
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
    ///
    /// Spent by `landed`, at whichever attach ends the drag. That is a round trip away, through
    /// the X server or the compositor, so nothing here times it out; `App::set_drop_active`
    /// drops one left over when the next drag begins.
    pub fn aim(&self, app: &Rc<App>, pane: &Rc<Pane>, zone: Zone) {
        *self.landing.borrow_mut() = Some(Landing {
            app: app.clone(),
            pane: pane.clone(),
            zone,
        });
    }

    /// The tab view a page let go over one of our drop zones belongs in, or `None` when the drop
    /// was nowhere of ours.
    pub fn where_to_land(&self) -> Option<adw::TabView> {
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
    /// what the drop asked for — a split at an edge, the end of the bar in the middle — and, for
    /// a page out of another window, the move into this window's bookkeeping.
    ///
    /// Either waits for an idle, because this runs inside libadwaita's drag handling: re-parenting
    /// the pane it is emitting from fails GTK's own assertion, and the attach has not selected
    /// the page yet.
    pub fn landed(self: &Rc<Self>, app: &Rc<App>, pane: &Rc<Pane>, page: &adw::TabPage) {
        let zone = self
            .landing
            .borrow_mut()
            .take()
            .filter(|l| Rc::ptr_eq(&l.pane, pane))
            .map(|l| l.zone);
        if let Some(zone) = zone {
            let (app, pane, page) = (app.clone(), pane.clone(), page.clone());
            glib::idle_add_local_once(move || match zone {
                Zone::Split(side) => app.split_page(&pane, side, &page),
                Zone::Here => app.move_in(&pane, &page),
            });
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
        if let Some(tab) = doc.tab().filter(|tab| tab.save.modified.get())
            && let Err(e) = from.flush_tab(tab)
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

    /// The open window remembered by `key`, a vault's key.
    pub(crate) fn app_for(&self, key: &Path) -> Option<Rc<App>> {
        self.windows
            .borrow()
            .iter()
            .find(|app| app.key.borrow().saved_as() == Some(key))
            .cloned()
    }

    /// Open `path` wherever it belongs: in the window whose vault contains it, or in the window
    /// kept for documents that are in no vault — never the one `accent --terminal` opened.
    fn open_file(self: &Rc<Self>, gtk_app: &adw::Application, path: PathBuf) {
        let inside = self.windows.borrow().iter().find_map(|app| {
            let rel = path.strip_prefix(app.key.borrow().vault()?).ok()?;
            Some((app.clone(), rel.to_string_lossy().into_owned()))
        });
        if let Some((app, rel)) = inside {
            app.window.present();
            app.open_path(&rel);
            return;
        }
        let Some(app) = self.loose_window(gtk_app, Loose::Documents) else {
            return;
        };
        app.window.present();
        app.open_path(&path.to_string_lossy());
    }

    /// The window with no vault of this `kind`, built if this is the first thing to want one since
    /// the last one closed. One of each, so a second shell joins the terminal window as a tab and
    /// a second loose file the documents window.
    pub fn loose_window(
        self: &Rc<Self>,
        gtk_app: &adw::Application,
        kind: Loose,
    ) -> Option<Rc<App>> {
        let loose = self
            .windows
            .borrow()
            .iter()
            .find(|app| *app.key.borrow() == WindowKey::Loose(kind))
            .cloned();
        if let Some(app) = loose {
            return Some(app);
        }
        self.add_window(gtk_app, WindowKey::Loose(kind), None)
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

/// Where a shell was asked for on the command line: on a host, as an `ssh://` address read back
/// the way `ssh::Url` spells it, or in a directory here (see [`shell_dir`]). `None`, said on
/// stderr, for an address that does not parse.
fn shell_at(command_line: &gio::ApplicationCommandLine, arg: &std::ffi::OsStr) -> Option<String> {
    let Some(address) = arg.to_str().filter(|a| ssh::is_remote(a)) else {
        return shell_dir(command_line, arg).map(|dir| dir.to_string_lossy().into_owned());
    };
    match ssh::parse(address) {
        Ok(url) => Some(url.to_string()),
        Err(e) => {
            eprintln!("cannot open {address}, opening at home: {e}");
            None
        }
    }
}

/// The directory a shell was asked for on the command line, resolved against the invoking
/// process's directory as a vault path is, or `None` — said on stderr — when it is not one.
fn shell_dir(command_line: &gio::ApplicationCommandLine, arg: &std::ffi::OsStr) -> Option<PathBuf> {
    let path = command_line.create_file_for_arg(arg).path()?;
    match path.canonicalize() {
        Ok(dir) if dir.is_dir() => Some(dir),
        // A file is not taken as its parent: which directory was meant is a guess, and a shell
        // in the wrong one is worse than a shell at home that says so.
        Ok(other) => {
            eprintln!("not a folder, opening at home: {}", other.display());
            None
        }
        Err(e) => {
            eprintln!("cannot open {}: {e}", path.display());
            None
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

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
        // A place on a host is taken the same way.
        assert_eq!(
            cwd(&["accent", "-t", "ssh://box/srv/x"]).as_deref(),
            Some("ssh://box/srv/x")
        );
        // The bare form has no directory to offer, so the window decides.
        assert_eq!(cwd(&["accent", "--terminal"]), None);
        // argv[0] is the program, never the path.
        assert_eq!(cwd(&["accent"]), None);
        // The other flag a command line can carry is not a path either.
        assert_eq!(cwd(&["accent", "--new-window", "--terminal"]), None);
    }

    #[test]
    fn a_config_change_redoes_only_what_it_moved() {
        use accent_core::config::Theme;
        let old = Config::default();
        // A switch or a ring pick is none of the expensive parts.
        let minimap = Config {
            minimap: true,
            ..Config::default()
        };
        assert_eq!(Changed::between(&old, &minimap), Changed::default());
        let theme = Config {
            theme: Theme::Solarized,
            ..Config::default()
        };
        assert_eq!(
            Changed::between(&old, &theme),
            Changed {
                theme: true,
                ..Changed::default()
            }
        );
        // Ghost text is carried to the vault alongside its settings.
        let ghost = Config {
            ghost_text: false,
            ..Config::default()
        };
        assert_eq!(
            Changed::between(&old, &ghost),
            Changed {
                vault: true,
                ..Changed::default()
            }
        );
    }
}
