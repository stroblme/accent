//! The comparisons a row opens, and keeping the open ones current.

use super::*;
use crate::diff::OnLines;
use accent_api::git::{Comparison, Sides};
use accent_core::diff;

impl Panel {
    /// Open the comparison a row stands for. Both sides are read in one worker hop, because two
    /// would show the file mid-write if it changed between them. Only the last row clicked opens:
    /// reads land in whatever order the workers finish them, and two rows clicked within one read
    /// put the first in front, a staged or deleted file's preview closing the other's tab.
    pub(super) fn compare(self: &Rc<Self>, rel: &str, key: &str, sides: Sides) {
        let repo = {
            let state = self.state.borrow();
            match state.repos.get(state.selected) {
                Some(repo) => repo.clone(),
                None => return,
            }
        };
        let what = Comparison {
            repo,
            rel: rel.to_string(),
            key: key.to_string(),
            sides,
        };
        let asked = self.asked.get() + 1;
        self.asked.set(asked);
        let panel = self.clone();
        let vault = self.hooks.vault.clone();
        glib::spawn_future_local(async move {
            let read = {
                let what = what.clone();
                crate::work::off_thread("git", move || read(&what, &vault)).await
            };
            if panel.asked.get() != asked {
                return;
            }
            match read {
                Some(Ok(read)) => panel.show(what, read, false),
                Some(Err(why)) => (panel.hooks.toast)(&why),
                None => {}
            }
        });
    }

    /// Open the file at `rel`, keyed `key`, git left unmerged as a merge of its stages: a row's
    /// click, the last row clicked opening as a comparison's does.
    pub(super) fn open_merge(self: &Rc<Self>, rel: &str, key: &str) {
        let repo = {
            let state = self.state.borrow();
            match state.repos.get(state.selected) {
                Some(repo) => repo.clone(),
                None => return,
            }
        };
        let asked = self.asked.get() + 1;
        self.asked.set(asked);
        let what = Comparison {
            repo,
            rel: rel.to_string(),
            key: key.to_string(),
            sides: Sides::Merge,
        };
        self.show_merge(what, Some(asked));
    }

    /// Open `what`, a file git left unmerged, as a merge of its stages, read in one worker hop.
    /// `asked` is a row's click (see [`Panel::compare`]), which opens the file's tab as the
    /// pane's preview; without it the merge goes into the tab already showing the file, as a
    /// session restore puts one back or a comparison a merge has outgrown is switched to one,
    /// and a file no longer unmerged stays as it is without a word. A conflict that is not two
    /// texts — a binary, or a side that deleted the file — leaves the file alone, saying why.
    fn show_merge(self: &Rc<Self>, what: Comparison, asked: Option<u64>) {
        let (panel, vault) = (self.clone(), self.hooks.vault.clone());
        glib::spawn_future_local(async move {
            let read = {
                let (repo, rel) = (what.repo.clone(), what.rel.clone());
                crate::work::off_thread("git", move || {
                    [":1", ":2", ":3"].map(|stage| vault.git_show(&repo, stage, &rel))
                })
                .await
            };
            let fresh = asked.is_none_or(|asked| panel.asked.get() == asked);
            let Some(read) = read.filter(|_| fresh) else {
                return;
            };
            let name = split_name(&what.rel).1.to_string();
            let [base, current, incoming] = match read {
                [Ok(base), Ok(current), Ok(incoming)] => [base, current, incoming],
                read => {
                    let why = read.into_iter().find_map(Result::err);
                    let why = why.map(|e| format!("{e:#}")).unwrap_or_default();
                    return (panel.hooks.toast)(&format!("Cannot read {name}'s conflict: {why}"));
                }
            };
            let say = |why: &str| {
                (panel.hooks.toast)(&format!("{name} {why}"));
                match asked {
                    Some(_) => (panel.hooks.open)(&what.key),
                    None => (panel.hooks.leave)(&what.key),
                }
            };
            let (current, incoming) = match (current, incoming) {
                (Some(current), Some(incoming)) => (current, incoming),
                (None, None) if asked.is_none() => return,
                _ => return say("was deleted on one side: keep it with Stage, or delete it"),
            };
            let text = |blob: Option<Blob>| match blob {
                Some(Blob::Text(text)) => Some(text),
                Some(Blob::Binary) => None,
                None => Some(String::new()),
            };
            let [Some(base), Some(current), Some(incoming)] =
                [text(base), text(Some(current)), text(Some(incoming))]
            else {
                return say("is binary");
            };
            let (weak, key) = (Rc::downgrade(&panel), what.key.clone());
            let register = Box::new(move |merge: Weak<Merge>| {
                if let Some(panel) = weak.upgrade() {
                    panel.watch(what, Target::Merge(merge));
                }
            });
            (panel.hooks.merge_file)(&key, [base, current, incoming], asked.is_none(), register);
        });
    }

    /// Open `what` again as a session restore puts it back: into the pane the restore keeps for
    /// its tab, in front of nothing, and saying nothing where there is nothing left to show. `done`
    /// runs once it is open or given up.
    pub fn restore(self: &Rc<Self>, what: Comparison, done: impl FnOnce() + 'static) {
        if what.sides == Sides::Merge {
            self.show_merge(what, None);
            return done();
        }
        let (panel, vault) = (self.clone(), self.hooks.vault.clone());
        glib::spawn_future_local(async move {
            let read = {
                let what = what.clone();
                crate::work::off_thread("git", move || read(&what, &vault)).await
            };
            match read {
                Some(Ok(read)) => panel.show(what, read, true),
                Some(Err(why)) => tracing::debug!("{why}"),
                None => {}
            }
            done();
        });
    }

    /// The comparisons open, each under the key of the tab showing it: what a session keeps.
    pub fn kept(&self) -> Vec<(String, Comparison)> {
        let watches = self.watches.borrow();
        let tab = |w: &Watch| match &w.target {
            Target::Tab(compare) => compare.upgrade().map(|_| w.what.key.clone()),
            Target::Diff(tab) => tab.upgrade().map(|tab| tab.key()),
            Target::Merge(merge) => merge.upgrade().map(|_| w.what.key.clone()),
        };
        watches
            .iter()
            .filter_map(|w| Some((tab(w)?, w.what.clone())))
            .collect()
    }

    /// Put a freshly read comparison on screen, and remember it for the refreshes to come and
    /// for the session. A `restored` one says nothing where it shows nothing: the reader did not
    /// ask for it in this sitting.
    fn show(self: &Rc<Self>, what: Comparison, read: (Blob, Blob), restored: bool) {
        let name = split_name(&what.rel).1.to_string();
        let say = {
            let panel = Rc::downgrade(self);
            move |text: &str| {
                if let Some(panel) = panel.upgrade().filter(|_| !restored) {
                    (panel.hooks.toast)(text);
                }
            }
        };
        // The same test the tab opener uses, and the same answer: a diff of two binaries is
        // noise, so the pane says why instead of showing it.
        let (Blob::Text(left), Blob::Text(right)) = read else {
            return say(&format!("{name} is binary"));
        };
        let left_name = split_name(left_rel(&what)).1;
        let left_title = format!("{left_name} ({})", left_title(&what.sides));
        let right_title = format!("{name} ({})", right_title(&what.sides));
        let nothing = nothing_to_show(&what.sides);
        let endings = line_endings_only(&left, &right);
        let unchanged = left == right;
        match what.sides.clone() {
            // The working tree is the file itself, so the comparison lives in its tab and the
            // refresh only ever has the index side to re-read.
            Sides::Worktree => {
                let (panel, key) = (Rc::downgrade(self), what.key.clone());
                let register = move |compare: Weak<Compare>| {
                    let Some(panel) = panel.upgrade() else {
                        return false;
                    };
                    if let Some(compare) = compare.upgrade() {
                        // A row names a path; it does not hold what git said about it. By the
                        // time the comparison is read the file may have been staged, discarded
                        // or committed — from the pane itself, or from a terminal — and then the
                        // two sides carry the same text and the comparison shows nothing. Two
                        // identical columns are not an answer: say so and ask git again, so the
                        // row goes as well. Unless the file's line endings are all that changed,
                        // which git goes on listing however often it is asked. And unless the
                        // disk does differ from the index, and it is the tab that has not caught
                        // up with the disk yet: the tab reads it again as it opens (`compare_file`)
                        // and the comparison is laid again over what it reads.
                        if compare.counts().1 == 0 {
                            if endings {
                                say(&format!("{name} differs only in line endings"));
                                return false;
                            }
                            if unchanged {
                                say(&format!("{name} {nothing}"));
                                if !restored {
                                    panel.schedule_refresh(Depth::Everything);
                                }
                                return false;
                            }
                        }
                        panel.offer_lines(&compare, &what);
                    }
                    panel.watch(what, Target::Tab(compare));
                    true
                };
                (self.hooks.compare_file)(&key, &left_title, &left, restored, Box::new(register));
            }
            Sides::Staged { .. } | Sides::Deleted | Sides::Commit { .. } | Sides::Merge => {
                // The same test, made where this side can make it: before the tab is opened
                // rather than once it holds a comparison. A Staged row the index has outgrown
                // and a file listed under a commit that did not change it both read the same
                // text twice, and a tab of two identical columns is not an answer.
                if left == right {
                    say(&format!("{name} {nothing}"));
                    if !restored {
                        self.schedule_refresh(Depth::Everything);
                    }
                    return;
                }
                if endings {
                    return say(&format!("{name} differs only in line endings"));
                }
                let key = format!("diff:{}:{}", tag(&what.sides), what.key);
                let tab = (self.hooks.open_diff)(
                    &key,
                    &name,
                    &right_title,
                    (&left_title, &left),
                    (&right_title, &right),
                    restored,
                );
                if let Some(tab) = tab {
                    self.offer_lines(tab.comparison(), &what);
                    self.watch(what, Target::Diff(Rc::downgrade(&tab)));
                }
            }
        }
    }

    /// Stage Selected Lines and Revert Selected Lines on a comparison of the working tree with
    /// the index, with Stage and Revert on each of its hunks, Unstage Selected Lines on one of the
    /// index with HEAD, and nothing on the rest. What is staged or unstaged is the index's text
    /// with the selected changes made or undone, worked out from the two texts on screen, which
    /// are what the selection was made in.
    fn offer_lines(self: &Rc<Self>, compare: &Rc<Compare>, what: &Comparison) {
        let (name, label, unstage) = match what.sides {
            Sides::Worktree => ("stage", "Stage Selected Lines", false),
            Sides::Staged { .. } => ("unstage", "Unstage Selected Lines", true),
            Sides::Deleted | Sides::Commit { .. } | Sides::Merge => return,
        };
        let (panel, repo, rel) = (Rc::downgrade(self), what.repo.clone(), what.rel.clone());
        let stage: OnLines = Rc::new(move |side, lines, old, new| {
            let Some(panel) = panel.upgrade() else {
                return;
            };
            // The index is the left side of the one and the right side of the other.
            let (text, index) = match unstage {
                false => (diff::apply_lines(old, new, side, lines), old),
                true => (diff::revert_lines(old, new, side, lines), new),
            };
            if text == index {
                return (panel.hooks.toast)("No changes in the selection");
            }
            panel.stage_text(repo.clone(), rel.clone(), text, unstage);
        });
        let mut entries = vec![(name, label, stage.clone())];
        // The working tree is the file's own tab, so its side of the comparison is the editor:
        // the selected lines go back to the index's there, as an edit Ctrl+Z takes back, and are
        // saved as any edit is.
        if !unstage {
            let (panel, weak) = (Rc::downgrade(self), Rc::downgrade(compare));
            let revert: OnLines = Rc::new(move |side, lines, old, new| {
                let (Some(panel), Some(compare)) = (panel.upgrade(), weak.upgrade()) else {
                    return;
                };
                let text = diff::revert_lines(old, new, side, lines);
                if text == new {
                    return (panel.hooks.toast)("No changes in the selection");
                }
                compare.rewrite_mine(&text);
            });
            entries.push(("revert", "Revert Selected Lines", revert.clone()));
            // Each arrow points where the lines go, from the half they come from.
            compare.offer_hunks(vec![
                (
                    "go-next-symbolic",
                    "Revert",
                    "Revert this hunk to the index",
                    revert,
                ),
                ("go-previous-symbolic", "Stage", "Stage this hunk", stage),
            ]);
        }
        compare.offer(entries);
    }

    /// The working-tree comparison of `key`, for the bench: the vault root is the repository,
    /// so the key is the path git knows.
    #[cfg(feature = "bench")]
    pub fn compare_worktree(self: &Rc<Self>, key: &str) {
        self.compare(key, key, Sides::Worktree);
    }

    /// The staged comparison of `key`, HEAD against the index, likewise.
    #[cfg(feature = "bench")]
    pub fn compare_staged(self: &Rc<Self>, key: &str) {
        self.compare(key, key, Sides::Staged { orig: None });
    }

    /// The comparison a file under a history row opens: `oid` against `parent`, likewise.
    #[cfg(feature = "bench")]
    pub fn compare_commit(self: &Rc<Self>, key: &str, oid: &str, parent: &str) {
        let sides = Sides::Commit {
            oid: oid.to_string(),
            parent: Some(parent.to_string()),
            orig: None,
        };
        self.compare(key, key, sides);
    }

    /// One watch per comparison: asking for the same one again replaces the old entry.
    fn watch(&self, what: Comparison, target: Target) {
        let mut watches = self.watches.borrow_mut();
        watches.retain(|w| !(w.what.key == what.key && tag(&w.what.sides) == tag(&what.sides)));
        watches.push(Watch { what, target });
    }

    /// Re-read every comparison still open, now that what git says has moved under it. A commit
    /// never changes; the index does. A merge ends once git no longer lists its file as unmerged,
    /// and a working-tree comparison of a file a merge has since left unmerged becomes its merge:
    /// told from the status this refresh read, git refusing the index side of such a file.
    pub(super) fn reload_diffs(self: &Rc<Self>) {
        self.watches.borrow_mut().retain(|w| w.target.alive());
        let (open, mut watches) = (self.watches.borrow().clone(), Vec::new());
        for w in open {
            match (&w.what.sides, self.unmerged(&w.what)) {
                (Sides::Merge, Some(false)) => (self.hooks.leave)(&w.what.key),
                (Sides::Worktree, Some(true)) => {
                    let sides = Sides::Merge;
                    self.show_merge(Comparison { sides, ..w.what }, None);
                }
                (Sides::Commit { .. } | Sides::Merge, _) => {}
                _ => watches.push(w),
            }
        }
        if watches.is_empty() {
            return;
        }
        let vault = self.hooks.vault.clone();
        glib::spawn_future_local(async move {
            let reads = {
                let whats: Vec<Comparison> = watches.iter().map(|w| w.what.clone()).collect();
                crate::work::off_thread("git", move || {
                    whats.iter().map(|w| read(w, &vault)).collect::<Vec<_>>()
                })
                .await
            };
            let Some(reads) = reads else { return };
            for (watch, read) in watches.into_iter().zip(reads) {
                // What could not be read leaves the comparison showing what it had.
                let Ok((Blob::Text(left), Blob::Text(right))) =
                    read.inspect_err(|why| tracing::debug!("{why}"))
                else {
                    continue;
                };
                match watch.target {
                    Target::Tab(compare) => {
                        if let Some(compare) = compare.upgrade() {
                            compare.set_side(Side::Old, &left);
                        }
                    }
                    Target::Diff(tab) => {
                        if let Some(tab) = tab.upgrade() {
                            tab.set_texts(&left, &right);
                        }
                    }
                    // Never re-read: the conflict's stages stay what they were until it ends.
                    Target::Merge(_) => {}
                }
            }
        });
    }

    /// Whether the last refresh found `what`'s file unmerged, `None` where it read nothing of its
    /// repository.
    fn unmerged(&self, what: &Comparison) -> Option<bool> {
        let state = self.state.borrow();
        let at = state.repos.iter().position(|repo| *repo == what.repo)?;
        let status = state.statuses.get(at)?;
        Some(status.conflicts().any(|entry| entry.path == what.rel))
    }

    /// The merge open on `key`, which Mark Resolved stages.
    pub(super) fn merge_on(&self, key: &str) -> Option<Comparison> {
        let watches = self.watches.borrow();
        let open = watches.iter().filter(|w| w.target.alive());
        open.map(|w| &w.what)
            .find(|what| what.key == key && what.sides == Sides::Merge)
            .cloned()
    }
}

/// The revision the left pane reads, `None` meaning there is nothing on that side at all.
fn left_rev(sides: &Sides) -> Option<&str> {
    match sides {
        Sides::Staged { .. } => Some("HEAD"),
        Sides::Worktree | Sides::Deleted => Some(""),
        Sides::Commit { parent, .. } => parent.as_deref(),
        Sides::Merge => Some(":2"),
    }
}

fn left_title(sides: &Sides) -> String {
    match sides {
        Sides::Staged { .. } => "HEAD".to_string(),
        Sides::Worktree | Sides::Deleted => "Index".to_string(),
        Sides::Commit { parent, .. } => match parent {
            Some(parent) => short(parent),
            None => "Nothing".to_string(),
        },
        Sides::Merge => "Current".to_string(),
    }
}

fn right_title(sides: &Sides) -> String {
    match sides {
        Sides::Staged { .. } => "Index".to_string(),
        Sides::Worktree => "Working Tree".to_string(),
        Sides::Deleted => "Deleted".to_string(),
        Sides::Commit { oid, .. } => short(oid),
        Sides::Merge => "Incoming".to_string(),
    }
}

/// What a comparison whose two sides carry the same text says instead of showing them. A row
/// names a path; it does not hold what git said about it, and a commit's file list is read once
/// and stays where it is, so either can be older than the repository it describes.
fn nothing_to_show(sides: &Sides) -> &'static str {
    match sides {
        Sides::Staged { .. } => "has no staged changes",
        Sides::Worktree | Sides::Deleted => "has no unstaged changes",
        Sides::Commit { .. } => "is unchanged in this commit",
        Sides::Merge => "has no conflict",
    }
}

/// What keys the tab, so the comparisons of one file are a tab each and asking twice reveals the
/// one already open.
fn tag(sides: &Sides) -> String {
    match sides {
        Sides::Staged { .. } => "index".to_string(),
        Sides::Worktree => "worktree".to_string(),
        Sides::Deleted => "deleted".to_string(),
        Sides::Commit { oid, .. } => format!("commit:{}", short(oid)),
        Sides::Merge => "merge".to_string(),
    }
}

/// Whether `left` and `right` differ in their line endings and in nothing else. The panes hold
/// `\n` endings whatever the file has, so such a comparison would open with nothing marked in it.
fn line_endings_only(left: &str, right: &str) -> bool {
    left != right && crate::diff::normalise(left) == crate::diff::normalise(right)
}

/// A comparison that is open: what it compares, the half the worker reads with, and where it
/// is on screen.
#[derive(Clone)]
pub(super) struct Watch {
    what: Comparison,
    target: Target,
}

#[derive(Clone)]
enum Target {
    /// The file's own tab, comparing its buffer with the index.
    Tab(Weak<Compare>),
    /// A tab of its own over two blobs.
    Diff(Weak<DiffTab>),
    /// The file's own tab, merging it.
    Merge(Weak<Merge>),
}

impl Target {
    fn alive(&self) -> bool {
        match self {
            Target::Tab(w) => w.strong_count() > 0,
            Target::Diff(w) => w.strong_count() > 0,
            Target::Merge(w) => w.strong_count() > 0,
        }
    }
}

/// Both sides of `what`, on the worker, or the toast that says which one could not be read.
fn read(what: &Comparison, vault: &Vault) -> Result<(Blob, Blob), String> {
    let left = match left_rev(&what.sides) {
        Some(rev) => side(what, vault, rev, left_rel(what))?,
        None => Blob::Text(String::new()),
    };
    let right = match &what.sides {
        Sides::Staged { .. } => side(what, vault, "", &what.rel)?,
        // The working tree side is the file itself, which on a remote vault is on the other
        // machine: reading it through the vault is what makes the diff work there as well
        // as here. It is read even though the tab shows its own buffer, so that the same
        // hop answers "is this binary" for both.
        Sides::Worktree => worktree(what, vault)?,
        Sides::Deleted => Blob::Text(String::new()),
        Sides::Commit { oid, .. } => side(what, vault, oid, &what.rel)?,
        Sides::Merge => side(what, vault, ":3", &what.rel)?,
    };
    Ok((left, right))
}

/// The path the left side is read at: the old one, where a commit or the index renamed the file.
fn left_rel(what: &Comparison) -> &str {
    match &what.sides {
        Sides::Staged { orig: Some(orig) }
        | Sides::Commit {
            orig: Some(orig), ..
        } => orig,
        _ => &what.rel,
    }
}

/// `rel` at `rev`. A file git has none of there is a new or deleted file, and an empty string is
/// exactly the right thing to diff against. A read that failed is not that: an empty side would
/// draw the whole file as added or deleted, so it is the toast instead.
fn side(what: &Comparison, vault: &Vault, rev: &str, rel: &str) -> Result<Blob, String> {
    match vault.git_show(&what.repo, rev, rel) {
        Ok(blob) => Ok(blob.unwrap_or_else(|| Blob::Text(String::new()))),
        Err(e) => {
            let at = match rev {
                "" => "the index".to_string(),
                rev => short(rev),
            };
            Err(format!("Cannot read {} at {at}: {e:#}", split_name(rel).1))
        }
    }
}

/// The file on disk, as the working-tree side of a comparison.
///
/// A repository above the vault root gives its files absolute keys ([`vault_key`]), and
/// `Vault::read_text` refuses those: it resolves through `Local::join`, which rejects a path
/// with a root component rather than escape the vault. Such a file is read directly instead,
/// which is right because a key is only absolute when the file is outside the vault — and
/// impossible on a remote vault, where "outside the vault" is on the other machine and the
/// toast has to say so rather than diff against nothing.
fn worktree(what: &Comparison, vault: &Vault) -> Result<Blob, String> {
    let outside = Path::new(&what.key).is_absolute();
    // Outside the vault on a remote vault is on the other machine, and the path would name
    // this one's file if it named anything: refusing is the only honest answer.
    if outside && vault.is_remote() {
        let name = split_name(&what.rel).1;
        return Err(format!("{name} is outside the vault on the remote host"));
    }
    let read = match outside {
        true => accent_core::fs::read_text(Path::new(&what.key)),
        false => vault.read_text(&what.key),
    };
    Ok(match read {
        // With the line endings the file has, each line its own, which reading it as text
        // takes away: a change of those alone is a change to git, and [`line_endings_only`]
        // has to see it. A file that is not UTF-8 cannot be read as it is, and has CRLF put
        // back on every line.
        Ok(accent_api::fs::Read::Text(t)) if t.crlf => {
            let raw = match outside {
                true => accent_core::fs::read_note(Path::new(&what.key)),
                false => vault.read(&what.key),
            };
            Blob::Text(raw.map_or_else(
                |_| accent_api::fs::for_disk(&t.text, true, false),
                |(text, _)| text,
            ))
        }
        Ok(accent_api::fs::Read::Text(t)) => Blob::Text(t.text),
        Ok(_) => Blob::Binary,
        // A file that is no longer there really is a deletion, and an empty right side is
        // what draws one. This used to be every absolute key as well, which drew a file
        // whose repository is above the vault root as wholly deleted.
        Err(e) => {
            tracing::debug!("reading {}: {e}", what.key);
            Blob::Text(String::new())
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_change_of_line_endings_alone_is_told_from_no_change_and_from_an_edit() {
        assert!(line_endings_only("a\r\nb\r\n", "a\nb\n"));
        assert!(!line_endings_only("a\nb\n", "a\nb\n"), "no change at all");
        assert!(
            !line_endings_only("a\r\nb\r\n", "a\nc\n"),
            "an edit as well"
        );
    }

    #[test]
    fn a_comparison_with_nothing_in_it_names_the_side_the_row_came_from() {
        let commit = Sides::Commit {
            oid: "abc1234".to_string(),
            parent: None,
            orig: None,
        };
        assert_eq!(
            nothing_to_show(&Sides::Staged { orig: None }),
            "has no staged changes"
        );
        assert_eq!(nothing_to_show(&commit), "is unchanged in this commit");
        // A deleted file's row sits in the same section as a modified one, and says the same.
        assert_eq!(
            nothing_to_show(&Sides::Deleted),
            nothing_to_show(&Sides::Worktree)
        );
    }
}
