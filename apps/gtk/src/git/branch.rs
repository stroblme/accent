//! The branch row's popover, the branches it lists and switches between, the Sync button's
//! tooltip, and the readouts a branch's status becomes.

use super::*;

impl Panel {
    /// What the Sync button says it will do, and whether it can. Both read from the last refresh,
    /// so a background fetch that failed can put its own line on the tooltip without one.
    ///
    /// A branch with no upstream is not a dead end: syncing it publishes it (`git::push`), so the
    /// button stays live and the tooltip says which of the two it will be.
    pub(super) fn sync_state(&self) {
        let state = self.state.borrow();
        let branch = state.statuses.get(state.selected).map(|s| &s.branch);
        self.sync.set_sensitive(branch.is_some());
        // A branch git has said nothing about has no upstream anyone knows of, so it is not
        // "Publish" either.
        let mut tip = branch
            .filter(|b| branch_parts(b).is_some())
            .map(sync_hint)
            .unwrap_or_else(|| "Sync".to_string());
        // Quiet, and only here: the button still works and a sync is still what it does. What a
        // failed background fetch costs is the counts beside it, and what a status that did not
        // come back costs is everything the pane shows beside it; this is the one surface that
        // can say so without interrupting anyone.
        if state.status_kept {
            tip.push_str("\n\nThe status could not be refreshed, so the branch and the changes shown may be out of date.");
        }
        if self.fetch_failed.get() {
            tip.push_str("\n\nThe last background fetch did not go through, so the counts may be out of date.");
        }
        self.sync.set_tooltip_text(Some(&tip));
    }

    /// Put the branch popover on `rows` (see [`branch_model`]), `at` being the local row HEAD is
    /// on.
    pub(super) fn set_branches(self: &Rc<Self>, rows: &git::Branches, at: Option<usize>) {
        self.branch_label.set_text(
            at.and_then(|i| rows.local.get(i))
                .map_or("", String::as_str),
        );
        if *self.branch_shown.borrow() == (rows.clone(), at) {
            return;
        }
        self.branch_shown.replace((rows.clone(), at));
        while let Some(row) = self.branch_list.first_child() {
            self.branch_list.remove(&row);
        }
        for (i, name) in rows.local.iter().enumerate() {
            let row = self.branch_row(name, Some(i) != at);
            self.branch_list.append(&row);
        }
        if rows.remote.is_empty() {
            return;
        }
        self.branch_list.append(&remote_heading());
        // No trash button here: deleting a remote branch is a push, and stays a terminal job.
        for name in &rows.remote {
            self.branch_list
                .append(&self.pick_button(name, Panel::track));
        }
    }

    /// One row of the branch popover: the name, which switches to it, and — where git would let
    /// it go — a trash button. The branch HEAD is on has none: git refuses to delete it, and a
    /// control that cannot work is dead chrome (DESIGN.md, Principle 1).
    fn branch_row(self: &Rc<Self>, name: &str, deletable: bool) -> gtk::Box {
        let switch = self.pick_button(name, Panel::checkout);
        switch.set_hexpand(true);

        let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        row.append(&switch);
        if deletable {
            let trash = icon_button("user-trash-symbolic", "Delete Branch");
            // Shown on the row's hover and `:focus-within` by `.git-actions`: a `GtkListBoxRow`'s
            // node is `row`, which is what that CSS selects on.
            trash.add_css_class("git-actions");
            let (weak, asked) = (Rc::downgrade(self), name.to_string());
            trash.connect_clicked(move |_| {
                if let Some(panel) = weak.upgrade() {
                    panel.branch_menu.popdown();
                    panel.delete_branch(asked.clone(), false);
                }
            });
            row.append(&trash);
        }
        row
    }

    /// A branch's name as a flat button that puts the popover away and hands the name to `pick`.
    fn pick_button(self: &Rc<Self>, name: &str, pick: fn(&Rc<Panel>, String)) -> gtk::Button {
        let button = gtk::Button::builder()
            .child(&gtk::Label::builder().label(name).xalign(0.0).build())
            .build();
        button.add_css_class("flat");
        let (weak, asked) = (Rc::downgrade(self), name.to_string());
        button.connect_clicked(move |_| {
            if let Some(panel) = weak.upgrade() {
                panel.branch_menu.popdown();
                pick(&panel, asked.clone());
            }
        });
        button
    }
}

/// The heading over the branch popover's remote rows. A row that is not a branch, so nothing
/// activates it and the keyboard passes it by. Inset by the 17 px Adwaita gives a text button
/// either side of its label, so it sits over the names below it rather than out at the row's
/// edge; small and dim, because every name under it is already a bold button label.
fn remote_heading() -> gtk::ListBoxRow {
    let label = gtk::Label::builder()
        .label("Remote")
        .xalign(0.0)
        .margin_start(17)
        .margin_top(6)
        .build();
    for class in ["caption-heading", "dim-label"] {
        label.add_css_class(class);
    }
    gtk::ListBoxRow::builder()
        .child(&label)
        .activatable(false)
        .selectable(false)
        .focusable(false)
        .build()
}

/// The branch chooser's rows and which of the local ones HEAD is on.
///
/// `local` is the local branches, led by whatever HEAD is on when that is not one of them — a
/// detached HEAD, or a branch with no commit yet, so that the chooser always says where the
/// repository actually is. `remote` is the remote-tracking branches no local branch shares a name
/// with, which are the ones there is anything to check out: the others are a local row already.
/// `None` is a repository git told us nothing about, which shows an empty chooser as it used to
/// show an empty label.
pub(super) fn branch_model(
    head: Option<String>,
    branches: &git::Branches,
) -> (git::Branches, Option<usize>) {
    let Some(head) = head else {
        return (git::Branches::default(), None);
    };
    let remote = branches
        .remote
        .iter()
        .filter(|r| !branches.local.iter().any(|b| b == local_name(r)))
        .cloned()
        .collect();
    let (local, at) = match branches.local.iter().position(|b| *b == head) {
        Some(at) => (branches.local.clone(), at),
        None => (
            std::iter::once(head)
                .chain(branches.local.iter().cloned())
                .collect(),
            0,
        ),
    };
    let rows = git::Branches {
        local,
        remote,
        ..git::Branches::default()
    };
    (rows, Some(at))
}

/// The local branch a remote-tracking one checks out as: `origin/topic` is `topic`. Cut at the
/// first slash, which is the remote's name wherever that name has no slash of its own.
pub(super) fn local_name(remote: &str) -> &str {
    remote.split_once('/').map_or(remote, |(_, name)| name)
}

/// The branch name and its ahead/behind counts, the two labels of the branch row. `None` when git
/// told us nothing at all, which is what a failed `status` leaves behind.
pub(super) fn branch_parts(b: &Branch) -> Option<(String, String)> {
    if b.head.is_none() && b.oid.is_none() {
        return None;
    }
    // A detached HEAD has no name, so it says where it is instead: this string reaches the branch
    // button and the status bar alike, and both of them otherwise read as a branch called HEAD.
    let name = b.head.clone().unwrap_or_else(|| match &b.oid {
        Some(oid) => format!("Detached at {}", short(oid)),
        None => "Detached".to_string(),
    });
    let counts = [(b.ahead, '↑'), (b.behind, '↓')]
        .iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, arrow)| format!("{arrow}{count}"))
        .collect::<Vec<_>>()
        .join(" ");
    Some((name, counts))
}

/// The branch HEAD is on, as the chooser and the status bar both name it: what the status says,
/// or while git has given the repository no status, the branch its branch list marks. Only the
/// selected repository's branches are read, so `listed` is `None` for any other; a detached HEAD
/// marks none, and stays unnamed until a status says where it is.
pub(super) fn head_name(status: &Status, listed: Option<&git::Branches>) -> Option<String> {
    branch_parts(&status.branch)
        .map(|(name, _)| name)
        .or_else(|| listed?.head.clone())
}

/// The branch row on one line, for anywhere with room for one string.
fn branch_text(b: &Branch) -> Option<String> {
    branch_parts(b).map(|(name, counts)| match counts.is_empty() {
        true => name,
        false => format!("{name} {counts}"),
    })
}

/// The whole branch readout the status bar shows: the branch, its ahead and behind counts, and
/// the dot in front when the repository has work that is not committed.
///
/// The dot leads and is the very character a dirty tab wears, so one symbol means "there is
/// something here that is not written down" wherever it appears. What it counts is every record
/// `git status` produced, untracked files included ([`Status::dirty`]), so it and the Git pane's
/// changes list are the same answer.
pub(super) fn branch_line(status: &Status) -> Option<String> {
    let text = branch_text(&status.branch)?;
    Some(match status.dirty() {
        true => format!("• {text}"),
        false => text,
    })
}

/// What a Sync will do, for the Sync button's tooltip.
///
/// Words beside the arrows the button already shows, because `↓2 ↑1` is a readout and a tooltip
/// is where it is spelled out. A branch with no upstream reads as Publish: that is what syncing
/// one does now, and the tooltip is the only place that can say so before it happens.
fn sync_hint(b: &Branch) -> String {
    let Some(upstream) = &b.upstream else {
        return "Publish this branch to its remote and track it".to_string();
    };
    let moving: Vec<String> = [(b.behind, "to pull"), (b.ahead, "to push")]
        .iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, what)| format!("{count} {what}"))
        .collect();
    match moving.is_empty() {
        true => format!("Sync with {upstream}"),
        false => format!("Sync with {upstream}: {}", moving.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, x: char, y: char) -> Entry {
        Entry {
            path: path.to_string(),
            orig: None,
            x,
            y,
            unmerged: false,
            submodule: false,
        }
    }

    fn on_main(upstream: Option<&str>, ahead: u32, behind: u32) -> Branch {
        Branch {
            oid: Some("0123456789abcdef".to_string()),
            head: Some("main".to_string()),
            upstream: upstream.map(str::to_string),
            ahead,
            behind,
        }
    }

    #[test]
    fn the_branch_readout_wears_the_dot_when_anything_is_uncommitted() {
        let clean = Status {
            branch: on_main(Some("origin/main"), 1, 2),
            entries: Vec::new(),
            ignored: vec!["build/".to_string()],
            merging: false,
            rebasing: false,
        };
        assert_eq!(branch_line(&clean).as_deref(), Some("main ↑1 ↓2"));

        // Untracked on its own is enough: the dot counts what the changes list shows.
        let dirty = Status {
            entries: vec![entry("new.md", '?', '?')],
            ..clean.clone()
        };
        assert_eq!(branch_line(&dirty).as_deref(), Some("• main ↑1 ↓2"));
        assert_eq!(branch_line(&Status::default()), None, "git said nothing");
    }

    #[test]
    fn the_sync_tooltip_says_which_way_the_work_would_move() {
        assert_eq!(
            sync_hint(&on_main(Some("origin/main"), 0, 0)),
            "Sync with origin/main"
        );
        assert_eq!(
            sync_hint(&on_main(Some("origin/main"), 1, 2)),
            "Sync with origin/main: 2 to pull, 1 to push"
        );
        assert!(sync_hint(&on_main(None, 0, 0)).starts_with("Publish"));
    }

    #[test]
    fn branch_text_names_the_branch_and_only_the_counts_that_are_there() {
        let main = Branch {
            oid: Some("abc".into()),
            head: Some("main".into()),
            ..Branch::default()
        };
        assert_eq!(branch_text(&main).as_deref(), Some("main"));
        let ahead = Branch {
            ahead: 1,
            ..main.clone()
        };
        assert_eq!(branch_text(&ahead).as_deref(), Some("main ↑1"));
        let both = Branch {
            behind: 2,
            ..ahead.clone()
        };
        assert_eq!(branch_text(&both).as_deref(), Some("main ↑1 ↓2"));
        let detached = Branch { head: None, ..main };
        assert_eq!(branch_text(&detached).as_deref(), Some("Detached at abc"));
        assert_eq!(branch_text(&Branch::default()), None, "nothing to say");
    }

    fn listed(local: &[&str], remote: &[&str]) -> git::Branches {
        git::Branches {
            local: local.iter().map(|b| b.to_string()).collect(),
            remote: remote.iter().map(|b| b.to_string()).collect(),
            head: None,
        }
    }

    #[test]
    fn head_name_falls_back_on_the_branch_list_only_while_there_is_no_status() {
        let branches = git::Branches {
            head: Some("side".to_string()),
            ..listed(&["main", "side"], &[])
        };
        let status = Status {
            branch: on_main(None, 0, 0),
            ..Status::default()
        };
        assert_eq!(head_name(&status, Some(&branches)).as_deref(), Some("main"));
        assert_eq!(
            head_name(&Status::default(), Some(&branches)).as_deref(),
            Some("side")
        );
        assert_eq!(
            head_name(&Status::default(), None),
            None,
            "another repository's branches are not read"
        );
    }

    #[test]
    fn branch_model_always_shows_what_head_is_actually_on() {
        let locals = listed(&["main", "side"], &[]);
        assert_eq!(
            branch_model(Some("side".into()), &locals),
            (locals.clone(), Some(1))
        );
        assert_eq!(
            branch_model(Some("HEAD".into()), &locals),
            (listed(&["HEAD", "main", "side"], &[]), Some(0)),
            "a detached HEAD leads the list it is not in"
        );
        assert_eq!(
            branch_model(Some("main".into()), &listed(&[], &[])),
            (listed(&["main"], &[]), Some(0)),
            "a repository with no commits has a head and no branches"
        );
        assert_eq!(
            branch_model(None, &locals),
            (git::Branches::default(), None)
        );
    }

    #[test]
    fn branch_model_lists_a_remote_branch_only_where_no_local_one_has_its_name() {
        let both = listed(
            &["main", "side"],
            &["origin/main", "origin/topic", "upstream/side"],
        );
        assert_eq!(
            branch_model(Some("main".into()), &both),
            (listed(&["main", "side"], &["origin/topic"]), Some(0)),
            "after the local ones, and without the ones checked out already"
        );
    }
}
