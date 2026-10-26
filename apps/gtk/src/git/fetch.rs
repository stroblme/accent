//! The worker's half: one refresh's reads, and the background fetch that keeps the counts true.

use super::*;

/// Autofetch interval for the focused window, matching VS Code's default. Bounds network
/// activity while keeping upstream history reasonably current; unfocused windows skip fetches.
const AUTOFETCH: Duration = Duration::from_secs(300);

impl Panel {
    /// Fetch only the selected repository off the main thread, then refresh. Avoid timer-driven
    /// network work for repositories not being viewed. Failures appear in the Sync tooltip
    /// rather than interrupting the user with notifications.
    pub(super) fn autofetch(self: &Rc<Self>) {
        // Nor while a command runs: a sync's pull fetches anyway, and two fetches race for the
        // same refs.
        if self.fetch_busy.get() || self.held() {
            return;
        }
        let Some(repo) = ({
            let state = self.state.borrow();
            state.repos.get(state.selected).cloned()
        }) else {
            return;
        };
        self.fetch_busy.set(true);
        let (vault, lock) = (self.source(), self.fetch_lock.clone());
        let roots = if vault.is_remote() {
            Vec::new()
        } else {
            vec![repo.root.clone()]
        };
        self.running_git.borrow_mut().start(&roots);
        let generation = self.source_generation.get();
        let panel = self.clone();
        // As a command does: the process outlives the window until the fetch is stopped.
        let hold = self.hooks.window.application().map(|app| app.hold());
        glib::spawn_future_local(async move {
            let fetched = crate::work::off_thread("git fetch", move || {
                let _held = lock.lock();
                vault.git_fetch(&repo)
            })
            .await;
            drop(hold);
            panel.running_git.borrow_mut().finish(&roots);
            panel.fetch_busy.set(false);
            if panel.gone.get() {
                return;
            }
            if panel.source_generation.get() != generation {
                // A newly focused shell may have discovered its repository while this old fetch
                // was in flight. Give it its first fetch now rather than at the next five-minute tick.
                panel.autofetch();
                return;
            }
            let failed = !matches!(fetched, Some(Ok(_)));
            if panel.fetch_failed.replace(failed) != failed {
                panel.sync_state();
            }
            match fetched {
                // A fetch that brought nothing prints nothing, so this is quiet in the common case.
                Some(Ok(transcript)) => {
                    if !transcript.is_empty() {
                        tracing::debug!("git fetch: {transcript}");
                    }
                }
                // The autofetch is the one command nobody asked for, so it stays in the log
                // whatever happens: a toast here would interrupt someone who is writing.
                Some(Err(e)) => return tracing::debug!("git fetch: {e:#}"),
                None => return,
            }
            // `.git/refs/remotes` is not among the paths the vault watches, so what a fetch moved
            // is only seen because we ask.
            panel.schedule_refresh(Depth::Everything);
        });
    }

    /// Start the timer that keeps the remote-tracking refs current.
    ///
    /// The tick is gated on the window having the focus rather than started and stopped, because
    /// a `GSource` removed and re-added would also restart its five minutes; what a skipped tick
    /// leaves behind is a flag the next focus-in reads.
    pub(super) fn wire_autofetch(self: &Rc<Self>) {
        let (weak, window) = (Rc::downgrade(self), self.hooks.window.downgrade());
        glib::timeout_add_local(AUTOFETCH, move || {
            let (Some(panel), Some(window)) = (weak.upgrade(), window.upgrade()) else {
                return glib::ControlFlow::Break;
            };
            match window.is_active() {
                true => panel.autofetch(),
                false => panel.missed_fetch.set(true),
            }
            glib::ControlFlow::Continue
        });
        // Weak both ways: the window owns the sidebar that owns this pane, and the pane holds the
        // window through its hooks, so a strong capture here is a cycle neither side can break.
        let weak = Rc::downgrade(self);
        self.hooks
            .window
            .connect_notify_local(Some("is-active"), move |window, _| {
                if let Some(panel) = weak.upgrade()
                    && window.is_active()
                    && panel.missed_fetch.replace(false)
                {
                    panel.autofetch();
                }
            });
    }
}

/// Refresh results: all repository statuses, plus selected history and submodules. Optional
/// fields distinguish unavailable answers from empty results, allowing the pane to retain prior
/// data during failures.
pub(super) struct Fetched {
    /// Which repository this was asked about. A chooser moved while the read was in flight makes
    /// the answer somebody else's: `apply` drops it rather than storing one repository's history
    /// under another's index, and the refresh the chooser scheduled is the one that lands.
    pub(super) selected: usize,
    /// How many commits the history was asked for: fewer back is all of it.
    pub(super) rows: usize,
    pub(super) repos: Option<Vec<Repo>>,
    /// One per repository asked about, `None` where git did not answer.
    pub(super) statuses: Vec<Option<Status>>,
    pub(super) commits: Option<Vec<Commit>>,
    pub(super) branches: Option<git::Branches>,
    pub(super) submodules: Option<Vec<Submodule>>,
    /// The commits a pull would bring in, which is what marks the history's rows. Asked for only
    /// where the branch says there are any, so an up-to-date repository pays nothing for it.
    pub(super) incoming: Option<HashSet<String>>,
}

/// Read what `depth` asks for. `known` is the repositories the pane already holds, which is what
/// a refresh below [`Depth::Discover`] runs against rather than looking for them again, and `rows`
/// how many commits of history the pane shows.
pub(super) fn fetch(
    vault: &Source,
    selected: usize,
    depth: Depth,
    known: Vec<Repo>,
    rows: usize,
) -> Fetched {
    // A refusal is not "there are no repositories": on `Err` the pane keeps the ones it had,
    // which is what a remote whose link dropped mid-refresh needs.
    let repos = match (depth >= Depth::Discover).then(|| vault.repos()) {
        Some(Ok(repos)) => Some(repos),
        Some(Err(e)) => {
            tracing::debug!("listing the repositories: {e}");
            None
        }
        None => None,
    };
    let against: &[Repo] = repos.as_deref().unwrap_or(&known);
    let statuses: Vec<Option<Status>> = against
        .iter()
        .map(|repo| match vault.git_status(repo) {
            Ok(status) => Some(status),
            Err(e) => {
                // A repository git will not talk about keeps the status it last had and raises no
                // dialog: it may be mid-rebase, on a network mount, gone since discovery, or on a
                // host too busy with its first index to answer within the deadline.
                tracing::debug!("git status in {}: {e}", repo.root.display());
                None
            }
        })
        .collect();
    let at = clamp(selected, against.len());
    // Everything below is about the repository itself rather than the working tree, so a save
    // does not pay for it: five to seven processes per keystroke were what this cost before.
    let head = (depth >= Depth::Everything)
        .then(|| against.get(at))
        .flatten();
    let (commits, branches, submodules) = match head {
        Some(repo) => (
            Some(vault.git_log(repo, 0, rows).unwrap_or_else(|e| {
                // An empty page reads as "the history has not moved" in `apply`, so a refused log
                // leaves the rows that are on screen where they are.
                tracing::debug!("git log: {e}");
                Vec::new()
            })),
            vault
                .git_branches(repo)
                .inspect_err(|e| tracing::debug!("git for-each-ref: {e}"))
                .ok(),
            vault
                .git_submodules(repo)
                .inspect_err(|e| tracing::debug!("git submodule status: {e}"))
                .ok(),
        ),
        None => (None, None, None),
    };
    // `behind` is the count and this is the same set by oid, so one implies the other: nothing to
    // pull means no `rev-list` at all, which is what keeps a refresh on every save as cheap as it
    // was. A non-zero count also means there is an upstream, which the range needs. Without a
    // status there is no count, and the marks stay as they were.
    let behind = statuses
        .get(at)
        .and_then(Option::as_ref)
        .map(|s| s.branch.behind > 0);
    let incoming = head.zip(behind).map(|(repo, behind)| match behind {
        true => vault
            .git_incoming(repo)
            .unwrap_or_else(|e| {
                tracing::debug!("git rev-list HEAD..@{{u}}: {e}");
                Vec::new()
            })
            .into_iter()
            .collect(),
        false => HashSet::new(),
    });
    Fetched {
        selected,
        rows,
        repos,
        statuses,
        commits,
        branches,
        submodules,
        incoming,
    }
}
