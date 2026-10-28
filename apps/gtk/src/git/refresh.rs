//! The refresh that feeds the pane: asking git again, debounced and coalesced, one round at a
//! time off the main thread, and putting what it answered on screen.

use super::*;

/// What a refresh asks git: everything a [`Depth`] says of every repository, or the status alone
/// of the repositories holding some paths that changed.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Ask {
    Every(Depth),
    /// Absolute paths. A file outside every repository asks git nothing.
    Paths(HashSet<PathBuf>),
}

impl From<Depth> for Ask {
    fn from(depth: Depth) -> Ask {
        Ask::Every(depth)
    }
}

impl Ask {
    /// `paths` changed, or with `gone` went: the status of their repositories, unless one of them
    /// was a repository or held one, which leaves the repositories to be looked for again.
    pub(super) fn changed(paths: HashSet<PathBuf>, gone: bool, repos: &[Repo]) -> Ask {
        let lost = |repo: &Repo| paths.iter().any(|path| repo.root.starts_with(path));
        match gone && repos.iter().any(lost) {
            true => Ask::Every(Depth::Discover),
            false => Ask::Paths(paths),
        }
    }

    pub(super) fn depth(&self) -> Depth {
        match self {
            Ask::Every(depth) => *depth,
            Ask::Paths(_) => Depth::Status,
        }
    }

    /// Whether `repo`'s status is asked: for a path, the repository holding it and any that one
    /// is nested in, whose status a submodule's change is part of.
    pub(super) fn asks(&self, repo: &Repo) -> bool {
        match self {
            Ask::Every(_) => true,
            Ask::Paths(paths) => paths.iter().any(|path| path.starts_with(&repo.root)),
        }
    }
}

/// `ask` added to what was asked already: the deeper of the two, and the paths of both unless
/// either asks every repository.
fn joined(asked: Option<Ask>, ask: Ask) -> Ask {
    match (asked, ask) {
        (None, ask) => ask,
        (Some(Ask::Paths(mut a)), Ask::Paths(b)) => {
            a.extend(b);
            Ask::Paths(a)
        }
        (Some(a), b) => Ask::Every(a.depth().max(b.depth())),
    }
}

/// Only one refresh per source runs at a time; a new focused shell starts without waiting for
/// the old source's network call, whose late answer is ignored.
#[derive(Default)]
pub(super) struct RefreshGate {
    active: Option<u64>,
    again: Option<Ask>,
}

impl RefreshGate {
    pub(super) fn invalidate(&mut self) {
        self.active = None;
        self.again = None;
    }

    fn begin(&mut self, generation: u64, ask: &Ask) -> bool {
        if self.active == Some(generation) {
            self.again = Some(joined(self.again.take(), ask.clone()));
            return false;
        }
        self.active = Some(generation);
        self.again = None;
        true
    }

    fn finish(&mut self, generation: u64) -> Option<Ask> {
        if self.active != Some(generation) {
            return None;
        }
        self.active = None;
        self.again.take()
    }
}

impl Panel {
    /// Ask git again, once, in [`DEBOUNCE`], for at least `depth`. Calling this ten times in a
    /// row is one query, and the deepest of the ten is what it asks for.
    pub fn schedule_refresh(self: &Rc<Self>, depth: Depth) {
        self.schedule(Ask::Every(depth));
    }

    /// [`Panel::schedule_refresh`] for `keys` (vault keys) having changed, or with `gone` gone:
    /// only the repositories holding them are asked their status, so a save outside all of them
    /// costs no git process. One of them that was a repository or held one is looked for again.
    pub fn schedule_paths(
        self: &Rc<Self>,
        keys: impl IntoIterator<Item = impl AsRef<Path>>,
        gone: bool,
    ) {
        let root = self.source().key_root();
        let paths = keys.into_iter().map(|key| root.join(key)).collect();
        let ask = Ask::changed(paths, gone, &self.state.borrow().repos);
        self.schedule(ask);
    }

    fn schedule(self: &Rc<Self>, ask: Ask) {
        self.pending_ask
            .set(Some(joined(self.pending_ask.take(), ask)));
        let panel = self.clone();
        self.pending.call(move || {
            let ask = panel.pending_ask.take().unwrap_or(Depth::Status.into());
            panel.refresh(ask);
        });
    }

    /// Look for repositories again, at most once per [`REDISCOVER`]. For the indexing progress,
    /// which arrives many times a second, and for `.git` writes: [`Panel::schedule_refresh`]
    /// restarts its timer on every call, so passing each tick on would put the refresh off until
    /// the walk was over. One asked too soon asks again a [`REDISCOVER`] later instead, unless a
    /// look starts meanwhile: a `git init` just after a commit is found all the same.
    pub fn rediscover(self: &Rc<Self>) {
        let now = glib::monotonic_time();
        if now - self.discovered.get() < REDISCOVER.as_micros() as i64 {
            let panel = Rc::downgrade(self);
            self.rediscover_later.call_once(move || {
                if let Some(panel) = panel.upgrade() {
                    panel.rediscover();
                }
            });
            return;
        }
        self.discovered.set(now);
        self.schedule_refresh(Depth::Discover);
    }

    /// Ask git what `ask` says the pane needs, off the main thread, and put the answers on
    /// screen. Whatever it did not ask for, the pane keeps.
    pub(super) fn refresh(self: &Rc<Self>, ask: impl Into<Ask>) {
        let ask = ask.into();
        let generation = self.source_generation.get();
        if !self.refresh_gate.borrow_mut().begin(generation, &ask) {
            return;
        }
        // A look starting now answers every [`Panel::rediscover`] asked before it.
        if ask.depth() == Depth::Discover {
            self.discovered.set(glib::monotonic_time());
            self.rediscover_later.cancel();
        }
        let vault = self.source();
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
                fetch::fetch(&vault, selected, &ask, known, rows)
            })
            .await;
            let again = panel.refresh_gate.borrow_mut().finish(generation);
            if generation == panel.source_generation.get()
                && let Some(Some(fetched)) = fetched
            {
                panel.apply(fetched);
            }
            if let Some(ask) = again {
                panel.refresh(ask);
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
        let status_kept = match fetched.selected_asked {
            true => fetched.statuses.get(selected).is_some_and(Option::is_none),
            false => self.state.borrow().status_kept,
        };
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
                    .map(|path| ignored_key(&self.source().key_root(), repo, path))
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
        let read_history = fetched.commits.is_some();

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
            self.fill_log(page);
        }
        // The first refresh to read the history after a commit from the button has the commit.
        if read_history && self.to_head.take() {
            self.focus_head();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_source_starts_while_old_refresh_is_busy() {
        let mut gate = RefreshGate::default();
        assert!(gate.begin(1, &Depth::Discover.into()));
        assert!(!gate.begin(1, &Depth::Status.into()));
        gate.invalidate();
        assert!(gate.begin(2, &Depth::Discover.into()));
        assert_eq!(gate.finish(1), None);
        assert_eq!(gate.active, Some(2));
        assert_eq!(gate.finish(2), None);
    }

    fn paths(paths: &[&str]) -> HashSet<PathBuf> {
        paths.iter().map(PathBuf::from).collect()
    }

    fn repo(root: &str) -> Repo {
        Repo {
            root: PathBuf::from(root),
            git_dir: PathBuf::from(root).join(".git"),
            name: "r".to_string(),
        }
    }

    #[test]
    fn a_change_asks_only_the_repositories_holding_it() {
        let repos = [repo("/v/a"), repo("/v/a/inner"), repo("/v/b")];
        let asked = |ask: &Ask| -> Vec<bool> { repos.iter().map(|r| ask.asks(r)).collect() };
        // The innermost repository and the one it is nested in, which may have it as a submodule.
        let ask = Ask::changed(paths(&["/v/a/inner/x.md"]), false, &repos);
        assert_eq!(asked(&ask), [true, true, false]);
        // A file at the root of a vault that is no repository, and a sibling sharing a prefix.
        let ask = Ask::changed(paths(&["/v/test.pdf", "/v/bb/x.md"]), false, &repos);
        assert_eq!(asked(&ask), [false, false, false]);
        // A repository's own root, as a folder whose listing changed.
        assert_eq!(
            asked(&Ask::changed(paths(&["/v/b"]), false, &repos)),
            [false, false, true]
        );
        assert!(asked(&Ask::Every(Depth::Status)).iter().all(|&a| a));
    }

    #[test]
    fn a_repository_gone_is_looked_for_again() {
        let repos = [repo("/v/a"), repo("/v/a/inner")];
        let discover = Ask::Every(Depth::Discover);
        assert_eq!(Ask::changed(paths(&["/v/a/inner"]), true, &repos), discover);
        assert_eq!(Ask::changed(paths(&["/v"]), true, &repos), discover);
        // A file going is a change of its repository's status, and a folder changing is no loss.
        let file = paths(&["/v/a/x.md"]);
        assert_eq!(Ask::changed(file.clone(), true, &repos), Ask::Paths(file));
        let folder = paths(&["/v"]);
        assert_eq!(
            Ask::changed(folder.clone(), false, &repos),
            Ask::Paths(folder)
        );
    }

    #[test]
    fn asks_join_into_the_deeper_and_the_paths_of_both() {
        let (a, b) = (Ask::Paths(paths(&["/a"])), Ask::Paths(paths(&["/b"])));
        assert_eq!(joined(Some(a.clone()), b), Ask::Paths(paths(&["/a", "/b"])));
        assert_eq!(
            joined(Some(a.clone()), Ask::Every(Depth::Status)),
            Ask::Every(Depth::Status)
        );
        assert_eq!(
            joined(Some(Ask::Every(Depth::Everything)), a),
            Ask::Every(Depth::Everything)
        );
    }
}
