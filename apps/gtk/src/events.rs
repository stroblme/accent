//! What the vault worker reports, and what the window does about it.

use super::*;

/// The vault worker is polled instead of woken; 120 ms is below what a progress label needs.
const POLL: Duration = Duration::from_millis(120);

impl App {
    fn on_event(self: &Rc<Self>, event: Event) {
        // Anything that touched a file may have changed what git says about it. The pane
        // debounces, so a burst of watcher events still costs one `git status` — and only a walk
        // that found or lost directories is a reason to go looking for repositories again, which
        // is a `git rev-parse` per indexed directory holding a `.git`.
        let depth = match event {
            Event::Reconciled(_) | Event::DirsChanged(_) => Some(git::Depth::Discover),
            Event::FileChanged(_) | Event::FileRemoved(_) | Event::FileRenamed { .. } => {
                Some(git::Depth::Status)
            }
            _ => None,
        };
        if let Some(depth) = depth {
            if let Some(git) = self.git.get() {
                git.schedule_refresh(depth);
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
                // The same batches are what put a nested repository's directory in the index,
                // which is where discovery finds it: the Git pane looks again as the walk goes,
                // throttling itself, rather than only once it is over.
                if p.phase == Phase::Index
                    && let Some(git) = self.git.get()
                {
                    git.rediscover();
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
                // that is already there says how many are waiting in the vault. Counted on a
                // worker, the index being on the host for a remote vault.
                let message = format!(
                    "Indexed {} files ({} new, {} updated)",
                    stats.scanned, stats.added, stats.updated
                );
                let (Some(vault), weak) = (self.vault().cloned(), Rc::downgrade(self)) else {
                    return self.toast(&message);
                };
                glib::spawn_future_local(async move {
                    let counted =
                        gio::spawn_blocking(move || vault.conflicts().map(|c| c.len())).await;
                    let Some(app) = weak.upgrade() else { return };
                    match counted {
                        Ok(Ok(n)) if n > 0 => {
                            app.toast(&format!("{message}, {n} with sync conflicts"));
                        }
                        _ => app.toast(&message),
                    }
                });
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
                    git.schedule_refresh(git::Depth::Everything);
                }
            }
            // A remote vault is still coming up. It reads as the same wait as indexing, because
            // that is what it is: the window is open and the files are not there yet.
            // The bar carries the one step that can measure itself, the upload, and pulses
            // through the rest; the text says which step it is.
            Event::Connecting { what, fraction } => {
                self.statusbar.set_progress(Some(&format!("{what}…")));
                self.connect.show(fraction);
                // A reconnect keeps its banner up while it works, so that banner says which step
                // it is on rather than sitting on one word through a 6.4 MB upload.
                if self.connection.is_revealed() {
                    self.connection.set_title(&format!("{what}…"));
                }
            }
            // The vault answers from here on. Everything asked while it did not was told so
            // rather than made to wait, so all of it is asked again — and the tabs the session
            // was holding are opened now that there is something to open them from.
            Event::Connected => {
                self.statusbar.set_progress(None);
                self.connect.hide();
                self.connection_up();
                // A new server has each document as the tab last tried to send it, and an edit
                // refused while it was coming up is newer; the refresh sends that, and asks for
                // the symbols and folds of a server that has only just heard of the tab.
                for tab in self.open_tabs() {
                    lang::resync(&tab);
                }
                if let Some(tree) = self.tree.get() {
                    tree.refresh();
                }
                if let Some(sidebar) = self.sidebar.get() {
                    sidebar.mark_tags_dirty();
                }
                if let Some(git) = self.git.get() {
                    // Nothing was discovered while the link was down, so this starts at the top.
                    git.schedule_refresh(git::Depth::Discover);
                }
                self.refresh_corpus();
                self.restore_session();
                self.sync_active();
                // A remote shell the drop ended kept its tab, and starts again in it.
                for term in self.terminals() {
                    term.reopen();
                }
            }
            Event::Disconnected(why) => {
                self.statusbar.set_progress(None);
                self.connect.hide();
                self.connection_down(&why);
            }
            Event::Refused(why) => {
                self.statusbar.set_progress(None);
                self.connect.hide();
                self.connection_refused(&why);
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
}

/// Drain whatever the vault worker has said since the last tick.
///
/// This source is also what *owns* the window's state: every other closure holds `App` weakly, so
/// that a closed tab, a finished dialog or a dropped controller cannot keep it alive by accident.
///
/// ponytail: a 120 ms poll instead of wiring an `async-channel` into the GLib context. One timeout
/// source, no extra dependency, and the latency is below what a progress label needs.
pub fn start_events(app: &Rc<App>, events: Receiver<Event>) {
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
