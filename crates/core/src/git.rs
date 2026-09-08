//! Git for a vault: what changed, what the history looks like, and staging.
//!
//! The repositories in a vault belong to the user, not to us: they may be signed, hooked,
//! LFS-backed or configured in ways no reimplementation would honour. Driving their own `git`
//! binary is the only way what accent shows can agree with what `git status` shows in their
//! terminal, so every operation here is one subprocess plus a parser over its porcelain output.
//!
//! Everything is synchronous. A `git status` on a cold cache takes long enough to drop frames,
//! so callers run these off the main thread.

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde::{Deserialize, Serialize};

// ----------------------------------------------------------------- data types

/// One repository the vault touches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Repo {
    pub root: PathBuf,
    pub git_dir: PathBuf,
    pub name: String,
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    /// Ignored paths, as git reports them: a wholly ignored directory is one entry with a
    /// trailing slash rather than a row per file inside it.
    pub ignored: Vec<String>,
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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Commit {
    pub id: String,
    pub parents: Vec<String>,
    /// Decorations: branch and tag names pointing here.
    pub refs: Vec<String>,
    pub author: String,
    /// Author time, unix seconds.
    pub time: i64,
    pub summary: String,
    /// Everything after the subject and its blank line. Empty for a one-line message.
    pub body: String,
}

/// A commit placed on the history graph: which column it sits in, and which columns the edges
/// entering and leaving it occupy. See [`lanes`] for what the three edge lists mean.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogRow {
    pub commit: Commit,
    pub column: usize,
    /// Columns whose edge comes down into this commit from the rows above.
    pub above: Vec<usize>,
    /// Columns this commit's parent edges leave in, `below[0]` being its own column.
    pub below: Vec<usize>,
    /// Columns of unrelated branches passing this row untouched.
    pub through: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

// ------------------------------------------------------------------ subprocess

/// Run one `git` command in `root` and hand back its stdout.
///
/// `readonly` is not a hint: it sets `GIT_OPTIONAL_LOCKS=0`, and without it `git status` refreshes
/// and rewrites `.git/index`. The GTK layer watches the repository, sees that write, asks for a
/// status to explain it, and the two chase each other forever. Every query below passes `true`.
fn run(root: &Path, args: &[&str], stdin: Option<&[u8]>, readonly: bool) -> Result<Vec<u8>, Error> {
    Ok(output(root, args, stdin, readonly)?.stdout)
}

/// [`run`] for the two callers that want stderr as well.
fn output(
    root: &Path,
    args: &[&str],
    stdin: Option<&[u8]>,
    readonly: bool,
) -> Result<Output, Error> {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(root)
        .args(["-c", "color.ui=never"])
        .args(args)
        // Nothing here can answer a prompt, so a repository needing a password must fail rather
        // than hang; `LC_ALL=C` is what lets the callers below match on git's own wording.
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if readonly {
        cmd.env("GIT_OPTIONAL_LOCKS", "0");
    }

    let out = match stdin {
        Some(bytes) => {
            let mut child = cmd.stdin(Stdio::piped()).spawn()?;
            // Dropping the handle at the end of the statement closes the pipe, which is what
            // tells git the message is complete.
            child
                .stdin
                .take()
                .expect("stdin is piped")
                .write_all(bytes)?;
            child.wait_with_output()?
        }
        None => cmd.stdin(Stdio::null()).output()?,
    };
    if !out.status.success() {
        return Err(Error::Git(
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        ));
    }
    Ok(out)
}

// ------------------------------------------------------------------- discovery

/// The repository containing `dir`, if any.
pub fn toplevel(dir: &Path) -> Result<Option<Repo>, Error> {
    let out = match run(
        dir,
        &["rev-parse", "--show-toplevel", "--absolute-git-dir"],
        None,
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
    let out = match run(dir, &["rev-parse", "--absolute-git-dir"], None, true) {
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

pub fn status(repo: &Repo) -> Result<Status, Error> {
    let out = run(
        &repo.root,
        &["status", "--porcelain=v2", "-z", "--branch", "--ignored"],
        None,
        true,
    )?;
    Ok(parse_status(&out))
}

/// Parse `git status --porcelain=v2 -z --branch --ignored`.
///
/// Paths arrive as raw bytes under `-z` (no quoting), so one that is not UTF-8 comes through
/// lossily rather than being dropped: a note the user can see must not go missing from the list.
pub fn parse_status(bytes: &[u8]) -> Status {
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

// ------------------------------------------------------------------- history

/// One page of history across every ref, children before parents.
///
/// A repository with no commits is not a special case: `--all` simply has no refs to walk and git
/// exits cleanly with no output.
pub fn log(repo: &Repo, skip: usize, n: usize) -> Result<Vec<Commit>, Error> {
    let (skip, n) = (skip.to_string(), n.to_string());
    let out = run(
        &repo.root,
        &[
            "log",
            "--all",
            "--topo-order",
            "-n",
            &n,
            "--skip",
            &skip,
            // Unit and record separators: a summary line can hold anything else, including tabs,
            // and a body holds newlines, so the record separator has to be neither.
            "--format=%H%x1f%P%x1f%D%x1f%an%x1f%at%x1f%s%x1f%b%x1e",
        ],
        None,
        true,
    )?;
    Ok(parse_log(&out))
}

pub fn parse_log(bytes: &[u8]) -> Vec<Commit> {
    String::from_utf8_lossy(bytes)
        .split('\x1e')
        // git ends every record with a newline of its own, which lands before the next record.
        .map(str::trim)
        .filter(|record| !record.is_empty())
        .filter_map(|record| {
            let mut fields = record.split('\x1f');
            let id = fields.next()?.to_string();
            let parents = fields
                .next()?
                .split_whitespace()
                .map(String::from)
                .collect();
            let refs = fields.next()?;
            Some(Commit {
                id,
                parents,
                refs: match refs.is_empty() {
                    true => Vec::new(),
                    false => refs.split(", ").map(String::from).collect(),
                },
                author: fields.next()?.to_string(),
                time: fields.next()?.parse().unwrap_or(0),
                summary: fields.next().unwrap_or_default().to_string(),
                // Last, so its newlines are the record's own trailing whitespace and the trim
                // above has already taken them.
                body: fields.next().unwrap_or_default().to_string(),
            })
        })
        .collect()
}

/// Lay commits out on a graph, one column per line of history.
///
/// The state is one slot per column holding the oid that column is *waiting* for. A commit claims
/// the column already waiting for it (or a free one if nothing is), its parents claim columns for
/// the rows below, and everything else passes straight through. Because `--topo-order` guarantees
/// children come before parents, one forward pass is enough — no lookahead, no second walk.
///
/// Freed columns are reused, so a history of two branches never drifts rightwards: the width of
/// the drawing stays the number of lines of history actually open at that row.
pub fn lanes(commits: Vec<Commit>) -> Vec<LogRow> {
    let mut lanes: Vec<Option<String>> = Vec::new();
    let mut rows = Vec::with_capacity(commits.len());

    for commit in commits {
        let above: Vec<usize> = waiting_for(&lanes, &commit.id).collect();
        let column = match above.first() {
            Some(&column) => column,
            None => free_slot(&mut lanes),
        };
        for &i in &above {
            lanes[i] = None;
        }

        let through: Vec<usize> = (0..lanes.len())
            .filter(|&i| i != column && lanes[i].is_some())
            .collect();

        let mut below = Vec::with_capacity(commit.parents.len());
        for (nth, parent) in commit.parents.iter().enumerate() {
            // The first parent stays in this commit's own column; the others join a column
            // already waiting for them, or open one.
            let waiting = waiting_for(&lanes, parent).next();
            let i = match (nth, waiting) {
                (0, _) => column,
                (_, Some(i)) => i,
                (_, None) => free_slot(&mut lanes),
            };
            lanes[i] = Some(parent.clone());
            below.push(i);
        }

        while lanes.last().is_some_and(Option::is_none) {
            lanes.pop();
        }
        rows.push(LogRow {
            commit,
            column,
            above,
            below,
            through,
        });
    }
    rows
}

fn waiting_for<'a>(lanes: &'a [Option<String>], oid: &'a str) -> impl Iterator<Item = usize> + 'a {
    lanes
        .iter()
        .enumerate()
        .filter(move |(_, lane)| lane.as_deref() == Some(oid))
        .map(|(i, _)| i)
}

fn free_slot(lanes: &mut Vec<Option<String>>) -> usize {
    match lanes.iter().position(Option::is_none) {
        Some(i) => i,
        None => {
            lanes.push(None);
            lanes.len() - 1
        }
    }
}

// ----------------------------------------------------------------- submodules

pub fn submodules(repo: &Repo) -> Result<Vec<Submodule>, Error> {
    let out = run(&repo.root, &["submodule", "status"], None, true)?;
    Ok(String::from_utf8_lossy(&out)
        .lines()
        .filter_map(parse_submodule)
        .collect())
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

/// What one commit changed: a status letter and a path per file.
///
/// `-m --first-parent` is what makes a merge answer at all — plain `git show` prints nothing for
/// one, and `-m` alone prints a diff against every parent in turn. A root commit needs no special
/// case: every file in it comes back as `A`.
pub fn changed_files(repo: &Repo, oid: &str) -> Result<Vec<(char, String)>, Error> {
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
        None,
        true,
    )?;
    Ok(parse_name_status(&out))
}

/// Parse `--name-status -z`: a status token, then its path — except a rename or a copy, whose
/// token is followed by *two* paths. That is the same trap [`parse_status`] handles for porcelain
/// records, and it gets the same answer: the new path is the one the row is about.
pub fn parse_name_status(bytes: &[u8]) -> Vec<(char, String)> {
    let mut files = Vec::new();
    let mut tokens = bytes.split(|b| *b == 0).filter(|t| !t.is_empty());
    while let Some(token) = tokens.next() {
        let Some(letter) = String::from_utf8_lossy(token).chars().next() else {
            continue;
        };
        let Some(path) = tokens.next() else {
            break;
        };
        let path = match letter {
            'R' | 'C' => tokens.next().unwrap_or(path),
            _ => path,
        };
        files.push((letter, String::from_utf8_lossy(path).into_owned()));
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
    pub fn of(bytes: &[u8]) -> Blob {
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
/// an error, so the four ways git words "it isn't there" all become `Ok(None)`.
pub fn show(repo: &Repo, rev: &str, path: &str) -> Result<Option<Blob>, Error> {
    match run(&repo.root, &["show", &format!("{rev}:{path}")], None, true) {
        Ok(bytes) => Ok(Some(Blob::of(&bytes))),
        Err(Error::Git(msg))
            if msg.contains("does not exist") || msg.contains("exists on disk, but not in") =>
        {
            Ok(None)
        }
        Err(e) => Err(e),
    }
}

/// The repository's local branches, alphabetically, as `git branch` would list them.
pub fn branches(repo: &Repo) -> Result<Vec<String>, Error> {
    let out = run(
        &repo.root,
        &["for-each-ref", "--format=%(refname:short)", "refs/heads/"],
        None,
        true,
    )?;
    Ok(parse_branches(&out))
}

pub fn parse_branches(bytes: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(bytes)
        .lines()
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

/// Move HEAD to a local branch.
///
/// `switch` and not `checkout`: it takes branches alone, so a name that also happens to be a file
/// or a tag cannot quietly detach HEAD instead. Whether the switch is safe is git's decision, not
/// ours — it refuses where the working tree would be clobbered, and that refusal is the answer.
pub fn checkout(repo: &Repo, branch: &str) -> Result<(), Error> {
    run(&repo.root, &["switch", "--", branch], None, false)?;
    Ok(())
}

/// Move HEAD onto one commit, detached, which is how a past state is looked at without a branch
/// being moved. Git refuses this too where the working tree would be clobbered.
pub fn checkout_commit(repo: &Repo, oid: &str) -> Result<(), Error> {
    run(&repo.root, &["switch", "--detach", oid], None, false)?;
    Ok(())
}

/// Create `name` at HEAD, checking it out as it is created when `checkout` is set, which is what
/// `git switch -c` does. The name is git's to validate: both spellings refuse one that is not a
/// legal ref, and their refusal is the answer.
pub fn create_branch(repo: &Repo, name: &str, checkout: bool) -> Result<(), Error> {
    let args: &[&str] = match checkout {
        true => &["switch", "-c"],
        false => &["branch", "--"],
    };
    run(&repo.root, &[args, &[name]].concat(), None, false)?;
    Ok(())
}

/// Delete a local branch. `force` is `-D`, which deletes one whose commits are not merged
/// anywhere; without it git refuses that case and [`unmerged`] recognises the refusal.
pub fn delete_branch(repo: &Repo, name: &str, force: bool) -> Result<(), Error> {
    let flag = match force {
        true => "-D",
        false => "-d",
    };
    run(&repo.root, &["branch", flag, "--", name], None, false)?;
    Ok(())
}

/// Whether git refused a delete because the branch is not fully merged, which is the one refusal
/// worth offering to force. A string test rather than an [`Error`] variant on purpose: the RPC
/// boundary flattens every git error into its message, so a variant would stop recognising it on
/// a remote vault.
pub fn unmerged(message: &str) -> bool {
    message.contains("not fully merged")
}

pub fn stage(repo: &Repo, paths: &[&str]) -> Result<(), Error> {
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
pub fn unstage(repo: &Repo, paths: &[&str]) -> Result<(), Error> {
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
        None,
        true,
    )
    .is_err()
}

pub fn discard(repo: &Repo, paths: &[&str]) -> Result<(), Error> {
    write(repo, &["restore", "--worktree"], paths)
}

/// `--` keeps a note called `-f`, or one whose name matches a branch, from being read as an option.
fn write(repo: &Repo, verb: &[&str], paths: &[&str]) -> Result<(), Error> {
    let args = [verb, &["--"][..], paths].concat();
    run(&repo.root, &args, None, false)?;
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
pub fn commit(repo: &Repo, message: &str, all: bool) -> Result<String, Error> {
    let args: &[&str] = match all {
        true => &["commit", "-a", "-F", "-"],
        false => &["commit", "-F", "-"],
    };
    run(&repo.root, args, Some(message.as_bytes()), false)?;
    let out = run(&repo.root, &["rev-parse", "--short", "HEAD"], None, true)?;
    Ok(String::from_utf8_lossy(&out).trim().to_string())
}

pub fn push(repo: &Repo) -> Result<String, Error> {
    transcript(repo, "push")
}

pub fn pull(repo: &Repo) -> Result<String, Error> {
    transcript(repo, "pull")
}

/// Pull, then push, as one operation with one transcript.
///
/// Both halves run every time. Nothing in accent fetches on its own, so the `behind` count is
/// only ever as fresh as the last sync and cannot decide whether the pull is worth running. A
/// failed pull stops there — pushing onto a history the remote has moved past would only be
/// refused — and its error is the whole answer.
pub fn sync(repo: &Repo) -> Result<String, Error> {
    let pulled = pull(repo)?;
    let pushed = push(repo)?;
    let both = [pulled, pushed];
    Ok(both
        .iter()
        .filter(|half| !half.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join("\n"))
}

/// There is nothing worth parsing in what a transfer prints, and plenty worth reading, so the UI
/// gets the transcript as the terminal would show it — including stderr, where git puts the ref
/// summary that says what actually moved.
fn transcript(repo: &Repo, verb: &str) -> Result<String, Error> {
    let out = output(&repo.root, &[verb], None, false)?;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    Ok(text.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// These tests drive the real `git` binary. On a machine without one they skip rather than
    /// fail: nothing in accent requires git to be installed.
    fn have_git() -> bool {
        Command::new("git").arg("--version").output().is_ok()
    }

    /// Run git with the developer's own configuration shut out, so a signing key, a hooks path or
    /// another `init.defaultBranch` in `~/.gitconfig` cannot decide whether this suite passes.
    fn sh(dir: &Path, args: &[&str]) -> Output {
        Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "Accent Test")
            .env("GIT_AUTHOR_EMAIL", "test@accent.invalid")
            .env("GIT_COMMITTER_NAME", "Accent Test")
            .env("GIT_COMMITTER_EMAIL", "test@accent.invalid")
            .env("LC_ALL", "C")
            .output()
            .expect("git should be runnable")
    }

    fn ok(dir: &Path, args: &[&str]) {
        let out = sh(dir, args);
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// A repository with a deterministic branch name plus an identity and hook path of its own.
    /// These land in the repository's *local* config on purpose: the functions under test run the
    /// user's git in the user's environment, so only local config reaches them too.
    fn init(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        ok(dir, &["init", "-b", "main"]);
        configure(dir);
    }

    fn configure(dir: &Path) {
        ok(dir, &["config", "user.name", "Accent Test"]);
        ok(dir, &["config", "user.email", "test@accent.invalid"]);
        ok(dir, &["config", "commit.gpgsign", "false"]);
        ok(dir, &["config", "core.hooksPath", ".git/hooks-disabled"]);
    }

    fn write_file(dir: &Path, rel: &str, text: &str) {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn commit_all(dir: &Path, message: &str) {
        ok(dir, &["add", "-A"]);
        ok(dir, &["commit", "-m", message]);
    }

    fn head(dir: &Path) -> String {
        String::from_utf8_lossy(&sh(dir, &["rev-parse", "HEAD"]).stdout)
            .trim()
            .to_string()
    }

    fn open(dir: &Path) -> Repo {
        toplevel(dir)
            .unwrap()
            .expect("a repository at the test root")
    }

    fn paths<'a>(entries: impl Iterator<Item = &'a Entry>) -> Vec<&'a str> {
        let mut out: Vec<&str> = entries.map(|e| e.path.as_str()).collect();
        out.sort_unstable();
        out
    }

    // ------------------------------------------------------------------ status

    #[test]
    fn parse_status_reads_headers_changes_untracked_and_ignored() {
        if !have_git() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        init(dir);
        write_file(dir, "a.md", "one\n");
        write_file(dir, ".gitignore", "build/\n");
        commit_all(dir, "first");

        write_file(dir, "a.md", "one\ntwo\n");
        write_file(dir, "b.md", "b\n");
        ok(dir, &["add", "b.md"]);
        write_file(dir, "c d.md", "c\n");
        write_file(dir, "build/out.txt", "x\n");

        let st = status(&open(dir)).unwrap();
        assert_eq!(st.branch.head.as_deref(), Some("main"));
        assert!(st.branch.oid.as_deref().is_some_and(|oid| oid.len() == 40));
        assert_eq!(st.branch.upstream, None);
        assert_eq!((st.branch.ahead, st.branch.behind), (0, 0));

        let find = |p: &str| {
            st.entries
                .iter()
                .find(|e| e.path == p)
                .unwrap_or_else(|| panic!("no entry for {p}"))
        };
        assert_eq!((find("a.md").x, find("a.md").y), ('.', 'M'));
        assert_eq!((find("b.md").x, find("b.md").y), ('A', '.'));
        let untracked = find("c d.md");
        assert_eq!((untracked.x, untracked.y), ('?', '?'));
        assert!(!untracked.submodule);
        assert_eq!(st.ignored, ["build/"], "a whole ignored tree is one entry");

        assert_eq!(paths(st.staged()), ["b.md"]);
        assert_eq!(paths(st.changes()), ["a.md", "c d.md"]);
        assert_eq!(st.conflicts().count(), 0);
    }

    #[test]
    fn a_rename_carries_its_original_path() {
        if !have_git() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        init(dir);
        write_file(dir, "a.md", "one\n");
        commit_all(dir, "first");
        ok(dir, &["mv", "a.md", "b.md"]);

        let st = status(&open(dir)).unwrap();
        assert_eq!(
            st.entries.len(),
            1,
            "the original path is a second token, not a second entry"
        );
        assert_eq!(st.entries[0].path, "b.md");
        assert_eq!(st.entries[0].orig.as_deref(), Some("a.md"));
        assert_eq!(st.entries[0].x, 'R');
    }

    #[test]
    fn an_unmerged_entry_is_a_conflict() {
        if !have_git() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        init(dir);
        write_file(dir, "f.md", "base\n");
        commit_all(dir, "base");
        ok(dir, &["checkout", "-q", "-b", "side"]);
        write_file(dir, "f.md", "side\n");
        commit_all(dir, "side");
        ok(dir, &["checkout", "-q", "main"]);
        write_file(dir, "f.md", "main\n");
        commit_all(dir, "main");
        assert!(
            !sh(dir, &["merge", "side"]).status.success(),
            "the merge is supposed to conflict"
        );

        let st = status(&open(dir)).unwrap();
        assert_eq!(paths(st.conflicts()), ["f.md"]);
        assert_eq!(st.conflicts().next().unwrap().x, 'U');
        assert_eq!(st.staged().count(), 0, "a conflict is not staged work");
        assert_eq!(st.changes().count(), 0, "nor an ordinary change");
    }

    #[test]
    fn ahead_behind_counts_against_a_local_clone() {
        if !have_git() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("source");
        init(&source);
        write_file(&source, "a.md", "one\n");
        commit_all(&source, "first");
        // The origin is bare: pushing to a repository that has the branch checked out is refused.
        ok(tmp.path(), &["clone", "--bare", "-q", "source", "origin"]);
        ok(tmp.path(), &["clone", "-q", "origin", "work"]);

        let work = tmp.path().join("work");
        configure(&work);
        write_file(&work, "b.md", "b\n");
        commit_all(&work, "second");

        let repo = open(&work);
        let st = status(&repo).unwrap();
        assert_eq!(st.branch.upstream.as_deref(), Some("origin/main"));
        assert_eq!((st.branch.ahead, st.branch.behind), (1, 0));

        assert!(push(&repo).is_ok());
        assert_eq!(status(&repo).unwrap().branch.ahead, 0);
        assert!(pull(&repo).is_ok());
    }

    // ----------------------------------------------------------------- history

    /// The rules [`lanes`] must keep, whatever shape the history has.
    fn check_invariants(rows: &[LogRow]) {
        for (r, row) in rows.iter().enumerate() {
            assert_eq!(
                row.below.is_empty(),
                row.commit.parents.is_empty(),
                "row {r}: an edge leaves for every parent, and only then"
            );
            if let Some(&first) = row.below.first() {
                assert_eq!(
                    first, row.column,
                    "row {r}: the first parent stays in this commit's column"
                );
            }
            assert!(
                !row.through.contains(&row.column),
                "row {r}: a lane cannot pass through its own commit"
            );
            let has_child = rows[..r]
                .iter()
                .any(|drawn| drawn.commit.parents.contains(&row.commit.id));
            assert_eq!(
                row.above.is_empty(),
                !has_child,
                "row {r}: edges come in exactly when a child was drawn above"
            );
            if r > 0 {
                let leaving: BTreeSet<usize> = rows[r - 1]
                    .below
                    .iter()
                    .chain(&rows[r - 1].through)
                    .copied()
                    .collect();
                let entering: BTreeSet<usize> =
                    row.above.iter().chain(&row.through).copied().collect();
                assert_eq!(
                    leaving, entering,
                    "row {r}: every edge leaving the row above has to enter this one"
                );
            }
        }
    }

    #[test]
    fn parse_log_and_lanes_on_a_linear_history() {
        if !have_git() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        init(dir);
        for message in ["one", "two", "three"] {
            write_file(dir, "a.md", message);
            commit_all(dir, message);
        }

        let repo = open(dir);
        let commits = log(&repo, 0, 10).unwrap();
        let summaries: Vec<&str> = commits.iter().map(|c| c.summary.as_str()).collect();
        assert_eq!(summaries, ["three", "two", "one"]);
        assert_eq!(commits[0].author, "Accent Test");
        assert!(commits[0].time > 0);
        assert!(commits[0].refs.iter().any(|r| r.contains("main")));
        assert!(
            commits[1].refs.is_empty(),
            "an undecorated commit has no refs"
        );
        assert_eq!(commits[0].parents, [commits[1].id.clone()]);
        assert!(commits[2].parents.is_empty(), "the root has no parent");
        assert_eq!(
            log(&repo, 1, 1).unwrap()[0].summary,
            "two",
            "skip and limit"
        );

        let rows = lanes(commits);
        check_invariants(&rows);
        assert!(rows.iter().all(|r| r.column == 0 && r.through.is_empty()));

        // The subject and the rest of the message are two fields, and the body's own newlines
        // survive the record split because the record separator is not one of them.
        write_file(dir, "a.md", "four");
        commit_all(dir, "four\n\nwhy it happened\nand a second line");
        let head = log(&repo, 0, 1).unwrap();
        assert_eq!(head[0].summary, "four");
        assert_eq!(head[0].body, "why it happened\nand a second line");
        assert_eq!(log(&repo, 1, 1).unwrap()[0].body, "", "a one-line message");
    }

    #[test]
    fn lanes_on_a_diamond_merge() {
        if !have_git() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        init(dir);
        write_file(dir, "f.md", "base\n");
        commit_all(dir, "base");
        ok(dir, &["checkout", "-q", "-b", "side"]);
        write_file(dir, "s.md", "s\n");
        commit_all(dir, "side");
        ok(dir, &["checkout", "-q", "main"]);
        write_file(dir, "m.md", "m\n");
        commit_all(dir, "main");
        ok(dir, &["merge", "-q", "--no-ff", "side", "-m", "merge"]);

        let rows = lanes(log(&open(dir), 0, 10).unwrap());
        check_invariants(&rows);
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0].commit.summary, "merge");
        assert_eq!(rows[0].below, [0, 1], "the second parent opens a column");
        let base = rows.last().unwrap();
        assert_eq!(base.commit.summary, "base");
        assert_eq!(base.above, [0, 1], "both sides come back together here");
        assert!(base.below.is_empty());
        assert!(rows.iter().all(|r| r.column <= 1), "a diamond is two wide");
    }

    #[test]
    fn lanes_on_two_unmerged_tips_stay_two_columns_wide() {
        if !have_git() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        init(dir);
        write_file(dir, "f.md", "base\n");
        commit_all(dir, "base");
        ok(dir, &["checkout", "-q", "-b", "side"]);
        write_file(dir, "s.md", "s\n");
        commit_all(dir, "side");
        ok(dir, &["checkout", "-q", "main"]);
        write_file(dir, "m.md", "m\n");
        commit_all(dir, "main");

        let rows = lanes(log(&open(dir), 0, 10).unwrap());
        check_invariants(&rows);
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().all(|r| r.column <= 1), "two tips, two columns");
        let base = rows.last().unwrap();
        assert_eq!(base.above, [0, 1], "the fork drains both columns");
        assert_eq!(base.column, 0);
        assert!(base.through.is_empty());
    }

    #[test]
    fn lanes_on_an_octopus_merge() {
        if !have_git() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        init(dir);
        write_file(dir, "f.md", "base\n");
        commit_all(dir, "base");
        ok(dir, &["branch", "b"]);
        ok(dir, &["branch", "c"]);
        for branch in ["b", "c"] {
            ok(dir, &["checkout", "-q", branch]);
            write_file(dir, &format!("{branch}.md"), branch);
            commit_all(dir, branch);
        }
        ok(dir, &["checkout", "-q", "main"]);
        write_file(dir, "m.md", "m\n");
        commit_all(dir, "main");
        ok(dir, &["merge", "-q", "b", "c", "-m", "octopus"]);

        let rows = lanes(log(&open(dir), 0, 10).unwrap());
        check_invariants(&rows);
        assert_eq!(rows[0].commit.summary, "octopus");
        assert_eq!(rows[0].commit.parents.len(), 3);
        assert_eq!(rows[0].below, [0, 1, 2], "one column per parent");
        assert_eq!(rows.last().unwrap().above, [0, 1, 2]);
    }

    #[test]
    fn lanes_on_two_orphan_roots() {
        if !have_git() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        init(dir);
        write_file(dir, "f.md", "one\n");
        commit_all(dir, "first root");
        write_file(dir, "f.md", "two\n");
        commit_all(dir, "first tip");
        ok(dir, &["checkout", "-q", "--orphan", "other"]);
        ok(dir, &["rm", "-r", "-q", "-f", "."]);
        write_file(dir, "g.md", "one\n");
        commit_all(dir, "second root");
        write_file(dir, "g.md", "two\n");
        commit_all(dir, "second tip");

        let rows = lanes(log(&open(dir), 0, 10).unwrap());
        check_invariants(&rows);
        assert_eq!(rows.len(), 4);
        assert_eq!(
            rows.iter().filter(|r| r.commit.parents.is_empty()).count(),
            2,
            "two roots"
        );
        assert!(
            rows.iter().all(|r| r.column == 0),
            "the first root frees column 0 and the second history takes it back"
        );
    }

    // --------------------------------------------------------------- discovery

    #[test]
    fn discover_finds_the_toplevel_and_nested_repos() {
        if !have_git() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let vault = tmp.path().join("vault");
        init(&vault);
        write_file(&vault, "a.md", "one\n");
        commit_all(&vault, "first");
        ok(&vault, &["init", "-b", "main", "sub/inner"]);
        // A linked worktree has `.git` as a file, which is the case a `is_dir` check would miss.
        ok(&vault, &["worktree", "add", "-q", "wt", "-b", "wt"]);

        let dirs = ["sub", "sub/inner", "wt"].map(|d| vault.join(d)).to_vec();
        let repos = discover(&vault, &dirs);
        let names: Vec<&str> = repos.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(
            names,
            ["vault", "inner", "wt"],
            "the vault's own repo leads"
        );
        assert_eq!(repos[1].root, vault.join("sub/inner"));
        assert!(repos[2].git_dir.ends_with(".git/worktrees/wt"));
        assert_eq!(
            repos
                .iter()
                .map(|r| &r.git_dir)
                .collect::<BTreeSet<_>>()
                .len(),
            3,
            "no repository is listed twice"
        );

        let plain = tempfile::tempdir().unwrap();
        assert!(discover(plain.path(), &[]).is_empty());
    }

    // -------------------------------------------------------- read and write

    #[test]
    fn show_reads_head_index_and_reports_missing() {
        if !have_git() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        init(dir);
        write_file(dir, "a.md", "one\n");
        commit_all(dir, "first");
        write_file(dir, "a.md", "two\n");
        ok(dir, &["add", "a.md"]);
        write_file(dir, "new.md", "new\n");

        let repo = open(dir);
        assert_eq!(
            show(&repo, "HEAD", "a.md").unwrap(),
            Some(Blob::Text("one\n".into()))
        );
        assert_eq!(
            show(&repo, "", "a.md").unwrap(),
            Some(Blob::Text("two\n".into()))
        );
        assert_eq!(show(&repo, "HEAD", "gone.md").unwrap(), None);
        assert_eq!(
            show(&repo, "", "new.md").unwrap(),
            None,
            "untracked on disk"
        );
    }

    #[test]
    fn stage_unstage_commit_round_trip() {
        if !have_git() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        init(dir);
        write_file(dir, "a.md", "one\n");
        let repo = open(dir);

        stage(&repo, &["a.md"]).unwrap();
        assert_eq!(paths(status(&repo).unwrap().staged()), ["a.md"]);

        let id = commit(&repo, "first\n\nwith a body\n", false).unwrap();
        assert!(!id.is_empty());
        assert!(status(&repo).unwrap().entries.is_empty(), "a clean tree");
        assert_eq!(log(&repo, 0, 1).unwrap()[0].summary, "first");

        write_file(dir, "b.md", "b\n");
        stage(&repo, &["b.md"]).unwrap();
        assert_eq!(paths(status(&repo).unwrap().staged()), ["b.md"]);
        unstage(&repo, &["b.md"]).unwrap();
        assert_eq!(paths(status(&repo).unwrap().changes()), ["b.md"]);

        write_file(dir, "a.md", "clobbered\n");
        discard(&repo, &["a.md"]).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("a.md")).unwrap(), "one\n");
    }

    #[test]
    fn unstage_without_a_commit_takes_the_file_back_out_of_the_index() {
        if !have_git() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        init(dir);
        let repo = open(dir);

        write_file(dir, "a.md", "one\n");
        stage(&repo, &["a.md"]).unwrap();
        assert_eq!(paths(status(&repo).unwrap().staged()), ["a.md"]);

        unstage(&repo, &["a.md"]).unwrap();
        let st = status(&repo).unwrap();
        assert_eq!(paths(st.changes()), ["a.md"]);
        assert_eq!(st.entries[0].x, '?', "back to untracked");

        // Edited after staging: the index matches neither the worktree nor a HEAD that is not
        // there, which is the case `rm --cached` refuses without `-f`.
        stage(&repo, &["a.md"]).unwrap();
        write_file(dir, "a.md", "two\n");
        unstage(&repo, &["a.md"]).unwrap();
        assert_eq!(paths(status(&repo).unwrap().changes()), ["a.md"]);
        assert_eq!(std::fs::read_to_string(dir.join("a.md")).unwrap(), "two\n");
    }

    #[test]
    fn branches_lists_the_local_ones_and_checkout_refuses_to_clobber() {
        if !have_git() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        init(dir);
        write_file(dir, "a.md", "one\n");
        commit_all(dir, "root");
        ok(dir, &["branch", "side"]);
        let repo = open(dir);

        assert_eq!(branches(&repo).unwrap(), ["main", "side"]);

        checkout(&repo, "side").unwrap();
        assert_eq!(status(&repo).unwrap().branch.head.as_deref(), Some("side"));

        // A change that the other branch would overwrite is git's own refusal, and the whole
        // point of driving `git switch`: nothing here decides whether a checkout is safe.
        write_file(dir, "a.md", "two\n");
        commit_all(dir, "side moves on");
        checkout(&repo, "main").unwrap();
        write_file(dir, "a.md", "uncommitted\n");
        let refused = checkout(&repo, "side").unwrap_err();
        assert!(
            refused.to_string().contains("would be overwritten"),
            "{refused}"
        );
        assert_eq!(
            status(&repo).unwrap().branch.head.as_deref(),
            Some("main"),
            "a refused switch leaves HEAD where it was"
        );
    }

    #[test]
    fn checkout_commit_detaches_head_and_a_branch_takes_it_back() {
        if !have_git() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        init(dir);
        write_file(dir, "a.md", "one\n");
        commit_all(dir, "first");
        let first = head(dir);
        write_file(dir, "a.md", "two\n");
        commit_all(dir, "second");
        let repo = open(dir);

        checkout_commit(&repo, &first).unwrap();
        let detached = status(&repo).unwrap().branch;
        assert_eq!(detached.head, None, "no branch to be on");
        assert_eq!(detached.oid.as_deref(), Some(first.as_str()));

        checkout(&repo, "main").unwrap();
        assert_eq!(status(&repo).unwrap().branch.head.as_deref(), Some("main"));
    }

    #[test]
    fn branches_are_created_and_deleted_and_git_says_when_work_would_be_lost() {
        if !have_git() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        init(dir);
        write_file(dir, "a.md", "one\n");
        commit_all(dir, "root");
        let repo = open(dir);

        create_branch(&repo, "side", false).unwrap();
        assert_eq!(branches(&repo).unwrap(), ["main", "side"]);
        assert_eq!(
            status(&repo).unwrap().branch.head.as_deref(),
            Some("main"),
            "created without being switched to"
        );
        assert!(
            create_branch(&repo, "side", false).is_err(),
            "already taken"
        );

        create_branch(&repo, "work", true).unwrap();
        assert_eq!(status(&repo).unwrap().branch.head.as_deref(), Some("work"));

        // Nothing on `side` that `main` does not already have, so `-d` is enough.
        checkout(&repo, "main").unwrap();
        delete_branch(&repo, "side", false).unwrap();
        assert_eq!(branches(&repo).unwrap(), ["main", "work"]);

        checkout(&repo, "work").unwrap();
        write_file(dir, "b.md", "b\n");
        commit_all(dir, "work moves on");
        checkout(&repo, "main").unwrap();
        let refused = delete_branch(&repo, "work", false).unwrap_err().to_string();
        assert!(unmerged(&refused), "{refused}");
        delete_branch(&repo, "work", true).unwrap();
        assert_eq!(branches(&repo).unwrap(), ["main"]);

        // The checked-out branch is a refusal nothing can force, so it must not read as one.
        let refused = delete_branch(&repo, "main", false).unwrap_err().to_string();
        assert!(!unmerged(&refused), "{refused}");
    }

    #[test]
    fn unmerged_is_gits_own_wording() {
        assert!(unmerged("error: the branch 'side' is not fully merged."));
        assert!(!unmerged(
            "error: cannot delete branch 'main' used by worktree at '/tmp/v'"
        ));
    }

    #[test]
    fn parse_branches_drops_the_blank_line_git_ends_with() {
        assert_eq!(parse_branches(b"main\nfeature/x\n"), ["main", "feature/x"]);
        assert!(parse_branches(b"").is_empty());
    }

    #[test]
    fn changed_files_reads_a_commit_a_root_a_merge_and_a_rename() {
        if !have_git() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        init(dir);
        write_file(dir, "a.md", "one\n");
        write_file(dir, "b.md", "b\n");
        commit_all(dir, "root");
        let root = head(dir);
        write_file(dir, "a.md", "two\n");
        commit_all(dir, "second");
        let second = head(dir);
        ok(dir, &["mv", "a.md", "renamed.md"]);
        commit_all(dir, "rename");
        let rename = head(dir);
        ok(dir, &["checkout", "-q", "-b", "side", &root]);
        write_file(dir, "c.md", "c\n");
        commit_all(dir, "side");
        ok(dir, &["checkout", "-q", "main"]);
        ok(dir, &["merge", "-q", "--no-ff", "side", "-m", "merge"]);
        let merge = head(dir);

        let repo = open(dir);
        let files = |oid: &str| changed_files(&repo, oid).unwrap();
        assert_eq!(
            files(&root),
            [('A', "a.md".to_string()), ('A', "b.md".to_string())],
            "a root commit adds everything in it"
        );
        assert_eq!(files(&second), [('M', "a.md".to_string())]);
        assert_eq!(
            files(&rename),
            [('R', "renamed.md".to_string())],
            "a rename is one row, under the name it now has"
        );
        assert_eq!(
            files(&merge),
            [('A', "c.md".to_string())],
            "a merge shows its first-parent diff"
        );
    }

    #[test]
    fn commit_all_takes_tracked_changes_and_leaves_untracked_files_alone() {
        if !have_git() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        init(dir);
        write_file(dir, "a.md", "one\n");
        commit_all(dir, "first");

        write_file(dir, "a.md", "two\n");
        write_file(dir, "new.md", "new\n");
        let repo = open(dir);
        assert_eq!(status(&repo).unwrap().staged().count(), 0, "nothing staged");

        commit(&repo, "everything tracked\n", true).unwrap();
        let st = status(&repo).unwrap();
        assert_eq!(paths(st.changes()), ["new.md"], "still untracked");
        assert_eq!(
            show(&repo, "HEAD", "a.md").unwrap(),
            Some(Blob::Text("two\n".into())),
            "the tracked change went in"
        );
    }

    #[test]
    fn sync_moves_a_commit_each_way_through_the_bare_origin() {
        if !have_git() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("source");
        init(&source);
        write_file(&source, "a.md", "one\n");
        commit_all(&source, "first");
        ok(tmp.path(), &["clone", "--bare", "-q", "source", "origin"]);
        for clone in ["work", "other"] {
            ok(tmp.path(), &["clone", "-q", "origin", clone]);
            configure(&tmp.path().join(clone));
        }

        // The other clone puts a commit on the origin, which is what our pull has to bring back.
        let other = tmp.path().join("other");
        write_file(&other, "theirs.md", "theirs\n");
        commit_all(&other, "theirs");
        ok(&other, &["push", "-q"]);

        let work = tmp.path().join("work");
        write_file(&work, "mine.md", "mine\n");
        commit_all(&work, "mine");
        let repo = open(&work);

        sync(&repo).unwrap();
        let st = status(&repo).unwrap();
        assert_eq!(
            (st.branch.ahead, st.branch.behind),
            (0, 0),
            "both halves ran"
        );
        assert!(work.join("theirs.md").exists(), "the pull brought theirs");
        ok(&other, &["pull", "-q"]);
        assert!(other.join("mine.md").exists(), "the push sent mine");
    }

    #[test]
    fn a_repo_without_submodules_lists_none() {
        if !have_git() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        init(dir);
        write_file(dir, "a.md", "one\n");
        commit_all(dir, "first");
        assert!(submodules(&open(dir)).unwrap().is_empty());
    }

    #[test]
    fn parse_submodule_reads_state_oid_path_and_describe() {
        let line = " 1234567890abcdef1234567890abcdef12345678 vendor/lib (v1.2-3-gabc)";
        let sub = parse_submodule(line).unwrap();
        assert_eq!(sub.state, ' ');
        assert_eq!(sub.oid, "1234567890abcdef1234567890abcdef12345678");
        assert_eq!(sub.path, "vendor/lib");
        assert_eq!(sub.describe.as_deref(), Some("v1.2-3-gabc"));

        let bare = parse_submodule("-0000000000000000000000000000000000000000 vendor/off").unwrap();
        assert_eq!(bare.state, '-');
        assert_eq!(bare.path, "vendor/off");
        assert_eq!(bare.describe, None);
    }
}
