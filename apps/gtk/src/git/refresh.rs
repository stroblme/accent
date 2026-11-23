//! The refresh that feeds the pane: asking git again, debounced and coalesced, one round at a
//! time off the main thread, and putting what it answered on screen.

use super::*;

/// What waits on a refresh being on screen ([`Panel::refresh_now`]).
pub(super) type Shown = Box<dyn FnOnce(&Rc<Panel>)>;

/// One step of a list's bar while a refresh is on its way, as the search pane's
/// (DESIGN.md, Loading).
const WAIT_PULSE: Duration = Duration::from_millis(80);

/// How many steps a refresh takes before its bar shows: 160 ms, which most never reach.
const WAIT_AFTER: u32 = 2;

/// What a refresh asks git: everything a [`Depth`] says of every repository, or of the
/// repositories holding some paths: a file that changed, the repository a command ran in.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Ask {
    Every(Depth),
    /// Absolute paths, at a depth below [`Depth::Discover`]. A file outside every repository asks
    /// git nothing.
    Paths(HashSet<PathBuf>, Depth),
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
            false => Ask::Paths(paths, Depth::Status),
        }
    }

    /// Everything about the repositories rooted at `roots`, the ones a command ran git in, and
    /// those they are nested in; with none, the repositories are looked for again.
    pub(super) fn repos(roots: impl IntoIterator<Item = PathBuf>) -> Ask {
        let roots: HashSet<PathBuf> = roots.into_iter().collect();
        match roots.is_empty() {
            true => Ask::Every(Depth::Discover),
            false => Ask::Paths(roots, Depth::Everything),
        }
    }

    pub(super) fn depth(&self) -> Depth {
        match self {
            Ask::Every(depth) | Ask::Paths(_, depth) => *depth,
        }
    }

    /// Whether `repo`'s status is asked: for a path, the repository holding it and any that one
    /// is nested in, whose status a submodule's change is part of.
    pub(super) fn asks(&self, repo: &Repo) -> bool {
        match self {
            Ask::Every(_) => true,
            Ask::Paths(paths, _) => paths.iter().any(|path| path.starts_with(&repo.root)),
        }
    }
}

/// `ask` added to what was asked already: the deeper of the two, and the paths of both unless
/// either asks every repository.
fn joined(asked: Option<Ask>, ask: Ask) -> Ask {
    match (asked, ask) {
        (None, ask) => ask,
        (Some(Ask::Paths(mut a, x)), Ask::Paths(b, y)) => {
            a.extend(b);
            Ask::Paths(a, x.max(y))
        }
        (Some(a), b) => Ask::Every(a.depth().max(b.depth())),
    }
}

/// One refresh is put on screen at a time, each numbered as it starts. One asked meanwhile waits
/// for it, unless someone is waiting on it — a command's outcome, a pick — which starts at once:
/// the one under way read git before what it waits on, so its answer is dropped when it lands,
/// and what it asked is asked again after. A new focused shell starts afresh the same way.
#[derive(Default)]
pub(super) struct RefreshGate {
    /// The refresh whose answer is wanted, and what it asks.
    active: Option<(u64, Ask)>,
    /// How many refreshes have started.
    started: u64,
    again: Option<Ask>,
}

impl RefreshGate {
    pub(super) fn invalidate(&mut self) {
        self.active = None;
        self.again = None;
    }

    /// The number the refresh asking `ask` starts under, or `None` where it waits; `now` starts
    /// it at once.
    fn begin(&mut self, ask: &Ask, now: bool) -> Option<u64> {
        if let Some((_, under_way)) = &self.active {
            let waits = match now {
                true => under_way.clone(),
                false => ask.clone(),
            };
            self.again = Some(joined(self.again.take(), waits));
            if !now {
                return None;
            }
        }
        self.started += 1;
        self.active = Some((self.started, ask.clone()));
        Some(self.started)
    }

    /// Whether refresh `n`'s answer is still wanted, and what was asked meanwhile, to ask next.
    fn finish(&mut self, n: u64) -> (bool, Option<Ask>) {
        if self.active.as_ref().is_none_or(|(at, _)| *at != n) {
            return (false, None);
        }
        self.active = None;
        (true, self.again.take())
    }

    /// What the refresh under way and the one waiting ask between them.
    fn outstanding(&self) -> Option<Ask> {
        let active = self.active.as_ref().map(|(_, ask)| ask.clone());
        match self.again.clone() {
            Some(again) => Some(joined(active, again)),
            None => active,
        }
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

    /// A write in `git_dir`, a repository's own directory: everything about the repositories it
    /// is the git dir of, a linked worktree's sharing its refs, and those they are nested in.
    ///
    /// A write in a `.git` the pane holds no repository of may be a new one's — `git init` in a
    /// terminal, a clone into the vault root — so that one goes looking for repositories, which
    /// is what makes the pane show itself with nothing to click. Where the vault has some, the
    /// look is throttled: discovery is a `git rev-parse` per indexed directory, and a burst of
    /// writes pays for it once.
    pub fn git_written(self: &Rc<Self>, git_dir: Option<&Path>) {
        let roots: Vec<PathBuf> = match git_dir {
            Some(dir) => (self.state.borrow().repos.iter())
                .filter(|repo| {
                    repo.git_dir == dir || repo.git_dir.starts_with(dir.join("worktrees"))
                })
                .map(|repo| repo.root.clone())
                .collect(),
            None => Vec::new(),
        };
        match (roots.is_empty(), self.has_repos()) {
            (false, _) => self.schedule(Ask::repos(roots)),
            (true, true) => self.rediscover(),
            (true, false) => self.schedule_refresh(Depth::Discover),
        }
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
    /// which arrives many times a second, and for writes in a `.git` the pane holds no repository
    /// of ([`Panel::git_written`]): [`Panel::schedule_refresh`]
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
        self.ask_git(ask.into(), None);
    }

    /// [`Panel::refresh`] for someone waiting on it, which starts at once ([`RefreshGate`]):
    /// `then` runs once its answer, or a later one, is on screen.
    pub(super) fn refresh_now(self: &Rc<Self>, ask: Ask, then: impl FnOnce(&Rc<Panel>) + 'static) {
        self.ask_git(ask, Some(Box::new(then)));
    }

    /// Run whatever waits on the refreshes up to `n` being on screen, every one of them for a
    /// pane that has moved to another source.
    pub(super) fn shown_up_to(self: &Rc<Self>, n: u64) {
        let ready: Vec<Shown> = {
            let mut waiting = self.waiting.borrow_mut();
            let (ready, later) = waiting.drain(..).partition(|(at, _)| *at <= n);
            *waiting = later;
            ready.into_iter().map(|(_, then)| then).collect()
        };
        for then in ready {
            then(self);
        }
    }

    /// The bar over each list a refresh on its way will replace (DESIGN.md, Loading): the
    /// changes for one asking the selected repository's status, the history for one reading it.
    pub(super) fn show_waits(&self) {
        let outstanding = self.refresh_gate.borrow().outstanding();
        let state = self.state.borrow();
        let selected = state.repos.get(state.selected);
        let changes = outstanding
            .as_ref()
            .is_some_and(|ask| selected.is_some_and(|repo| ask.asks(repo)));
        let history = outstanding.as_ref().is_some_and(|ask| {
            ask.depth() >= Depth::Everything && selected.is_none_or(|repo| ask.asks(repo))
        });
        for (pulse, on) in [(&self.changes_wait, changes), (&self.log_wait, history)] {
            match on {
                true => pulse.start(WAIT_PULSE, WAIT_AFTER),
                false => pulse.hide(),
            }
        }
    }

    fn ask_git(self: &Rc<Self>, ask: Ask, then: Option<Shown>) {
        let generation = self.source_generation.get();
        let begun = self.refresh_gate.borrow_mut().begin(&ask, then.is_some());
        self.show_waits();
        let Some(n) = begun else {
            return;
        };
        if let Some(then) = then {
            self.waiting.borrow_mut().push((n, then));
        }
        // A look starting now answers every [`Panel::rediscover`] asked before it.
        if ask.depth() == Depth::Discover {
            self.discovered.set(glib::monotonic_time());
            self.rediscover_later.cancel();
        }
        let vault = self.source();
        // As many commits as are shown, so the pages a Load More brought in outlive a history
        // that moved: a bigger `git log` on every save, but only once Load More was used.
        let (selected, known, rows, recent) = {
            let state = self.state.borrow();
            let rows = state.commits.len().max(PAGE);
            (
                state.selected,
                state.repos.clone(),
                rows,
                state.recent.clone(),
            )
        };
        let panel = self.clone();
        glib::spawn_future_local(async move {
            let fetched = crate::work::off_thread("git", move || {
                fetch::fetch(&vault, selected, &ask, known, rows, &recent)
            })
            .await;
            let (wanted, again) = panel.refresh_gate.borrow_mut().finish(n);
            if wanted
                && generation == panel.source_generation.get()
                && let Some(Some(fetched)) = fetched
            {
                panel.apply(fetched);
            }
            if wanted {
                panel.shown_up_to(n);
            }
            match again {
                Some(ask) => panel.refresh(ask),
                None => panel.show_waits(),
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
            let selected = {
                let state = self.state.borrow();
                reselect(state.selected, &state.repos, &repos)
            };
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
        // A refused log leaves the rows on screen, and where it is a first load there are none.
        let failed = (fetched.log_refused).filter(|_| self.state.borrow().commits.is_empty());

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
        if read_history || failed.is_some() {
            self.history_failed(failed.as_deref());
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
        // Hidden rather than empty, which shows the arrows in its place (`build_branch_row`).
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
        assert_eq!(gate.begin(&Depth::Discover.into(), false), Some(1));
        assert_eq!(gate.begin(&Depth::Status.into(), false), None);
        gate.invalidate();
        assert_eq!(gate.begin(&Depth::Discover.into(), false), Some(2));
        assert_eq!(gate.finish(1), (false, None));
        assert_eq!(gate.finish(2), (true, None));
    }

    #[test]
    fn a_refresh_waited_on_drops_the_one_under_way_and_asks_its_question_after() {
        let mut gate = RefreshGate::default();
        let mine = Ask::repos([PathBuf::from("/v")]);
        assert_eq!(gate.begin(&Depth::Discover.into(), false), Some(1));
        assert_eq!(gate.begin(&Depth::Status.into(), false), None);
        // A command's outcome starts at once, and nothing the two before it asked is lost.
        assert_eq!(gate.begin(&mine, true), Some(2));
        assert_eq!(gate.outstanding(), Some(Ask::Every(Depth::Discover)));
        // The refresh that read git before the command lands with an answer nobody wants.
        assert_eq!(gate.finish(1), (false, None));
        assert_eq!(gate.finish(2), (true, Some(Ask::Every(Depth::Discover))));
        assert_eq!(gate.outstanding(), None);
        // With nothing under way it starts as any other does, and waits for nothing after it.
        assert_eq!(gate.begin(&mine, true), Some(3));
        assert_eq!(gate.finish(3), (true, None));
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
        assert_eq!(
            Ask::changed(file.clone(), true, &repos),
            Ask::Paths(file, Depth::Status)
        );
        let folder = paths(&["/v"]);
        assert_eq!(
            Ask::changed(folder.clone(), false, &repos),
            Ask::Paths(folder, Depth::Status)
        );
    }

    #[test]
    fn asks_join_into_the_deeper_and_the_paths_of_both() {
        let a = Ask::Paths(paths(&["/a"]), Depth::Status);
        let b = Ask::Paths(paths(&["/b"]), Depth::Everything);
        assert_eq!(
            joined(Some(a.clone()), b),
            Ask::Paths(paths(&["/a", "/b"]), Depth::Everything)
        );
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
