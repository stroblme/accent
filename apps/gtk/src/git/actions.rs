//! What the pane writes: commit, sync, the branch commands, and staging.
//!
//! Every one of them runs off the main thread and refreshes when it lands. A refusal is the
//! user's answer, so it is said in the wording the rest of the window uses — `Cannot <what>:
//! <why>` — and only a transcript worth reading whole gets a dialog.

use super::changes::Section;
use super::*;

impl Panel {
    /// Run one git command on the selected repository off the main thread, say what happened, and
    /// refresh. `hold` goes insensitive while the job runs, which is what a transfer needs; it is
    /// also what marks the job as one the user is waiting on, so [`Hooks::syncing`] runs with it
    /// and the status bar can spin for the same span.
    ///
    /// A failure gets a dialog rather than a toast: what git puts on stderr is the whole answer to
    /// "why did the push not go", and it is too long and too important to let scroll past.
    fn command(
        self: &Rc<Self>,
        verb: &'static str,
        hold: Option<gtk::Button>,
        job: impl FnOnce(&Vault, &Repo) -> anyhow::Result<String> + Send + 'static,
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
                Ok(Ok(message)) => (panel.hooks.toast)(&message),
                Ok(Err(e)) => panel.failed(verb, &format!("{e:#}")),
                Err(_) => tracing::warn!("the git worker panicked"),
            }
            // Straight away, not through the debounce: the user asked for this and is watching
            // the row it moves. The debounce is there to fold a burst of watcher events into one
            // query, and the `.git` write this just made will schedule one of those anyway.
            panel.refresh();
        });
    }

    fn failed(&self, verb: &str, message: &str) {
        let dialog = adw::AlertDialog::new(Some(&format!("{verb} Failed")), Some(message));
        dialog.add_response("close", "Close");
        dialog.set_default_response(Some("close"));
        dialog.set_close_response("close");
        dialog.present(Some(&self.hooks.window));
    }

    pub(super) fn do_commit(self: &Rc<Self>) {
        let message = self.message_text();
        if message.trim().is_empty() {
            return;
        }
        // Nothing staged means "commit what changed", which is `git commit -a`: every tracked
        // file goes in and an untracked one stays untracked, as VS Code's smart commit does.
        let all = !self.to_commit().0;
        self.message.buffer().set_text("");
        self.command("Commit", None, move |vault, repo| {
            vault
                .git_commit(repo, &message, all)
                .map(|id| format!("Committed {id}"))
        });
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
        let hold = self.sync.clone();
        self.command("Sync", Some(hold), |vault, repo| {
            vault.git_sync(repo).map(|transcript| {
                tracing::debug!("git sync: {transcript}");
                "Synced".to_string()
            })
        });
    }

    /// Switch the selected repository to a local branch.
    ///
    /// Whether that is safe is git's call: it refuses where a checkout would overwrite work that
    /// is not committed, and its refusal is a toast rather than a dialog because nothing was lost
    /// and there is nothing to decide. The refresh that follows puts the chooser back on whatever
    /// HEAD actually is, so a refused switch does not leave it naming a branch we are not on.
    pub(super) fn checkout(self: &Rc<Self>, branch: String) {
        // The row a detached HEAD adds to the list is a readout, not a branch to switch to.
        let repo = {
            let state = self.state.borrow();
            match state.branches.contains(&branch) {
                true => state.repos.get(state.selected).cloned(),
                false => None,
            }
        };
        let Some(repo) = repo else {
            return;
        };
        let panel = self.clone();
        let vault = self.hooks.vault.clone();
        glib::spawn_future_local(async move {
            let asked = branch.clone();
            let done = gio::spawn_blocking(move || vault.git_checkout(&repo, &asked)).await;
            match done {
                Ok(Ok(())) => (panel.hooks.toast)(&format!("Switched to {branch}")),
                Ok(Err(e)) => (panel.hooks.toast)(&format!(
                    "Could not switch to {branch}: {}",
                    reason(&format!("{e:#}"))
                )),
                Err(_) => tracing::warn!("the git worker panicked"),
            }
            panel.refresh();
        });
    }

    /// Branch from HEAD and switch to it in one step, which is `git switch -c`: no base picker,
    /// because the base a reader means is the state they are looking at.
    ///
    /// The name is git's to validate — a bad ref name, one already taken and a worktree the
    /// switch would clobber are all its refusals, and they come back through [`Panel::command`]'s
    /// dialog, which also brings the refresh.
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
                panel.command("Create Branch", None, move |vault, repo| {
                    vault
                        .git_create_branch(repo, &name, true)
                        .map(|()| format!("Switched to {name}"))
                });
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
        let repo = {
            let state = self.state.borrow();
            match state.repos.get(state.selected) {
                Some(repo) => repo.clone(),
                None => return,
            }
        };
        let panel = self.clone();
        let vault = self.hooks.vault.clone();
        glib::spawn_future_local(async move {
            let asked = name.clone();
            let done =
                gio::spawn_blocking(move || vault.git_delete_branch(&repo, &asked, force)).await;
            match done {
                Ok(Ok(())) => (panel.hooks.toast)(&format!("Deleted {name}")),
                Ok(Err(e)) => {
                    let message = format!("{e:#}");
                    match !force && git::unmerged(&message) {
                        true => panel.confirm_delete(name),
                        false => panel.failed("Delete Branch", &message),
                    }
                }
                Err(_) => tracing::warn!("the git worker panicked"),
            }
            panel.refresh();
        });
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

    /// Put HEAD on one commit, detached, so the repository can be read at that point.
    ///
    /// No confirmation, for the reason [`Panel::checkout`] gives: `git switch --detach` refuses
    /// where it would clobber uncommitted work, and that refusal is the whole answer. The refresh
    /// that follows puts `Detached at …` in the branch button and the status bar.
    pub(super) fn detach(self: &Rc<Self>, oid: String) {
        let repo = {
            let state = self.state.borrow();
            match state.repos.get(state.selected) {
                Some(repo) => repo.clone(),
                None => return,
            }
        };
        let panel = self.clone();
        let vault = self.hooks.vault.clone();
        glib::spawn_future_local(async move {
            let asked = oid.clone();
            let done = gio::spawn_blocking(move || vault.git_checkout_commit(&repo, &asked)).await;
            match done {
                Ok(Ok(())) => (panel.hooks.toast)(&format!("Checked out {}", short(&oid))),
                Ok(Err(e)) => (panel.hooks.toast)(&format!(
                    "Could not check out {}: {}",
                    short(&oid),
                    reason(&format!("{e:#}"))
                )),
                Err(_) => tracing::warn!("the git worker panicked"),
            }
            panel.refresh();
        });
    }

    pub(super) fn stage(self: &Rc<Self>, paths: Vec<String>) {
        let n = paths.len();
        self.write("Stage", paths, move |vault, repo, paths| {
            vault
                .git_stage(repo, paths)
                .map(|()| format!("Staged {}", files(n)))
        });
    }

    pub(super) fn unstage(self: &Rc<Self>, paths: Vec<String>) {
        let n = paths.len();
        self.write("Unstage", paths, move |vault, repo, paths| {
            vault
                .git_unstage(repo, paths)
                .map(|()| format!("Unstaged {}", files(n)))
        });
    }

    /// [`Panel::command`] for the three that take paths, which have to outlive the borrow.
    fn write(
        self: &Rc<Self>,
        verb: &'static str,
        paths: Vec<String>,
        job: impl FnOnce(&Vault, &Repo, &[String]) -> anyhow::Result<String> + Send + 'static,
    ) {
        if paths.is_empty() {
            return;
        }
        self.command(verb, None, move |vault, repo| job(vault, repo, &paths));
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
                        panel.schedule_refresh();
                    }
                    false => panel.write("Discard", vec![path], move |vault, repo, paths| {
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
