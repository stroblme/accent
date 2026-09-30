//! The comparisons a row opens, and keeping the open ones current.

use super::*;
use accent_core::diff;

impl Panel {
    /// Open the comparison a row stands for. Both sides are read in one worker hop, because two
    /// would show the file mid-write if it changed between them.
    pub(super) fn compare(self: &Rc<Self>, rel: &str, key: &str, sides: Sides) {
        let repo = {
            let state = self.state.borrow();
            match state.repos.get(state.selected) {
                Some(repo) => repo.clone(),
                None => return,
            }
        };
        let what = What {
            repo,
            rel: rel.to_string(),
            key: key.to_string(),
            sides,
        };
        let panel = self.clone();
        let vault = self.hooks.vault.clone();
        glib::spawn_future_local(async move {
            let read = {
                let what = what.clone();
                crate::work::off_thread("git", move || what.read(&vault)).await
            };
            match read {
                Some(Ok(read)) => panel.show(what, read),
                Some(Err(why)) => (panel.hooks.toast)(&why),
                None => {}
            }
        });
    }

    /// Put a freshly read comparison on screen, and remember it for the refreshes to come.
    fn show(self: &Rc<Self>, what: What, read: (Blob, Blob)) {
        let name = split_name(&what.rel).1.to_string();
        // The same test the tab opener uses, and the same answer: a diff of two binaries is
        // noise, so the pane says why instead of showing it.
        let (Blob::Text(left), Blob::Text(right)) = read else {
            return (self.hooks.toast)(&format!("{name} is binary"));
        };
        let left_name = split_name(what.left_rel()).1;
        let left_title = format!("{left_name} ({})", what.sides.left_title());
        let right_title = format!("{name} ({})", what.sides.right_title());
        let nothing = what.sides.nothing_to_show();
        let endings = line_endings_only(&left, &right);
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
                        // which git goes on listing however often it is asked.
                        if compare.counts().1 == 0 {
                            if endings {
                                (panel.hooks.toast)(&format!(
                                    "{name} differs only in line endings"
                                ));
                                return false;
                            }
                            (panel.hooks.toast)(&format!("{name} {nothing}"));
                            panel.schedule_refresh(Depth::Everything);
                            return false;
                        }
                        panel.offer_lines(&compare, &what);
                    }
                    panel.watch(what, Target::Tab(compare));
                    true
                };
                (self.hooks.compare_file)(&key, &left_title, &left, Box::new(register));
            }
            Sides::Staged { .. } | Sides::Deleted | Sides::Commit { .. } => {
                // The same test, made where this side can make it: before the tab is opened
                // rather than once it holds a comparison. A Staged row the index has outgrown
                // and a file listed under a commit that did not change it both read the same
                // text twice, and a tab of two identical columns is not an answer.
                if left == right {
                    (self.hooks.toast)(&format!("{name} {nothing}"));
                    return self.schedule_refresh(Depth::Everything);
                }
                if endings {
                    return (self.hooks.toast)(&format!("{name} differs only in line endings"));
                }
                let key = format!("diff:{}:{}", what.sides.tag(), what.key);
                let tab = (self.hooks.open_diff)(
                    &key,
                    &name,
                    &right_title,
                    (&left_title, &left),
                    (&right_title, &right),
                );
                // A commit never changes; the index does.
                if let (Some(tab), Sides::Staged { .. } | Sides::Deleted) = (tab, &what.sides) {
                    self.offer_lines(tab.comparison(), &what);
                    self.watch(what, Target::Diff(Rc::downgrade(&tab)));
                }
            }
        }
    }

    /// Stage Selected Lines on a comparison of the working tree with the index, Unstage Selected
    /// Lines on one of the index with HEAD, and nothing on the rest. What is written either way
    /// is the index's text with the selected changes made or undone, worked out from the two
    /// texts on screen, which are what the selection was made in.
    fn offer_lines(self: &Rc<Self>, compare: &Compare, what: &What) {
        let (label, unstage) = match what.sides {
            Sides::Worktree => ("Stage Selected Lines", false),
            Sides::Staged { .. } => ("Unstage Selected Lines", true),
            Sides::Deleted | Sides::Commit { .. } => return,
        };
        let (panel, repo, rel) = (Rc::downgrade(self), what.repo.clone(), what.rel.clone());
        compare.offer(label, move |side, lines, old, new| {
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
    fn watch(&self, what: What, target: Target) {
        let mut watches = self.watches.borrow_mut();
        watches.retain(|w| !(w.what.key == what.key && w.what.sides.tag() == what.sides.tag()));
        watches.push(Watch { what, target });
    }

    /// Re-read every comparison still open, now that what git says has moved under it.
    pub(super) fn reload_diffs(self: &Rc<Self>) {
        self.watches.borrow_mut().retain(|w| w.target.alive());
        let watches = self.watches.borrow().clone();
        if watches.is_empty() {
            return;
        }
        let vault = self.hooks.vault.clone();
        glib::spawn_future_local(async move {
            let reads = {
                let whats: Vec<What> = watches.iter().map(|w| w.what.clone()).collect();
                crate::work::off_thread("git", move || {
                    whats.iter().map(|w| w.read(&vault)).collect::<Vec<_>>()
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
                }
            }
        });
    }
}

/// Which two things a row's diff compares.
#[derive(Clone)]
pub(super) enum Sides {
    /// HEAD against the index: what this commit would add. `orig` is the path a staged rename
    /// or copy came from, which is the one HEAD has.
    Staged { orig: Option<String> },
    /// The index against the file on disk: what is not staged yet.
    Worktree,
    /// The index against nothing: a file deleted from the working tree, which has no tab to
    /// compare inside, so this is a tab of its own the way a staged change is.
    Deleted,
    /// One commit against its first parent, which is what a file under an expanded history row
    /// shows. `parent` is `None` on a root commit, whose left side is simply empty. `orig` is the
    /// path a rename or copy came from, which is the one the parent has.
    Commit {
        oid: String,
        parent: Option<String>,
        orig: Option<String>,
    },
}

impl Sides {
    /// The revision the left pane reads, `None` meaning there is nothing on that side at all.
    fn left_rev(&self) -> Option<&str> {
        match self {
            Sides::Staged { .. } => Some("HEAD"),
            Sides::Worktree | Sides::Deleted => Some(""),
            Sides::Commit { parent, .. } => parent.as_deref(),
        }
    }

    fn left_title(&self) -> String {
        match self {
            Sides::Staged { .. } => "HEAD".to_string(),
            Sides::Worktree | Sides::Deleted => "Index".to_string(),
            Sides::Commit { parent, .. } => match parent {
                Some(parent) => short(parent),
                None => "Nothing".to_string(),
            },
        }
    }

    fn right_title(&self) -> String {
        match self {
            Sides::Staged { .. } => "Index".to_string(),
            Sides::Worktree => "Working Tree".to_string(),
            Sides::Deleted => "Deleted".to_string(),
            Sides::Commit { oid, .. } => short(oid),
        }
    }

    /// What a comparison whose two sides carry the same text says instead of showing them. A row
    /// names a path; it does not hold what git said about it, and a commit's file list is read
    /// once and stays where it is, so either can be older than the repository it describes.
    fn nothing_to_show(&self) -> &'static str {
        match self {
            Sides::Staged { .. } => "has no staged changes",
            Sides::Worktree | Sides::Deleted => "has no unstaged changes",
            Sides::Commit { .. } => "is unchanged in this commit",
        }
    }

    /// What keys the tab, so the comparisons of one file are a tab each and asking twice reveals
    /// the one already open.
    fn tag(&self) -> String {
        match self {
            Sides::Staged { .. } => "index".to_string(),
            Sides::Worktree => "worktree".to_string(),
            Sides::Deleted => "deleted".to_string(),
            Sides::Commit { oid, .. } => format!("commit:{}", short(oid)),
        }
    }
}

/// Whether `left` and `right` differ in their line endings and in nothing else. The panes hold
/// `\n` endings whatever the file has, so such a comparison would open with nothing marked in it.
fn line_endings_only(left: &str, right: &str) -> bool {
    left != right && crate::diff::normalise(left) == crate::diff::normalise(right)
}

/// What one comparison compares: the half of a [`Watch`] the worker reads with.
#[derive(Clone)]
struct What {
    repo: Repo,
    /// Repository-relative, which is what git is asked with.
    rel: String,
    /// Vault key, which is what the working tree is read by and the tab is keyed by.
    key: String,
    sides: Sides,
}

/// A comparison that is open: what it compares, and where it is on screen.
#[derive(Clone)]
pub(super) struct Watch {
    what: What,
    target: Target,
}

#[derive(Clone)]
enum Target {
    /// The file's own tab, comparing its buffer with the index.
    Tab(Weak<Compare>),
    /// A tab of its own over two blobs.
    Diff(Weak<DiffTab>),
}

impl Target {
    fn alive(&self) -> bool {
        match self {
            Target::Tab(w) => w.strong_count() > 0,
            Target::Diff(w) => w.strong_count() > 0,
        }
    }
}

impl What {
    /// Both sides, on the worker, or the toast that says which one could not be read.
    fn read(&self, vault: &Vault) -> Result<(Blob, Blob), String> {
        let left = match self.sides.left_rev() {
            Some(rev) => self.side(vault, rev, self.left_rel())?,
            None => Blob::Text(String::new()),
        };
        let right = match &self.sides {
            Sides::Staged { .. } => self.side(vault, "", &self.rel)?,
            // The working tree side is the file itself, which on a remote vault is on the other
            // machine: reading it through the vault is what makes the diff work there as well
            // as here. It is read even though the tab shows its own buffer, so that the same
            // hop answers "is this binary" for both.
            Sides::Worktree => self.worktree(vault)?,
            Sides::Deleted => Blob::Text(String::new()),
            Sides::Commit { oid, .. } => self.side(vault, oid, &self.rel)?,
        };
        Ok((left, right))
    }

    /// The path the left side is read at: the old one, where a commit or the index renamed the
    /// file.
    fn left_rel(&self) -> &str {
        match &self.sides {
            Sides::Staged { orig: Some(orig) }
            | Sides::Commit {
                orig: Some(orig), ..
            } => orig,
            _ => &self.rel,
        }
    }

    /// `rel` at `rev`. A file git has none of there is a new or deleted file, and an empty string
    /// is exactly the right thing to diff against. A read that failed is not that: an empty side
    /// would draw the whole file as added or deleted, so it is the toast instead.
    fn side(&self, vault: &Vault, rev: &str, rel: &str) -> Result<Blob, String> {
        match vault.git_show(&self.repo, rev, rel) {
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
    fn worktree(&self, vault: &Vault) -> Result<Blob, String> {
        let outside = Path::new(&self.key).is_absolute();
        // Outside the vault on a remote vault is on the other machine, and the path would name
        // this one's file if it named anything: refusing is the only honest answer.
        if outside && vault.is_remote() {
            let name = split_name(&self.rel).1;
            return Err(format!("{name} is outside the vault on the remote host"));
        }
        let read = match outside {
            true => accent_core::fs::read_text(Path::new(&self.key)),
            false => vault.read_text(&self.key),
        };
        Ok(match read {
            // With the line endings the file has, which reading it as text takes away: a change
            // of those alone is a change to git, and [`line_endings_only`] has to see it.
            Ok(accent_api::fs::Read::Text(t)) => {
                Blob::Text(accent_api::fs::for_disk(&t.text, t.crlf, false))
            }
            Ok(_) => Blob::Binary,
            // A file that is no longer there really is a deletion, and an empty right side is
            // what draws one. This used to be every absolute key as well, which drew a file
            // whose repository is above the vault root as wholly deleted.
            Err(e) => {
                tracing::debug!("reading {}: {e}", self.key);
                Blob::Text(String::new())
            }
        })
    }
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
            Sides::Staged { orig: None }.nothing_to_show(),
            "has no staged changes"
        );
        assert_eq!(commit.nothing_to_show(), "is unchanged in this commit");
        // A deleted file's row sits in the same section as a modified one, and says the same.
        assert_eq!(
            Sides::Deleted.nothing_to_show(),
            Sides::Worktree.nothing_to_show()
        );
    }
}
