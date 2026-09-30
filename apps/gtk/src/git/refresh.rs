//! The refresh that feeds the pane: asking git again, debounced and coalesced, one round at a
//! time off the main thread, and putting what it answered on screen.

use super::*;

impl Panel {
    /// Ask git again, once, in [`DEBOUNCE`], for at least `depth`. Calling this ten times in a
    /// row is one query, and the deepest of the ten is what it asks for.
    pub fn schedule_refresh(self: &Rc<Self>, depth: Depth) {
        self.pending_depth
            .set(self.pending_depth.get().max(Some(depth)));
        let panel = self.clone();
        self.pending.call(move || {
            let depth = panel.pending_depth.take().unwrap_or(Depth::Status);
            panel.refresh(depth);
        });
    }

    /// Look for repositories again, at most once per [`REDISCOVER`]. For the indexing progress,
    /// which arrives many times a second: [`Panel::schedule_refresh`] restarts its timer on every
    /// call, so passing each tick on would put the refresh off until the walk was over.
    pub fn rediscover(self: &Rc<Self>) {
        let now = glib::monotonic_time();
        if now - self.discovered.get() < REDISCOVER.as_micros() as i64 {
            return;
        }
        self.discovered.set(now);
        self.schedule_refresh(Depth::Discover);
    }

    /// Ask git what `depth` says the pane needs, off the main thread, and put the answers on
    /// screen. Whatever it did not ask for, the pane keeps.
    pub(super) fn refresh(self: &Rc<Self>, depth: Depth) {
        if self.busy.get() {
            self.again.set(self.again.get().max(Some(depth)));
            return;
        }
        self.busy.set(true);
        let vault = self.hooks.vault.clone();
        // As many commits as are shown, so the pages a Load More brought in outlive a history
        // that moved: a bigger `git log` on every save, but only once Load More was used.
        let (selected, known, rows) = {
            let state = self.state.borrow();
            let rows = state.commits.len().max(PAGE);
            (state.selected, state.repos.clone(), rows)
        };
        let panel = self.clone();
        glib::spawn_future_local(async move {
            let fetched = crate::work::off_thread("git", move || {
                fetch::fetch(&vault, selected, depth, known, rows)
            })
            .await;
            panel.busy.set(false);
            if let Some(fetched) = fetched {
                panel.apply(fetched);
            }
            if let Some(depth) = panel.again.take() {
                panel.refresh(depth);
            }
        });
    }

    fn apply(self: &Rc<Self>, fetched: fetch::Fetched) {
        // The chooser moved while git was answering, so this is the old repository's answer.
        // Dropping it is safe: changing the selection scheduled a refresh of its own.
        if self.state.borrow().selected != fetched.selected {
            return;
        }
        // A refusal is not an answer: where git could not be asked, the pane keeps what it had
        // rather than emptying itself. Only the fields the last refresh really learned move.
        let repos = match fetched.repos {
            Some(repos) => repos,
            None => self.state.borrow().repos.clone(),
        };
        if self.state.borrow().repos != repos {
            self.syncing.set(true);
            let names: Vec<&str> = repos.iter().map(|r| r.name.as_str()).collect();
            self.names.splice(0, self.names.n_items(), &names);
            let selected = clamp(self.state.borrow().selected, repos.len());
            self.state.borrow_mut().selected = selected;
            self.chooser.set_selected(selected as u32);
            self.syncing.set(false);
        }
        self.chooser.set_visible(repos.len() > 1);

        let selected = clamp(self.state.borrow().selected, repos.len());
        let status_kept = fetched.statuses.get(selected).is_some_and(Option::is_none);
        let statuses = {
            let state = self.state.borrow();
            merge_statuses(&repos, fetched.statuses, &state.repos, &state.statuses)
        };
        let heads: HashMap<PathBuf, String> = repos
            .iter()
            .zip(&statuses)
            .filter_map(|(repo, status)| Some((repo.git_dir.clone(), status.branch.oid.clone()?)))
            .collect();
        let ignored = repos
            .iter()
            .zip(&statuses)
            .flat_map(|(repo, status)| {
                status
                    .ignored
                    .iter()
                    .map(|path| ignored_key(&self.hooks.vault.root(), repo, path))
            })
            .collect();

        // Most refreshes read back the history that is already on screen — a save, a watcher
        // event and a `.git` write each schedule one — and splicing then costs an expanded commit
        // its file list and flashes every row, so only a real difference is drawn. A page that
        // has not moved also leaves whatever Load More added below it alone.
        // The incoming set is part of what a row draws, and pulling a fast-forward leaves the
        // commit list from `--all` exactly as it was — same oids, same order — so without this
        // the marks would survive the pull that cleared them.
        // A refresh that did not read the history has nothing to say about it.
        let moved = match &fetched.commits {
            Some(commits) => {
                let state = self.state.borrow();
                !same_head(&state.commits, commits)
                    || fetched
                        .incoming
                        .as_ref()
                        .is_some_and(|i| state.incoming != *i)
            }
            None => false,
        };
        let page = moved.then(|| fetched.commits.clone().unwrap_or_default());

        {
            let mut state = self.state.borrow_mut();
            state.head_moved = state.heads != heads;
            state.heads = heads;
            state.ignored = ignored;
            state.repos = repos;
            state.statuses = statuses;
            state.status_kept = status_kept;
            if let (true, Some(commits)) = (moved, fetched.commits) {
                state.commits = commits;
            }
            if let Some(branches) = fetched.branches {
                state.branches = branches;
            }
            if let Some(submodules) = fetched.submodules {
                state.submodules = submodules;
            }
            if let Some(incoming) = fetched.incoming {
                state.incoming = incoming;
            }
            state.selected = selected;
            let State { repos, seen, .. } = &mut *state;
            seen.retain(|dir, _| repos.iter().any(|repo| repo.git_dir == *dir));
        }
        // git has answered for the first time since the window opened this vault, so there is a
        // repository to fetch at last. Everything after this is the timer's.
        if !self.state.borrow().repos.is_empty() && !self.fetched_once.replace(true) {
            self.autofetch();
        }
        self.draw();
        tracing::debug!(
            repos = self.state.borrow().repos.len(),
            status_kept,
            branch = %self.branch_label.text(),
            "git refresh landed"
        );
        if let Some(page) = page {
            self.has_more.set(page.len() >= fetched.rows);
            self.fill_log(page, 0);
        }
        (self.hooks.changed)();
        self.reload_diffs();
    }

    /// Put the selected repository on screen as the state has it, bar the history: the branch
    /// row, the banner, the commit box and the changes. After the state is written, so that the
    /// tree toggle and a folder's chevron redraw the same rows without a `git status` of their own.
    pub(super) fn draw(self: &Rc<Self>) {
        let (counts, (rows, at)) = {
            let state = self.state.borrow();
            let status = state.statuses.get(state.selected);
            let counts = status
                .and_then(|s| branch_parts(&s.branch))
                .map(|(_, counts)| counts)
                .unwrap_or_default();
            let name = status.and_then(|s| head_name(s, Some(&state.branches)));
            (counts, branch_model(name, &state.branches))
        };
        self.counts.set_text(&counts);
        // Hidden rather than empty: the box spends its spacing on an empty label too, which made
        // the Sync button wider than its icon and put the icon off its centre.
        self.counts.set_visible(!counts.is_empty());
        self.set_branches(&rows, at);
        self.sync_state();
        let rebasing = self.rebasing();
        self.banner.set_title(match rebasing {
            true => "A rebase is in progress",
            false => "A merge is in progress",
        });
        self.banner.set_revealed(rebasing || self.merging());
        self.rebuild_changes();
        self.sync_commit();
    }
}
