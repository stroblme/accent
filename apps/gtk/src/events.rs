//! What the vault worker reports, and what the window does about it.

use super::*;
use accent_core::path::parent_dir;

impl App {
    /// Retarget tabs under `from` after a rename or move, before reloading rewritten
    /// notes whose paths already use `to`.
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
            // A new extension that makes it another kind of page opens it anew in its place.
            if !open::same_page(&key, &moved) && !matches!(doc, Doc::Loading(_)) {
                self.reopen_renamed(&doc, &moved);
                continue;
            }
            doc.retarget(&self.root(), &moved);
            self.watch_folder_of(&moved);
            // A file still on its way is read again under its new name: the read in flight
            // finds no tab under the old one when it lands.
            if let Doc::Loading(held) = &doc
                && !held.failed()
                && let Some((key, path)) = self.locate(&moved)
            {
                self.read(&key, &path, Opened::Kept);
            }
        }
        accent_core::config::rename_in(&mut self.recent_files.borrow_mut(), from, to);
        self.nav.borrow_mut().rename(from, to);
        self.debugger.renamed(from, to);
        self.relinks_renamed(from, to);
        self.sync_active();
    }

    /// Open `to` in the place of `doc`, which a rename made another kind of page: in its pane, at
    /// its place in the bar, pinned if it was. A text tab's caret goes along to a text tab, and
    /// so does its text if it was typed into after the rename wrote it out (`fileops::apply`);
    /// its undo starts over.
    fn reopen_renamed(self: &Rc<Self>, doc: &Doc, to: &str) {
        let page = doc.page().clone();
        let Some(pane) = self.pane_of(&page) else {
            return;
        };
        let at = pane.tabs.page_position(&page);
        let how = match self.is_pinned(&page) {
            true => Opened::Pinned,
            false => Opened::Kept,
        };
        let carried = doc.tab().map(|tab| {
            let unsaved = tab.save.modified.get().then(|| tab.text());
            (unsaved, tab.buffer.cursor_position())
        });
        // Its file has gone, so there is nothing to write what it holds into.
        match doc {
            Doc::Text(tab) => tab.discard(),
            Doc::Diagram(diagram) => diagram.discard(),
            Doc::Notebook(notebook) => notebook.discard(),
            _ => {}
        }
        self.pending.place(to, &pane);
        match (carried, open::text_flavour(to)) {
            (Some((unsaved, caret)), Some(_)) => {
                self.with_tab(to, how, "open", move |_, tab| {
                    if let Some(text) = unsaved {
                        tab.buffer.set_text(&text);
                    }
                    tab.buffer.place_cursor(&tab.buffer.iter_at_offset(caret));
                    tab.view
                        .scroll_to_mark(&tab.buffer.get_insert(), 0.0, true, 0.0, 0.5);
                });
            }
            _ => self.open_as(to, how),
        }
        if let Some(new) = self.doc_for(to) {
            pane.tabs.reorder_page(new.page(), at);
        }
        self.close_page(&page);
    }

    /// Watch `key`'s parent folder for changes the index does not walk, and have the index keep
    /// `key` although it may not walk there ([`Vault::keep_opened`](accent_api::Vault::keep_opened)).
    ///
    /// Gitignored and dependency files need [`Event::UnindexedChanged`] to follow external edits.
    /// Indexed folders stay with the worker; one-level watches last for the window's lifetime.
    pub(crate) fn watch_folder_of(&self, key: &str) {
        let Some(vault) = self.vault().cloned() else {
            return;
        };
        if doc::is_loose_key(key) {
            return;
        }
        let (key, dir) = (key.to_string(), parent_dir(key).to_string());
        // A round trip on a remote vault, and a watch that cannot be sent yet is kept and asked of
        // the host once it answers (`tree::watch_unindexed`).
        gio::spawn_blocking(move || {
            if let Err(e) = vault.keep_opened(&key) {
                tracing::debug!("keeping an opened file in the index: {e:#}");
            }
            if !dir.is_empty()
                && let Err(e) = vault.watch_unindexed(&[dir])
            {
                tracing::debug!("watching a document's folder: {e:#}");
            }
        });
    }

    /// Refresh open documents for an external change to `rel`.
    pub(crate) fn changed_on_disk(self: &Rc<Self>, rel: &str) {
        self.reshow_preview_image(rel);
        self.details_stale(rel);
        // A terminal editor's tab is keyed as a shell, not by the file it edits.
        self.saved_changed(rel);
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
                let (page, image) = (image.page.clone(), Rc::downgrade(image));
                let path = self.root().join(rel);
                self.local_copy(rel, &path, &page, move |app, copy| {
                    match (image.upgrade(), copy) {
                        (Some(image), Ok(copy)) => app.show_image(&image, Some(copy)),
                        (Some(image), Err(e)) => app.image_failed(&image, e),
                        (None, _) => {}
                    }
                });
            }
            // What was drawn and has not reached the host goes up instead, beside the host's
            // file if that has moved on: a fetch would write over it.
            Doc::Pdf(pdf) if pdf.unsent() => self.push_pdf(pdf),
            // One whose fetch failed is fetched again; one whose bytes are on their way is left
            // to them.
            Doc::Pdf(pdf) if !pdf.started() => {
                if pdf.fetch_failed() {
                    self.load_pdf(pdf);
                }
            }
            // A rebuilt PDF, which is what a LaTeX loop produces: re-read it in place
            // rather than sending the reader back to page one. On a remote vault the
            // reader has a cached copy open, which is fetched again first.
            Doc::Pdf(pdf) => {
                let (page, pdf) = (pdf.page.clone(), Rc::downgrade(pdf));
                let path = self.root().join(rel);
                self.local_copy(rel, &path, &page, move |app, copy| {
                    match (pdf.upgrade(), copy) {
                        (Some(pdf), Ok(_)) => pdf.refresh(),
                        (Some(_), Err(e)) => app.cannot("reload", e),
                        (None, _) => {}
                    }
                });
            }
            Doc::Diagram(d) => self.model_changed(d),
            Doc::Notebook(n) => self.model_changed(n),
            // A read that failed, the link down perhaps, is tried again; one on its way reads
            // the file as it is when it gets there.
            Doc::Loading(held) => self.read_again(held),
            // Neither a diff nor a shell is keyed by a path, so a file changing under one
            // reaches none of these; a preview is rendered again from its note's tab.
            Doc::Status(_) | Doc::Diff(_) | Doc::Terminal(_) | Doc::Rendered(_) => {}
        }
    }

    fn on_event(self: &Rc<Self>, event: Event) {
        // Anything that touched a file may have changed what git says about it. The pane
        // debounces, so a burst of watcher events still costs one round of `git status`, asked
        // only of the repositories holding the paths: a save outside all of them costs nothing.
        // Looking for repositories again is a `git rev-parse` per indexed directory holding a
        // `.git`, so only a walk asks for it, and a path that was a repository or held one going:
        // a folder that comes with a repository in it is walked, and an empty one holds none.
        let touched = matches!(
            event,
            Event::Reconciled(_)
                | Event::DirsChanged(_)
                | Event::FileChanged(_)
                | Event::FileRemoved(_)
                | Event::FileRenamed { .. }
        );
        if let Some(panel) = self.bibliography.get() {
            panel.follow(&event);
            if panel.changed_by(&event) {
                panel.refresh();
            }
        }
        if touched {
            if let Some(git) = self.git.get() {
                match &event {
                    Event::Reconciled(_) => git.schedule_refresh(git::Depth::Discover),
                    Event::DirsChanged(dirs) => git.schedule_paths(dirs, false),
                    Event::FileChanged(rel) => git.schedule_paths([rel], false),
                    Event::FileRemoved(rel) => git.schedule_paths([rel], true),
                    Event::FileRenamed { from, to } => git.schedule_paths([from, to], true),
                    _ => {}
                }
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
            // The saved searches on a preview's page answer from the same index.
            self.each_preview(|preview| preview.requery_soon());
        }
        // A note's hints are about the index as much as about its own text: the file a
        // `[[link]]` names may have just been created, renamed or deleted, and none of that is
        // an edit of the note holding the link. So the open notes, and the notebooks' markdown
        // cells, are diagnosed again whenever the walk found or lost files, rather than at the
        // next keystroke. Notes only: a language server publishes its own diagnostics when it has
        // something new to say.
        if matches!(event, Event::Reconciled(_) | Event::DirsChanged(_)) {
            for tab in self.open_tabs().iter().filter(|t| t.flavour().is_note()) {
                lang::rediagnose(tab);
            }
            for notebook in self.notebooks() {
                notebook.rediagnose();
            }
        }
        // A code tab with no language server is outlined from the index, which has just taken
        // in a walk, or a change of the file made elsewhere.
        let indexed = |tab: &Rc<Tab>| match &event {
            Event::Reconciled(_) => true,
            Event::FileChanged(rel) => *rel == tab.rel(),
            _ => false,
        };
        if matches!(event, Event::Reconciled(_) | Event::FileChanged(_)) {
            for tab in self.open_tabs().iter().filter(|t| indexed(t)) {
                lang::index_outline(tab);
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
            Event::PdfText {
                done,
                total,
                running,
            } => {
                self.statusbar
                    .set_pdf_text(running.then_some((done, total)));
                // The rows a search shows may be short of the pages read meanwhile.
                if !running && let Some(sidebar) = self.sidebar.get() {
                    sidebar.requery_search_soon();
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
                        // The page edits made while the index was partial follow now.
                        self.relink_all();
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
                self.refresh_corpus(false);
                self.refresh_symbols();
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
                    true => "Indexing paused. Indexed files remain available".to_string(),
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
                self.relinks_removed(&rel);
                // A remote file's cached copy goes with it, so a new file given the name opens as
                // itself; changes it held that had not reached the host go too, and are said.
                if let Some(remote) = self.vault().and_then(|v| v.remote())
                    && remote.removed(&rel)
                {
                    self.toast(&format!(
                        "{} was deleted on {} before your changes reached it",
                        doc::file_name(&rel),
                        self.host()
                    ));
                }
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
                // After the tabs, whose closing kept their breakpoints.
                self.debugger.forget(&rel);
                self.refill_breakpoints();
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
            // itself with nothing to click. Where it has some, the write may still be a new
            // repository's, a `git init` in one of its folders, so the pane looks as well, but
            // throttled: discovery is a `git rev-parse` per indexed directory, and a burst of
            // commits pays for it once.
            Event::GitChanged => {
                if let Some(git) = self.git.get() {
                    match git.has_repos() {
                        true => {
                            git.schedule_refresh(git::Depth::Everything);
                            git.rediscover();
                        }
                        false => git.schedule_refresh(git::Depth::Discover),
                    }
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
                // Before a file opens in a terminal editor there, which runs the program it has.
                self.probe_host();
                // A new server is a new worker on the host.
                self.unwatched.set_revealed(false);
                // A new server has each document as the tab last tried to send it, and an edit
                // refused while it was coming up is newer; the refresh sends that, and asks for
                // the symbols and folds of a server that has only just heard of the tab.
                for tab in self.open_tabs() {
                    lang::resync(&tab);
                }
                for notebook in self.notebooks() {
                    notebook.resync_language();
                }
                if let Some(tree) = self.tree.get() {
                    tree.refresh();
                }
                if let Some(sidebar) = self.sidebar.get() {
                    sidebar.mark_tags_dirty();
                    sidebar.refresh_ports();
                }
                if let Some(git) = self.git.get() {
                    // Nothing was discovered while the link was down, so this starts at the top.
                    git.schedule_refresh(git::Depth::Discover);
                }
                self.refresh_corpus(false);
                self.refresh_symbols();
                // A file the host changed while the link was down is news to no tab: the new
                // server reports only what its own walk wrote, which the old one or another accent
                // on the host may have written first, and its report can land before this, when
                // no stat is answered. So every document open across the drop looks at its file
                // again, as a watcher's news makes it (a text reads only a file that moved), and a
                // PDF drawn on meanwhile sends what it holds instead. The session's are read fresh.
                for file in self.docs().iter().filter_map(Doc::file) {
                    self.changed_on_disk(&file);
                }
                self.restore_session();
                self.sync_active();
                // What was drawn while the link was down in a tab closed since reached only the
                // file here, and the notes' page links into it were not rewritten.
                self.push_closed_pdfs();
                self.relink_all();
                // A remote shell the drop ended kept its tab, and starts again in it; one whose tab
                // was closed meanwhile is ended now.
                for term in self.terminals() {
                    term.reopen();
                }
                if let Some(remote) = self.vault().and_then(|v| v.remote()) {
                    terminal::end_orphans(remote.control_path());
                }
            }
            Event::Disconnected(why) => {
                self.statusbar.set_progress(None);
                self.connect.hide();
                self.connection_down(&why);
                self.debug_lost();
                self.kernels_lost();
            }
            Event::Refused(why) => {
                self.statusbar.set_progress(None);
                self.connect.hide();
                self.connection_refused(&why);
            }
            // No pause: the worker walks again when next asked, and the index stays partial,
            // `reconciled` with it. Its error, if any is new, is an `Error` of its own.
            Event::WalkFailed => {
                self.statusbar.set_progress(None);
                self.statusbar.set_indexing(statusbar::Indexing::Idle);
            }
            // The same, its reason an `Error` too, and for good: nothing watches or indexes the
            // vault until a new worker does, which the banner's Reload Window starts.
            Event::WorkerGone => {
                self.statusbar.set_progress(None);
                self.statusbar.set_indexing(statusbar::Indexing::Idle);
                self.unwatched.set_revealed(true);
            }
            Event::Error(message) => self.toast(&message),
            Event::Debug { session, news } => self.on_debug(session, news),
            Event::Kernel { session, news } => self.on_kernel(session, news),
            // Every tab of the file holds the check, so a switch to one shows it.
            Event::Checking { rel, lines } => {
                let line = lines.first().map(|lines| lines.start);
                for tab in self.open_tabs().iter().filter(|tab| tab.rel() == rel) {
                    tab.lang.checking.set(line);
                    tab.set_checking(lines.clone());
                    if self.is_active(tab) {
                        self.statusbar.set_checking(line);
                        // What was asked while it checked may have gone unanswered; and
                        // until a first answer, the Code pane says why there is none.
                        if line.is_none() {
                            self.refresh_goals();
                        } else if let Some(sidebar) = self.sidebar.get() {
                            sidebar.sync_code(&rel, true);
                        }
                    }
                }
            }
            // Over the tabs of its own files, naming the document, until it builds again.
            Event::Compiled { rel, failed, files } => {
                self.statusbar.set_compiled(&rel, failed, files)
            }
            Event::Diagnostics { rel, items, stale } => {
                if let Some(tab) = self.tab_for(&rel) {
                    tab.lang.doc.published(stale);
                    tab.set_diagnostics(items);
                    // The count lives in the status bar, which only speaks for the active tab.
                    if self.is_active(&tab) {
                        self.sync_status();
                    }
                } else if let Some(notebook) =
                    self.notebooks().into_iter().find(|n| n.has_document(&rel))
                {
                    notebook.set_diagnostics(&rel, items, stale);
                    if self
                        .active_notebook()
                        .is_some_and(|n| Rc::ptr_eq(&n, &notebook))
                    {
                        self.sync_status();
                    }
                }
            }
        }
    }
}

/// Forward worker events through a channel awaited by the main loop.
///
/// A blocking relay thread avoids periodic idle wakeups. It ends when the vault drops its
/// sender or the next event finds the window gone.
pub fn start_events(app: &Rc<App>, events: Receiver<Event>) {
    // Weak: `Shell.windows` holds the only strong `App`, so closing a window drops it along with
    // its vault and its worker thread.
    let app = Rc::downgrade(app);
    let (tx, mut rx) = futures_channel::mpsc::unbounded();
    let _ = std::thread::Builder::new()
        .name("accent-events".to_string())
        .spawn(move || {
            for event in events {
                if tx.unbounded_send(event).is_err() {
                    break;
                }
            }
        });
    glib::spawn_future_local(async move {
        while let Ok(event) = rx.recv().await {
            let Some(app) = app.upgrade() else {
                return;
            };
            #[cfg(feature = "bench")]
            crate::bench::heard(&event);
            app.on_event(event);
        }
    });
}
