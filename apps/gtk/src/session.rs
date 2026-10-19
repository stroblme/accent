//! Session state for the palette, recent items and window restoration.

use super::*;
use crate::work::or_empty;
use accent_api::SymbolName;
use accent_api::git::Sides;

/// What the palette lists before the user types anything.
const RECENT_FILES: usize = 50;

/// Limit recently used commands in the session while retaining common actions.
const RECENT_COMMANDS: usize = 20;

/// Session-write debounce, avoiding noisy writes while bounding recoverable state loss.
pub(crate) const SESSION: Duration = Duration::from_secs(1);

/// What the palette lists, kept warm so the dialog never waits on the vault.
#[derive(Default)]
pub struct Corpus {
    /// Files followed by unresolved note targets, ranked together with existing files winning ties.
    /// Gitignored folders use the last explicit walk after indexed files.
    files: palette::Files,
    /// Live file-list handle for the open palette, refreshing ranks when delayed listings arrive.
    open: Option<palette::Refill>,
    tags: Rc<Vec<String>>,
    /// The index's recently changed notes, up to [`RECENT_FILES`]: one refresh behind at worst,
    /// which a list of what changed lately can afford.
    recent: Rc<Vec<String>>,
    /// Whether `@` has been typed in this window, so that Go to Symbol's list (`files.symbols`)
    /// is held and read again where the files are: as the palette opens, a walk ends or the link
    /// comes back.
    symbols_wanted: bool,
}

impl App {
    /// Refresh palette files off-thread, retaining an immediately available list.
    ///
    /// Reconciliation and palette opening refresh indexed files. Only opening requests `relist`
    /// of gitignored folders because that requires a full vault walk.
    pub fn refresh_corpus(self: &Rc<Self>, relist: bool) {
        let Some(vault) = self.vault().cloned() else {
            return;
        };
        // A file from outside the vault has no path in it to ask about.
        let opened: Vec<String> = self
            .recent_files
            .borrow()
            .iter()
            .filter(|rel| !doc::is_loose_key(rel))
            .cloned()
            .collect();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let loaded = crate::work::off_thread("corpus", move || {
                // Never widened: Go to File has no All toggle, and the tree is where an ignored
                // file is reached, dimmed but listed.
                let mut files = or_empty("listing the files", vault.file_paths(false));
                let ignored = palette::with_ignored(
                    &mut files,
                    or_empty("listing the ignored notes", vault.ignored_notes(relist)),
                );
                let real = files.len();
                // What this window opened and is gone since: deleted, or renamed where no event
                // said so. A typed query ranks the history with the files, so a gone one would
                // be offered again. Only the files the index does not list are asked about, and
                // only an answer of "nothing there" drops one: a link that is down says nothing.
                let gone: Vec<String> = {
                    let listed: HashSet<&str> = files.iter().map(String::as_str).collect();
                    opened
                        .into_iter()
                        .filter(|rel| !listed.contains(rel.as_str()))
                        .filter(|rel| matches!(vault.stat(rel), Ok(None)))
                        .collect()
                };
                // A question the vault cannot answer leaves the files alone, and the aliases none.
                files.extend(or_empty("listing the missing notes", vault.missing_notes()));
                let aliases = or_empty("listing the aliases", vault.note_aliases());
                (
                    (files, real, ignored, gone, aliases),
                    or_empty("listing the tags", vault.tags())
                        .into_iter()
                        .map(|(name, _)| name)
                        .collect::<Vec<_>>(),
                    or_empty("listing the recent files", vault.recent_files(RECENT_FILES)),
                )
            })
            .await;
            if let (Some(app), Some(((files, real, ignored, gone, aliases), tags, recent))) =
                (weak.upgrade(), loaded)
            {
                app.recent_files
                    .borrow_mut()
                    .retain(|rel| !gone.contains(rel));
                let files = palette::Files {
                    paths: Rc::new(files),
                    real,
                    ignored: Rc::new(ignored),
                    aliases: Rc::new(aliases),
                    symbols: app.corpus.borrow().files.symbols.clone(),
                };
                {
                    let mut corpus = app.corpus.borrow_mut();
                    corpus.files = files;
                    corpus.tags = Rc::new(tags);
                    corpus.recent = Rc::new(recent);
                }
                app.refill_palette();
            }
        });
    }

    /// Read Go to Symbol's list off the main loop, on the host for a remote vault, and hand it
    /// to an open palette as the files are, once `@` has been typed in this window.
    pub fn refresh_symbols(self: &Rc<Self>) {
        let Some(vault) = self.vault().cloned() else {
            return;
        };
        if !self.corpus.borrow().symbols_wanted {
            return;
        }
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let read = crate::work::off_thread("symbols", move || vault.symbol_names()).await;
            let (Some(app), Some(read)) = (weak.upgrade(), read) else {
                return;
            };
            let symbols = Rc::new(or_empty("listing the symbols", read));
            app.corpus.borrow_mut().files.symbols = Some(symbols);
            app.refill_palette();
        });
    }

    /// Hand an open palette the lists as they are now. Taken out while the dialog ranks:
    /// nothing it runs may find the corpus borrowed.
    fn refill_palette(&self) {
        let (files, open) = {
            let mut corpus = self.corpus.borrow_mut();
            (corpus.files.clone(), corpus.open.take())
        };
        if let Some(refill) = open {
            refill(files);
            self.corpus.borrow_mut().open.get_or_insert(refill);
        }
    }

    pub fn palette(self: &Rc<Self>, initial: palette::Mode) {
        // This one shows what is already here, and takes the fresh files when they land. Only Go
        // to File walks the gitignored folders again for its notes.
        self.refresh_corpus(initial == palette::Mode::Files);
        self.refresh_symbols();
        // Two answers to "recent": what this window opened, and what changed on disk. The first
        // is what the user means, so it leads and the index's mtime list fills the page below it.
        let mru = self.recent_files.borrow().clone();
        let mut recent = mru.clone();
        for rel in self.corpus.borrow().recent.iter() {
            if !recent.contains(rel) {
                recent.push(rel.clone());
            }
        }
        // Pruned before the config is borrowed for the rest: dropping a gone folder writes it.
        let vaults = start::recent_vaults(&self.config);
        let used = self.recent_commands.borrow();
        // What the SyncTeX pair cannot run for over a LaTeX build without a SyncTeX file, and
        // Invert Image Colours over an animation.
        let (source, show) = self.synctex_missing(self.active_doc().as_ref());
        let animation = self.active_animation();
        let why = |action: &str| match action {
            "win.pdf-go-to-source" if source => Some(synctex::NO_SYNCTEX.to_string()),
            "win.show-in-pdf" if show => Some(synctex::NO_SYNCTEX.to_string()),
            "win.image-invert" if animation => Some(crate::look::ANIMATED.to_string()),
            _ => None,
        };
        let config = self.config.borrow();
        let key = self.key.borrow();
        let sources = palette::Sources {
            recent,
            mru,
            // Every file, not only the notes: a source file has to be reachable by name too.
            files: self.corpus.borrow().files.clone(),
            commands: ACTIONS
                .iter()
                .map(|(action, label, _)| palette::Item::Command {
                    action: action.to_string(),
                    label: label.to_string(),
                    accels: accels_for(&config, action),
                    recent: used.iter().position(|a| a == action),
                    why: why(action),
                })
                .collect(),
            tags: self.corpus.borrow().tags.clone(),
            // Filtered here rather than in the dialog: the window is the only thing that knows
            // which vault it is already on, and a row that raises the window it was picked from
            // would be the one row in the list that does nothing.
            vaults: start::other_vaults(&vaults, self.vault().map(|v| v.key()).or(key.saved_as())),
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
            // Go to Symbol's list, read the first time `@` is typed and refilled as the files are.
            on_symbols: Box::new({
                let app = Rc::downgrade(self);
                move || {
                    let Some(app) = app.upgrade() else {
                        return;
                    };
                    let first =
                        !std::mem::replace(&mut app.corpus.borrow_mut().symbols_wanted, true);
                    if first {
                        app.refresh_symbols();
                    }
                }
            }),
            // The start screen's own removal, so one list is written one way.
            on_forget: Box::new({
                let app = Rc::downgrade(self);
                move |key: &str, removed: Box<dyn FnOnce()>| {
                    if let Some(app) = app.upgrade()
                        && let Some(shell) = app.shell.upgrade()
                    {
                        shell.remove_recent(app.window.upcast_ref(), Path::new(key), removed);
                    }
                }
            }),
        };
        drop(key);
        drop(config);
        drop(used);
        let refill = palette::present(
            &self.window,
            initial,
            sources,
            glib::clone!(
                #[weak(rename_to = app)]
                self,
                move |item: &palette::Item| match item {
                    palette::Item::File(rel)
                    | palette::Item::Ignored(rel)
                    | palette::Item::Alias { rel, .. } => app.open_as(rel, Opened::Picked),
                    // Followed as the link would be: New File, unless the note has been written
                    // since the list was read.
                    palette::Item::Missing(rel) => app.open_target(rel),
                    palette::Item::Command { action, .. } => {
                        let _ = WidgetExt::activate_action(&app.window, action, None);
                    }
                    palette::Item::Tag(tag) => app.show_tag(tag),
                    palette::Item::Symbol(symbol) => app.goto_symbol(symbol),
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
        self.corpus.borrow_mut().open = Some(refill);
    }

    /// The Info pane's Tags section with `tag` picked, the sidebar brought up for it: a tag
    /// picked in the palette, or clicked in the preview.
    pub(crate) fn show_tag(&self, tag: &str) {
        self.sidebar_column.set_visible(true);
        if let Some(sidebar) = self.sidebar.get() {
            sidebar.show_tag(tag);
        }
    }

    /// Open the file a declaration is in, as Go to File does, with the caret on its name.
    fn goto_symbol(self: &Rc<Self>, symbol: &SymbolName) {
        self.mark();
        let name = symbol.name.clone();
        let (first, last) = (
            symbol.line.saturating_sub(1),
            symbol.end_line.saturating_sub(1),
        );
        self.with_tab(&symbol.rel_path, Opened::Picked, "go to", move |_, tab| {
            let text = tab.text();
            let lines: Vec<&str> = text.lines().collect();
            tab.goto_pos(lang::name_at(&lines, first, last, &name));
        });
    }

    /// Remember that this file was just looked at. Called from `sync_active`, so it covers
    /// opening a file, switching to its tab and coming back to the window.
    pub fn file_used(self: &Rc<Self>, rel: &str) {
        if self.recent_files.borrow().first().is_some_and(|r| r == rel) {
            return;
        }
        accent_core::config::touch(&mut self.recent_files.borrow_mut(), rel, RECENT_FILES);
        self.save_session_soon();
    }

    /// Remember a command by full action name, whichever surface fired it.
    pub fn command_used(self: &Rc<Self>, action: &str) {
        accent_core::config::touch(
            &mut self.recent_commands.borrow_mut(),
            action,
            RECENT_COMMANDS,
        );
        self.save_session_soon();
    }

    pub fn save_session_soon(self: &Rc<Self>) {
        self.session.call_once(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move || app.save_session()
        ));
    }

    pub fn save_session(&self) {
        if !self.keeps_session() {
            return;
        }
        self.write_session(&self.current_session());
    }

    /// The session as this window stands, over the one it last wrote: what a save writes, and
    /// what Reload Window carries into the window that takes this one's place.
    pub(crate) fn current_session(&self) -> Session {
        let stored = self.stored_session();
        let (sidebar, width) = self.sidebar_saved();
        let session = Session {
            open: self
                .docs
                .borrow()
                .iter()
                .filter(|d| d.is_kept())
                .map(|d| d.key())
                .collect(),
            active: self.active_doc().filter(Doc::is_kept).map(|d| d.key()),
            layout: self.layout(),
            // Neither presentation nor a narrow window is session state, so a sidebar either one
            // hid is saved as it was.
            sidebar,
            sidebar_width: sidebar_width(width),
            info: self
                .sidebar
                .get()
                .map(|sidebar| sidebar.info_saved())
                .unwrap_or_default(),
            code: self
                .sidebar
                .get()
                .map(|sidebar| sidebar.code_saved())
                .unwrap_or_default(),
            view: self.mode.get().name().to_string(),
            zoom: self.zoom.get(),
            recent_files: self.recent_files.borrow().clone(),
            recent_commands: self.recent_commands.borrow().clone(),
            // Merged rather than replaced: a PDF closed earlier in this session keeps the place
            // it was left at, which is the whole point of remembering it.
            pdf: {
                let mut places = stored.pdf.clone();
                for pdf in self.pdfs() {
                    places.insert(pdf.key(), pdf.place());
                }
                places
            },
            diagram: {
                let mut places = stored.diagram.clone();
                for d in self.diagrams() {
                    places.insert(d.key(), d.place());
                }
                places
            },
            // Only the open ones: a shell that was closed has ended, and has nowhere to go back to.
            terminals: self
                .terminals()
                .iter()
                .filter_map(|t| Some((t.key(), ShellPlace { at: t.at()? })))
                .collect(),
            pinned: self
                .pinned
                .borrow()
                .iter()
                .filter_map(|page| self.doc_for_page(page))
                .map(|d| d.key())
                .collect(),
            web_images: {
                let mut keys: Vec<String> = self.web_images.borrow().iter().cloned().collect();
                keys.sort();
                keys
            },
            compared: self
                .git
                .get()
                .map(|git| git.kept().into_iter().collect())
                .unwrap_or_default(),
            breakpoints: self.debugger.saved(self),
            watches: self.debugger.watches(),
        };
        match self.restored.get() {
            true => session,
            false => unrestored(stored, session),
        }
    }

    /// Whether a vault or named terminal window persists a session.
    /// Loose-file and unnamed-shell windows have no persistent session key.
    pub(crate) fn keeps_session(&self) -> bool {
        self.key.borrow().saved_as().is_some()
    }

    /// Save the active note/diagram, or save a shell-only window's session, prompting for its first name.
    pub(crate) fn save(self: &Rc<Self>) {
        if self.active().is_some() || self.active_diagram().is_some() {
            return self.save_active();
        }
        self.save_shells();
    }

    /// Save Session for shell windows, naming it once. Vault windows already save session changes automatically.
    pub(crate) fn save_shells(self: &Rc<Self>) {
        let named = match &*self.key.borrow() {
            shell::WindowKey::Terminal(key) => terminal::session_name(key).map(str::to_string),
            key if key.is_terminal() => None,
            _ => return,
        };
        match named {
            Some(name) => {
                self.save_session();
                self.toast(&format!("Saved the session as {name}"));
            }
            None => self.save_session_dialog(),
        }
    }

    /// The name this window's shells are saved under, asked once: from then on the window
    /// writes its session the way a vault's does.
    fn save_session_dialog(self: &Rc<Self>) {
        let entry = dialogs::name_entry("Session name", "");
        let form = dialogs::form();
        form.append(&entry);
        let dialog = dialogs::name_dialog("Save Session", "Save", &form);
        let typed = entry.clone();
        dialogs::choose(
            &dialog,
            Some(&self.window),
            glib::clone!(
                #[weak(rename_to = app)]
                self,
                move |response| {
                    if response == dialogs::CONFIRM {
                        app.save_session_as(typed.text().trim());
                    }
                }
            ),
        );
        dialogs::focus_entry(&entry, |entry| entry.select_region(0, -1));
    }

    /// Save as session `name`, confirming replacement and ending old shells absent from this window.
    fn save_session_as(self: &Rc<Self>, name: &str) {
        let key = terminal::session_key(name);
        if terminal::session_name(&key).is_none() {
            return self.toast("A session name cannot be empty or hold a /");
        }
        let taken = self.shell.upgrade().and_then(|shell| shell.app_for(&key));
        if taken.is_some_and(|app| !Rc::ptr_eq(&app, self)) {
            return self.toast(&format!("{name} is open in another window"));
        }
        if self.key.borrow().saved_as() == Some(key.as_path())
            || !accent_core::config::state_path(&key).is_file()
        {
            return self.name_session(name);
        }
        let mine: HashSet<String> = self.terminals().iter().map(|t| t.key()).collect();
        let ending = Session::load(&key)
            .terminals
            .keys()
            .filter(|id| !mine.contains(*id))
            .count();
        let body = match ending {
            0 => "This window takes its place.".to_string(),
            1 => "Its shell ends, and this window takes its place.".to_string(),
            n => format!("Its {n} shells end, and this window takes its place."),
        };
        let name = name.to_string();
        dialogs::confirm(
            &self.window,
            &format!("Replace session {name}?"),
            &body,
            "Replace",
            true,
            glib::clone!(
                #[weak(rename_to = app)]
                self,
                move || app.name_session(&name)
            ),
        );
    }

    /// Key this window by the session `name` and write it: into the recent list, and into the
    /// state file every later change goes to.
    fn name_session(self: &Rc<Self>, name: &str) {
        let key = terminal::session_key(name);
        if self.key.borrow().saved_as() != Some(key.as_path()) {
            // A session of that name written before is replaced, so the shells it held and this
            // window does not have would be held with nothing left to open them again.
            let mine: HashSet<String> = self.terminals().iter().map(|t| t.key()).collect();
            for (id, place) in Session::load(&key).terminals {
                if !mine.contains(&id) {
                    terminal::end(&id, &place.at);
                }
            }
            self.rekey(shell::WindowKey::Terminal(key.clone()));
            self.config.borrow_mut().touch_recent(&key);
            settings::save(&self.config.borrow());
            self.title.set_title(name);
            self.window.set_title(Some(name));
        }
        self.save_session();
        self.toast(&format!("Saved the session as {name}"));
    }

    /// Remember this window by `key` from now on, and offer in its menu what that key can do: the
    /// menu is built for the key a window opens on, and Close Session is a named session's alone.
    fn rekey(&self, key: shell::WindowKey) {
        self.menu.set_menu_model(Some(&primary_menu(&key)));
        *self.key.borrow_mut() = key;
    }

    /// Reload through normal window close so saves, confirmations, session writes and shell detachment run.
    /// After successful close, Shell::reopen restores it; [`App::keep_open`] cancels the reload.
    pub(crate) fn reload(&self) {
        self.reloading.set(true);
        self.window.close();
    }

    /// A close was given up — a question about unsaved edits cancelled, git left to finish or
    /// failed — so the window stays, and a reload that asked for the close is off.
    pub(crate) fn keep_open(&self) {
        self.reloading.set(false);
    }

    /// Whether the close under way is a reload's.
    pub(crate) fn reloading(&self) -> bool {
        self.reloading.get()
    }

    /// Confirm and permanently close a terminal session, ending shells and removing state and recents.
    /// Present the start screen afterwards, matching Close Vault.
    pub(crate) fn close_session(self: &Rc<Self>) {
        let named = match &*self.key.borrow() {
            shell::WindowKey::Terminal(key) => {
                terminal::session_name(key).map(|name| (key.clone(), name.to_string()))
            }
            _ => None,
        };
        let Some((key, name)) = named else {
            return;
        };
        let body = match self.terminals().len() {
            0 => "It leaves the recent list.".to_string(),
            1 => "Its shell ends, and it leaves the recent list.".to_string(),
            n => format!("Its {n} shells end, and it leaves the recent list."),
        };
        dialogs::confirm(
            &self.window,
            &format!("Close the session {name}?"),
            &body,
            "Close Session",
            true,
            glib::clone!(
                #[weak(rename_to = app)]
                self,
                move || app.end_session(&key)
            ),
        );
    }

    fn end_session(self: &Rc<Self>, key: &std::path::Path) {
        self.forget_session(key);
        let _ = WidgetExt::activate_action(&self.window, "app.close-vault", None);
    }

    /// Clear the session name before closing so shells end and no state is saved.
    /// Then remove its recent entry and state through start::forget_vault.
    pub(crate) fn forget_session(&self, key: &std::path::Path) {
        self.rekey(shell::WindowKey::Loose(shell::Loose::Terminal));
        start::forget_vault(&self.config, key);
    }

    /// Detach shells for saved sessions or reload; end shells when no future window can restore them.
    pub(crate) fn release_shells(&self) {
        if !self.keeps_session() && !self.reloading.get() {
            for term in self.terminals() {
                term.kill();
            }
        }
    }

    /// The session as this window last wrote it, or the defaults.
    fn stored_session(&self) -> Session {
        match (self.vault(), self.key.borrow().saved_as()) {
            (Some(vault), _) => vault.session(),
            (None, Some(key)) => Session::load(key),
            (None, None) => Session::default(),
        }
    }

    fn write_session(&self, session: &Session) {
        let written = match (self.vault(), self.key.borrow().saved_as()) {
            (Some(vault), _) => vault.save_session(session),
            (None, Some(key)) => session.save(key).map_err(Into::into),
            (None, None) => Ok(()),
        };
        if let Err(e) = written {
            tracing::warn!("saving the session: {e:#}");
        }
    }

    /// Read pane layout from the widget tree.
    /// Keep stored ratios during presentation or before allocation because hidden/unsized splits have no valid ratio.
    pub(crate) fn layout(&self) -> Option<Layout> {
        let root = self
            .content
            .child_by_name("tabs")
            .and_downcast::<adw::Bin>()?
            .child()?;
        match self.presenting.get() {
            None => self.layout_of(&root),
            Some(_) => Err(Unsized),
        }
        .unwrap_or_else(|Unsized| self.stored_session().layout)
    }

    /// The layout under `widget`: a pane's column, or a `GtkPaned` between two such trees. A pane
    /// with no tab is left out, and the other side of its split takes the split's place.
    fn layout_of(&self, widget: &gtk::Widget) -> Result<Option<Layout>, Unsized> {
        if let Some(paned) = widget.downcast_ref::<gtk::Paned>() {
            let (Some(start), Some(end)) = (paned.start_child(), paned.end_child()) else {
                return Ok(None);
            };
            return Ok(match (self.layout_of(&start)?, self.layout_of(&end)?) {
                (Some(start), Some(end)) => {
                    let vertical = paned.orientation() == gtk::Orientation::Vertical;
                    let extent = extent_of(paned);
                    if extent <= 0 {
                        return Err(Unsized);
                    }
                    Some(Layout::Split {
                        vertical,
                        ratio: f64::from(paned.position()) / f64::from(extent),
                        start: Box::new(start),
                        end: Box::new(end),
                    })
                }
                (one, other) => one.or(other),
            });
        }
        let Some(pane) = self
            .panes
            .borrow()
            .iter()
            .find(|p| p.widget() == widget)
            .cloned()
        else {
            return Ok(None);
        };
        let key = |page: &adw::TabPage| {
            let doc = self.doc_for_page(page).filter(Doc::is_kept);
            doc.map(|d| d.key())
        };
        let tabs: Vec<String> = pane.pages().iter().filter_map(key).collect();
        let selected = pane.tabs.selected_page().as_ref().and_then(key);
        Ok((!tabs.is_empty()).then_some(Layout::Pane { tabs, selected }))
    }

    /// Restored after the window is on screen, so nothing here is on the path to the first frame.
    pub fn restore_session(self: &Rc<Self>) {
        // Once per window, whether it was restored on opening or on the connection arriving.
        if self.restored.replace(true) || !self.keeps_session() {
            return;
        }
        // A terminal session named a moment ago has no file yet, and the defaults it would read
        // instead are a vault window's: a sidebar, for one.
        let file = self
            .key
            .borrow()
            .saved_as()
            .map(accent_core::config::state_path);
        if self.vault().is_none() && !file.is_some_and(|file| file.is_file()) {
            return;
        }
        self.sync_placeholder();
        self.restore(&self.stored_session());
    }

    /// Put `session` back into this window: its tabs, panes, shells, sidebar, zoom and view. What
    /// a window restores from its state file, and what one reloaded without a state file is handed
    /// ([`App::reload`]).
    pub(crate) fn restore(self: &Rc<Self>, session: &Session) {
        // Before the tabs, so each one is built at the right size instead of being restyled
        // afterwards. A state file written before zoom existed defaults to 1.0.
        self.set_zoom(session.zoom);
        // Before the tabs too: a diagram opening draws its pictures on the web, or asks.
        let allowed = session.web_images.iter().cloned();
        self.web_images.borrow_mut().extend(allowed);
        // And the breakpoints, which a tab is opened with.
        self.debugger
            .restore(&session.breakpoints, &session.watches);
        if let Some(layout) = session.panes() {
            self.restore_panes(layout, session);
        }
        // Which pane was showing is deliberately not restored: Files is where a vault is opened,
        // every time. A window that came back on Search or Git left the reader looking at the
        // answer to a question they asked in another sitting.
        self.restore_sidebar(session.sidebar, sidebar_width(session.sidebar_width));
        if let Some(sidebar) = self.sidebar.get() {
            sidebar.restore_info(&session.info);
            sidebar.restore_code(&session.code);
        }
        self.set_mode(Mode::from_name(&session.view));
        // Last, and merged rather than assigned: opening the tabs above ran `file_used` for each
        // of them, and the order they happened to restore in says nothing about how they were
        // used. Touching the stored list back to front puts it in front of those, and a note the
        // restore opened that the stored list does not know about still keeps its place at the end.
        for rel in session.recent_files.iter().rev() {
            accent_core::config::touch(&mut self.recent_files.borrow_mut(), rel, RECENT_FILES);
        }
        for action in session.recent_commands.iter().rev() {
            accent_core::config::touch(
                &mut self.recent_commands.borrow_mut(),
                action,
                RECENT_COMMANDS,
            );
        }
    }

    /// Show pending stored-tab count before a remote host answers, distinguishing loading from a lost session.
    pub fn sync_placeholder(&self) {
        let Some(page) = self
            .content
            .child_by_name("empty")
            .and_downcast::<adw::StatusPage>()
        else {
            return;
        };
        let waiting = match !self.restored.get() && self.offline() {
            true => self.vault().map_or(0, |v| v.session().open.len()),
            false => 0,
        };
        let (icon, title, body) = match waiting {
            0 if self.key.borrow().is_terminal() => (
                "utilities-terminal-symbolic",
                "No Shell Open".to_string(),
                empty_hint(&self.config.borrow(), true, palette::accel_label),
            ),
            0 => (
                "text-x-generic-symbolic",
                "No Note Open".to_string(),
                empty_hint(&self.config.borrow(), false, palette::accel_label),
            ),
            1 => (
                "remote-server-symbolic",
                format!("Waiting for {}", self.host()),
                "1 tab in this vault.".to_string(),
            ),
            n => (
                "remote-server-symbolic",
                format!("Waiting for {}", self.host()),
                format!("{n} tabs in this vault."),
            ),
        };
        page.set_icon_name(Some(icon));
        page.set_title(&title);
        page.set_description(Some(&body));
    }

    /// Restore splits before opening tabs, recording destinations in `placing`.
    /// Worker reads finish later, so [`App::tabs_for`] chooses the pane when each tab arrives.
    fn restore_panes(self: &Rc<Self>, layout: Layout, session: &Session) {
        let active = session.active.clone();
        let (mut placed, mut splits) = (Vec::new(), Vec::new());
        self.arrange(&self.pane(), layout, &mut placed, &mut splits);
        // Each split made the pane it added the active one. Until the active tab lands, a note
        // opened meanwhile — the one on the command line — goes to the pane the reader was in.
        let reader = active
            .as_deref()
            .and_then(|key| self.placing.borrow().get(key)?.upgrade());
        if let Some(pane) = reader {
            self.set_active_pane(&pane);
        }
        if let Some(root) = self.content.child_by_name("tabs") {
            hold_ratios(&root, splits);
        }
        let restore = Rc::new(Restore {
            placed,
            active,
            pinned: session.pinned.clone(),
            taken: RefCell::default(),
            selecting: Cell::new(false),
        });
        // Weak: the work waiting on each tab holds it, so it goes once the last one has settled.
        *self.restore.borrow_mut() = Rc::downgrade(&restore);
        for key in restore.placed.iter().flat_map(|p| &p.tabs) {
            // Opened while a remote vault was still connecting, and left where it is.
            if self.doc_for(key).is_some() {
                continue;
            }
            // A shell is no read: it is back in its pane at once.
            if terminal::is_key(key) {
                self.restore_shell(key, session.terminals.get(key));
                continue;
            }
            let asked = Asked {
                app: Rc::downgrade(self),
                restore: restore.clone(),
                landed: Cell::new(false),
            };
            let compared = session.compared.get(key).cloned();
            // A comparison with git in a tab of its own is read from git again. It waits as a
            // read does, so its pane is kept for it, and taking it out of `awaiting` once it is
            // open or given up drops `asked`, which puts the panes back.
            if let Some(what) = compared
                .clone()
                .filter(|what| !matches!(what.sides, Sides::Worktree | Sides::Merge))
            {
                if let Some(git) = self.git.get() {
                    let waiting = Waiting {
                        what: None,
                        run: Box::new(|_, _| {}),
                    };
                    self.awaiting.borrow_mut().insert(key.clone(), waiting);
                    let key = key.clone();
                    git.restore(what, move || {
                        if let Some(app) = asked.app.upgrade() {
                            app.awaiting.borrow_mut().remove(&key);
                        }
                    });
                }
                continue;
            }
            // At once rather than from `Asked`'s idle, so nothing a landing moves is painted before
            // it is put back. A note compared with the index comes back compared, and one merging
            // as a merge while git still lists it unmerged.
            self.with_tab(key, Opened::Restored, "restore", move |app, _| {
                asked.landed.set(true);
                app.put_back(&asked.restore);
                if let (Some(what), Some(git)) = (compared, app.git.get()) {
                    git.restore(what, || {});
                }
            });
        }
        // What is still on its way keeps its place; the rest has landed, or failed to open.
        self.placing
            .borrow_mut()
            .retain(|key, _| self.awaiting.borrow().contains_key(key));
        self.put_back(&restore);
        // A shell is back at once, where a note lands later: the one in front of the pane the
        // reader was in takes the keyboard, as a new one does.
        if let Some(Doc::Terminal(term)) = self.active_doc()
            && restore.active.as_deref() == Some(term.key().as_str())
        {
            self.focus_document(&self.pane());
        }
    }

    /// Split `pane` the way `layout` is split, and note which pane each tab belongs in. The pane
    /// keeps the start of each split and a new one takes the end, which is where `split_beside`
    /// puts a pane on the right or below.
    fn arrange(
        self: &Rc<Self>,
        pane: &Rc<Pane>,
        layout: Layout,
        placed: &mut Vec<Placed>,
        splits: &mut Vec<(gtk::Paned, f64)>,
    ) {
        match layout {
            Layout::Pane { tabs, selected } => {
                for key in &tabs {
                    self.placing
                        .borrow_mut()
                        .insert(key.clone(), Rc::downgrade(pane));
                }
                placed.push(Placed {
                    pane: Rc::downgrade(pane),
                    tabs,
                    selected,
                });
            }
            Layout::Split {
                vertical,
                ratio,
                start,
                end,
            } => {
                let side = match vertical {
                    true => Side::Down,
                    false => Side::Right,
                };
                let new = self.split_beside(pane, side);
                if let Some(paned) = new.widget().parent().and_downcast::<gtk::Paned>() {
                    splits.push((paned, ratio));
                }
                self.arrange(pane, *start, placed, splits);
                self.arrange(&new, *end, placed, splits);
            }
        }
    }

    /// Restore landed tabs in saved bar order, pins, pane selections and active pane.
    ///
    /// Reads may complete out of order; run after requests begin and after each settles
    /// ([`Asked`]). Close panes with no tabs or pending opens. User-changed pane selections and
    /// active pane override saved choices, while tab order still restores.
    fn put_back(self: &Rc<Self>, restore: &Restore) {
        // What it selects is not the reader's doing: see `reader_in`.
        restore.selecting.set(true);
        for leaf in &restore.placed {
            let Some(pane) = leaf.pane.upgrade() else {
                continue;
            };
            let pages: Vec<adw::TabPage> = leaf
                .tabs
                .iter()
                .filter_map(|key| self.doc_for(key))
                .map(|doc| doc.page().clone())
                .filter(|page| pane.has(page))
                .collect();
            for (at, page) in (0..).zip(&pages) {
                pane.tabs.reorder_page(page, at);
            }
            // In bar order, so each joins the end of the pinned tabs it was saved behind.
            for page in &pages {
                let key = self.doc_for_page(page).map(|doc| doc.key());
                if key.is_some_and(|key| restore.pinned.contains(&key)) {
                    self.set_pinned(page, true);
                }
            }
            let taken = restore.taken.borrow().iter().any(|p| p.ptr_eq(&leaf.pane));
            if !taken
                && let Some(doc) = leaf.selected.as_deref().and_then(|key| self.doc_for(key))
                && pane.has(doc.page())
            {
                pane.tabs.set_selected_page(doc.page());
            }
            let waiting = leaf
                .tabs
                .iter()
                .any(|key| self.awaiting.borrow().contains_key(key));
            if pane.tabs.n_pages() == 0 && !waiting {
                self.close_pane(&pane);
            }
        }
        // The reader stays where they last went, and until they go anywhere notes open beside the
        // active tab. Either way the active pane is set again: a tab landing in an empty pane is
        // selected by being added, which makes that pane the active one.
        let reader = restore.taken.borrow().last().cloned();
        match reader {
            None => self.select_doc(restore.active.as_deref()),
            Some(pane) => {
                // Unless it has closed since, which left notes going to whichever pane was first.
                let open = pane
                    .upgrade()
                    .filter(|pane| self.panes.borrow().iter().any(|p| Rc::ptr_eq(p, pane)));
                if let Some(pane) = open
                    && self.set_active_pane(&pane)
                {
                    self.sync_active();
                }
            }
        }
        restore.selecting.set(false);
    }

    /// Whether current selections come from restoration rather than user navigation.
    /// Selection notifications are shared, so history and [`App::reader_in`] use this guard.
    pub(crate) fn restoring(&self) -> bool {
        self.restore
            .borrow()
            .upgrade()
            .is_some_and(|restore| restore.selecting.get())
    }

    /// The reader picked a tab in `pane`, or moved the keyboard to it. While a restore is landing
    /// that makes the pane theirs, which [`App::put_back`] leaves alone.
    pub(crate) fn reader_in(&self, pane: &Rc<Pane>) {
        if self.restoring() {
            return;
        }
        let Some(restore) = self.restore.borrow().upgrade() else {
            return;
        };
        let pane = Rc::downgrade(pane);
        let mut taken = restore.taken.borrow_mut();
        // Last is where the reader is now.
        taken.retain(|p| !p.ptr_eq(&pane));
        taken.push(pane);
    }

    /// Bring the tab holding `key` to the front, if there is one, and make its pane the one notes
    /// open into.
    fn select_doc(self: &Rc<Self>, key: Option<&str>) {
        let Some(doc) = key.and_then(|key| self.doc_for(key)) else {
            return;
        };
        let Some(pane) = self.pane_of(doc.page()) else {
            return;
        };
        pane.tabs.set_selected_page(doc.page());
        // A tab that was already in front notifies nothing, so only a pane change syncs here.
        if self.set_active_pane(&pane) {
            self.sync_active();
        }
    }
}

/// A pane a restore made, and what the session says belongs in it.
struct Placed {
    pane: std::rc::Weak<Pane>,
    tabs: Vec<String>,
    selected: Option<String>,
}

/// A restore whose tabs are still landing: see [`App::put_back`].
pub struct Restore {
    placed: Vec<Placed>,
    active: Option<String>,
    /// The keys the session had pinned, each pinned again as it lands.
    pinned: Vec<String>,
    /// The panes the reader has picked a tab in or moved the keyboard to since, the latest last.
    taken: RefCell<Vec<std::rc::Weak<Pane>>>,
    /// Set while `put_back` selects, so the notify that fires is not taken for the reader's.
    selecting: Cell<bool>,
}

/// Track a pending restored tab and rerun pane restoration if opening fails.
/// Drop runs from an idle because `awaiting` is borrowed when work releases; successful work already restores panes.
struct Asked {
    app: std::rc::Weak<App>,
    restore: Rc<Restore>,
    /// The tab landed and the work ran, putting the panes back itself: the drop has nothing to do.
    landed: Cell<bool>,
}

impl Drop for Asked {
    fn drop(&mut self) {
        if self.landed.get() {
            return;
        }
        let (app, restore) = (self.app.clone(), self.restore.clone());
        glib::idle_add_local_once(move || {
            if let Some(app) = app.upgrade() {
                app.put_back(&restore);
            }
        });
    }
}

/// A split with no size to take a share of: one made a moment ago, or one in presentation mode.
struct Unsized;

/// How long a split is along the way it splits.
pub(crate) fn extent_of(paned: &gtk::Paned) -> i32 {
    match paned.orientation() {
        gtk::Orientation::Vertical => paned.height(),
        _ => paned.width(),
    }
}

/// Restore split ratios once allocations exist, repeating until no handle moves.
///
/// Nested splits settle after outer handles move, and GtkPaned divides resize space equally
/// rather than by ratio. Each nesting level therefore needs another frame.
fn hold_ratios(root: &gtk::Widget, splits: Vec<(gtk::Paned, f64)>) {
    if splits.is_empty() {
        return;
    }
    let splits: Vec<(glib::WeakRef<gtk::Paned>, f64)> = splits
        .iter()
        .map(|(paned, ratio)| (paned.downgrade(), *ratio))
        .collect();
    root.add_tick_callback(move |_, _| {
        let mut moved = false;
        // A split whose pane has closed since is out of the window, and has nothing to hold.
        let live = splits
            .iter()
            .filter_map(|(paned, ratio)| Some((paned.upgrade()?, *ratio)))
            .filter(|(paned, _)| paned.parent().is_some());
        for (paned, ratio) in live {
            // Not laid out yet: no tab has landed, so the stack still shows its placeholder.
            let extent = extent_of(&paned);
            if extent <= 0 {
                return glib::ControlFlow::Continue;
            }
            let at = ((ratio * f64::from(extent)).round() as i32)
                .min(paned.max_position())
                .max(paned.min_position());
            if paned.position() != at {
                paned.set_position(at);
                moved = true;
            }
        }
        match moved {
            true => glib::ControlFlow::Continue,
            false => glib::ControlFlow::Break,
        }
    });
}

/// A sidebar width in pixels, falling back to the default for anything a sidebar would never
/// be: a hidden column's zero position, or the fraction an older session file may still hold.
fn sidebar_width(stored: i32) -> i32 {
    match stored >= 50 {
        true => stored,
        false => Session::default().sidebar_width,
    }
}

/// Choose which session state a partially restored window may save.
///
/// Before restoration, layout and zoom are defaults. Preserve stored state and merge only
/// new additions, avoiding empty remote windows overwriting sessions before hosts answer.
fn unrestored(mut stored: Session, now: Session) -> Session {
    for key in now.open {
        if !stored.open.contains(&key) {
            stored.open.push(key);
        }
    }
    for key in now.pinned {
        if !stored.pinned.contains(&key) {
            stored.pinned.push(key);
        }
    }
    stored.active = stored.active.or(now.active);
    for rel in now.recent_files.iter().rev() {
        accent_core::config::touch(&mut stored.recent_files, rel, RECENT_FILES);
    }
    for action in now.recent_commands.iter().rev() {
        accent_core::config::touch(&mut stored.recent_commands, action, RECENT_COMMANDS);
    }
    stored.pdf.extend(now.pdf);
    stored.diagram.extend(now.diagram);
    stored.terminals.extend(now.terminals);
    stored.compared.extend(now.compared);
    stored.breakpoints.extend(now.breakpoints);
    for expression in now.watches {
        if !stored.watches.contains(&expression) {
            stored.watches.push(expression);
        }
    }
    for key in now.web_images {
        if !stored.web_images.contains(&key) {
            stored.web_images.push(key);
        }
    }
    stored
}

/// What an empty window's page tells the reader to do: press the chord in force for New Terminal
/// or Go to File…, spelled by `label`, or take a way that needs none once the action is unbound.
fn empty_hint(config: &Config, terminal: bool, label: impl Fn(&str) -> Option<String>) -> String {
    let action = match terminal {
        true => "win.terminal",
        false => "win.palette-files",
    };
    let chord = accels_for(config, action)
        .iter()
        .find_map(|accel| label(accel));
    match (terminal, chord) {
        (true, Some(chord)) => format!("Press {chord} to open one."),
        (true, None) => "Open one from the main menu.".to_string(),
        (false, Some(chord)) => {
            format!("Pick one in the sidebar, or press {chord} to go to a file.")
        }
        (false, None) => "Pick one in the sidebar.".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The empty page names the chord that opens a file or a shell as the bindings stand, and a
    /// way that needs none once the action is unbound.
    #[test]
    fn the_empty_state_names_the_chord_in_force() {
        let label = |accel: &str| Some(accel.to_string());
        let mut config = Config::default();
        assert!(empty_hint(&config, false, label).contains("<Control>e"));
        assert!(empty_hint(&config, true, label).contains("<Control>j"));

        let files = "win.palette-files".to_string();
        config
            .shortcuts
            .insert(files.clone(), vec!["<Control>o".into()]);
        let hint = empty_hint(&config, false, label);
        assert!(
            hint.contains("<Control>o") && !hint.contains("<Control>e"),
            "{hint}"
        );

        config.shortcuts.insert(files, vec![]);
        assert_eq!(
            empty_hint(&config, false, label),
            "Pick one in the sidebar."
        );
        config.shortcuts.insert("win.terminal".into(), vec![]);
        assert_eq!(
            empty_hint(&config, true, label),
            "Open one from the main menu."
        );
    }

    /// A window that never restored keeps what was stored, and adds what it opened itself.
    #[test]
    fn a_window_that_never_restored_keeps_the_stored_tabs() {
        let pane = |key: &str| {
            Box::new(Layout::Pane {
                tabs: vec![key.into()],
                selected: Some(key.into()),
            })
        };
        let stored = Session {
            open: vec!["a.md".into(), "b.md".into()],
            active: Some("b.md".into()),
            layout: Some(Layout::Split {
                vertical: false,
                ratio: 0.4,
                start: pane("a.md"),
                end: pane("b.md"),
            }),
            zoom: 1.5,
            recent_commands: vec!["win.find".into()],
            terminals: [("terminal:1".into(), place("/srv"))].into(),
            pinned: vec!["a.md".into()],
            ..Session::default()
        };
        let merged = unrestored(stored.clone(), Session::default());
        assert_eq!(merged.open, stored.open);
        assert_eq!(merged.active, stored.active);
        // The panes a window has before its restore are not a layout anyone chose.
        assert_eq!(merged.layout, stored.layout);
        assert_eq!(merged.zoom, 1.5);
        assert_eq!(merged.recent_commands, stored.recent_commands);
        // Where a stored shell was is kept for the restore that has not happened yet.
        assert_eq!(merged.terminals, stored.terminals);
        assert_eq!(merged.pinned, stored.pinned);

        let now = Session {
            open: vec!["b.md".into(), "c.md".into()],
            active: Some("c.md".into()),
            recent_commands: vec!["win.palette".into()],
            terminals: [("terminal:2".into(), place("/tmp"))].into(),
            ..Session::default()
        };
        let merged = unrestored(stored.clone(), now);
        assert_eq!(merged.open, ["a.md", "b.md", "c.md"]);
        assert_eq!(merged.active.as_deref(), Some("b.md"));
        assert_eq!(merged.layout, stored.layout);
        assert_eq!(merged.recent_commands, ["win.palette", "win.find"]);
        assert_eq!(
            merged.terminals.keys().collect::<Vec<_>>(),
            ["terminal:1", "terminal:2"]
        );
    }

    fn place(at: &str) -> ShellPlace {
        ShellPlace { at: at.into() }
    }
}
