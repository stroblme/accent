//! The session: what the palette lists, what was recently used, and what a window writes down
//! and restores between runs.

use super::*;

/// What the palette lists before the user types anything.
const RECENT_NOTES: usize = 50;

/// Commands kept in the session's recently-used list. There are only about forty of them, so a
/// shorter list is still every command the user actually reaches for.
const RECENT_COMMANDS: usize = 20;

/// Session state is cheap to lose and noisy to write, so it follows a change by a second.
const SESSION: Duration = Duration::from_secs(1);

/// What the palette lists, kept warm so the dialog never waits on the vault.
#[derive(Default)]
pub struct Corpus {
    files: Rc<Vec<String>>,
    tags: Rc<Vec<String>>,
}

impl App {
    /// Re-read what the palette lists, off the main loop. Cheap enough to do on every reconcile
    /// and every time the dialog opens, which is what keeps the answer both instant and current.
    pub fn refresh_corpus(self: &Rc<Self>) {
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

    pub fn palette(self: &Rc<Self>, initial: palette::Mode) {
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
        // Pruned before the config is borrowed for the rest: dropping a gone folder writes it.
        let vaults = start::recent_vaults(&self.config);
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
            vaults: start::other_vaults(&vaults, self.vault().map(|v| v.key())),
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

    /// Remember that this note was just looked at. Called from `sync_active`, so it covers
    /// opening a note, switching to its tab and coming back to the window.
    pub fn note_used(self: &Rc<Self>, rel: &str) {
        if self.recent_notes.borrow().first().is_some_and(|r| r == rel) {
            return;
        }
        accent_core::config::touch(&mut self.recent_notes.borrow_mut(), rel, RECENT_NOTES);
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

    pub fn save_session(&self) {
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
            layout: self.layout(),
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
                for pdf in self.pdfs() {
                    places.insert(pdf.key(), pdf.place());
                }
                places
            },
        };
        let Some(vault) = self.vault() else {
            // Nothing to key a session file on, and nothing worth restoring: a window opened on
            // one file is opened again the same way.
            return;
        };
        let session = match self.restored.get() {
            true => session,
            false => unrestored(vault.session(), session),
        };
        if let Err(e) = vault.save_session(&session) {
            tracing::warn!("saving the session: {e:#}");
        }
    }

    /// The panes as the session records them, read off the widget tree, which is the layout.
    /// While presentation mode has the panes off screen, or a split has no size yet, there is no
    /// ratio to read and the stored layout stands.
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
        .unwrap_or_else(|Unsized| self.vault().and_then(|v| v.session().layout))
    }

    /// The layout under `widget`: a pane's column, or a `GtkPaned` between two such trees. A pane
    /// with nothing to put back — shells and comparisons are not files — is left out, and the
    /// other side of its split takes the split's place.
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
            self.doc_for_page(page)
                .filter(|d| !d.is_transient())
                .map(|d| d.key())
        };
        let tabs: Vec<String> = pane.pages().iter().filter_map(key).collect();
        let selected = pane.tabs.selected_page().as_ref().and_then(key);
        Ok((!tabs.is_empty()).then_some(Layout::Pane { tabs, selected }))
    }

    /// Restored after the window is on screen, so nothing here is on the path to the first frame.
    pub fn restore_session(self: &Rc<Self>) {
        let Some(vault) = self.vault() else {
            return;
        };
        // Once per window, whether it was restored on opening or on the connection arriving.
        if self.restored.replace(true) {
            return;
        }
        self.sync_placeholder();
        let session = vault.session();
        // Before the tabs, so each one is built at the right size instead of being restyled
        // afterwards. A state file written before zoom existed defaults to 1.0.
        self.set_zoom(session.zoom);
        if let Some(layout) = session.panes() {
            self.restore_panes(layout, session.active.clone());
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

    /// What the empty document column says. A remote window whose host has not answered yet
    /// says how many stored tabs are waiting for it, because an empty window with a failed
    /// connection over it otherwise reads as a session that was lost.
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
            0 => (
                "text-x-generic-symbolic",
                "No Note Open".to_string(),
                "Pick one in the sidebar, or press Ctrl+E to go to a file.".to_string(),
            ),
            1 => (
                "network-server-symbolic",
                format!("Waiting for {}", self.host()),
                "1 tab will open when it answers.".to_string(),
            ),
            n => (
                "network-server-symbolic",
                format!("Waiting for {}", self.host()),
                format!("{n} tabs will open when it answers."),
            ),
        };
        page.set_icon_name(Some(icon));
        page.set_title(&title);
        page.set_description(Some(&body));
    }

    /// Split the window the way the session left it, then open every tab into its own pane.
    ///
    /// The splits come first and stand empty until their tabs land: a text tab only exists once
    /// the worker's read is back, so each open looks its pane up in `placing` (see
    /// [`App::tabs_for`]) instead of being moved there afterwards.
    fn restore_panes(self: &Rc<Self>, layout: Layout, active: Option<String>) {
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
        let placed = Rc::new(placed);
        let active = Rc::new(active);
        for key in placed.iter().flat_map(|p| &p.tabs) {
            // Opened while a remote vault was still connecting, and left where it is.
            if self.doc_for(key).is_some() {
                continue;
            }
            let asked = Asked {
                app: Rc::downgrade(self),
                placed: placed.clone(),
                active: active.clone(),
                landed: Cell::new(false),
            };
            // At once rather than from `Asked`'s idle, so a landing tab is never painted in front
            // of its pane's own.
            self.with_tab(key, Opened::Kept, move |app, _| {
                asked.landed.set(true);
                app.put_back(&asked.placed, asked.active.as_deref())
            });
        }
        // What is still on its way keeps its place; the rest has landed, or failed to open.
        self.placing
            .borrow_mut()
            .retain(|key, _| self.awaiting.borrow().contains_key(key));
        self.put_back(&placed, active.as_deref());
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

    /// Put back what the session had in front, as far as it has landed: each pane's tabs in the
    /// order its bar had them, whatever order the reads came back in, with its own tab selected;
    /// then the active tab, whose pane is the one notes open into. A pane still empty with nothing
    /// on its way has lost every note it held since, and closes, the other side of its split
    /// taking the room.
    ///
    /// Run once every open has been asked for, and again as each one settles (see [`Asked`]): a
    /// landing tab is selected in its pane as it arrives, and a failed one may have emptied one.
    fn put_back(self: &Rc<Self>, placed: &[Placed], active: Option<&str>) {
        for leaf in placed {
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
            if let Some(doc) = leaf.selected.as_deref().and_then(|key| self.doc_for(key))
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
        self.select_doc(active);
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

/// One tab a restore asked for, held by the work waiting on it in `awaiting`. A tab that fails to
/// open drops that work without running it, and says nothing else, so the drop is when the panes
/// are looked at again. From an idle: the drop happens inside a borrow of `awaiting`, which
/// `put_back` reads.
struct Asked {
    app: std::rc::Weak<App>,
    placed: Rc<Vec<Placed>>,
    active: Rc<Option<String>>,
    /// The tab landed and the work ran, putting the panes back itself. A second pass from the
    /// idle would take the front from a note opened since — the one on the command line.
    landed: Cell<bool>,
}

impl Drop for Asked {
    fn drop(&mut self) {
        if self.landed.get() {
            return;
        }
        let (app, placed, active) = (self.app.clone(), self.placed.clone(), self.active.clone());
        glib::idle_add_local_once(move || {
            if let Some(app) = app.upgrade() {
                app.put_back(&placed, active.as_deref());
            }
        });
    }
}

/// A split with no size to take a share of: one made a moment ago, or one in presentation mode.
struct Unsized;

/// How long a split is along the way it splits.
fn extent_of(paned: &gtk::Paned) -> i32 {
    match paned.orientation() {
        gtk::Orientation::Vertical => paned.height(),
        _ => paned.width(),
    }
}

/// Move each restored split's handle to its share of the split, once the split has a size.
///
/// Every frame rather than once: a split inside another has its final size only after the outer
/// handle has moved, and `GtkPaned` hands a resize out half and half rather than by share. So
/// each frame puts every handle back at its share of what it has now, until a frame finds nothing
/// left to move — one frame per level of nesting.
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

/// What a window may write before it has put the stored session back.
///
/// Until then its tabs, zoom and layout are the defaults it was built with, not anything the
/// reader chose, so the stored session stands and only what the window added since is merged in.
/// A remote window closed before its host ever answered used to write its empty tab list over
/// the tabs it was waiting to open.
fn unrestored(mut stored: Session, now: Session) -> Session {
    for key in now.open {
        if !stored.open.contains(&key) {
            stored.open.push(key);
        }
    }
    stored.active = stored.active.or(now.active);
    for rel in now.recent_notes.iter().rev() {
        accent_core::config::touch(&mut stored.recent_notes, rel, RECENT_NOTES);
    }
    for action in now.recent_commands.iter().rev() {
        accent_core::config::touch(&mut stored.recent_commands, action, RECENT_COMMANDS);
    }
    stored.pdf.extend(now.pdf);
    stored
}

#[cfg(test)]
mod tests {
    use super::*;

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
            ..Session::default()
        };
        let merged = unrestored(stored.clone(), Session::default());
        assert_eq!(merged.open, stored.open);
        assert_eq!(merged.active, stored.active);
        // The panes a window has before its restore are not a layout anyone chose.
        assert_eq!(merged.layout, stored.layout);
        assert_eq!(merged.zoom, 1.5);
        assert_eq!(merged.recent_commands, stored.recent_commands);

        let now = Session {
            open: vec!["b.md".into(), "c.md".into()],
            active: Some("c.md".into()),
            recent_commands: vec!["win.palette".into()],
            ..Session::default()
        };
        let merged = unrestored(stored.clone(), now);
        assert_eq!(merged.open, ["a.md", "b.md", "c.md"]);
        assert_eq!(merged.active.as_deref(), Some("b.md"));
        assert_eq!(merged.layout, stored.layout);
        assert_eq!(merged.recent_commands, ["win.palette", "win.find"]);
    }
}
