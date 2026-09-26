//! What the pane writes: commit, sync, the branch commands, and staging.
//!
//! Every one of them runs off the main thread and refreshes when it lands. A refusal is the
//! user's answer, so it is said in the wording the rest of the window uses — `Cannot <what>:
//! <why>` — and only a transcript worth reading whole gets a dialog.

use super::changes::Section;
use super::*;
use crate::dialogs;

/// What a refusal does with git's own words, beyond saying them.
pub(super) enum Fail {
    /// Say it and stop, which is every command here but one.
    Say,
    /// A branch delete git refused because the work is not merged anywhere else: ask, and run it
    /// again with `--force`. Carries the branch, `delete_branch` needing it a second time.
    AskToForce(String),
}

impl Panel {
    /// Run one git command on the selected repository off the main thread, say what happened, and
    /// refresh.
    ///
    /// `what` is the verb phrase a refusal is reported with — "commit", "switch to main" — so
    /// every failure here reads `Cannot <what>: <why>`, the wording the rest of the window uses.
    /// `sync` marks the job as the sync, the one transfer the user waits on: the Sync button gives
    /// way to a spinner while it runs, and [`Hooks::syncing`] greys the status bar's branch for
    /// the same span.
    fn command(
        self: &Rc<Self>,
        what: String,
        sync: bool,
        on_err: Fail,
        job: impl FnOnce(&Vault, &Repo) -> anyhow::Result<String> + Send + 'static,
    ) {
        self.command_then(what, sync, on_err, job, |_| ());
    }

    /// [`Panel::command`] with something to do back on the main thread once it worked, which only
    /// the commit box needs: a widget cannot be touched from the worker the job runs on.
    fn command_then(
        self: &Rc<Self>,
        what: String,
        sync: bool,
        on_err: Fail,
        job: impl FnOnce(&Vault, &Repo) -> anyhow::Result<String> + Send + 'static,
        then: impl FnOnce(&Rc<Panel>) + 'static,
    ) {
        let repo = {
            let state = self.state.borrow();
            match state.repos.get(state.selected) {
                Some(repo) => repo.clone(),
                None => return,
            }
        };
        if sync {
            self.sync_slot.set_visible_child_name("spinner");
            self.sync_busy.set(true);
            (self.hooks.syncing)(true);
        }
        self.jobs.set(self.jobs.get() + 1);
        // The last window closing must not end the process under git (`Panel::stop`).
        let hold = self.hooks.window.application().map(|app| app.hold());
        let panel = self.clone();
        let vault = self.hooks.vault.clone();
        glib::spawn_future_local(async move {
            let done = crate::work::off_thread("git", move || job(&vault, &repo)).await;
            panel.jobs.set(panel.jobs.get() - 1);
            drop(hold);
            if panel.gone.get() {
                return;
            }
            // The button comes back as `sync_state` last left it, which the refresh below
            // brings up to date.
            if sync {
                panel.sync_slot.set_visible_child_name("button");
                panel.sync_busy.set(false);
                panel.pushing.store(false, Ordering::Relaxed);
                (panel.hooks.syncing)(false);
            }
            // A close waiting for git: the last command to go through closes the window, and one
            // that fails keeps it open under the failure said below.
            let ok = matches!(done, Some(Ok(_)));
            if (!ok || panel.jobs.get() == 0)
                && let Some(leave) = panel.leaving.take()
            {
                leave(ok);
                if ok {
                    return;
                }
            }
            match done {
                Some(Ok(message)) => {
                    then(&panel);
                    (panel.hooks.toast)(&message);
                }
                Some(Err(e)) => {
                    let message = format!("{e:#}");
                    match on_err {
                        Fail::AskToForce(name) if git::unmerged(&message) => {
                            panel.confirm_delete(name)
                        }
                        _ => panel.failed(&what, &message),
                    }
                }
                // A command the user pressed a button for: a worker that stopped is reported the
                // way any other failure of it is, rather than leaving the spinner's reset as the
                // only sign anything happened.
                None => panel.failed(&what, "the worker stopped"),
            }
            // Straight away, not through the debounce: the user asked for this and is watching
            // the row it moves. The debounce is there to fold a burst of watcher events into one
            // query, and the `.git` write this just made will schedule one of those anyway.
            panel.refresh(Depth::Everything);
        });
    }

    /// Whether closing the window now would cut git off while it rewrites the files: any command
    /// the pane has running, except a sync that has reached its push, which like a fetch is
    /// stopped instead ([`Panel::stop`]) (DESIGN.md, States).
    pub fn busy(&self) -> bool {
        self.jobs.get() > usize::from(self.pushing())
    }

    /// Whether the sync in flight has pulled and is pushing.
    pub fn pushing(&self) -> bool {
        self.sync_busy.get() && self.pushing.load(Ordering::Relaxed)
    }

    /// Call `then` once the commands under way have ended, with whether they all went through,
    /// or at once where none is running. What a close [`Panel::busy`] held back waits on.
    pub fn when_done(&self, then: impl FnOnce(bool) + 'static) {
        match self.jobs.get() {
            0 => then(true),
            _ => *self.leaving.borrow_mut() = Some(Box::new(then)),
        }
    }

    /// Whether a close is already waiting on git, which a second one leaves to it.
    pub fn closing(&self) -> bool {
        self.leaving.borrow().is_some()
    }

    /// The window is closing for good: stop the fetches and pushes still running here, and let
    /// everything else end without a word, there being nowhere left to say it. A remote vault's
    /// git is the host's, which finishes it within its own bounds once the link closes.
    pub fn stop(&self) {
        self.gone.set(true);
        if !self.hooks.vault.is_remote() {
            for repo in &self.state.borrow().repos {
                git::interrupt(&repo.root);
            }
        }
    }

    /// Say why a command did not run. One toast in the window's own wording where git answered in
    /// one line, and a dialog only where it said more than that: what `git push` puts on stderr is
    /// the whole answer to "why did it not go", and it is too long to let scroll past.
    fn failed(&self, what: &str, message: &str) {
        if message.lines().count() < 2 {
            return (self.hooks.toast)(&format!("Cannot {what}: {}", reason(message)));
        }
        let dialog = dialogs::alert(
            &format!("Cannot {what}"),
            message,
            &[("close", "Close", adw::ResponseAppearance::Default)],
            "close",
        );
        dialog.present(Some(&self.hooks.window));
    }

    pub(super) fn do_commit(self: &Rc<Self>) {
        // The button reads Continue while a rebase is under way, and that is what it does.
        if self.rebasing() {
            return self.continue_rebase();
        }
        let message = self.message_text();
        // A merge under way commits with git's own message where the box is empty.
        let merging = self.merging();
        // `Ctrl+Return` reaches here without the button, so it asks what the button asked.
        if (message.trim().is_empty() && !merging) || self.unresolved() {
            return;
        }
        // Nothing staged means "commit what changed", which is `git commit -a`: every tracked
        // file goes in and an untracked one stays untracked, as VS Code's smart commit does. Never
        // during a merge, where it would stage the unresolved files with their markers in them.
        let all = !merging && !self.to_commit().0;
        // The box is cleared once the commit is in, not before it runs: a commit git refuses — an
        // unset identity, a hook that said no, nothing staged after all — must leave the message
        // where it was written rather than make the user type it again.
        self.command_then(
            "commit".to_string(),
            false,
            Fail::Say,
            move |vault, repo| {
                vault
                    .git_commit(repo, &message, all)
                    .map(|id| format!("Committed {id}"))
            },
            |panel| panel.message.buffer().set_text(""),
        );
    }

    /// Pull and then push the repository `key` sits in, or the selected one where `key` names no
    /// repository. The pane's selection follows, so the status bar's branch and the pane never
    /// end up talking about two different repositories.
    pub fn sync(self: &Rc<Self>, key: Option<&str>) {
        // One at a time. The pane's button and the status bar's branch are out of reach for the
        // duration, but the palette still names the action.
        if self.sync_busy.get() {
            return;
        }
        let index = {
            let state = self.state.borrow();
            key.and_then(|key| index_of(&state, &self.hooks.vault.root(), key))
                .unwrap_or(state.selected)
        };
        if self.state.borrow().selected != index {
            self.state.borrow_mut().selected = index;
            // The notify this fires is the same one a user's pick fires, refresh included.
            self.chooser.set_selected(index as u32);
        }
        // What the branch said before the transfer, which is what a sync is about to move. Read
        // here rather than counted afterwards: the refresh that follows has already taken both
        // counts back to zero.
        let moved = self
            .state
            .borrow()
            .statuses
            .get(index)
            .map(|s| (s.branch.behind, s.branch.ahead))
            .unwrap_or_default();
        let (fetch_lock, pushing) = (self.fetch_lock.clone(), self.pushing.clone());
        self.command("sync".to_string(), true, Fail::Say, move |vault, repo| {
            // Behind a background fetch still running, which its pull would race for the refs.
            let _fetched = fetch_lock.lock();
            // Two calls, so the window can tell the halves apart. A close in the instant between
            // the flag and git starting its push lets that push finish, as a host's always does.
            let pulled = vault.git_pull(repo)?;
            pushing.store(true, Ordering::Relaxed);
            let pushed = vault.git_push(repo)?;
            tracing::debug!("git sync: {pulled}\n{pushed}");
            Ok(match moved {
                (0, 0) => "Synced".to_string(),
                (pulled, pushed) => format!("Synced · {pulled} pulled, {pushed} pushed"),
            })
        });
    }

    /// Switch the selected repository to a local branch.
    ///
    /// Whether that is safe is git's call: it refuses where a checkout would overwrite work that
    /// is not committed, and that refusal is one line, so it is a toast. The refresh that follows
    /// puts the chooser back on whatever HEAD actually is, so a refused switch does not leave it
    /// naming a branch we are not on.
    pub(super) fn checkout(self: &Rc<Self>, branch: String) {
        // The row a detached HEAD adds to the list is a readout, not a branch to switch to.
        if !self.state.borrow().branches.local.contains(&branch) {
            return;
        }
        let asked = branch.clone();
        self.command(
            format!("switch to {branch}"),
            false,
            Fail::Say,
            move |vault, repo| {
                vault
                    .git_checkout(repo, &asked)
                    .map(|()| format!("Switched to {asked}"))
            },
        );
    }

    /// Check out a remote-tracking branch as a local branch that tracks it, which is `git switch
    /// --track`. git names the branch after the remote one and refuses where that name is taken.
    pub(super) fn track(self: &Rc<Self>, remote: String) {
        let asked = remote.clone();
        self.command(
            format!("check out {remote}"),
            false,
            Fail::Say,
            move |vault, repo| {
                vault
                    .git_track(repo, &asked)
                    .map(|()| format!("Switched to {}", local_name(&asked)))
            },
        );
    }

    /// Branch from HEAD and switch to it in one step, which is `git switch -c`: no base picker,
    /// because the base a reader means is the state they are looking at.
    ///
    /// The name is git's to validate — a bad ref name, one already taken and a worktree the
    /// switch would clobber are all its refusals, and they come back through [`Panel::command`],
    /// which also brings the refresh.
    pub(super) fn create_branch(self: &Rc<Self>) {
        self.branch_menu.popdown();
        let entry = dialogs::name_entry("Branch name", "");
        let form = gtk::Box::new(gtk::Orientation::Vertical, 12);
        form.append(&entry);
        let dialog = dialogs::name_dialog("Create Branch", "Create", &form);

        let (panel, field) = (self.clone(), entry.clone());
        dialogs::choose(&dialog, Some(&self.hooks.window), move |response| {
            let name = field.text().trim().to_string();
            if response != dialogs::CONFIRM || name.is_empty() {
                return;
            }
            let asked = name.clone();
            panel.command(
                format!("create {name}"),
                false,
                Fail::Say,
                move |vault, repo| {
                    vault
                        .git_create_branch(repo, &asked, true)
                        .map(|()| format!("Switched to {asked}"))
                },
            );
        });
        // After `choose` has presented the dialog: the entry is mapped only by then.
        entry.grab_focus();
    }

    /// Delete a local branch.
    ///
    /// `git branch -d` first, so that whether the work would be lost is git's answer and not a
    /// guess of ours at a default branch. Its one refusal worth escalating is "not fully merged",
    /// which asks before running `-D`; every other refusal is reported as it comes.
    pub(super) fn delete_branch(self: &Rc<Self>, name: String, force: bool) {
        let asked = name.clone();
        let on_err = match force {
            true => Fail::Say,
            false => Fail::AskToForce(name.clone()),
        };
        self.command(
            format!("delete {name}"),
            false,
            on_err,
            move |vault, repo| {
                vault
                    .git_delete_branch(repo, &asked, force)
                    .map(|()| format!("Deleted {asked}"))
            },
        );
    }

    /// The one delete that loses commits, so it asks first (DESIGN.md, States).
    fn confirm_delete(self: &Rc<Self>, name: String) {
        let panel = self.clone();
        dialogs::confirm(
            &self.hooks.window,
            &format!("Delete {name}?"),
            "Its commits are not merged into any other branch and will be lost.",
            "Delete",
            true,
            move || panel.delete_branch(name, true),
        );
    }

    /// Pick one of the selected repository's other local branches and delete it: the palette's
    /// Delete Branch…, which is how the keyboard reaches the popover's trash buttons. The pick
    /// goes down the trash button's own path, asking again where git says the work is not merged.
    pub fn delete_other_branch(self: &Rc<Self>) {
        let others = self.other_branches();
        if others.is_empty() {
            return (self.hooks.toast)("There is no other branch to delete");
        }
        let labels: Vec<&str> = others.iter().map(String::as_str).collect();
        let picker = gtk::DropDown::from_strings(&labels);
        let form = dialogs::form();
        form.append(&dialogs::labelled("Branch", &picker));
        let dialog = dialogs::alert(
            "Delete Branch",
            "A branch whose commits are merged nowhere else asks again before it goes.",
            &[
                ("cancel", "Cancel", adw::ResponseAppearance::Default),
                ("delete", "Delete", adw::ResponseAppearance::Destructive),
            ],
            "cancel",
        );
        dialog.set_extra_child(Some(&form));

        let panel = self.clone();
        dialogs::choose(&dialog, Some(&self.hooks.window), move |response| {
            if response != "delete" {
                return;
            }
            if let Some(branch) = others.get(picker.selected() as usize) {
                panel.delete_branch(branch.clone(), false);
            }
        });
    }

    /// The selected repository's local branches other than the one HEAD is on: what Merge
    /// Branch… and Delete Branch… pick from.
    fn other_branches(&self) -> Vec<String> {
        let state = self.state.borrow();
        let head = state
            .statuses
            .get(state.selected)
            .and_then(|s| s.branch.head.as_ref());
        state
            .branches
            .local
            .iter()
            .filter(|b| Some(*b) != head)
            .cloned()
            .collect()
    }

    /// Pick one of the selected repository's other local branches and merge it into HEAD. The
    /// palette's Merge Branch… and the branch popover's both land here.
    pub fn merge_branch(self: &Rc<Self>) {
        self.branch_menu.popdown();
        let into = {
            let state = self.state.borrow();
            let Some(branch) = state.statuses.get(state.selected).map(|s| &s.branch) else {
                return;
            };
            let Some(into) = branch
                .head
                .clone()
                .or_else(|| branch.oid.as_deref().map(short))
            else {
                return;
            };
            into
        };
        let others = self.other_branches();
        if others.is_empty() {
            return (self.hooks.toast)("There is no other branch to merge");
        }
        let labels: Vec<&str> = others.iter().map(String::as_str).collect();
        let picker = gtk::DropDown::from_strings(&labels);
        let form = dialogs::form();
        form.append(&dialogs::labelled("Branch", &picker));
        let dialog = dialogs::name_dialog(&format!("Merge into {into}"), "Merge", &form);

        let panel = self.clone();
        dialogs::choose(&dialog, Some(&self.hooks.window), move |response| {
            if response != dialogs::CONFIRM {
                return;
            }
            if let Some(branch) = others.get(picker.selected() as usize) {
                panel.merge(branch.clone());
            }
        });
    }

    /// Merge `branch` into HEAD. What git made of it is the toast; conflicts land in the Merge
    /// Conflicts section and the banner, with the refresh [`Panel::command`] brings.
    fn merge(self: &Rc<Self>, branch: String) {
        let asked = branch.clone();
        self.command(
            format!("merge {branch}"),
            false,
            Fail::Say,
            move |vault, repo| {
                vault.git_merge(repo, &asked).map(|merged| match merged {
                    git::Merge::UpToDate => format!("Already up to date with {asked}"),
                    git::Merge::FastForward => format!("Fast-forwarded to {asked}"),
                    git::Merge::Commit => format!("Merged {asked}"),
                    git::Merge::Conflicts(paths) => {
                        format!("Merging {asked}: conflicts in {}", files(paths.len()))
                    }
                })
            },
        );
    }

    /// Give up the merge under way. It throws away every resolution made so far, so it asks
    /// first (DESIGN.md, States).
    pub fn abort_merge(self: &Rc<Self>) {
        if !self.merging() {
            return (self.hooks.toast)("No merge in progress");
        }
        let panel = self.clone();
        dialogs::confirm(
            &self.hooks.window,
            "Abort Merge?",
            "The files go back to how they were before the merge, and the conflicts resolved so \
             far are lost.",
            "Abort",
            true,
            move || {
                panel.command(
                    "abort the merge".to_string(),
                    false,
                    Fail::Say,
                    |vault, repo| {
                        vault
                            .git_merge_abort(repo)
                            .map(|()| "Merge aborted".to_string())
                    },
                );
            },
        );
    }

    /// Carry on with the rebase under way, each commit keeping its own message. One that stops on
    /// the next commit's conflicts leaves the banner and the Merge Conflicts rows up, as a merge
    /// does, with the refresh [`Panel::command`] brings.
    fn continue_rebase(self: &Rc<Self>) {
        if self.unresolved() {
            return;
        }
        self.command(
            "continue the rebase".to_string(),
            false,
            Fail::Say,
            |vault, repo| {
                vault.git_rebase_continue(repo).map(|rebase| match rebase {
                    git::Rebase::Done => "Rebase finished".to_string(),
                    git::Rebase::Stopped(paths) if paths.is_empty() => "Rebase stopped".to_string(),
                    git::Rebase::Stopped(paths) => {
                        format!("Rebasing: conflicts in {}", files(paths.len()))
                    }
                })
            },
        );
    }

    /// Give up the rebase under way, which asks first for the reason [`Panel::abort_merge`] does.
    pub(super) fn abort_rebase(self: &Rc<Self>) {
        let panel = self.clone();
        dialogs::confirm(
            &self.hooks.window,
            "Abort Rebase?",
            "The branch goes back to where it was before the rebase, and the conflicts resolved \
             so far are lost.",
            "Abort",
            true,
            move || {
                panel.command(
                    "abort the rebase".to_string(),
                    false,
                    Fail::Say,
                    |vault, repo| {
                        vault
                            .git_rebase_abort(repo)
                            .map(|()| "Rebase aborted".to_string())
                    },
                );
            },
        );
    }

    /// Put HEAD on one commit, detached, so the repository can be read at that point.
    ///
    /// No confirmation, for the reason [`Panel::checkout`] gives: `git switch --detach` refuses
    /// where it would clobber uncommitted work, and that refusal is the whole answer. The refresh
    /// that follows puts `Detached at …` in the branch button and the status bar.
    pub(super) fn detach(self: &Rc<Self>, oid: String) {
        let asked = oid.clone();
        self.command(
            format!("check out {}", short(&oid)),
            false,
            Fail::Say,
            move |vault, repo| {
                vault
                    .git_checkout_commit(repo, &asked)
                    .map(|()| format!("Checked out {}", short(&asked)))
            },
        );
    }

    pub(super) fn stage(self: &Rc<Self>, paths: Vec<String>) {
        let n = paths.len();
        self.write("stage", paths, move |vault, repo, paths| {
            vault
                .git_stage(repo, paths)
                .map(|()| format!("Staged {}", files(n)))
        });
    }

    pub(super) fn unstage(self: &Rc<Self>, paths: Vec<String>) {
        let n = paths.len();
        self.write("unstage", paths, move |vault, repo, paths| {
            vault
                .git_unstage(repo, paths)
                .map(|()| format!("Unstaged {}", files(n)))
        });
    }

    /// Make `text` what the index holds for `rel`: Stage or Unstage Selected Lines. `repo` is the
    /// one the comparison was opened on, which need not be the one the pane shows by now.
    pub(super) fn stage_text(
        self: &Rc<Self>,
        repo: Repo,
        rel: String,
        text: String,
        unstage: bool,
    ) {
        let (what, done) = match unstage {
            false => ("stage the selected lines", "Staged the selected lines"),
            true => ("unstage the selected lines", "Unstaged the selected lines"),
        };
        self.command(what.to_string(), false, Fail::Say, move |vault, _| {
            vault
                .git_stage_text(&repo, &rel, &text)
                .map(|()| done.to_string())
        });
    }

    /// [`Panel::command`] for the three that take paths, which have to outlive the borrow.
    fn write(
        self: &Rc<Self>,
        what: &'static str,
        paths: Vec<String>,
        job: impl FnOnce(&Vault, &Repo, &[String]) -> anyhow::Result<String> + Send + 'static,
    ) {
        if paths.is_empty() {
            return;
        }
        self.command(what.to_string(), false, Fail::Say, move |vault, repo| {
            job(vault, repo, &paths)
        });
    }

    /// The entries of one section under `under`: a folder's path with its slash, or `""` for the
    /// whole section, which is what its header's bulk button takes.
    pub(super) fn section_entries(&self, section: Section, under: &str) -> Vec<Entry> {
        let state = self.state.borrow();
        let Some(status) = state.statuses.get(state.selected) else {
            return Vec::new();
        };
        let entries: Box<dyn Iterator<Item = &Entry>> = match section {
            Section::Conflicts => Box::new(status.conflicts()),
            Section::Staged => Box::new(status.staged()),
            Section::Changes => Box::new(status.changes()),
        };
        entries
            .filter(|e| e.path.starts_with(under))
            .cloned()
            .collect()
    }

    /// [`Panel::section_entries`]' paths, for Stage and Unstage.
    pub(super) fn section_paths(&self, section: Section, under: &str) -> Vec<String> {
        self.section_entries(section, under)
            .into_iter()
            .map(|e| e.path)
            .collect()
    }

    /// Discarding is the one thing here that loses work, so it asks first (DESIGN.md, States).
    ///
    /// `folder` is the folder row it was asked from, whose `entries` are every one under it, or
    /// `""` for the Changes header's Discard All and every entry of the section; a file's own row
    /// passes `None` and itself. An untracked file has nothing in the index to go back to, so what
    /// "discard" means for it is that the file itself goes — to the trash, which is at least
    /// recoverable, and all of a folder's in one go.
    pub(super) fn discard(self: &Rc<Self>, folder: Option<&str>, entries: Vec<Entry>) {
        let Some(repo) = ({
            let state = self.state.borrow();
            state.repos.get(state.selected).cloned()
        }) else {
            return;
        };
        let (untracked, tracked): (Vec<Entry>, Vec<Entry>) =
            entries.into_iter().partition(|e| e.x == '?');
        let what = match (folder, tracked.first().or(untracked.first())) {
            (_, None) => return,
            (Some(folder), _) => folder.to_string(),
            (None, Some(entry)) => split_name(&entry.path).1.to_string(),
        };
        let body = discard_body(&what, folder.is_some(), tracked.len(), untracked.len());
        let root = self.hooks.vault.root();
        let keys: Vec<String> = untracked
            .iter()
            .map(|e| vault_key(&root, &repo, &e.path))
            .collect();
        let paths: Vec<String> = tracked.into_iter().map(|e| e.path).collect();
        let done = match folder {
            Some(_) => format!("Discarded {}", files(paths.len())),
            None => format!("Discarded {what}"),
        };
        let (heading, verb) = match folder {
            Some("") => ("Discard All Changes?", "Discard All"),
            _ => ("Discard Changes?", "Discard"),
        };
        let panel = self.clone();
        dialogs::confirm(&self.hooks.window, heading, &body, verb, true, move || {
            if !keys.is_empty() {
                (panel.hooks.trash)(&keys);
                panel.schedule_refresh(Depth::Status);
            }
            panel.write("discard", paths, move |vault, repo, paths| {
                vault.git_discard(repo, paths).map(|()| done)
            });
        });
    }
}

/// What the Discard confirmation says will happen. A file's own row names the file; a folder's
/// names the folder and how many files of each kind it takes, the ones that go back to the index
/// and the untracked ones that go to the trash, and Discard All, the folder `""`, says the same
/// of the whole section.
fn discard_body(what: &str, folder: bool, tracked: usize, untracked: usize) -> String {
    let undone = match tracked {
        0 => "",
        _ => " This cannot be undone.",
    };
    if !folder {
        return match untracked {
            0 => format!("{what} goes back to what the index holds.{undone}"),
            _ => format!("{what} is not tracked, so it moves to the trash."),
        };
    }
    let back = match tracked {
        0 => None,
        1 => Some("1 file goes back to what the index holds".to_string()),
        n => Some(format!("{n} files go back to what the index holds")),
    };
    let trashed = match untracked {
        0 => None,
        1 => Some("1 untracked file moves to the trash".to_string()),
        n => Some(format!("{n} untracked files move to the trash")),
    };
    let parts = back
        .into_iter()
        .chain(trashed)
        .collect::<Vec<_>>()
        .join(" and ");
    match what {
        "" => format!("{parts}.{undone}"),
        _ => format!("In {what}, {parts}.{undone}"),
    }
}

/// The one line of a git refusal that fits in a toast: git's own first line, without the prefix
/// it puts on it and without the colon that introduces the file list underneath.
fn reason(message: &str) -> &str {
    message
        .lines()
        .next()
        .unwrap_or(message)
        .trim_start_matches("error: ")
        .trim_end_matches(':')
}

fn files(n: usize) -> String {
    match n {
        1 => "1 file".to_string(),
        n => format!("{n} files"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_discard_says_how_many_files_go_where() {
        assert_eq!(
            discard_body("a.md", false, 1, 0),
            "a.md goes back to what the index holds. This cannot be undone."
        );
        assert_eq!(
            discard_body("new.md", false, 0, 1),
            "new.md is not tracked, so it moves to the trash."
        );
        assert_eq!(
            discard_body("src", true, 3, 1),
            "In src, 3 files go back to what the index holds and 1 untracked file moves to the \
             trash. This cannot be undone."
        );
        assert_eq!(
            discard_body("src", true, 0, 2),
            "In src, 2 untracked files move to the trash."
        );
        assert_eq!(
            discard_body("", true, 3, 2),
            "3 files go back to what the index holds and 2 untracked files move to the trash. \
             This cannot be undone."
        );
    }

    #[test]
    fn reason_is_gits_own_first_line() {
        assert_eq!(
            reason("error: Your local changes would be overwritten:\n\tnote.md\nAborting"),
            "Your local changes would be overwritten"
        );
        assert_eq!(
            reason("fatal: invalid reference"),
            "fatal: invalid reference"
        );
    }
}
