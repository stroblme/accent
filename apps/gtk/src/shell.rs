//! One process, one config, one window per vault: the `Shell` owns every window's `App`, answers
//! the command line and moves tabs between windows.

use super::*;

/// One process, one config, one window per vault.
pub struct Shell {
    pub config: Rc<RefCell<Config>>,
    /// Last applied config for [`Changed`] comparisons; the shared config may already contain edits.
    pub applied: RefCell<Config>,
    /// The write [`Shell::save_config_soon`] has scheduled and not yet done.
    pub config_write: RefCell<Option<glib::SourceId>>,
    /// Watches `config.toml` for someone else's edits ([`Shell::watch_config`]); kept only so it
    /// lives as long as the shell.
    pub config_monitor: RefCell<Option<gio::FileMonitor>>,
    /// Suppress repeated invalid-config messages until the file parses again.
    pub config_broken: Cell<bool>,
    /// Sole strong window-state references; `forget` releases vaults and workers after close.
    pub windows: RefCell<Vec<Rc<App>>>,
    /// Weak start-screen handle so New Window raises the existing screen; GTK owns its lifetime.
    pub start: glib::WeakRef<adw::ApplicationWindow>,
    /// Where a dragged tab was let go, between our drop zone seeing it and libadwaita asking for
    /// somewhere to put it. See [`Landing`].
    pub landing: RefCell<Option<Landing>>,
    /// The window a row is being dragged out of, while a drag from a tree is in flight: what tells
    /// every other window the row names a file in that window's vault (`App::dragged_from`).
    pub tree_drag: RefCell<Option<std::rc::Weak<App>>>,
    /// Where the keyboard was when the shared accelerator table was last built
    /// (`Shell::apply_accels`), and whether its window debugged; one for all windows, as the
    /// table is.
    pub keys: Cell<(crate::actions::Keys, bool)>,
}

/// Window identity: vault key, named `terminal://<name>` session or unkeyed [`Loose`] kind.
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

/// One terminal and one documents window for launches; later manual opens do not change their kind.
#[derive(Clone, Copy, PartialEq)]
pub enum Loose {
    /// Where every `accent --terminal` opens its shell.
    Terminal,
    /// Where every file from outside the open vaults opens: `accent <file>`, or the file
    /// manager's Open With.
    Documents,
    /// Dedicated window for an explicit Open/Move to New Window; excluded from launch-window reuse.
    Apart,
}

/// Reload snapshot: key, unsaved session for unkeyed windows, size and state.
/// GTK4 leaves positioning to the compositor.
struct Reopen {
    key: WindowKey,
    session: Session,
    size: (i32, i32),
    maximized: bool,
    fullscreen: bool,
}

impl Reopen {
    fn of(app: &App) -> Self {
        Reopen {
            key: app.key.borrow().clone(),
            session: app.current_session(),
            size: app.window.default_size(),
            maximized: app.window.is_maximized(),
            fullscreen: app.window.is_fullscreen(),
        }
    }
}

/// What an `app.` action does, given the shell and the application it was fired at.
type AppAction = fn(&Rc<Shell>, &adw::Application);

/// How long a preference changed outside the dialog waits to be written, so a run of ring picks is
/// one write. The session's beat.
const CONFIG_WRITE: Duration = Duration::from_secs(1);

/// Record the tab drop target until AdwTabView's `create-window` handler can attach it.
///
/// Libadwaita temporarily detaches dragged pages and exposes no public reattach method.
/// Drop zones record the destination; `create-window` returns that view for libadwaita to use.
pub struct Landing {
    app: Rc<App>,
    pane: Rc<Pane>,
    zone: Zone,
}

/// Changed-input flags for expensive config work: theme restyling, font layout,
/// remote settings and accelerator rebuilds. Cheap switches can always be reapplied.
#[derive(Debug, Default, PartialEq)]
pub struct Changed {
    pub theme: bool,
    /// Which kinds of file the theme recolours: a re-render of each, as a theme change is.
    pub recolour: bool,
    pub font: bool,
    pub vault: bool,
    pub shortcuts: bool,
}

impl Changed {
    pub fn between(old: &Config, new: &Config) -> Changed {
        Changed {
            theme: old.theme != new.theme,
            recolour: old.recolour != new.recolour,
            font: old.editor_font != new.editor_font,
            // Every vault's entries, not this window's: a `hello` too many is harmless.
            vault: old.vaults != new.vaults
                || old.lsp != new.lsp
                || old.ghost_text != new.ghost_text
                || old.word_suggestions != new.word_suggestions,
            // What a focused shell is left is part of the table.
            shortcuts: old.shortcuts != new.shortcuts
                || old.forward_keys_to_terminal != new.forward_keys_to_terminal,
        }
    }
}

impl Shell {
    /// Register application actions that can open or close windows and therefore outlive them.
    ///
    /// Record actions in the active window's recent-command list so palette ranking learns
    /// from them, matching the window-action trampoline in [`install_actions`].
    pub fn install_app_actions(self: &Rc<Self>, gtk_app: &adw::Application) {
        // GNOME Shell offers New Window in the launcher's context menu only when it finds an
        // `app.new-window` action, the `new-window` desktop action, or one of the SingleWindow
        // keys. Both of the first two are provided: this is the one DESIGN.md's Keyboard rule
        // asks for, and the desktop file carries the other for a shell that reads it first.
        let actions: [(&str, AppAction); 5] = [
            ("open-local", Shell::choose_local),
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

    /// Quit by closing windows so `close-request` can save buffers or refuse failed saves.
    ///
    /// Direct application quit bypasses this protection. The process ends after the last window
    /// closes; record the command first so each window's session save includes it.
    fn quit(self: &Rc<Self>, gtk_app: &adw::Application) {
        for window in gtk_app.windows() {
            window.close();
        }
    }

    /// Resolve the active window state for application actions and their usage recording.
    pub(crate) fn app_at(&self, window: &gtk::Window) -> Option<Rc<App>> {
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

    /// Apply shared preferences to every window, comparing prior values to avoid unnecessary restyling.
    pub fn apply_config(&self, config: &Config) {
        let changed = Changed::between(&self.applied.replace(config.clone()), config);
        // The theme is the display's rather than a window's, so it goes on once.
        if changed.theme {
            theme::apply(config.theme);
        }
        if changed.recolour {
            theme::set_recolour(config.recolour);
        }
        // Cloned out of the borrow: applying a config reaches a long way into each window.
        let apps: Vec<Rc<App>> = self.windows.borrow().clone();
        for app in apps {
            app.apply_config(config, &changed);
        }
    }

    /// Debounce config writes process-wide so bursts share one write and window closure cannot cancel it.
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

    /// Watch external config edits; [`Config::reread`] distinguishes echoes of accent's own writes.
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

    /// Merge valid external config with pending local edits and apply only changes.
    /// Report invalid input once, retain current settings and suspend writes until it parses.
    fn config_file_changed(self: &Rc<Self>) {
        let reread = self.config.borrow().reread();
        match reread {
            Ok(None) => {}
            Ok(Some(taken)) => {
                self.config_broken.set(false);
                *self.config.borrow_mut() = taken.config.clone();
                self.apply_config(&taken.config);
                // A Preferences dialog up in any window still shows the values it was built with.
                let apps: Vec<Rc<App>> = self.windows.borrow().clone();
                for dialog in apps.iter().filter_map(|app| app.window.visible_dialog()) {
                    settings::reload(&dialog);
                }
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

    /// Close the vault window and present the start screen first.
    ///
    /// Closing releases the App-owned vault, worker and preview resources through the normal
    /// window-close path. Presenting first prevents last-window application shutdown; a later
    /// vault starts with default geometry.
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

    /// Release the shell's sole strong [`App`] reference once window close is certain.
    ///
    /// Widgets and in-flight queries can retain vault references until they finish. Handlers must
    /// not capture their own widgets strongly; `ACCENT_BENCH_CLOSE` checks closed
    /// window retention. Vault drop joins its worker and may wait for a running reconcile.
    fn forget(&self, window: &adw::ApplicationWindow) {
        let mut windows = self.windows.borrow_mut();
        let Some(i) = windows.iter().position(|app| &app.window == window) else {
            return;
        };
        let app = windows.remove(i);
        // Out of the borrow before the drop: `App` reaches a long way as it goes.
        drop(windows);
        drop(app);
        crate::malloc::trim_soon();
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
        // Every folder is a vault in a window of its own, opened first so that the files named
        // with it find it open. A file after a folder that is inside it is that vault's NOTE.
        let mut code = glib::ExitCode::SUCCESS;
        let mut files = Vec::new();
        let mut named = args[1..].iter().peekable();
        while let Some(arg) = named.next() {
            let Some(path) = resolve(command_line, arg) else {
                code = glib::ExitCode::FAILURE;
                continue;
            };
            if !path.is_dir() {
                files.push(path);
                continue;
            }
            let note = named.peek().and_then(|next| note_in(&path, next));
            if note.is_some() {
                named.next();
            }
            self.open_vault(gtk_app, path, note);
        }
        // The files open where the first belongs, which is how accent works as the system's PDF
        // viewer and text editor, and the rest as tabs beside it in the order named, since Files
        // hands over every file picked at once: from outside that window's vault where it does
        // not hold them.
        if let Some(app) = files
            .first()
            .and_then(|first| self.file_window(gtk_app, first))
        {
            app.window.present();
            app.launched.borrow_mut().clear();
            for (at, path) in files.iter().enumerate() {
                app.open_as(&app.key_for(path), Opened::Launched(at));
            }
        }
        code
    }

    /// Open the local folder picker immediately from any window, including a remote one.
    ///
    /// The selected vault gets its own window. The start screen remains for launches without
    /// a vault, avoiding an extra screen before an explicitly requested picker.
    fn choose_local(self: &Rc<Self>, gtk_app: &adw::Application) {
        let parent = gtk_app.active_window();
        let dialog = gtk::FileDialog::builder().title("Open Vault").build();
        let (shell, gtk_app) = (self.clone(), gtk_app.clone());
        dialog.select_folder(parent.as_ref(), gio::Cancellable::NONE, move |result| {
            // A dismissed chooser is an error here, and not one worth saying anything about.
            if let Some(path) = result.ok().and_then(|folder| folder.path()) {
                shell.open_from_start(&gtk_app, path);
            }
        });
    }

    /// Show the shared host/path form over the active window for Open Remote.
    ///
    /// Remote windows prefill their host and path and request folders immediately, making other
    /// host folders and mistyped paths easy to reach.
    fn choose_remote(self: &Rc<Self>, gtk_app: &adw::Application) {
        let Some(window) = gtk_app.active_window() else {
            return;
        };
        let at = self
            .app_at(&window)
            .and_then(|app| Some(app.vault()?.remote()?.url().clone()));
        let (shell, gtk_app) = (self.clone(), gtk_app.clone());
        let recent = self.config.borrow().recent_vaults.clone();
        start::connect_dialog(
            &window,
            at.as_ref(),
            &recent,
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
        let on_forget: start::Forget = Rc::new({
            let shell = Rc::downgrade(self);
            move |over: &gtk::Widget, key: &Path, removed: Box<dyn FnOnce()>| {
                if let Some(shell) = shell.upgrade() {
                    shell.remove_recent(over, key, removed);
                }
            }
        });
        let window = start::present(
            gtk_app,
            self.config.clone(),
            {
                let (shell, gtk_app) = (self.clone(), gtk_app.clone());
                move |root| shell.open_from_start(&gtk_app, root)
            },
            on_forget,
        );
        #[cfg(feature = "bench")]
        if std::env::var("ACCENT_BENCH_START").is_ok() {
            crate::bench::bench_start(&window);
        }
        self.start.set(Some(&window));
    }

    /// Remove a recent entry and call `removed` when its row may disappear.
    ///
    /// Vault entries need no teardown. Terminal sessions require confirmation and shell shutdown
    /// before deleting state. Open sessions close unnamed, ending shells without saving state.
    pub(crate) fn remove_recent(&self, over: &gtk::Widget, key: &Path, removed: Box<dyn FnOnce()>) {
        let Some(name) = terminal::session_name(key) else {
            start::forget_vault(&self.config, key);
            return removed();
        };
        let open = self.app_for(key).map(|app| Rc::downgrade(&app));
        let (config, key) = (self.config.clone(), key.to_path_buf());
        dialogs::confirm(
            over,
            &format!("Remove session {name}?"),
            "Its shells end now.",
            "Remove",
            true,
            move || {
                match open.and_then(|app| app.upgrade()) {
                    Some(app) => {
                        app.forget_session(&key);
                        app.window.close();
                    }
                    None => {
                        terminal::end_stored(&key);
                        start::forget_vault(&config, &key);
                    }
                }
                removed();
            },
        );
    }

    /// Open the chosen vault and close the start screen through the shell to avoid callback ownership cycles.
    fn open_from_start(self: &Rc<Self>, gtk_app: &adw::Application, root: PathBuf) {
        self.open_vault(gtk_app, root, None);
        if let Some(window) = self.start.upgrade() {
            window.close();
        }
    }

    /// Open one window per vault or terminal-session key, raising an existing window if present.
    ///
    /// `root` is a directory, `ssh://` address or `terminal://<name>`. Sharing one keyed window
    /// avoids duplicate index, watcher and session ownership. For sessions, `note` adds a shell
    /// in the named directory.
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

    /// Build and register window `key` through one ownership path with shared teardown.
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
            let (shell, gtk_app) = (Rc::downgrade(self), gtk_app.clone());
            move |window| {
                if let Some(shell) = shell.upgrade() {
                    // Read while the window is still whole: what a reload opens again.
                    let again = shell
                        .app_at(window.upcast_ref())
                        .filter(|app| app.reloading())
                        .map(|app| Reopen::of(&app));
                    shell.forget(window);
                    if let Some(again) = again {
                        shell.reopen_soon(&gtk_app, again);
                    }
                }
                glib::Propagation::Proceed
            }
        });
        self.windows.borrow_mut().push(app.clone());
        Some(app)
    }

    /// Reopen after the old vault worker/watcher drops ([`Self::forget`]); hold the application through the gap.
    fn reopen_soon(self: &Rc<Self>, gtk_app: &adw::Application, again: Reopen) {
        let hold = gtk_app.hold();
        let (shell, gtk_app) = (self.clone(), gtk_app.clone());
        glib::idle_add_local_once(move || {
            shell.reopen(&gtk_app, again);
            drop(hold);
        });
    }

    fn reopen(self: &Rc<Self>, gtk_app: &adw::Application, again: Reopen) {
        let Some(app) = self.add_window(gtk_app, again.key.clone(), None) else {
            // Nothing to open again, a vault gone from under it: the start screen, as Close Vault
            // leaves the reader on.
            return self.start_screen(gtk_app);
        };
        let (width, height) = again.size;
        app.window.set_default_size(width, height);
        if again.maximized {
            app.window.maximize();
        }
        if again.fullscreen {
            app.window.fullscreen();
        }
        // A vault and a named session restore from the state file the close has just written. The
        // other windows keep none, so theirs is handed over, after the window's own start, which
        // restores nothing for them.
        if again.key.saved_as().is_none() {
            glib::idle_add_local_once(move || app.restore(&again.session));
        }
    }

    /// Find the source window/document when libadwaita transfers a tab across windows.
    fn owner_of(&self, page: &adw::TabPage) -> Option<(Rc<App>, Doc)> {
        self.windows
            .borrow()
            .iter()
            .find_map(|app| Some((app.clone(), app.doc_for_page(page)?)))
    }

    /// Inspect an unknown attached page on an idle to distinguish transfers from local opens.
    ///
    /// Libadwaita still holds the page during its drop handler, and newly built pages attach
    /// before entering `docs`; deferring allows both operations to finish.
    fn adopt_soon(self: &Rc<Self>, into: &Rc<App>, page: &adw::TabPage) {
        let (shell, into, page) = (Rc::downgrade(self), Rc::downgrade(into), page.clone());
        glib::idle_add_local_once(move || {
            if let (Some(shell), Some(into)) = (shell.upgrade(), into.upgrade()) {
                shell.adopt_page(&into, &page);
            }
        });
    }

    /// A drag out of `from`'s tree starting (`on`) or over. Every window's drop sheets go up with
    /// it, as a tab drag's do: over another window's panes the row is refused, which leaves no
    /// note view there to take its path as text.
    pub fn tree_drag(&self, from: &Rc<App>, on: bool) {
        *self.tree_drag.borrow_mut() = on.then(|| Rc::downgrade(from));
        for app in self.windows.borrow().iter() {
            app.set_drop_active(on);
        }
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

    /// Finish a page attachment: apply its drop placement and adopt cross-window bookkeeping.
    ///
    /// Defer to an idle because libadwaita is still handling the drag and selecting the page;
    /// reparenting its emitting pane immediately would violate GTK assertions.
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

    /// Move a dragged tab between vault windows by saving and reopening its absolute file path.
    ///
    /// One vault has one window, so the receiving window adopts it as a loose file without index
    /// or backlinks. Save first because the destination opens disk bytes, matching focus-change saves.
    fn adopt_page(self: &Rc<Self>, into: &Rc<App>, page: &adw::TabPage) {
        // Gone again, or one of `into`'s own that had not reached `docs` when it was attached.
        if into.pane_of(page).is_none() || into.doc_for_page(page).is_some() {
            return;
        }
        let Some((from, doc)) = self.owner_of(page) else {
            return tracing::debug!("a tab in no window's bookkeeping; left where it is");
        };
        let path = match written_out(&from, &doc) {
            Ok(path) => path,
            Err(why) => return return_page(into, &from, page, &why),
        };
        // Opened before the old page goes, so a pane that the drop has just split off never
        // stands empty and closes itself out from under the note arriving in it.
        into.open_as(&into.key_for(&path), kept_as(&from, page));
        from.forget_page(page);
        into.close_page(page);
    }

    /// Open `path` in a dedicated standalone window, even if another window already shows it.
    pub fn open_apart(
        self: &Rc<Self>,
        gtk_app: &adw::Application,
        path: &Path,
        how: Opened,
    ) -> Option<Rc<App>> {
        let app = self.add_window(gtk_app, WindowKey::Loose(Loose::Apart), None)?;
        app.window.present();
        app.open_as(&app.key_for(path), how);
        Some(app)
    }

    /// Save and reopen `page` in a dedicated standalone window ([`Self::adopt_page`]).
    pub fn move_apart(
        self: &Rc<Self>,
        gtk_app: &adw::Application,
        from: &Rc<App>,
        page: &adw::TabPage,
    ) {
        let Some(doc) = from.doc_for_page(page) else {
            return;
        };
        let path = match written_out(from, &doc) {
            Ok(path) => path,
            Err(why) => return from.toast(&why),
        };
        if self
            .open_apart(gtk_app, &path, kept_as(from, page))
            .is_some()
        {
            from.close_page(page);
        }
    }

    /// The open window remembered by `key`, a vault's key.
    pub(crate) fn app_for(&self, key: &Path) -> Option<Rc<App>> {
        self.windows
            .borrow()
            .iter()
            .find(|app| app.key.borrow().saved_as() == Some(key))
            .cloned()
    }

    /// The window `path` opens in: the one whose vault contains it, or the one kept for
    /// documents that are in no vault — never the one `accent --terminal` opened.
    fn file_window(self: &Rc<Self>, gtk_app: &adw::Application, path: &Path) -> Option<Rc<App>> {
        let inside = self
            .windows
            .borrow()
            .iter()
            .find(|app| {
                app.key
                    .borrow()
                    .vault()
                    .is_some_and(|root| path.starts_with(root))
            })
            .cloned();
        inside.or_else(|| self.loose_window(gtk_app, Loose::Documents))
    }

    /// Reuse or create the unkeyed window of `kind` so launches group loose documents and terminals separately.
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

/// The flags [`Shell::command_line`] reads: every other option is turned away before the launch
/// ([`local_command_line`]), and none of these is a path ([`terminal_cwd`]).
const FLAGS: [&str; 3] = ["--terminal", "-t", "--new-window"];

/// What `accent --help` prints: every form [`Shell::command_line`] takes.
const USAGE: &str = "\
Usage:
  accent [PATH [NOTE]]...       Open vaults and files
  accent ADDRESS [NOTE]         Open a vault on another machine
  accent -t, --terminal [DIR]   Open a shell at DIR, or at home
  accent terminal://NAME [DIR]  Open terminal session NAME, and a shell at DIR
  accent --new-window           Show the start screen
  accent --render-diagram PAGE  Draw page PAGE of a diagram on stdin as PNG
  accent -h, --help             Print this help
  accent --version              Print the version

PATH is a folder, opened as a vault in a window of its own, or a file (a note,
PDF, image, diagram or any text). NOTE, a file inside the folder before it,
named from there or in full, opens in that vault. The files open as tabs in the
order named, all in the window of the open vault that holds the first, else in
a window without a vault. Without PATH the last vault opens, or the start
screen if there is none. ADDRESS is ssh://[USER@]HOST[:PORT]/PATH, a vault on
that host, and its NOTE a path in it. DIR may be an ssh:// address, for a shell
on that host. A session NAME not seen before is made. PAGE counts from 0, and
the PNG goes to stdout.
";

/// Handle help, version and unknown options before forwarding to the running
/// instance, whose stdout belongs to a different caller. `None` continues launch.
pub fn local_command_line(
    args: impl Iterator<Item = std::ffi::OsString>,
) -> Option<glib::ExitCode> {
    for arg in args.skip(1) {
        match arg.to_str() {
            Some("-h" | "--help") => print!("{USAGE}"),
            Some("--version") => println!("accent {}", env!("CARGO_PKG_VERSION")),
            Some(option) if option.starts_with('-') && !FLAGS.contains(&option) => {
                eprintln!("accent: unknown option {option}; see accent --help");
                return Some(glib::ExitCode::FAILURE);
            }
            _ => continue,
        }
        return Some(glib::ExitCode::SUCCESS);
    }
    None
}

/// First non-flag terminal argument, or `None`; callers validate and resolve the directory.
fn terminal_cwd(args: &[std::ffi::OsString]) -> Option<&std::ffi::OsStr> {
    args.iter()
        .skip(1)
        .map(|arg| arg.as_os_str())
        .find(|arg| !arg.to_str().is_some_and(|arg| FLAGS.contains(&arg)))
}

/// Resolve launch paths against the invoking process's cwd, since single-instance forwarding changes context.
fn resolve(command_line: &gio::ApplicationCommandLine, arg: &std::ffi::OsStr) -> Option<PathBuf> {
    let Some(path) = command_line.create_file_for_arg(arg).path() else {
        // `printerr_literal` needs glib 2.80, which this build does not enable; a local
        // invocation is the only one that has a terminal to print to anyway.
        eprintln!("cannot resolve: {}", arg.to_string_lossy());
        return None;
    };
    path.canonicalize()
        .inspect_err(|e| eprintln!("cannot open {}: {e}", path.display()))
        .ok()
}

/// Resolve a NOTE argument within its named vault; `None` leaves it an independent path.
fn note_in(root: &Path, arg: &std::ffi::OsStr) -> Option<String> {
    let path = root.join(arg);
    let rel = path.strip_prefix(root).ok()?.to_str()?.to_string();
    path.is_file().then_some(rel)
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

/// Whether another window could open what `doc` holds in `app`. A shell is a running process, a
/// diff is a view of two texts and a remote vault's file is on its host: none is a file here.
pub fn movable(app: &App, doc: &Doc) -> bool {
    !doc.is_transient() && !app.on_host(&doc.key())
}

/// Save `doc` before another window reopens its file, preventing lost buffered edits; return errors without moving.
fn written_out(from: &Rc<App>, doc: &Doc) -> Result<PathBuf, String> {
    let key = doc.key();
    if !movable(from, doc) {
        return Err("This tab cannot move between windows.".to_string());
    }
    if let Some(tab) = doc.tab().filter(|tab| tab.save.modified.get())
        && let Err(e) = from.flush_tab(tab)
    {
        return Err(format!("Save failed: {e}"));
    }
    // A diagram the same way, a label still being typed going into its cell first.
    if let Doc::Diagram(d) = doc {
        d.finish_label();
        if let Err(e) = from.flush_diagram(d) {
            return Err(format!("Save failed: {e}"));
        }
    }
    Ok(match doc::is_loose_key(&key) {
        true => PathBuf::from(&key),
        false => from.root().join(&key),
    })
}

/// How a tab moved to another window opens there: a pinned tab pinned there too, as one moved
/// into another pane is.
fn kept_as(from: &App, page: &adw::TabPage) -> Opened {
    match from.is_pinned(page) {
        true => Opened::Pinned,
        false => Opened::Kept,
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
    fn a_note_after_a_folder_is_a_file_inside_it() {
        // `std::env::temp_dir`, as `start`'s tests: apps/gtk has no `tempfile`.
        let vault = std::env::temp_dir().join(format!("accent-note-{}", std::process::id()));
        std::fs::create_dir_all(vault.join("Daily")).unwrap();
        std::fs::write(vault.join("Daily/a.md"), "a").unwrap();
        let note = |arg: &Path| note_in(&vault, arg.as_os_str());

        // Named from the vault's root, as `accent ~/Notes Daily/a.md` always was, or in full.
        assert_eq!(note(Path::new("Daily/a.md")).as_deref(), Some("Daily/a.md"));
        assert_eq!(
            note(&vault.join("Daily/a.md")).as_deref(),
            Some("Daily/a.md")
        );
        // A folder inside it is a vault of its own, and neither a file elsewhere nor one that is
        // not there is the vault's.
        assert_eq!(note(&vault.join("Daily")), None);
        assert_eq!(note(Path::new("/etc/hostname")), None);
        assert_eq!(note(Path::new("b.md")), None);

        std::fs::remove_dir_all(&vault).unwrap();
    }

    #[test]
    fn the_help_names_every_option() {
        let words: Vec<&str> = USAGE
            .split(|c: char| c.is_whitespace() || c == ',')
            .collect();
        for option in FLAGS.iter().chain(&["-h", "--help", "--version"]) {
            assert!(words.contains(option), "--help does not name {option}");
        }
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
        // Ghost text and word suggestions are carried to the vault alongside its settings.
        let ghost = Config {
            ghost_text: false,
            ..Config::default()
        };
        let words = Config {
            word_suggestions: false,
            ..Config::default()
        };
        for new in [ghost, words] {
            assert_eq!(
                Changed::between(&old, &new),
                Changed {
                    vault: true,
                    ..Changed::default()
                }
            );
        }
    }
}
