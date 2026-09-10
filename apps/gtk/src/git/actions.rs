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
    /// `hold` goes insensitive while the job runs, which is what a transfer needs; it is also what
    /// marks the job as one the user is waiting on, so [`Hooks::syncing`] runs with it and the
    /// status bar can spin for the same span.
    fn command(
        self: &Rc<Self>,
        what: String,
        hold: Option<gtk::Button>,
        on_err: Fail,
        job: impl FnOnce(&Vault, &Repo) -> anyhow::Result<String> + Send + 'static,
    ) {
        self.command_then(what, hold, on_err, job, |_| ());
    }

    /// [`Panel::command`] with something to do back on the main thread once it worked, which only
    /// the commit box needs: a widget cannot be touched from the worker the job runs on.
    fn command_then(
        self: &Rc<Self>,
        what: String,
        hold: Option<gtk::Button>,
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
        if let Some(button) = &hold {
            button.set_sensitive(false);
            self.sync_busy.set(true);
            (self.hooks.syncing)(true);
        }
        let panel = self.clone();
        let vault = self.hooks.vault.clone();
        glib::spawn_future_local(async move {
            let done = gio::spawn_blocking(move || job(&vault, &repo)).await;
            if let Some(button) = &hold {
                button.set_sensitive(true);
                panel.sync_busy.set(false);
                (panel.hooks.syncing)(false);
            }
            match done {
                Ok(Ok(message)) => {
                    then(&panel);
                    (panel.hooks.toast)(&message);
                }
                Ok(Err(e)) => {
                    let message = format!("{e:#}");
                    match on_err {
                        Fail::AskToForce(name) if git::unmerged(&message) => {
                            panel.confirm_delete(name)
                        }
                        _ => panel.failed(&what, &message),
                    }
                }
                Err(_) => tracing::warn!("the git worker panicked"),
            }
            // Straight away, not through the debounce: the user asked for this and is watching
            // the row it moves. The debounce is there to fold a burst of watcher events into one
            // query, and the `.git` write this just made will schedule one of those anyway.
            panel.refresh(Depth::Everything);
        });
    }

    /// Say why a command did not run. One toast in the window's own wording where git answered in
    /// one line, and a dialog only where it said more than that: what `git push` puts on stderr is
    /// the whole answer to "why did it not go", and it is too long to let scroll past.
    fn failed(&self, what: &str, message: &str) {
        if message.lines().count() < 2 {
            return (self.hooks.toast)(&format!("Cannot {what}: {}", reason(message)));
        }
        let dialog = adw::AlertDialog::new(Some(&format!("Cannot {what}")), Some(message));
        dialog.add_response("close", "Close");
        dialog.set_default_response(Some("close"));
        dialog.set_close_response("close");
        dialog.present(Some(&self.hooks.window));
    }

    pub(super) fn do_commit(self: &Rc<Self>) {
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
            None,
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
        // One at a time. The pane's own button is insensitive for the duration, but the status
        // bar's branch is a second surface on the same action and stays clickable.
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
        let hold = self.sync.clone();
        self.command(
            "sync".to_string(),
            Some(hold),
            Fail::Say,
            move |vault, repo| {
                vault.git_sync(repo).map(|transcript| {
                    tracing::debug!("git sync: {transcript}");
                    match moved {
                        (0, 0) => "Synced".to_string(),
                        (pulled, pushed) => format!("Synced · {pulled} pulled, {pushed} pushed"),
                    }
                })
            },
        );
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
            None,
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
            None,
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
        let entry = fileops::name_entry("Branch name", "");
        let form = gtk::Box::new(gtk::Orientation::Vertical, 12);
        form.append(&entry);
        let dialog = fileops::name_dialog("Create Branch", "Create", &form);

        let (panel, field) = (self.clone(), entry.clone());
        dialog.choose(
            Some(&self.hooks.window),
            gio::Cancellable::NONE,
            move |response| {
                let name = field.text().trim().to_string();
                if response != fileops::CONFIRM || name.is_empty() {
                    return;
                }
                let asked = name.clone();
                panel.command(
                    format!("create {name}"),
                    None,
                    Fail::Say,
                    move |vault, repo| {
                        vault
                            .git_create_branch(repo, &asked, true)
                            .map(|()| format!("Switched to {asked}"))
                    },
                );
            },
        );
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
            None,
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
        let dialog = adw::AlertDialog::new(
            Some(&format!("Delete {name}?")),
            Some("Its commits are not merged into any other branch and will be lost."),
        );
        dialog.add_responses(&[("cancel", "Cancel"), ("delete", "Delete")]);
        dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let panel = self.clone();
        dialog.choose(
            Some(&self.hooks.window),
            gio::Cancellable::NONE,
            move |response| {
                if response == "delete" {
                    panel.delete_branch(name, true);
                }
            },
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
        dialog.choose(
            Some(&self.hooks.window),
            gio::Cancellable::NONE,
            move |response| {
                if response != "delete" {
                    return;
                }
                if let Some(branch) = others.get(picker.selected() as usize) {
                    panel.delete_branch(branch.clone(), false);
                }
            },
        );
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
        dialog.choose(
            Some(&self.hooks.window),
            gio::Cancellable::NONE,
            move |response| {
                if response != dialogs::CONFIRM {
                    return;
                }
                if let Some(branch) = others.get(picker.selected() as usize) {
                    panel.merge(branch.clone());
                }
            },
        );
    }

    /// Merge `branch` into HEAD. What git made of it is the toast; conflicts land in the Merge
    /// Conflicts section and the banner, with the refresh [`Panel::command`] brings.
    fn merge(self: &Rc<Self>, branch: String) {
        let asked = branch.clone();
        self.command(
            format!("merge {branch}"),
            None,
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
        let dialog = dialogs::alert(
            "Abort Merge?",
            "The files go back to how they were before the merge, and the conflicts resolved so \
             far are lost.",
            &[
                ("cancel", "Cancel", adw::ResponseAppearance::Default),
                ("abort", "Abort", adw::ResponseAppearance::Destructive),
            ],
            "cancel",
        );
        let panel = self.clone();
        dialog.choose(
            Some(&self.hooks.window),
            gio::Cancellable::NONE,
            move |response| {
                if response != "abort" {
                    return;
                }
                panel.command(
                    "abort the merge".to_string(),
                    None,
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

    /// Put HEAD on one commit, detached, so the repository can be read at that point.
    ///
    /// No confirmation, for the reason [`Panel::checkout`] gives: `git switch --detach` refuses
    /// where it would clobber uncommitted work, and that refusal is the whole answer. The refresh
    /// that follows puts `Detached at …` in the branch button and the status bar.
    pub(super) fn detach(self: &Rc<Self>, oid: String) {
        let asked = oid.clone();
        self.command(
            format!("check out {}", short(&oid)),
            None,
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
        self.command(what.to_string(), None, Fail::Say, move |vault, repo| {
            job(vault, repo, &paths)
        });
    }

    /// The paths of one whole section, for its header's bulk button.
    pub(super) fn section_paths(&self, section: Section) -> Vec<String> {
        let state = self.state.borrow();
        let Some(status) = state.statuses.get(state.selected) else {
            return Vec::new();
        };
        let entries: Box<dyn Iterator<Item = &Entry>> = match section {
            Section::Conflicts => Box::new(status.conflicts()),
            Section::Staged => Box::new(status.staged()),
            Section::Changes => Box::new(status.changes()),
        };
        entries.map(|e| e.path.clone()).collect()
    }

    /// Discarding is the one thing here that loses work, so it asks first (DESIGN.md, States).
    pub(super) fn discard(self: &Rc<Self>, entry: &Entry, key: &str) {
        let name = split_name(&entry.path).1.to_string();
        // An untracked file has nothing in the index to go back to, so what "discard" means for
        // it is that the file itself goes — to the trash, which is at least recoverable.
        let untracked = entry.x == '?';
        let body = match untracked {
            true => format!("{name} is not tracked, so it moves to the trash."),
            false => format!("{name} goes back to what the index holds. This cannot be undone."),
        };
        let dialog = adw::AlertDialog::new(Some("Discard Changes?"), Some(&body));
        dialog.add_responses(&[("cancel", "Cancel"), ("discard", "Discard")]);
        dialog.set_response_appearance("discard", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");

        let (panel, path, key) = (self.clone(), entry.path.clone(), key.to_string());
        dialog.choose(
            Some(&self.hooks.window),
            gio::Cancellable::NONE,
            move |response| {
                if response != "discard" {
                    return;
                }
                match untracked {
                    true => {
                        (panel.hooks.trash)(&key);
                        panel.schedule_refresh(Depth::Status);
                    }
                    false => panel.write("discard", vec![path], move |vault, repo, paths| {
                        vault
                            .git_discard(repo, paths)
                            .map(|()| format!("Discarded {name}"))
                    }),
                }
            },
        );
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
