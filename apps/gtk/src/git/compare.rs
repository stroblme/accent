//! The comparisons a row opens, and keeping the open ones current.

use super::*;

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
                gio::spawn_blocking(move || what.read(&vault)).await
            };
            match read {
                Ok(read) => panel.show(what, read),
                Err(_) => tracing::warn!("the git worker panicked"),
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
        let left_title = format!("{name} ({})", what.sides.left_title());
        let right_title = format!("{name} ({})", what.sides.right_title());
        match what.sides.clone() {
            // The working tree is the file itself, so the comparison lives in its tab and the
            // refresh only ever has the index side to re-read.
            Sides::Worktree => {
                let (panel, key) = (Rc::downgrade(self), what.key.clone());
                let register = move |compare: Weak<Compare>| {
                    if let Some(panel) = panel.upgrade() {
                        panel.watch(what, Target::Tab(compare));
                    }
                };
                (self.hooks.compare_file)(&key, &left_title, &left, Box::new(register));
            }
            Sides::Staged | Sides::Commit { .. } => {
                let key = format!("diff:{}:{}", what.sides.tag(), what.key);
                let tab = (self.hooks.open_diff)(
                    &key,
                    &name,
                    &right_title,
                    (&left_title, &left),
                    (&right_title, &right),
                );
                // A commit never changes; the index does.
                if let (Some(tab), Sides::Staged) = (tab, &what.sides) {
                    self.watch(what, Target::Diff(Rc::downgrade(&tab)));
                }
            }
        }
    }

    /// The working-tree comparison of `key`, for the bench: the vault root is the repository,
    /// so the key is the path git knows.
    pub fn compare_worktree(self: &Rc<Self>, key: &str) {
        self.compare(key, key, Sides::Worktree);
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
                gio::spawn_blocking(move || {
                    whats.iter().map(|w| w.read(&vault)).collect::<Vec<_>>()
                })
                .await
            };
            let Ok(reads) = reads else {
                return tracing::warn!("the git worker panicked");
            };
            for (watch, (left, right)) in watches.into_iter().zip(reads) {
                let (Blob::Text(left), Blob::Text(right)) = (left, right) else {
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
    /// HEAD against the index: what this commit would add.
    Staged,
    /// The index against the file on disk: what is not staged yet.
    Worktree,
    /// One commit against its first parent, which is what a file under an expanded history row
    /// shows. `parent` is `None` on a root commit, whose left side is simply empty.
    Commit { oid: String, parent: Option<String> },
}

impl Sides {
    /// The revision the left pane reads, `None` meaning there is nothing on that side at all.
    fn left_rev(&self) -> Option<&str> {
        match self {
            Sides::Staged => Some("HEAD"),
            Sides::Worktree => Some(""),
            Sides::Commit { parent, .. } => parent.as_deref(),
        }
    }

    fn left_title(&self) -> String {
        match self {
            Sides::Staged => "HEAD".to_string(),
            Sides::Worktree => "Index".to_string(),
            Sides::Commit { parent, .. } => match parent {
                Some(parent) => short(parent),
                None => "Nothing".to_string(),
            },
        }
    }

    fn right_title(&self) -> String {
        match self {
            Sides::Staged => "Index".to_string(),
            Sides::Worktree => "Working Tree".to_string(),
            Sides::Commit { oid, .. } => short(oid),
        }
    }

    /// What keys the tab, so the comparisons of one file are a tab each and asking twice reveals
    /// the one already open.
    fn tag(&self) -> String {
        match self {
            Sides::Staged => "index".to_string(),
            Sides::Worktree => "worktree".to_string(),
            Sides::Commit { oid, .. } => format!("commit:{}", short(oid)),
        }
    }
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
    /// Both sides, on the worker. A side git has no file for is a new or deleted file, and an
    /// empty string is exactly the right thing to diff against.
    fn read(&self, vault: &Vault) -> (Blob, Blob) {
        let left = match self.sides.left_rev() {
            Some(rev) => side(vault.git_show(&self.repo, rev, &self.rel)),
            None => Blob::Text(String::new()),
        };
        let right = match &self.sides {
            Sides::Staged => side(vault.git_show(&self.repo, "", &self.rel)),
            // The working tree side is the file itself, which on a remote vault is on the other
            // machine: reading it through the vault is what makes the diff work there as well
            // as here. It is read even though the tab shows its own buffer, so that the same
            // hop answers "is this binary" for both.
            Sides::Worktree => self.worktree(vault),
            Sides::Commit { oid, .. } => side(vault.git_show(&self.repo, oid, &self.rel)),
        };
        (left, right)
    }

    /// The file on disk, as the working-tree side of a comparison.
    ///
    /// A repository above the vault root gives its files absolute keys ([`vault_key`]), and
    /// `Vault::read_text` refuses those: it resolves through `Local::join`, which rejects a path
    /// with a root component rather than escape the vault. Such a file is read directly instead,
    /// which is right because a key is only absolute when the file is outside the vault — and
    /// impossible on a remote vault, where "outside the vault" is on the other machine and the
    /// diff has to say so rather than diff against nothing.
    fn worktree(&self, vault: &Vault) -> Blob {
        let outside = Path::new(&self.key).is_absolute();
        // Outside the vault on a remote vault is on the other machine, and the path would name
        // this one's file if it named anything: refusing is the only honest answer.
        if outside && vault.is_remote() {
            return Blob::Binary;
        }
        let read = match outside {
            true => accent_core::fs::read_text(Path::new(&self.key)),
            false => vault.read_text(&self.key),
        };
        match read {
            Ok(accent_api::fs::Read::Text(t)) => Blob::Text(t.text),
            Ok(_) => Blob::Binary,
            // A file that is no longer there really is a deletion, and an empty right side is
            // what draws one. This used to be every absolute key as well, which drew a file
            // whose repository is above the vault root as wholly deleted.
            Err(e) => {
                tracing::debug!("reading {}: {e}", self.key);
                Blob::Text(String::new())
            }
        }
    }
}

fn side(read: anyhow::Result<Option<Blob>>) -> Blob {
    match read {
        Ok(blob) => blob.unwrap_or_else(|| Blob::Text(String::new())),
        Err(e) => {
            tracing::debug!("git show: {e:#}");
            Blob::Text(String::new())
        }
    }
}
