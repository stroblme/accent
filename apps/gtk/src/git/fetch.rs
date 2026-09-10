//! The worker's half: one refresh's reads, and the background fetch that keeps the counts true.

use super::*;

/// How often the selected repository's remote is fetched while the window has the focus. VS
/// Code's `git.autofetchPeriod` default, and for its reason: it is often enough that a colleague's
/// push shows up in the history within a coffee break, and rare enough that a laptop on a phone
/// tether is not woken by us. Only while the window is focused, so a window left open behind
/// others stops talking to the network at all.
const AUTOFETCH: Duration = Duration::from_secs(300);

impl Panel {
    /// Fetch the selected repository's remote, off the main thread, and refresh once it lands.
    ///
    /// The selected repository only. `git::discover` finds every repository a vault touches and
    /// fetching all of them would put one network round trip per repository on a timer, for rows
    /// nobody is looking at; this is the one the pane and the status bar are speaking for.
    ///
    /// A failure interrupts nobody — no toast, no dialog, no badge. A fetch nobody asked for that
    /// fails every five minutes because the laptop is on a train would otherwise be a notification
    /// every five minutes. It is not silent either: [`Panel::sync_state`] puts it on the Sync
    /// button's tooltip, beside the counts a failed fetch is the reason for.
    pub(super) fn autofetch(self: &Rc<Self>) {
        if self.fetch_busy.get() {
            return;
        }
        let Some(repo) = ({
            let state = self.state.borrow();
            state.repos.get(state.selected).cloned()
        }) else {
            return;
        };
        self.fetch_busy.set(true);
        let vault = self.hooks.vault.clone();
        let panel = self.clone();
        glib::spawn_future_local(async move {
            let fetched = gio::spawn_blocking(move || vault.git_fetch(&repo)).await;
            panel.fetch_busy.set(false);
            let failed = !matches!(fetched, Ok(Ok(_)));
            if panel.fetch_failed.replace(failed) != failed {
                panel.sync_state();
            }
            match fetched {
                // A fetch that brought nothing prints nothing, so this is quiet in the common case.
                Ok(Ok(transcript)) => {
                    if !transcript.is_empty() {
                        tracing::debug!("git fetch: {transcript}");
                    }
                }
                Ok(Err(e)) => return tracing::debug!("git fetch: {e:#}"),
                Err(_) => return tracing::warn!("the git worker panicked"),
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

/// What one refresh reads. Every repository's status, because the ignored set spans them all, and
/// the history and submodules of the selected one only.
///
/// The fields git may refuse to answer are `Option`, and `None` means "could not ask" rather than
/// "there are none": the pane keeps what the last refresh learned instead of drawing an empty
/// answer it was never given. A hiccup on a remote vault would otherwise take the branch list
/// down to HEAD alone, or hide the pane outright.
pub(super) struct Fetched {
    /// Which repository this was asked about. A chooser moved while the read was in flight makes
    /// the answer somebody else's: `apply` drops it rather than storing one repository's history
    /// under another's index, and the refresh the chooser scheduled is the one that lands.
    pub(super) selected: usize,
    pub(super) repos: Option<Vec<Repo>>,
    pub(super) statuses: Vec<Status>,
    pub(super) commits: Option<Vec<Commit>>,
    pub(super) branches: Option<git::Branches>,
    pub(super) submodules: Option<Vec<Submodule>>,
    /// The commits a pull would bring in, which is what marks the history's rows. Asked for only
    /// where the branch says there are any, so an up-to-date repository pays nothing for it.
    pub(super) incoming: Option<HashSet<String>>,
}

/// Read what `depth` asks for. `known` is the repositories the pane already holds, which is what
/// a refresh below [`Depth::Discover`] runs against rather than looking for them again.
pub(super) fn fetch(vault: &Vault, selected: usize, depth: Depth, known: Vec<Repo>) -> Fetched {
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
    let statuses: Vec<Status> = against
        .iter()
        .map(|repo| match vault.git_status(repo) {
            Ok(status) => status,
            Err(e) => {
                // A repository git will not talk about costs an empty row, not a dialog: it may
                // be mid-rebase, on a network mount, or gone since discovery.
                tracing::debug!("git status in {}: {e}", repo.root.display());
                Status::default()
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
            Some(vault.git_log(repo, 0, PAGE).unwrap_or_else(|e| {
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
    // was. A non-zero count also means there is an upstream, which the range needs.
    let behind = statuses.get(at).is_some_and(|s| s.branch.behind > 0);
    let incoming = head.map(|repo| match behind {
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
        repos,
        statuses,
        commits,
        branches,
        submodules,
        incoming,
    }
}
