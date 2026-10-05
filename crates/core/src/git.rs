//! Git for a vault: what changed, what the history looks like, and staging.
//!
//! The repositories in a vault belong to the user, not to us: they may be signed, hooked,
//! LFS-backed or configured in ways no reimplementation would honour. Driving their own `git`
//! binary is the only way what accent shows can agree with what `git status` shows in their
//! terminal, so every operation here is one subprocess plus a parser over its porcelain output.
//!
//! Everything is synchronous. A `git status` on a cold cache takes long enough to drop frames,
//! so callers run these off the main thread.

mod lanes;
mod proc;

pub use lanes::{lanes, log};
pub use proc::{FETCH_TIMEOUT, TRANSFER_TIMEOUT, interrupt, running, set_askpass};

use std::borrow::Borrow;
use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};

use proc::{bounded, checked, command, network, run};
use serde::{Deserialize, Serialize};

// ----------------------------------------------------------------- data types

/// One repository the vault touches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Repo {
    pub root: PathBuf,
    pub git_dir: PathBuf,
    pub name: String,
}

/// One comparison of a file with git: what the Git pane opens for a row, and what a session
/// keeps of it to open it again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Comparison {
    pub repo: Repo,
    /// Repository-relative, which is what git is asked with.
    pub rel: String,
    /// Vault key, which is what the working tree is read by and the tab is keyed by.
    pub key: String,
    pub sides: Sides,
}

/// Which two things a comparison compares.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Sides {
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
    /// A file git left unmerged, between its current side (`:2`) and its incoming one (`:3`): a
    /// merge, in the file's own tab.
    Merge,
}

/// Where HEAD is and how far it has drifted from its upstream.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Branch {
    /// `None` in a repository with no commits yet.
    pub oid: Option<String>,
    /// `None` when HEAD is detached.
    pub head: Option<String>,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
}

/// One path `git status` had something to say about.
///
/// `x` and `y` are porcelain's two state columns — index and worktree — with `.` for "unchanged"
/// and `?` for untracked, which is how the [`Status`] filters below tell the three lists apart.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Entry {
    pub path: String,
    /// Where a rename or copy came from.
    pub orig: Option<String>,
    pub x: char,
    pub y: char,
    pub unmerged: bool,
    pub submodule: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    pub branch: Branch,
    pub entries: Vec<Entry>,
    /// Ignored paths, as git reports them: a directory an ignore pattern matches is one entry with
    /// a trailing slash rather than a row per file inside it.
    pub ignored: Vec<String>,
    /// A merge stopped part way and is waiting for a commit or an abort.
    pub merging: bool,
    /// A rebase stopped part way — one started in a terminal, since a Sync merges — and is
    /// waiting for a continue or an abort.
    pub rebasing: bool,
}

impl Status {
    /// Paths with a merge still to resolve. Nothing can be committed while this is non-empty.
    pub fn conflicts(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter().filter(|e| e.unmerged)
    }

    /// Paths with something in the index, ready to go into the next commit.
    pub fn staged(&self) -> impl Iterator<Item = &Entry> {
        self.entries
            .iter()
            .filter(|e| !e.unmerged && e.x != '.' && e.x != '?')
    }

    /// Paths changed in the worktree but not staged, plus untracked files.
    pub fn changes(&self) -> impl Iterator<Item = &Entry> {
        self.entries
            .iter()
            .filter(|e| !e.unmerged && (e.y != '.' || e.x == '?'))
    }

    /// Whether the repository has work that is not committed — anything `git status` had
    /// something to say about, untracked files included.
    ///
    /// Deliberately the whole of `entries` rather than a sum of the three lists above: it is the
    /// same set the Git pane's changes list is drawn from, so a dot elsewhere in the window and
    /// that list can never disagree. Ignored paths are their own field and do not count.
    pub fn dirty(&self) -> bool {
        !self.entries.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Commit {
    pub id: String,
    pub parents: Vec<String>,
    /// Decorations: the branches and tags pointing here, HEAD's first (see [`lanes::parse_refs`]).
    pub refs: Vec<Ref>,
    /// The author's name and email as `git log` gives them, `.mailmap` applied.
    pub author: String,
    pub email: String,
    /// Author time, unix seconds.
    pub time: i64,
    pub summary: String,
    /// Everything after the subject and its blank line. Empty for a one-line message.
    pub body: String,
}

/// One decoration on a commit.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Ref {
    /// As git shortens it: `main`, `origin/main`, `v1`, `HEAD` for a detached HEAD, or `stash`.
    pub name: String,
    pub kind: RefKind,
    /// HEAD is here: this is the branch it is on, or HEAD itself where it is detached.
    pub head: bool,
}

/// What a [`Ref`] is, in the order a commit lists them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum RefKind {
    /// A detached HEAD, which names no branch.
    Head,
    LocalBranch,
    RemoteBranch,
    Tag,
    /// `refs/stash`, whose commits `log --all` walks like any other ref's.
    Stash,
}

/// A commit placed on the history graph: which column it sits in, and which columns the edges
/// entering and leaving it occupy. See [`lanes()`] for what the three edge lists mean.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LogRow {
    pub commit: Commit,
    pub column: usize,
    /// Columns whose edge comes down into this commit from the rows above.
    pub above: Vec<usize>,
    /// Columns this commit's parent edges leave in, `below[0]` being its own column.
    pub below: Vec<usize>,
    /// Columns of unrelated branches passing this row untouched.
    pub through: Vec<usize>,
    /// The branch this commit's column draws: the first decoration found on it, going down from
    /// its tip. `None` for a line of history no branch or tag names.
    pub lane: Option<String>,
    /// The named columns that end at this commit besides its own: the branches that forked here.
    pub forks: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Submodule {
    pub path: String,
    pub oid: String,
    /// `git submodule status`' first column: ` ` in sync, `-` uninitialised, `+` at another
    /// commit than the superproject records, `U` conflicted.
    pub state: char,
    pub describe: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    /// git ran and refused, with whatever it put on stderr.
    #[error("{0}")]
    Git(String),
}

// ------------------------------------------------------------------- discovery

/// The repository containing `dir`, if any.
pub fn toplevel(dir: &Path) -> Result<Option<Repo>, Error> {
    let out = match run(
        dir,
        &["rev-parse", "--show-toplevel", "--absolute-git-dir"],
        true,
    ) {
        Ok(out) => out,
        Err(Error::Git(msg)) if msg.contains("not a git repository") => return Ok(None),
        Err(e) => return Err(e),
    };
    let text = String::from_utf8_lossy(&out);
    let mut lines = text.lines();
    match (lines.next(), lines.next()) {
        (Some(root), Some(git_dir)) => Ok(Some(repo(PathBuf::from(root), PathBuf::from(git_dir)))),
        _ => Ok(None),
    }
}

/// The repository rooted *at* `dir`, if `dir` is one itself.
pub fn nested(dir: &Path) -> Result<Option<Repo>, Error> {
    // A linked worktree and a submodule both have `.git` as a *file* pointing at the real git
    // directory, so this stats the entry instead of asking whether a directory is there.
    if std::fs::symlink_metadata(dir.join(".git")).is_err() {
        return Ok(None);
    }
    let out = match run(dir, &["rev-parse", "--absolute-git-dir"], true) {
        Ok(out) => out,
        Err(Error::Git(msg)) if msg.contains("not a git repository") => return Ok(None),
        Err(e) => return Err(e),
    };
    let git_dir = String::from_utf8_lossy(&out).trim().to_string();
    if git_dir.is_empty() {
        return Ok(None);
    }
    // `dir` is kept exactly as the index gave it to us and is never canonicalised: a vault reaches
    // repositories through directory symlinks on purpose, and a resolved root would no longer
    // match the paths the rest of the app is holding.
    Ok(Some(repo(dir.to_path_buf(), PathBuf::from(git_dir))))
}

/// Every repository the vault touches: the one holding `vault_root`, then each indexed directory
/// that is a repository in its own right.
///
/// Errors are swallowed on purpose. Discovery runs whenever a vault opens, and a directory that
/// vanished mid-scan or a repository git dislikes must cost the user a missing row, not a dialog.
pub fn discover(vault_root: &Path, dirs: &[PathBuf]) -> Vec<Repo> {
    let top = match toplevel(vault_root) {
        Ok(top) => top,
        // No git binary at all: the whole feature is simply absent, and saying so once beats one
        // failed subprocess per indexed directory.
        Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::debug!("no git binary on PATH; the vault has no repositories");
            return Vec::new();
        }
        Err(e) => {
            tracing::debug!("git discovery at the vault root: {e}");
            None
        }
    };

    let mut seen: HashSet<PathBuf> = top.iter().map(|r| r.git_dir.clone()).collect();
    let mut found: Vec<Repo> = Vec::new();
    for dir in dirs {
        match nested(dir) {
            Ok(Some(repo)) if seen.insert(repo.git_dir.clone()) => found.push(repo),
            Ok(_) => {}
            Err(e) => tracing::debug!("git discovery in {}: {e}", dir.display()),
        }
    }
    found.sort_by(|a, b| a.name.cmp(&b.name));
    // The vault's own repository leads: it is the one the user means by "the repository".
    top.into_iter().chain(found).collect()
}

fn repo(root: PathBuf, git_dir: PathBuf) -> Repo {
    let name = match root.file_name() {
        Some(name) => name.to_string_lossy().into_owned(),
        None => root.display().to_string(),
    };
    Repo {
        root,
        git_dir,
        name,
    }
}

// ---------------------------------------------------------------------- status

/// Every untracked file on its own row (`-uall`), as VS Code lists them: git's default makes an
/// untracked folder one `dir/` entry, which says nothing about what is in it and leaves nothing
/// to open or stage one at a time. A nested repository stays one `dir/` entry either way.
///
/// `--ignored=matching` is what keeps that affordable: the default mode under `-uall` lists every
/// file inside an ignored folder, a `target/` or an ignored `node_modules/` included, while this
/// one reports a folder its pattern matches as one `dir/` entry without walking it. A file
/// matched by a file pattern (`*.log`) comes one by one, never as its folder.
pub fn status(repo: &Repo) -> Result<Status, Error> {
    let out = run(
        &repo.root,
        &[
            "status",
            "--porcelain=v2",
            "-z",
            "--branch",
            "--untracked-files=all",
            "--ignored=matching",
        ],
        true,
    )?;
    Ok(Status {
        merging: merging(repo),
        rebasing: rebasing(repo),
        ..parse_status(&out)
    })
}

/// What Discard trashes of the untracked paths under the folder `dir` (`""` for the whole
/// repository): the names git's default mode gives them, so a folder holding nothing tracked
/// goes whole rather than file by file, leaving its emptied folders behind.
///
/// Never a repository of its own, nor a folder holding one anywhere below — an ignored folder
/// included, where no status lists it: such a folder goes file by file instead (`-uall`), and
/// the repository, its history with it, stays. A `.git` file counts as much as a directory,
/// which is what a worktree has.
pub fn untracked(repo: &Repo, dir: &str) -> Result<Vec<String>, Error> {
    let mut found = Vec::new();
    for path in untracked_as(repo, dir, "normal")? {
        match path.ends_with('/') && holds_git(&repo.root.join(&path)) {
            false => found.push(path),
            true => found.extend(
                untracked_as(repo, &path, "all")?
                    .into_iter()
                    .filter(|p| !p.ends_with('/')),
            ),
        }
    }
    Ok(found)
}

/// The untracked paths under `dir` as `--untracked-files=<mode>` names them.
fn untracked_as(repo: &Repo, dir: &str, mode: &str) -> Result<Vec<String>, Error> {
    let mode = format!("--untracked-files={mode}");
    let mut args = vec!["status", "--porcelain=v2", "-z", &mode];
    if !dir.is_empty() {
        args.extend(["--", dir]);
    }
    let out = run(&repo.root, &args, true)?;
    Ok(parse_status(&out)
        .entries
        .into_iter()
        .filter(|e| e.x == '?')
        .map(|e| e.path)
        .collect())
}

/// Whether a `.git` lies in `dir` or anywhere below it. Symlinks are not followed: trashing one
/// takes the link, not what it points at.
fn holds_git(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .any(|e| {
            e.file_name() == ".git"
                || (e.file_type().is_ok_and(|t| t.is_dir()) && holds_git(&e.path()))
        })
}

/// Whether a merge is under way, which porcelain does not say: git keeps `MERGE_HEAD` for exactly
/// as long as one is waiting to be committed or aborted.
fn merging(repo: &Repo) -> bool {
    repo.git_dir.join("MERGE_HEAD").exists()
}

/// Whether a rebase is under way, read the way `git status` reads it: `rebase-merge/` for the
/// default backend, `rebase-apply/` for the `apply` one — unless `applying` is in it, which makes
/// it a `git am`. Both live in the worktree's own git dir, which is what `git_dir` is.
fn rebasing(repo: &Repo) -> bool {
    let apply = repo.git_dir.join("rebase-apply");
    repo.git_dir.join("rebase-merge").exists()
        || (apply.exists() && !apply.join("applying").exists())
}

/// Parse `git status --porcelain=v2 -z --branch` with its `--ignored` records.
///
/// Paths arrive as raw bytes under `-z` (no quoting), so one that is not UTF-8 comes through
/// lossily rather than being dropped: a note the user can see must not go missing from the list.
fn parse_status(bytes: &[u8]) -> Status {
    let mut status = Status::default();
    let mut tokens = bytes.split(|b| *b == 0).filter(|t| !t.is_empty());
    while let Some(token) = tokens.next() {
        let line = String::from_utf8_lossy(token);
        match line.split_at_checked(2) {
            Some(("# ", rest)) => header(rest, &mut status.branch),
            Some(("1 ", _)) => status.entries.extend(entry(&line, 9)),
            Some(("2 ", _)) => {
                if let Some(mut e) = entry(&line, 10) {
                    // The one real trap in this format: under `-z` a rename is *two* tokens, the
                    // record and then the original path on its own. Not consuming it here would
                    // turn every rename into a second, bogus entry.
                    e.orig = tokens
                        .next()
                        .map(|t| String::from_utf8_lossy(t).into_owned());
                    status.entries.push(e);
                }
            }
            Some(("u ", _)) => {
                if let Some(mut e) = entry(&line, 11) {
                    e.unmerged = true;
                    status.entries.push(e);
                }
            }
            Some(("? ", path)) => status.entries.push(Entry {
                path: path.to_string(),
                orig: None,
                x: '?',
                y: '?',
                unmerged: false,
                submodule: false,
            }),
            Some(("! ", path)) => status.ignored.push(path.to_string()),
            // A header we do not know about, or a record type added by a future git.
            _ => {}
        }
    }
    status
}

fn header(rest: &str, branch: &mut Branch) {
    let Some((key, value)) = rest.split_once(' ') else {
        return;
    };
    match key {
        "branch.oid" => branch.oid = (value != "(initial)").then(|| value.to_string()),
        "branch.head" => branch.head = (value != "(detached)").then(|| value.to_string()),
        "branch.upstream" => branch.upstream = Some(value.to_string()),
        "branch.ab" => {
            for field in value.split_whitespace() {
                let Some((sign, count)) = field.split_at_checked(1) else {
                    continue;
                };
                let count = count.parse().unwrap_or(0);
                match sign {
                    "+" => branch.ahead = count,
                    "-" => branch.behind = count,
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

/// One `1`/`2`/`u` record. `fields` counts the space-separated fields the record type has,
/// including its leading letter; the path is the last of them, and `splitn` is what keeps a path
/// containing spaces in one piece.
fn entry(line: &str, fields: usize) -> Option<Entry> {
    let parts: Vec<&str> = line.splitn(fields, ' ').collect();
    if parts.len() < fields {
        return None;
    }
    let mut xy = parts[1].chars();
    Some(Entry {
        path: parts[fields - 1].to_string(),
        orig: None,
        x: xy.next()?,
        y: xy.next()?,
        unmerged: false,
        // `<sub>` is `N...` for a file and `S<c><m><u>` when the entry is a submodule commit.
        submodule: parts[2].starts_with('S'),
    })
}

// ------------------------------------------------------------------ the remote

/// Bring the remote-tracking refs up to date, and nothing else.
///
/// Nothing is merged and nothing in the worktree moves, so this is safe to run on a timer: it is
/// what makes [`Status`]' `behind` count and the upstream-only commits [`log`] lists mean
/// anything at all. Only this repository's configured remotes, because the caller is showing one
/// repository and a vault may hold several.
///
/// Nobody is waiting for it, so it is the shortest-lived of the bounded calls (see
/// [`FETCH_TIMEOUT`]), and a closing window stops it ([`interrupt`]).
pub fn fetch(repo: &Repo) -> Result<String, Error> {
    let cmd = network(&repo.root, &["fetch"], false);
    let out = bounded(cmd, None, FETCH_TIMEOUT, "fetch", Some(&repo.root))?;
    Ok(transcribe(out))
}

/// The commits the upstream has and HEAD does not: exactly what a pull would bring in.
///
/// The oids alone, so a history already on screen can mark the rows a fetch found without asking
/// git about each one. `HEAD..@{upstream}` is git's own spelling for the range, which means this
/// refuses — rather than answers emptily — on a detached HEAD, a branch with no upstream and a
/// repository with no commits. The caller knows which of those it is from [`Branch`] and asks
/// only when `behind` says there is something to list.
pub fn incoming(repo: &Repo) -> Result<Vec<String>, Error> {
    let out = run(&repo.root, &["rev-list", "HEAD..@{upstream}"], true)?;
    Ok(String::from_utf8_lossy(&out)
        .lines()
        .map(str::to_string)
        .collect())
}

// ----------------------------------------------------------------- submodules

/// The submodules of `repo`, as `git submodule status` reports them.
///
/// A repository with no `.gitmodules` has none, and that is a stat rather than a process: the
/// command costs about as much as `git status` (30 ms on a 40 000-file repository) and the Git
/// pane asks on every refresh, so the common case must not pay for it. Where the index may still
/// hold a gitlink — broken, with no `.gitmodules` to say where it comes from, and refused by
/// `git submodule status` for it — the index is asked instead ([`gitlinks`]). So is it where
/// `git submodule status` refuses: one gitlink `.gitmodules` does not map is enough for it to
/// list none, and the index lists them all, the mapped ones without their describe.
pub fn submodules(repo: &Repo) -> Result<Vec<Submodule>, Error> {
    if !repo.root.join(".gitmodules").exists() {
        return match may_hold_gitlink(&repo.git_dir) {
            true => gitlinks(repo),
            false => Ok(Vec::new()),
        };
    }
    let out = match run(&repo.root, &["submodule", "status"], true) {
        Err(Error::Git(e)) => {
            tracing::debug!("git submodule status: {e}");
            return gitlinks(repo);
        }
        out => out?,
    };
    Ok(String::from_utf8_lossy(&out)
        .lines()
        .filter_map(parse_submodule)
        .collect())
}

/// Whether the index may hold a gitlink: its mode, 0o160000, is stored as the four bytes
/// `00 00 e0 00` in every index version, so where they appear nowhere in the index — nor in the
/// shared index a split one keeps the rest of its entries in — there is none. A match elsewhere
/// in an entry costs no more than the `ls-files` it would otherwise spare. For 40 000 files, a
/// 2.8 MB index, this is 1–3 ms of reading, where `ls-files` and its answer take 20.
fn may_hold_gitlink(git_dir: &Path) -> bool {
    let gitlink = [0, 0, 0xe0, 0];
    std::fs::read_dir(git_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| {
            let name = e.file_name();
            name == "index" || name.to_string_lossy().starts_with("sharedindex.")
        })
        .any(|e| {
            std::fs::read(e.path()).is_ok_and(|b| memchr::memmem::find(&b, &gitlink).is_some())
        })
}

/// The gitlinks the index holds, as `git submodule status` lists submodules: ` ` where the folder
/// holds its repository, `-` where nobody has checked it out, `U` while it is conflicted.
fn gitlinks(repo: &Repo) -> Result<Vec<Submodule>, Error> {
    let out = run(&repo.root, &["ls-files", "--stage", "-z"], true)?;
    let mut subs = out
        .split(|b| *b == 0)
        .filter_map(|record| {
            let record = String::from_utf8_lossy(record);
            // `<mode> <oid> <stage>\t<path>`
            let (meta, path) = record.split_once('\t')?;
            let mut fields = meta.split(' ');
            let (mode, oid, stage) = (fields.next()?, fields.next()?, fields.next()?);
            if mode != "160000" {
                return None;
            }
            let state = match (stage, repo.root.join(path).join(".git").exists()) {
                ("0", true) => ' ',
                ("0", false) => '-',
                _ => 'U',
            };
            Some(Submodule {
                path: path.to_string(),
                oid: oid.to_string(),
                state,
                describe: None,
            })
        })
        .collect::<Vec<_>>();
    // A conflicted one is a record per stage.
    subs.dedup_by(|a, b| a.path == b.path);
    Ok(subs)
}

/// `<state><oid> <path>` with an optional ` (<describe>)` tail.
fn parse_submodule(line: &str) -> Option<Submodule> {
    let mut chars = line.chars();
    let state = chars.next()?;
    if !matches!(state, ' ' | '-' | '+' | 'U') {
        return None;
    }
    let (oid, rest) = chars.as_str().split_once(' ')?;
    // A submodule path may contain spaces, so the optional tail is taken off the end.
    let (path, describe) = match rest.strip_suffix(')').and_then(|r| r.rsplit_once(" (")) {
        Some((path, describe)) => (path, Some(describe.to_string())),
        None => (rest, None),
    };
    Some(Submodule {
        path: path.to_string(),
        oid: oid.to_string(),
        state,
        describe,
    })
}

// -------------------------------------------------------------- read and write

/// One file a commit changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangedFile {
    /// git's status letter: `A`, `M`, `D`, `R`, `C` or `T`.
    pub letter: char,
    pub path: String,
    /// Where a rename or copy came from: the path the parent has the file under.
    pub orig: Option<String>,
}

/// What one commit changed, a row per file.
///
/// `-m --first-parent` is what makes a merge answer at all — plain `git show` prints nothing for
/// one, and `-m` alone prints a diff against every parent in turn. A root commit needs no special
/// case: every file in it comes back as `A`.
pub fn changed_files(repo: &Repo, oid: &str) -> Result<Vec<ChangedFile>, Error> {
    let out = run(
        &repo.root,
        &[
            "show",
            "--format=",
            "--name-status",
            "-z",
            "-m",
            "--first-parent",
            oid,
        ],
        true,
    )?;
    Ok(parse_name_status(&out))
}

/// Parse `--name-status -z`: a status token, then its path — except a rename or a copy, whose
/// token is followed by *two* paths, the old one first. That is the same trap [`parse_status`]
/// handles for porcelain records, and it gets the same answer: the new path is the one the row is
/// about, and the old one is where the parent's side of it is read.
fn parse_name_status(bytes: &[u8]) -> Vec<ChangedFile> {
    let lossy = |bytes: &[u8]| String::from_utf8_lossy(bytes).into_owned();
    let mut files = Vec::new();
    let mut tokens = bytes.split(|b| *b == 0).filter(|t| !t.is_empty());
    while let Some(token) = tokens.next() {
        let Some(letter) = String::from_utf8_lossy(token).chars().next() else {
            continue;
        };
        let Some(path) = tokens.next() else {
            break;
        };
        let (path, orig) = match letter {
            'R' | 'C' => match tokens.next() {
                Some(new) => (new, Some(lossy(path))),
                None => (path, None),
            },
            _ => (path, None),
        };
        files.push(ChangedFile {
            letter,
            path: lossy(path),
            orig,
        });
    }
    files
}

/// One version of a file, as a diff can use it.
///
/// Text or not: every caller decides that first and shows a message instead of a screen of
/// noise, so the test belongs where the bytes are rather than in each of them. It is the same
/// NUL test `grep` and `git` use, and it is what lets this cross a wire as a string.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Blob {
    Text(String),
    Binary,
}

impl Blob {
    fn of(bytes: &[u8]) -> Blob {
        match bytes.contains(&0) {
            true => Blob::Binary,
            false => Blob::Text(String::from_utf8_lossy(bytes).into_owned()),
        }
    }

    /// The text, or "" for a binary: a caller that has already said its piece about binaries can
    /// carry on without a second match.
    pub fn text(&self) -> &str {
        match self {
            Blob::Text(t) => t,
            Blob::Binary => "",
        }
    }
}

/// The bytes of `path` at `rev`, or `None` when that revision has no such file.
///
/// An empty `rev` means the index, which is what the diff view compares a staged change against.
/// A file that is new — untracked, or added but not yet committed — is a normal answer here, not
/// an error, so the four ways git words "it isn't there" all become `Ok(None)`, and so does HEAD
/// before the first commit, which has no files at all.
///
/// A revision is peeled to its commit first. git words a full object name it does not have the
/// same way as a path the commit lacks, so a commit asked of the wrong repository would read as
/// a file that is not there; peeled, it is "invalid object name", which stays an error.
///
/// `:1`, `:2` and `:3` are an unmerged file's base, ours and theirs, and a stage the conflict
/// has none of — no base where both sides added the file, no theirs where they deleted it — is
/// `None` too.
pub fn show(repo: &Repo, rev: &str, path: &str) -> Result<Option<Blob>, Error> {
    let stage = matches!(rev, ":1" | ":2" | ":3");
    let object = match rev {
        "" => format!(":{path}"),
        _ if stage => format!("{rev}:{path}"),
        rev => format!("{rev}^{{commit}}:{path}"),
    };
    match run(&repo.root, &["show", &object], true) {
        Ok(bytes) => Ok(Some(Blob::of(&bytes))),
        Err(Error::Git(msg))
            if msg.contains("does not exist")
                || msg.contains("exists on disk, but not in")
                || (stage && msg.contains("but not at stage")) =>
        {
            Ok(None)
        }
        Err(Error::Git(msg)) if rev == "HEAD" && msg.contains("invalid object name") => Ok(None),
        Err(e) => Err(e),
    }
}

/// A repository's branches, each list alphabetical as `git branch -a` would give it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Branches {
    pub local: Vec<String>,
    /// Remote-tracking branches, `origin/main` and so on, as the last fetch left them.
    pub remote: Vec<String>,
    /// The local branch HEAD is on; `None` when it is detached or its branch has no commit yet.
    /// [`Status`] says the same, and this is what the pane falls back on when a status did not
    /// come back.
    pub head: Option<String>,
}

/// The repository's local and remote-tracking branches, in one `for-each-ref`.
pub fn branches(repo: &Repo) -> Result<Branches, Error> {
    let out = run(
        &repo.root,
        &[
            "for-each-ref",
            "--format=%(refname)%00%(symref)%00%(HEAD)",
            "refs/heads/",
            "refs/remotes/",
        ],
        true,
    )?;
    Ok(parse_branches(&out))
}

/// Parse `<refname> NUL <symref> NUL <HEAD>` lines. Whole ref names rather than `refname:short`,
/// which is the only way a local branch called `origin/x` stays apart from the remote one. A
/// symbolic ref — `origin/HEAD`, which a clone sets — names another branch rather than being one,
/// so it goes. `%(HEAD)` is `*` on the branch HEAD is on and a space everywhere else.
fn parse_branches(bytes: &[u8]) -> Branches {
    let mut branches = Branches::default();
    for line in String::from_utf8_lossy(bytes).lines() {
        let mut fields = line.split('\0');
        let (Some(name), Some("")) = (fields.next(), fields.next()) else {
            continue;
        };
        if let Some(local) = name.strip_prefix("refs/heads/") {
            if fields.next() == Some("*") {
                branches.head = Some(local.to_string());
            }
            branches.local.push(local.to_string());
        } else if let Some(remote) = name.strip_prefix("refs/remotes/") {
            branches.remote.push(remote.to_string());
        }
    }
    branches
}

/// Move HEAD to a local branch.
///
/// `switch` and not `checkout`: it takes branches alone, so a name that also happens to be a file
/// or a tag cannot quietly detach HEAD instead. Whether the switch is safe is git's decision, not
/// ours — it refuses where the working tree would be clobbered, and that refusal is the answer.
pub fn checkout(repo: &Repo, branch: &str) -> Result<(), Error> {
    switch(repo, &["switch", "--", branch])
}

/// Make a local branch of a remote-tracking one (`origin/x`) and move HEAD to it, the way
/// `git switch --track` does: git names it after the remote branch and sets it as the upstream.
/// A local branch of that name already there is git's refusal, like every other.
pub fn track(repo: &Repo, remote: &str) -> Result<(), Error> {
    switch(repo, &["switch", "--track", "--", remote])
}

/// Move HEAD onto one commit, detached, which is how a past state is looked at without a branch
/// being moved. Git refuses this too where the working tree would be clobbered.
pub fn checkout_commit(repo: &Repo, oid: &str) -> Result<(), Error> {
    switch(repo, &["switch", "--detach", oid])
}

/// Create `name` at HEAD, checking it out as it is created when `checkout` is set, which is what
/// `git switch -c` does. The name is git's to validate: both spellings refuse one that is not a
/// legal ref, and their refusal is the answer.
pub fn create_branch(repo: &Repo, name: &str, checkout: bool) -> Result<(), Error> {
    let args: &[&str] = match checkout {
        true => &["switch", "-c"],
        false => &["branch", "--"],
    };
    switch(repo, &[args, &[name]].concat())
}

/// `typed` as a branch name git takes, made the way VS Code makes one: whatever `git
/// check-ref-format --branch` refuses — whitespace and control characters, `~ ^ : ? * [ \`, `..`,
/// `@{`, an empty component, one starting with `.` or ending in `.lock`, a trailing `.` — becomes
/// a dash, a run of dashes one dash, and the name neither starts nor ends with one. Empty where
/// nothing in `typed` can name a branch.
pub fn branch_name(typed: &str) -> String {
    // One pass can uncover another refusal (`a.-` loses its dash and ends in a dot), so it runs
    // until nothing moves. Each change shortens the name or turns a character into a dash, and
    // none undoes either, so it ends.
    let mut name = typed.to_string();
    loop {
        let next = branch_name_pass(&name);
        if next == name {
            return name;
        }
        name = next;
    }
}

fn branch_name_pass(name: &str) -> String {
    let dashed: String = name
        .chars()
        .map(|c| match c {
            '~' | '^' | ':' | '?' | '*' | '[' | '\\' => '-',
            c if c.is_whitespace() || c.is_control() => '-',
            c => c,
        })
        .collect();
    let dashed = dashed.replace("..", "-").replace("@{", "-{");
    let mut joined = dashed
        .split('/')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let part = match part.strip_prefix('.') {
                Some(rest) => format!("-{rest}"),
                None => part.to_string(),
            };
            match part.strip_suffix(".lock") {
                Some(stem) => format!("{stem}-lock"),
                None => part,
            }
        })
        .collect::<Vec<_>>()
        .join("/");
    if joined.ends_with('.') || joined == "@" {
        joined.pop();
        joined.push('-');
    }
    let mut out = String::with_capacity(joined.len());
    for c in joined.chars() {
        if !(c == '-' && out.ends_with('-')) {
            out.push(c);
        }
    }
    out.trim_matches('-').to_string()
}

/// One of the commands above, bounded like the commit: checking files out runs the user's own
/// programs too, a `post-checkout` hook and smudge filters such as LFS's, which download.
fn switch(repo: &Repo, args: &[&str]) -> Result<(), Error> {
    let cmd = command(&repo.root, args, false);
    bounded(cmd, None, TRANSFER_TIMEOUT, args[0], None)?;
    Ok(())
}

/// Delete a local branch. `force` is `-D`, which deletes one whose commits are not merged
/// anywhere; without it git refuses that case and [`unmerged`] recognises the refusal.
pub fn delete_branch(repo: &Repo, name: &str, force: bool) -> Result<(), Error> {
    let flag = match force {
        true => "-D",
        false => "-d",
    };
    run(&repo.root, &["branch", flag, "--", name], false)?;
    Ok(())
}

/// Whether git refused a delete because the branch is not fully merged, which is the one refusal
/// worth offering to force. A string test rather than an [`Error`] variant on purpose: the RPC
/// boundary flattens every git error into its message, so a variant would stop recognising it on
/// a remote vault.
pub fn unmerged(message: &str) -> bool {
    message.contains("not fully merged")
}

/// What a merge did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Merge {
    /// HEAD already had everything the branch has.
    UpToDate,
    /// HEAD moved onto the branch and no commit was made.
    FastForward,
    /// A merge commit joins the two.
    Commit,
    /// The merge stopped on these paths and waits for a commit or an abort.
    Conflicts(Vec<String>),
}

/// Merge a branch into HEAD, the way `git merge` would in a terminal.
///
/// No `--ff` or `--no-ff`: git's default applies, and so does the user's own `merge.ff`. Whether
/// the working tree allows a merge is git's call too. The answer is read off the repository
/// afterwards rather than off git's wording, which would also have to be read off stdout, where
/// git reports a conflict.
///
/// Bounded like [`commit`], because a merge commit runs the same hooks.
pub fn merge(repo: &Repo, branch: &str) -> Result<Merge, Error> {
    let (before, under_way) = (rev(repo, "HEAD"), merging(repo));
    let cmd = command(&repo.root, &["merge", "--no-edit", "--", branch], false);
    if let Err(e) = bounded(cmd, None, TRANSFER_TIMEOUT, "merge", None) {
        // Stopped part way rather than refused: the conflicts are what is left to do. A merge
        // left waiting with none — a hook that refused its commit — has only git's words to say.
        if under_way || !merging(repo) {
            return Err(e);
        }
        let conflicts: Vec<String> = status(repo)?
            .conflicts()
            .map(|entry| entry.path.clone())
            .collect();
        return match conflicts.is_empty() {
            true => Err(e),
            false => Ok(Merge::Conflicts(conflicts)),
        };
    }
    // Compared with the branch rather than by counting HEAD's parents: a fast-forward onto a
    // branch whose tip is itself a merge commit would count two as well.
    let after = rev(repo, "HEAD");
    Ok(if after == before {
        Merge::UpToDate
    } else if after == rev(repo, branch) {
        Merge::FastForward
    } else {
        Merge::Commit
    })
}

/// Give up the merge under way and put the repository back where it was before it started.
pub fn merge_abort(repo: &Repo) -> Result<(), Error> {
    run(&repo.root, &["merge", "--abort"], false)?;
    Ok(())
}

/// How far a `git rebase --continue` got.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Rebase {
    /// Every commit is replayed and the branch is back.
    Done,
    /// It stopped again before the end: on these conflicts of the next commit, or on none where
    /// the rebase itself asked to stop (an `edit` in an interactive one).
    Stopped(Vec<String>),
}

/// Carry on with the rebase under way, once its conflicts are resolved and staged. Each commit
/// keeps its own message: `GIT_EDITOR=true` takes the one git offers where it would open an editor.
///
/// Bounded like [`merge`], and read off the repository the same way: a stop on the next commit's
/// conflicts is where the rebase is, not a refusal.
pub fn rebase_continue(repo: &Repo) -> Result<Rebase, Error> {
    let mut cmd = command(&repo.root, &["rebase", "--continue"], false);
    cmd.env("GIT_EDITOR", "true");
    let done = bounded(cmd, None, TRANSFER_TIMEOUT, "rebase", None);
    if !rebasing(repo) {
        return done.map(|_| Rebase::Done);
    }
    let conflicts: Vec<String> = status(repo)?
        .conflicts()
        .map(|entry| entry.path.clone())
        .collect();
    match (done, conflicts.is_empty()) {
        (Err(e), true) => Err(e),
        _ => Ok(Rebase::Stopped(conflicts)),
    }
}

/// Give up the rebase under way and put the branch back where it was before it started.
pub fn rebase_abort(repo: &Repo) -> Result<(), Error> {
    run(&repo.root, &["rebase", "--abort"], false)?;
    Ok(())
}

/// The commit `spec` names, or `None` where it names none — an unborn HEAD, a missing branch.
fn rev(repo: &Repo, spec: &str) -> Option<String> {
    let out = run(
        &repo.root,
        &["rev-parse", "--verify", "--quiet", spec],
        true,
    )
    .ok()?;
    Some(String::from_utf8_lossy(&out).trim().to_string())
}

pub fn stage(repo: &Repo, paths: &[impl Borrow<str>]) -> Result<(), Error> {
    write(repo, &["add"], paths)
}

/// Take paths back out of the index.
///
/// `restore --staged` restores them from HEAD, so a repository whose first commit has not been
/// made yet needs the other spelling: with nothing to restore from, every index entry is an
/// addition and removing it is exactly "unstage". `--cached` touches nothing on disk, `-r` lets a
/// directory row be unstaged as one path, and `-f` is load-bearing — the safety check refuses a
/// path whose index content differs from both the worktree and HEAD, and with no HEAD that would
/// be any file edited after it was staged.
pub fn unstage(repo: &Repo, paths: &[impl Borrow<str>]) -> Result<(), Error> {
    match unborn(repo) {
        true => write(repo, &["rm", "--cached", "-r", "-f", "-q"], paths),
        false => write(repo, &["restore", "--staged"], paths),
    }
}

/// Whether HEAD points at a branch that has no commit yet. `--quiet` makes git exit non-zero
/// without a message rather than complain on stderr.
fn unborn(repo: &Repo) -> bool {
    run(
        &repo.root,
        &["rev-parse", "--verify", "--quiet", "HEAD"],
        true,
    )
    .is_err()
}

pub fn discard(repo: &Repo, paths: &[impl Borrow<str>]) -> Result<(), Error> {
    write(repo, &["restore", "--worktree"], paths)
}

/// Make `text` what the index holds for `path`, the file itself untouched: Stage Selected Lines
/// and Unstage Selected Lines, whose text is the index's with some lines of the other side's
/// taken over (`diff::apply_lines`).
///
/// A new blob and an index entry pointing at it, as VS Code stages a range, rather than a patch
/// for `git apply --cached`: the text is already worked out, and a patch would only have git work
/// it out again from hunk headers that must agree with the index to the line. `text` has `\n`
/// endings, as a buffer holds it, so the file's own go back on first; `--path` then runs the
/// attributes' clean filters and line-ending conversion over it, which is what `git add` does to
/// the file itself. The entry keeps its mode, and a path the index does not hold yet becomes a
/// plain file. A conflicted path is refused: writing it would resolve the conflict with whatever
/// the text holds.
///
/// Bounded like the commit, and for the same reason: a clean filter is one of the user's own
/// programs.
pub fn stage_text(repo: &Repo, path: &str, text: &str) -> Result<(), Error> {
    // `<mode> <oid> <stage>\t<path>`, a line per stage; `:(literal)` keeps a `[` in a name from
    // matching other files.
    let spec = format!(":(literal){path}");
    let entry = run(&repo.root, &["ls-files", "--stage", "--", &spec], true)?;
    let entry = String::from_utf8_lossy(&entry);
    let mut fields = entry.split_whitespace();
    let (mode, stage) = (
        fields.next().unwrap_or("100644"),
        fields.nth(1).unwrap_or("0"),
    );
    if stage != "0" {
        return Err(Error::Git(format!("{path} has a merge conflict")));
    }
    let crlf =
        std::fs::read(repo.root.join(path)).is_ok_and(|b| b.windows(2).any(|w| w == b"\r\n"));
    let text = match crlf {
        true => text.replace('\n', "\r\n"),
        false => text.to_string(),
    };
    let cmd = command(
        &repo.root,
        &["hash-object", "-w", "--stdin", &format!("--path={path}")],
        false,
    );
    let out = bounded(cmd, Some(text.as_bytes()), TRANSFER_TIMEOUT, "stage", None)?;
    let oid = String::from_utf8_lossy(&out.stdout);
    let info = format!("{mode},{},{path}", oid.trim());
    run(
        &repo.root,
        &["update-index", "--add", "--cacheinfo", &info],
        false,
    )?;
    Ok(())
}

/// The paths go in over stdin, NUL-separated, rather than on the command line, which the kernel
/// caps at 2 MB: a Stage All over an untracked tree of 60 000 files was refused before git ran.
/// Nor can a note called `-f` be read as an option there. Borrowed or owned strings alike, so a
/// caller holding `String`s, as the rpc server does, passes them as they are.
fn write(repo: &Repo, verb: &[&str], paths: &[impl Borrow<str>]) -> Result<(), Error> {
    let args = [verb, &["--pathspec-from-file=-", "--pathspec-file-nul"]].concat();
    let mut child = command(&repo.root, &args, false)
        .stdin(Stdio::piped())
        .spawn()?;
    let (pipe, list) = (child.stdin.take(), paths.join("\0"));
    // On a thread, so that git's output is read while the list goes in: either pipe may fill.
    // The pipe closing as the thread ends is what tells git the list is complete.
    std::thread::spawn(move || pipe.map(|mut pipe| pipe.write_all(list.as_bytes())));
    checked(child.wait_with_output()?)?;
    Ok(())
}

/// Commit what is staged, returning the short id of the new commit.
///
/// The message goes in over stdin rather than as an argument: a note's commit message is written
/// in a text box and may be of any length and contain anything.
///
/// `all` is `git commit -a`, which is what the pane sends when nothing is staged: every tracked
/// file's change goes in, deletions included, and an untracked file stays untracked. Deliberately
/// not `git add -A`, which would sweep up whatever the user has not decided about yet.
///
/// An empty message is git's own, which only a merge under way has: `MERGE_MSG`, with the
/// commented `# Conflicts:` list stripped the way an editor session would strip it. Anywhere else
/// git refuses it as it refuses an empty message in a terminal.
///
/// Bounded like the transfers, and for the same reason: a `pre-commit` hook is one of the user's
/// own programs, and one that never returns would hold the thread — and the pane's Commit button
/// — for the life of the process.
pub fn commit(repo: &Repo, message: &str, all: bool) -> Result<String, Error> {
    let args: &[&str] = match all {
        true => &["commit", "-a"],
        false => &["commit"],
    };
    let (from, stdin): (&[&str], _) = match message.trim().is_empty() {
        true => (&["--no-edit", "--cleanup=strip"], None),
        false => (&["-F", "-"], Some(message.as_bytes())),
    };
    let cmd = command(&repo.root, &[args, from].concat(), false);
    bounded(cmd, stdin, TRANSFER_TIMEOUT, "commit", None)?;
    let out = run(&repo.root, &["rev-parse", "--short", "HEAD"], true)?;
    Ok(String::from_utf8_lossy(&out).trim().to_string())
}

/// A Sync's second half. A push can be stopped part way ([`interrupt`]): the remote takes it
/// whole or not at all.
///
/// A branch that has never been pushed has no ref to push onto, so there the push is publishing
/// it.
pub fn push(repo: &Repo) -> Result<String, Error> {
    if upstream(repo).is_none() {
        return publish(repo);
    }
    transcript(repo, &["push"], true)
}

/// A Sync's first half. A pull cannot be stopped: its merge rewrites the working tree.
///
/// It merges, which was git's own default until it began refusing a diverged branch that
/// `pull.rebase` and `pull.ff` say nothing about. A merge never rewrites a commit, and a conflict
/// stops where the pane already shows one. `--no-rebase` outranks either setting where the user
/// has one, `pull.ff = only` included: a Sync always merges.
///
/// Both halves run every time: the `behind` count a background [`fetch`] keeps current is a
/// readout and not a decision — it can be a whole fetch interval old — and the pull is also what
/// keeps the push from landing on a history the remote has moved past. A branch with no upstream
/// has nothing to pull from, so there this does nothing and the push publishes it.
///
/// Two calls rather than one, so the window knows which half it is in: closing during the pull
/// waits for git, closing during the push stops it (DESIGN.md, States).
pub fn pull(repo: &Repo) -> Result<String, Error> {
    if upstream(repo).is_none() {
        return Ok(String::new());
    }
    transcript(repo, &["pull", "--no-rebase"], false)
}

/// Where the current branch's upstream is, if it has one. `None` covers a detached HEAD and a
/// repository with no commits as well: neither has an upstream to sync with.
fn upstream(repo: &Repo) -> Option<String> {
    let out = run(
        &repo.root,
        &[
            "rev-parse",
            "--abbrev-ref",
            "--symbolic-full-name",
            "@{upstream}",
        ],
        true,
    )
    .ok()?;
    let name = String::from_utf8_lossy(&out).trim().to_string();
    (!name.is_empty()).then_some(name)
}

/// The remote a branch with no upstream should be published to, decided the way git itself decides
/// it: the only remote when there is exactly one, and `origin` when there are several.
///
/// Several remotes and no `origin` is a choice, not a default, so this refuses and names them
/// rather than picking one — pushing a branch to the wrong host is not something an automatic
/// guess should be allowed to do.
fn default_remote(repo: &Repo) -> Result<String, Error> {
    let out = run(&repo.root, &["remote"], true)?;
    let text = String::from_utf8_lossy(&out);
    let remotes: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .collect();
    match remotes.as_slice() {
        [] => Err(Error::Git(
            "this repository has no remote, so there is nowhere to publish the branch".to_string(),
        )),
        [only] => Ok(only.to_string()),
        many if many.contains(&"origin") => Ok("origin".to_string()),
        many => Err(Error::Git(format!(
            "this branch has no upstream and there are several remotes ({}) with no `origin`: \
             push it to the one you mean with `git push -u <remote> HEAD`",
            many.join(", ")
        ))),
    }
}

/// Push a branch that has no upstream and record the remote copy as its upstream, so that the
/// next sync is an ordinary pull and push. VS Code calls this Publish Branch and reaches it from
/// the same control.
///
/// `HEAD` rather than the branch name so nothing here has to parse one: git resolves it to the
/// branch it is on, and refuses if it is on none.
fn publish(repo: &Repo) -> Result<String, Error> {
    let remote = default_remote(repo)?;
    transcript(repo, &["push", "--set-upstream", &remote, "HEAD"], true)
}

/// There is nothing worth parsing in what a transfer prints, and plenty worth reading, so the UI
/// gets the transcript as the terminal would show it — including stderr, where git puts the ref
/// summary that says what actually moved.
///
/// Bounded by [`TRANSFER_TIMEOUT`]: this is where a pull that never answers would otherwise leave
/// the Sync button insensitive for good. `stoppable` is whether a closing window may cut it off.
///
/// Every transfer through here was asked for by hand, so this is the one place that may ask for a
/// passphrase (see [`network`]).
fn transcript(repo: &Repo, args: &[&str], stoppable: bool) -> Result<String, Error> {
    let cmd = network(&repo.root, args, true);
    let what = args.first().copied().unwrap_or("transfer");
    let root = stoppable.then_some(repo.root.as_path());
    let out = bounded(cmd, None, TRANSFER_TIMEOUT, what, root)?;
    Ok(transcribe(out))
}

/// Both streams in the order a terminal would have shown them.
fn transcribe(out: Output) -> String {
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    text.trim().to_string()
}

#[cfg(test)]
mod tests;
