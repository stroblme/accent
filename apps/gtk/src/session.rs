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

    /// Restored after the window is on screen, so nothing here is on the path to the first frame.
    pub fn restore_session(self: &Rc<Self>) {
        let Some(vault) = self.vault() else {
            return;
        };
        // Once per window, whether it was restored on opening or on the connection arriving.
        if self.restored.replace(true) {
            return;
        }
        let session = vault.session();
        // Before the tabs, so each one is built at the right size instead of being restyled
        // afterwards. A state file written before zoom existed defaults to 1.0.
        self.set_zoom(session.zoom);
        // ponytail: every note comes back into one pane, because the session does not record the
        // pane layout. Add a tree of splits to `Session` the day restoring into one column stops
        // being what someone who left four panes open expects.
        // A text tab arrives from the worker later and selects itself as it lands, so the one
        // that was active is put back in front after every arrival; the synchronous opens, a PDF
        // or an image, are in place by the end of the loop and get the same treatment once.
        for key in &session.open {
            let active = session.active.clone();
            self.with_tab(key, Opened::Kept, move |app, _| {
                app.select_doc(active.as_deref())
            });
        }
        self.select_doc(session.active.as_deref());
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

    /// Bring the tab holding `key` to the front, if there is one.
    fn select_doc(&self, key: Option<&str>) {
        if let Some(doc) = key.and_then(|key| self.doc_for(key)) {
            self.reveal_page(doc.page());
        }
    }
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
        let stored = Session {
            open: vec!["a.md".into(), "b.md".into()],
            active: Some("b.md".into()),
            zoom: 1.5,
            recent_commands: vec!["win.find".into()],
            ..Session::default()
        };
        let merged = unrestored(stored.clone(), Session::default());
        assert_eq!(merged.open, stored.open);
        assert_eq!(merged.active, stored.active);
        assert_eq!(merged.zoom, 1.5);
        assert_eq!(merged.recent_commands, stored.recent_commands);

        let now = Session {
            open: vec!["b.md".into(), "c.md".into()],
            active: Some("c.md".into()),
            recent_commands: vec!["win.palette".into()],
            ..Session::default()
        };
        let merged = unrestored(stored, now);
        assert_eq!(merged.open, ["a.md", "b.md", "c.md"]);
        assert_eq!(merged.active.as_deref(), Some("b.md"));
        assert_eq!(merged.recent_commands, ["win.palette", "win.find"]);
    }
}
