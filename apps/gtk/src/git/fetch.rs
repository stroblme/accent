//! The worker's half: one refresh's reads, and the background fetch that keeps the counts true.

use super::refresh::Ask;
use super::*;

/// How many repositories' statuses a refresh asks at once, beside the selected one's history.
const AT_ONCE: usize = 8;

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
        let root = repo.root.clone();
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
            // is only seen because we ask: at once, and of the repository it fetched alone.
            panel.refresh(Ask::repos([root]));
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
    /// The repositories, sorted, where they are not the ones the pane holds in that order.
    pub(super) repos: Option<Vec<Repo>>,
    /// One per repository, `None` where git did not answer or was not asked.
    pub(super) statuses: Vec<Option<Status>>,
    /// Whether the selected repository's status was asked: one of some paths' may leave it out.
    pub(super) selected_asked: bool,
    pub(super) commits: Option<Vec<Commit>>,
    pub(super) branches: Option<git::Branches>,
    pub(super) submodules: Option<Vec<Submodule>>,
    /// The commits a pull would bring in, which is what marks the history's rows. Asked for only
    /// where the branch says there are any, so an up-to-date repository pays nothing for it.
    pub(super) incoming: Option<HashSet<String>>,
}

/// Read what `ask` asks for, or nothing where it names paths no repository holds. `known` is the
/// repositories the pane already holds, which is what a refresh below [`Depth::Discover`] runs
/// against rather than looking for them again, `rows` how many commits of history the pane
/// shows, and `recent` the git dirs picked, most recent first, which the repositories are sorted
/// by ([`by_recent`]).
pub(super) fn fetch(
    vault: &Source,
    selected: usize,
    ask: &Ask,
    known: Vec<Repo>,
    rows: usize,
    recent: &[String],
) -> Option<Fetched> {
    let depth = ask.depth();
    // A refusal is not "there are no repositories": on `Err` the pane keeps the ones it had,
    // which is what a remote whose link dropped mid-refresh needs.
    let found = match (depth >= Depth::Discover).then(|| vault.repos()) {
        Some(Ok(repos)) => Some(repos),
        Some(Err(e)) => {
            tracing::debug!("listing the repositories: {e}");
            None
        }
        None => None,
    };
    // Sorted before anything is read of them, so every answer below lines up with the order the
    // chooser will list them in. A pick moves its repository to the top this way, with the
    // refresh it asks for.
    let mut against = found.unwrap_or_else(|| known.clone());
    by_recent(&mut against, recent);
    let repos = (against != known).then(|| against.clone());
    // A save outside every repository asks git nothing, and leaves the pane as it is.
    let asked: Vec<bool> = against.iter().map(|repo| ask.asks(repo)).collect();
    if matches!(ask, Ask::Paths(..)) && !asked.contains(&true) {
        return None;
    }
    let at = reselect(selected, &known, &against);
    // Everything below is about the repository itself rather than the working tree, so a save
    // does not pay for it: five to seven processes per keystroke were what this cost before.
    let head = (depth >= Depth::Everything)
        .then(|| against.get(at))
        .flatten();
    // A repository git will not talk about keeps the status it last had and raises no dialog: it
    // may be mid-rebase, on a network mount, gone since discovery, or on a host too busy with its
    // first index to answer within the deadline. One not asked keeps its status the same way.
    let status_of = |repo: &Repo| {
        vault
            .git_status(repo)
            .inspect_err(|e| tracing::debug!("git status in {}: {e}", repo.root.display()))
            .ok()
    };
    // Every call at once, each a round trip to a remote vault's host, bar the other repositories'
    // statuses, which go [`AT_ONCE`] at a time. A refused read is `None`, which leaves what the
    // pane shows of it alone.
    let mut statuses: Vec<Option<Status>> = against.iter().map(|_| None).collect();
    let (mine, commits, branches, submodules) = std::thread::scope(|scope| {
        let against = &against;
        // The selected repository's status, and the commits a pull would bring in, asked for only
        // where the status says there are any: `behind` is the count and this is the same set by
        // oid, so nothing to pull means no `rev-list` at all. A non-zero count also means there
        // is an upstream, which the range needs.
        let mine = (asked.get(at) == Some(&true)).then(|| {
            scope.spawn(move || {
                let status = status_of(&against[at]);
                let behind = status.as_ref().map(|s| s.branch.behind > 0);
                let incoming = head.zip(behind).and_then(|(repo, behind)| match behind {
                    true => vault
                        .git_incoming(repo)
                        .inspect_err(|e| tracing::debug!("git rev-list HEAD..@{{u}}: {e}"))
                        .ok()
                        .map(|oids| oids.into_iter().collect()),
                    false => Some(HashSet::new()),
                });
                (status, incoming)
            })
        });
        let log = head.map(|repo| {
            scope.spawn(move || {
                vault
                    .git_log(repo, 0, rows)
                    .inspect_err(|e| tracing::debug!("git log: {e}"))
                    .ok()
            })
        });
        let branches = head.map(|repo| {
            scope.spawn(move || {
                vault
                    .git_branches(repo)
                    .inspect_err(|e| tracing::debug!("git for-each-ref: {e}"))
                    .ok()
            })
        });
        let submodules = head.map(|repo| {
            scope.spawn(move || {
                vault
                    .git_submodules(repo)
                    .inspect_err(|e| tracing::debug!("git submodule status: {e}"))
                    .ok()
            })
        });
        let others: Vec<usize> = (0..against.len())
            .filter(|&i| asked[i] && i != at)
            .collect();
        for chunk in others.chunks(AT_ONCE) {
            let reads: Vec<_> = chunk
                .iter()
                .map(|&i| (i, scope.spawn(move || status_of(&against[i]))))
                .collect();
            for (i, read) in reads {
                statuses[i] = read.join().ok().flatten();
            }
        }
        (
            joined(mine),
            joined(log).flatten(),
            joined(branches).flatten(),
            joined(submodules).flatten(),
        )
    });
    let (status, incoming) = mine.unwrap_or_default();
    if let Some(slot) = statuses.get_mut(at) {
        *slot = status;
    }
    Some(Fetched {
        selected,
        rows,
        repos,
        statuses,
        selected_asked: asked.get(at).copied().unwrap_or(true),
        commits,
        branches,
        submodules,
        incoming,
    })
}

/// What a read on a thread of its own answered, `None` for one never asked or that panicked.
fn joined<T>(read: Option<std::thread::ScopedJoinHandle<'_, T>>) -> Option<T> {
    read.and_then(|read| read.join().ok())
}
