//! One process, one config, one window per vault: the `Shell` owns every window's `App`, answers
//! the command line and moves tabs between windows.

use super::*;

/// One process, one config, one window per vault.
pub struct Shell {
    pub config: Rc<RefCell<Config>>,
    /// The open vaults, and the only strong reference to each window's state: an entry is dropped
    /// in `forget` when the window closes, which is what releases the vault and its worker thread.
    /// `None` for the one window opened on files rather than on a folder.
    pub windows: RefCell<Vec<(Option<PathBuf>, Rc<App>)>>,
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

/// What an `app.` action does, given the shell and the application it was fired at.
type AppAction = fn(&Rc<Shell>, &adw::Application);

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

    /// Put a config into effect in every window. There is one config per process, so a preference
    /// changed in one window — in its dialog or anywhere else (`App::config_changed`) — is the
    /// same preference in all of them.
    pub fn apply_config(&self, config: &Config) {
        // The theme is the display's rather than a window's, so it goes on once.
        theme::apply(config.theme);
        // Cloned out of the borrow: applying a config reaches a long way into each window.
        let apps: Vec<Rc<App>> = self
            .windows
            .borrow()
            .iter()
            .map(|(_, app)| app.clone())
            .collect();
        for app in apps {
            app.apply_config(config);
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
        let Some(i) = windows.iter().position(|(_, app)| &app.window == window) else {
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
            start::connect_dialog(&app.window, Some(remote.url()), move |address| {
                let root = PathBuf::from(address);
                let replace = root != here;
                shell.open_from_start(&gtk_app, root);
                // Once the new one is up, so the application never stands at zero windows.
                if let Some(window) = from.upgrade().filter(|_| replace) {
                    window.close();
                }
            });
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
        start::connect_dialog(&window, None, move |address| {
            shell.open_from_start(&gtk_app, PathBuf::from(address));
        });
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
    /// another machine. The two are one list, one rule and one window each.
    pub fn open_vault(
        self: &Rc<Self>,
        gtk_app: &adw::Application,
        root: PathBuf,
        note: Option<String>,
    ) {
        if let Some(app) = self.app_for(&root) {
            app.window.present();
            if let Some(note) = note {
                app.open_path(&note);
            }
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

    fn app_for(&self, root: &Path) -> Option<Rc<App>> {
        let windows = self.windows.borrow();
        let (_, app) = windows
            .iter()
            .find(|(path, _)| path.as_deref() == Some(root))?;
        Some(app.clone())
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
    pub fn loose_window(self: &Rc<Self>, gtk_app: &adw::Application) -> Option<Rc<App>> {
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
        // The bare form has no directory to offer, so the window decides.
        assert_eq!(cwd(&["accent", "--terminal"]), None);
        // argv[0] is the program, never the path.
        assert_eq!(cwd(&["accent"]), None);
        // The other flag a command line can carry is not a path either.
        assert_eq!(cwd(&["accent", "--new-window", "--terminal"]), None);
    }
}
