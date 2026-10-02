//! What the vault worker reports, and what the window does about it.

use super::*;
use accent_core::path::parent_dir;

/// The vault worker is polled instead of woken; 120 ms is below what a progress label needs.
const POLL: Duration = Duration::from_millis(120);

impl App {
    /// Point every tab at or under `from` at the same place under `to`. The watcher's rename
    /// does this, and so does a move of our own before it reloads the notes it rewrote: those
    /// are named by where they are now, which their tabs are not until they follow.
    pub(crate) fn follow_rename(self: &Rc<Self>, from: &str, to: &str) {
        // On a remote vault a PDF reads and writes its cached copy, which has to be under the
        // new name before the tab is pointed there.
        if let Some(remote) = self.vault().and_then(|v| v.remote()) {
            remote.moved(from, to);
        }
        let prefix = format!("{from}/");
        for doc in self.docs() {
            let key = doc.key();
            let moved = match key.strip_prefix(&prefix) {
                _ if key == from => to.to_string(),
                Some(rest) => format!("{to}/{rest}"),
                None => continue,
            };
            doc.retarget(&self.root(), &moved);
            self.watch_folder_of(&moved);
        }
        accent_core::config::rename_in(&mut self.recent_files.borrow_mut(), from, to);
        // The links a PDF's page delete left are found again by their note's path at its Undo.
        for pdf in self.docs().iter().filter_map(Doc::pdf) {
            for kept in pdf.relinks.borrow_mut().left.values_mut().flatten() {
                accent_core::config::rename_in(std::slice::from_mut(&mut kept.note), from, to);
            }
        }
        self.sync_active();
    }

    /// Have the vault report on the folder `key` is in, which the walk may not enter: a file
    /// in a gitignored folder or a dependency tree changes nothing the index hears, so its tab
    /// would not follow an edit made outside accent. The news comes as
    /// [`Event::UnindexedChanged`], as the tree's does for the folders it lists; the worker
    /// leaves a folder it walks to the index. Watched for as long as the window is open, as the
    /// tree's are: one folder per document, one level each.
    pub(crate) fn watch_folder_of(&self, key: &str) {
        let dir = parent_dir(key);
        let Some(vault) = self.vault().cloned() else {
            return;
        };
        if dir.is_empty() || doc::is_loose_key(key) {
            return;
        }
        let dirs = vec![dir.to_string()];
        // A round trip on a remote vault, and one that cannot be sent yet is kept and asked of
        // the host once it answers (`tree::watch_unindexed`).
        gio::spawn_blocking(move || {
            if let Err(e) = vault.watch_unindexed(&dirs) {
                tracing::debug!("watching a document's folder: {e:#}");
            }
        });
    }

    /// Bring whatever shows `rel` up to date with its file, which something other than this
    /// window has changed.
    pub(crate) fn changed_on_disk(self: &Rc<Self>, rel: &str) {
        self.reshow_preview_image(rel);
        let Some(doc) = self.doc_for(rel) else {
            return;
        };
        match &doc {
            Doc::Text(tab) => {
                self.file_changed(tab);
                if self.is_active(tab) {
                    self.sync_active();
                }
            }
            // Read the file again: the texture on screen is of the old contents, so
            // redrawing alone would show them again. The file is found as opening it
            // was, which on a remote vault fetches a fresh copy.
            Doc::Image(image) => {
                let image = Rc::downgrade(image);
                self.local_copy(rel, &self.root().join(rel), move |app, copy| {
                    match (image.upgrade(), copy) {
                        (Some(image), Ok(copy)) => app.show_image(&image, Some(copy)),
                        (Some(_), Err(e)) => app.cannot("reload", e),
                        (None, _) => {}
                    }
                });
            }
            // A rebuilt PDF, which is what a LaTeX loop produces: re-read it in place
            // rather than sending the reader back to page one. On a remote vault the
            // reader has a cached copy open, which is fetched again first.
            Doc::Pdf(pdf) => {
                let pdf = Rc::downgrade(pdf);
                self.local_copy(rel, &self.root().join(rel), move |app, copy| {
                    match (pdf.upgrade(), copy) {
                        (Some(pdf), Ok(_)) => pdf.refresh(),
                        (Some(_), Err(e)) => app.cannot("reload", e),
                        (None, _) => {}
                    }
                });
            }
            Doc::Diagram(d) => self.diagram_changed(d),
            // Neither a diff nor a shell is keyed by a path, so a file changing under one
            // reaches none of these.
            Doc::Status(_) | Doc::Diff(_) | Doc::Terminal(_) => {}
        }
    }

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
            // And that the vault's tags moved, and that the rows a search is showing are of text
            // that has changed: a note edited in another editor, one deleted, a whole folder
            // trashed. Every one of these is emitted after the index has taken the change in, so
            // both panes read what is there now.
            if let Some(sidebar) = self.sidebar.get() {
                sidebar.mark_tags_dirty();
                sidebar.requery_search_soon();
            }
        }
        // A note's hints are about the index as much as about its own text: the file a
        // `[[link]]` names may have just been created, renamed or deleted, and none of that is
        // an edit of the note holding the link. So the open notes are diagnosed again whenever
        // the walk found or lost files, rather than at the next keystroke. Notes only: a
        // language server publishes its own diagnostics when it has something new to say.
        if matches!(event, Event::Reconciled(_) | Event::DirsChanged(_)) {
            for tab in self.open_tabs().iter().filter(|t| t.flavour().is_note()) {
                lang::rediagnose(tab);
            }
        }
        match event {
            Event::Progress(p) => {
                self.statusbar
                    .set_progress(Some(&statusbar::indexing_label(p.done, p.total)));
                // A walk is running, so the control beside the line is Stop. Set on every batch
                // rather than once, which is also what turns Resume back into Stop.
                self.statusbar.set_indexing(statusbar::Indexing::Running);
                // The indexer commits rows in batches and the walk hands it files depth-first,
                // so the root level is queryable long before the reconcile ends. Without this the
                // tree of a cold vault stays empty for the whole two seconds. Throttled, and
                // deliberately not marking the tags pane dirty: that is a whole-pane rebuild and
                // it can wait for `Reconciled`. Not while a host is still being connected to (see
                // `Reconciled`).
                let now = glib::monotonic_time();
                if p.phase == Phase::Index
                    && !self.offline()
                    && now - self.tree_painted.get() >= TREE_REPAINT
                {
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
            Event::Busy {
                what,
                busy,
                message,
            } => {
                self.statusbar
                    .set_provider_busy(busy.then_some(what.as_str()), message.as_deref());
            }
            Event::Reconciled(stats) => {
                tracing::debug!(
                    t_ms = ms(),
                    scanned = stats.scanned,
                    unchanged = stats.unchanged,
                    "reconcile done"
                );
                // A stopped walk left a *partial* index, so nothing downstream may read this as
                // "the vault is indexed": `reconciled` is what the file operations and the
                // benches ask, and it stays false until a walk finishes. The vault's own line
                // keeps the slot meanwhile — it wins it over a transfer and over the suggestion
                // index — and carries the Resume that finishes the job.
                self.reconciled.set(!stats.stopped);
                match stats.stopped {
                    true => {
                        self.statusbar.set_progress(Some(statusbar::PAUSED));
                        self.statusbar.set_indexing(statusbar::Indexing::Paused);
                    }
                    false => {
                        self.statusbar.set_progress(None);
                        self.statusbar.set_indexing(statusbar::Indexing::Idle);
                    }
                }
                // A host's walk reports while the link to it is still being made, when every
                // folder the tree asked after would fail "still connecting"; `Connected` refills
                // the tree once the host answers.
                if !self.offline()
                    && let Some(tree) = self.tree.get()
                {
                    tree.refresh();
                }
                self.refresh_corpus();
                self.sync_active();
                // A walk of one folder is news only to the reader who asked for it with Reload,
                // and answers every such ask for that folder or one inside it.
                let asked = {
                    let mut reloads = self.reloads.borrow_mut();
                    let before = reloads.len();
                    reloads.retain(|d| !Path::new(d).starts_with(&stats.dir));
                    reloads.len() < before
                };
                if !stats.stopped && !stats.dir.is_empty() && !asked {
                    return;
                }
                // Conflicts on files nobody has open have no banner to appear on, so the toast
                // that is already there says how many are waiting in the vault. Counted on a
                // worker, the index being on the host for a remote vault.
                let message = match stats.stopped {
                    true => "Indexing paused — what is indexed so far is kept".to_string(),
                    false => format!(
                        "Indexed {} files ({} new, {} updated)",
                        stats.scanned, stats.added, stats.updated
                    ),
                };
                let (Some(vault), weak) = (self.vault().cloned(), Rc::downgrade(self)) else {
                    return self.toast(&message);
                };
                glib::spawn_future_local(async move {
                    let counted = crate::work::off_thread("conflict count", move || {
                        vault.conflicts().map(|c| c.len())
                    })
                    .await;
                    let Some(app) = weak.upgrade() else { return };
                    match counted {
                        Some(Ok(n)) if n > 0 => {
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
            }
            // The index hears nothing from these folders, so a document open on a file in one
            // is told here instead, as `FileChanged` tells one the index holds: a text or a
            // diagram tab reads again only a file that really changed (`check_disk`), and a PDF
            // once its file has been left alone (`PdfTab::refresh`).
            Event::UnindexedChanged(dirs) => {
                if let Some(tree) = self.tree.get() {
                    tree.invalidate(&dirs);
                }
                for doc in self.docs() {
                    let key = doc.key();
                    if dirs.iter().any(|dir| dir == parent_dir(&key)) {
                        self.changed_on_disk(&key);
                    }
                }
            }
            Event::FileChanged(rel) => self.changed_on_disk(&rel),
            Event::FileRemoved(rel) => {
                self.reshow_preview_image(&rel);
                // A conflict copy is never a tab of its own; what its removal changes is the
                // banner on the file it was a copy of.
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
                    match doc.tab().filter(|tab| tab.save.modified.get()) {
                        Some(tab) => {
                            tab.save.disk_changed.set(true);
                            tab.show_alert(Alert::Restore);
                        }
                        None => self.close_page(doc.page()),
                    }
                }
            }
            Event::FileRenamed { from, to } => {
                self.reshow_preview_image(&from);
                self.follow_rename(&from, &to);
            }
            Event::Conflict { original, .. } => self.sync_conflict_banner(&original, None),
            // A repository moved under us: a commit in a shell, a checkout, a rebase. The pane
            // asks git what changed; nothing else in the window is affected.
            //
            // Where the vault has no repository at all, the one thing a `.git` write can mean is
            // that it has one now — `git init` in a terminal, a clone into the vault root — and
            // the watcher reports that write like any other, `.git` being watched on purpose. So
            // that one goes looking for repositories instead, which is what makes the pane show
            // itself with nothing to click. Not the deeper ask everywhere: discovery is a
            // `git rev-parse` per indexed directory, and every commit would pay for it.
            Event::GitChanged => {
                if let Some(git) = self.git.get() {
                    git.schedule_refresh(match git.has_repos() {
                        true => git::Depth::Everything,
                        false => git::Depth::Discover,
                    });
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
                // What was drawn while the link was down reached only the file here.
                for pdf in self.docs().iter().filter_map(Doc::pdf) {
                    if pdf.unsent() {
                        self.push_pdf(pdf);
                    }
                }
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
