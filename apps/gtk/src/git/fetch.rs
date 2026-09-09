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
    /// A failure is logged and nothing else — no toast, no dialog, no badge. A fetch nobody asked
    /// for that fails every five minutes because the laptop is on a train would otherwise be a
    /// notification every five minutes, and the honest consequence of a failed fetch is already
    /// on screen: the counts stay as stale as they were.
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
            panel.schedule_refresh();
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
pub(super) struct Fetched {
    pub(super) repos: Vec<Repo>,
    pub(super) statuses: Vec<Status>,
    pub(super) commits: Vec<Commit>,
    pub(super) branches: Vec<String>,
    pub(super) submodules: Vec<Submodule>,
    /// The commits a pull would bring in, which is what marks the history's rows. Asked for only
    /// where the branch says there are any, so an up-to-date repository pays nothing for it.
    pub(super) incoming: HashSet<String>,
}

pub(super) fn fetch(vault: &Vault, selected: usize) -> Fetched {
    let repos = vault.repos();
    let statuses: Vec<Status> = repos
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
    let at = clamp(selected, repos.len());
    let (commits, branches, submodules) = match repos.get(at) {
        Some(repo) => (
            vault.git_log(repo, 0, PAGE).unwrap_or_else(|e| {
                tracing::debug!("git log: {e}");
                Vec::new()
            }),
            vault.git_branches(repo).unwrap_or_default(),
            vault.git_submodules(repo).unwrap_or_default(),
        ),
        None => (Vec::new(), Vec::new(), Vec::new()),
    };
    // `behind` is the count and this is the same set by oid, so one implies the other: nothing to
    // pull means no `rev-list` at all, which is what keeps a refresh on every save as cheap as it
    // was. A non-zero count also means there is an upstream, which the range needs.
    let behind = statuses.get(at).is_some_and(|s| s.branch.behind > 0);
    let incoming = match repos.get(at).filter(|_| behind) {
        Some(repo) => vault
            .git_incoming(repo)
            .unwrap_or_else(|e| {
                tracing::debug!("git rev-list HEAD..@{{u}}: {e}");
                Vec::new()
            })
            .into_iter()
            .collect(),
        None => HashSet::new(),
    };
    Fetched {
        repos,
        statuses,
        commits,
        branches,
        submodules,
        incoming,
    }
}
