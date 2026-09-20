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
use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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
    /// A merge stopped part way and is waiting for a commit or an abort.
    pub merging: bool,
    /// A rebase stopped part way — a `pull.rebase=true` pull on a conflict, most often — and is
    /// waiting for a continue or an abort. Defaulted, so a server that does not send it reads.
    #[serde(default)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Commit {
    pub id: String,
    pub parents: Vec<String>,
    /// Decorations: the branches and tags pointing here, HEAD's first (see [`parse_refs`]).
    pub refs: Vec<Ref>,
    pub author: String,
    /// Author time, unix seconds.
    pub time: i64,
    pub summary: String,
    /// Everything after the subject and its blank line. Empty for a one-line message.
    pub body: String,
}

/// One decoration on a commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ref {
    /// As git shortens it: `main`, `origin/main`, `v1`, or `HEAD` for a detached HEAD.
    pub name: String,
    pub kind: RefKind,
    /// HEAD is here: this is the branch it is on, or HEAD itself where it is detached.
    pub head: bool,
}

/// What a [`Ref`] is, in the order a commit lists them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum RefKind {
    /// A detached HEAD, which names no branch.
    Head,
    LocalBranch,
    RemoteBranch,
    Tag,
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
    /// The branch this commit's column draws: the first decoration found on it, going down from
    /// its tip. `None` for a line of history no branch or tag names.
    pub lane: Option<String>,
    /// The named columns that end at this commit besides its own: the branches that forked here.
    pub forks: Vec<String>,
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
///
/// It waits for ever, which is right for reading a local repository: this is a `git status` per
/// save, and the poll interval [`bounded`] wakes on would be latency on every one of them.
/// Everything that talks to a network — and the commit and the switches, which run the user's own
/// hooks and filters — goes through [`bounded`] instead.
fn run(root: &Path, args: &[&str], readonly: bool) -> Result<Vec<u8>, Error> {
    let out = command(root, args, readonly)
        .stdin(Stdio::null())
        .output()?;
    Ok(checked(out)?.stdout)
}

/// The command every call here runs, configured but not spawned. Its own function because
/// [`fetch`] has to wait on the child itself rather than let `Command` wait forever.
fn command(root: &Path, args: &[&str], readonly: bool) -> Command {
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
    cmd
}

/// How git's ssh learns that a link has died: a probe every 5 s, given up on after three go
/// unanswered, so 15 s; and 10 s to connect at all. Without it ssh waits as long as the kernel
/// does, and a link that dropped without closing is two hours of that.
const SSH: &str = "ssh -o ServerAliveInterval=5 -o ServerAliveCountMax=3 -o ConnectTimeout=10";

/// The same 15 s for https, as TCP keepalive on curl's socket: probes from 5 s idle, 5 s apart, two
/// unanswered. Only an idle socket is probed, so a link that drops with data still in flight is
/// left to the bound on the call; and a git that predates the variables ignores them. Each is
/// beside the name of the `http.keepAlive*` setting it would override, as `git config` spells it.
const KEEPALIVE: [(&str, &str, &str); 3] = [
    ("GIT_HTTP_KEEPALIVE_IDLE", "keepaliveidle", "5"),
    ("GIT_HTTP_KEEPALIVE_INTERVAL", "keepaliveinterval", "5"),
    ("GIT_HTTP_KEEPALIVE_COUNT", "keepalivecount", "2"),
];

/// [`command`] for the three that talk to a remote — fetch, pull and push — told to give up on a
/// link that has stopped answering (see [`SSH`]).
///
/// The user's own settings win. Each variable here would override the configuration, so it is set
/// only where neither the environment nor the configuration — `core.sshCommand`, and
/// `http.keepAlive*` including its per-URL `http.<url>.keepAlive*` form — has one of its own.
fn network(root: &Path, args: &[&str]) -> Command {
    let mut cmd = command(root, args, false);
    let set = |key: &str| std::env::var_os(key).is_some();
    let re = r"^(core\.sshcommand|http\..*keepalive(idle|interval|count))$";
    // No match is an exit status of 1, which is the same as nothing configured.
    let configured = run(root, &["config", "--name-only", "--get-regexp", re], true)
        .map(|out| String::from_utf8_lossy(&out).into_owned())
        .unwrap_or_default();
    let names: Vec<&str> = configured.lines().collect();
    let own_ssh = set("GIT_SSH_COMMAND") || set("GIT_SSH") || names.contains(&"core.sshcommand");
    if !own_ssh {
        cmd.env("GIT_SSH_COMMAND", SSH);
    }
    for (key, name, value) in KEEPALIVE {
        if !set(key) && !names.iter().any(|n| n.ends_with(name)) {
            cmd.env(key, value);
        }
    }
    cmd
}

/// A refusal is whatever git put on stderr, which is the only thing worth reporting.
fn checked(out: Output) -> Result<Output, Error> {
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

pub fn status(repo: &Repo) -> Result<Status, Error> {
    let out = run(
        &repo.root,
        &["status", "--porcelain=v2", "-z", "--branch", "--ignored"],
        true,
    )?;
    Ok(Status {
        merging: merging(repo),
        rebasing: rebasing(repo),
        ..parse_status(&out)
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

// ------------------------------------------------------------------ the remote

/// How long a fetch may run before it is killed.
///
/// Fetching is the one call here that talks to a network, and `GIT_TERMINAL_PROMPT=0` only stops
/// it hanging on a *prompt*: a host that accepts the connection and then says nothing holds the
/// thread it runs on for as long as ssh's own timeout, which is minutes. A background fetch is a
/// convenience, so it is bounded and its answer is allowed to be "not this time".
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(8);

/// How long a transfer the user asked for may run before it is killed.
///
/// Longer than [`FETCH_TIMEOUT`] because somebody is watching it: a large push over a slow link is
/// not a hang, and cutting it off would be worse than waiting. Bounded all the same, because the
/// UI holds the Sync button insensitive for the whole call — a pull that will never answer would
/// otherwise pin it for the life of the process, and pin the thread it runs on with it.
pub const TRANSFER_TIMEOUT: Duration = Duration::from_secs(60);

/// How often the wait below looks at the child. Short enough that a quick fetch is not padded,
/// long enough that a slow one costs nothing to wait for.
const POLL: Duration = Duration::from_millis(50);

/// How long a stopped command has to clean up — git removes its lock files on `SIGTERM` — before
/// what is left of it is killed.
const GRACE: Duration = Duration::from_secs(2);

/// How long git's output is waited for once git has exited. Its pipes close with it, unless a
/// helper that left its process group — a daemon, an ssh master going into the background — still
/// holds them.
const LINGER: Duration = Duration::from_secs(1);

/// The commands running right now that may be stopped part way ([`interrupt`]): the repository,
/// what the command is, and the flag its [`bounded`] wait watches.
static STOPPABLE: Mutex<Vec<(PathBuf, String, Arc<AtomicBool>)>> = Mutex::new(Vec::new());

fn registry() -> std::sync::MutexGuard<'static, Vec<(PathBuf, String, Arc<AtomicBool>)>> {
    STOPPABLE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Whether a `what` — "fetch", "push" — is running in `root`: which half of a sync a closing
/// window has caught.
pub fn running(root: &Path, what: &str) -> bool {
    registry().iter().any(|(r, w, _)| r == root && w == what)
}

/// Stop every fetch and push running in `root`, for a window that is closing.
///
/// Those two only: a fetch moves remote-tracking refs and nothing else, and the remote takes a
/// push whole or not at all, so either can be cut off at any point. Everything that rewrites the
/// working tree is left to finish. The thread waiting on each does the stopping, so this returns
/// at once.
pub fn interrupt(root: &Path) {
    for (_, _, stop) in registry().iter().filter(|(r, _, _)| r == root) {
        stop.store(true, Ordering::Relaxed);
    }
}

/// Run one git command and wait for it, stopping it after `cap`, or on [`interrupt`] where
/// `stoppable` names its repository.
///
/// The wait is ours rather than `Command`'s, which waits for ever. Everything that talks to a
/// network needs to be able to give up — `GIT_TERMINAL_PROMPT=0` only stops it hanging on a
/// *prompt*, and a host that accepts the connection and then says nothing holds the thread it runs
/// on for as long as ssh's own timeout — and so does a commit, whose hooks are the user's own
/// programs. `what` names the command in the refusal.
///
/// git runs in a session of its own, so it has no terminal to prompt on and everything it starts —
/// a hook, ssh, `git-remote-https` — shares its process group, which is what [`end`] signals. Its
/// output is read while it runs: a pipe holds 64 KiB, and a git with more to say would otherwise
/// block on it until the cap.
fn bounded(
    mut cmd: Command,
    stdin: Option<&[u8]>,
    cap: Duration,
    what: &str,
    stoppable: Option<&Path>,
) -> Result<Output, Error> {
    // SAFETY: `setsid` is async-signal-safe and touches no memory of ours, which is all that may
    // run between `fork` and `exec`.
    unsafe {
        cmd.pre_exec(|| match libc::setsid() {
            -1 => Err(std::io::Error::last_os_error()),
            _ => Ok(()),
        });
    }
    let input = match stdin {
        Some(_) => Stdio::piped(),
        None => Stdio::null(),
    };
    let mut child = cmd.stdin(input).spawn()?;
    let (out, err) = (drain(child.stdout.take()), drain(child.stderr.take()));
    if let (Some(mut pipe), Some(bytes)) = (child.stdin.take(), stdin) {
        // On a thread too, so a git that stops reading cannot hold the wait below. The pipe
        // closing when the thread ends is what tells git the message is complete; a git that has
        // exited says why on stderr, so a failed write has nothing to add.
        let bytes = bytes.to_vec();
        std::thread::spawn(move || pipe.write_all(&bytes));
    }
    let stop = Arc::new(AtomicBool::new(false));
    if let Some(root) = stoppable {
        registry().push((root.to_path_buf(), what.to_string(), stop.clone()));
    }
    let deadline = Instant::now() + cap;
    let waited = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if stop.load(Ordering::Relaxed) => {
                end(&mut child);
                break Err(Error::Git(format!("the {what} was stopped")));
            }
            Ok(None) if Instant::now() >= deadline => {
                end(&mut child);
                break Err(Error::Git(format!(
                    "the {what} did not finish within {} seconds",
                    cap.as_secs()
                )));
            }
            Ok(None) => std::thread::sleep(POLL),
            Err(e) => {
                end(&mut child);
                break Err(e.into());
            }
        }
    };
    registry().retain(|(_, _, s)| !Arc::ptr_eq(s, &stop));
    let until = Instant::now() + LINGER;
    checked(Output {
        status: waited?,
        stdout: collect(&out, until),
        stderr: collect(&err, until),
    })
}

/// Stop `child` and everything it started: `SIGTERM` to its process group, which git answers by
/// removing its lock files, then `SIGKILL` for whatever is still there after [`GRACE`].
fn end(child: &mut Child) {
    let group = -(child.id() as libc::pid_t);
    let signal = |sig| {
        // SAFETY: `kill` takes plain integers and has no memory-safety preconditions. The group's
        // id stays taken while anything in the group lives, so a signal sent after `try_wait` has
        // reaped the leader reaches what is left of the group or nothing.
        unsafe { libc::kill(group, sig) };
    };
    signal(libc::SIGTERM);
    let until = Instant::now() + GRACE;
    while Instant::now() < until && matches!(child.try_wait(), Ok(None)) {
        std::thread::sleep(POLL);
    }
    signal(libc::SIGKILL);
    let _ = child.wait();
}

/// Read a pipe to its end on a thread of its own, a chunk at a time, so that what arrived is
/// there to [`collect`] even if the end never comes.
fn drain(pipe: Option<impl Read + Send + 'static>) -> Receiver<Vec<u8>> {
    let (tx, rx) = channel();
    if let Some(mut pipe) = pipe {
        std::thread::spawn(move || {
            let mut buf = [0; 8192];
            loop {
                match pipe.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) if tx.send(buf[..n].to_vec()).is_ok() => {}
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    _ => break,
                }
            }
        });
    }
    rx
}

/// What [`drain`] has read by `until`, which is all of it unless something still holds the pipe.
fn collect(chunks: &Receiver<Vec<u8>>, until: Instant) -> Vec<u8> {
    let mut out = Vec::new();
    while let Ok(chunk) = chunks.recv_timeout(until.saturating_duration_since(Instant::now())) {
        out.extend(chunk);
    }
    out
}

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
    let cmd = network(&repo.root, &["fetch"]);
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
            // Whole ref names in `%D`, which is what tells a branch from a tag of the same name.
            "--decorate=full",
            // Unit and record separators: a summary line can hold anything else, including tabs,
            // and a body holds newlines, so the record separator has to be neither.
            "--format=%H%x1f%P%x1f%D%x1f%an%x1f%at%x1f%s%x1f%b%x1e",
        ],
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
            let refs = parse_refs(fields.next()?);
            Some(Commit {
                id,
                parents,
                refs,
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

/// Read `%D` under `--decorate=full`: `HEAD -> refs/heads/main, tag: refs/tags/v1,
/// refs/remotes/origin/main`, or `HEAD` alone where it is detached.
///
/// Only branches, tags and HEAD are kept: `origin/HEAD` points at a branch rather than being one,
/// and the stash and notes are refs no row has anything to say about. Sorted HEAD's first, then by
/// [`RefKind`], git's own order kept within each kind.
pub fn parse_refs(decorations: &str) -> Vec<Ref> {
    let mut refs: Vec<Ref> = decorations
        .split(", ")
        .filter_map(|decoration| {
            let (head, full) = match decoration.strip_prefix("HEAD -> ") {
                Some(full) => (true, full),
                None => (decoration == "HEAD", decoration),
            };
            let (kind, name) = if full == "HEAD" {
                (RefKind::Head, full)
            } else if let Some(name) = full.strip_prefix("refs/heads/") {
                (RefKind::LocalBranch, name)
            } else if let Some(name) = full.strip_prefix("tag: refs/tags/") {
                (RefKind::Tag, name)
            } else {
                let name = full
                    .strip_prefix("refs/remotes/")
                    .filter(|name| !name.ends_with("/HEAD"))?;
                (RefKind::RemoteBranch, name)
            };
            Some(Ref {
                name: name.to_string(),
                kind,
                head,
            })
        })
        .collect();
    refs.sort_by_key(|r| (!r.head, r.kind));
    refs
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
///
/// Each column also carries the name of the branch it draws, taken from the first decorated
/// commit on it and handed down its first parents, so a branch merged into it by fast-forward
/// does not rename it. Where several columns end at one commit, the commit carries its own column
/// on and the others are the branches that forked from it: the graph's own reading, one line
/// going straight on and the rest joining it.
pub fn lanes(commits: Vec<Commit>) -> Vec<LogRow> {
    let mut lanes: Vec<Option<Lane>> = Vec::new();
    let mut rows = Vec::with_capacity(commits.len());

    for commit in commits {
        let above: Vec<usize> = waiting_for(&lanes, &commit.id).collect();
        let column = match above.first() {
            Some(&column) => column,
            None => free_slot(&mut lanes),
        };
        // Every column drawn into this commit ends here, its own first.
        let ending: Vec<Option<String>> = above
            .iter()
            .map(|&i| lanes[i].take().and_then(|l| l.name))
            .collect();
        let lane = ending
            .first()
            .cloned()
            .flatten()
            .or_else(|| lane_name(&commit.refs));
        let forks: Vec<String> = ending.into_iter().skip(1).flatten().collect();

        let through: Vec<usize> = (0..lanes.len())
            .filter(|&i| i != column && lanes[i].is_some())
            .collect();

        let mut below = Vec::with_capacity(commit.parents.len());
        for (nth, parent) in commit.parents.iter().enumerate() {
            // The first parent stays in this commit's own column, and carries its name on; the
            // others join a column already waiting for them, or open one no name has reached yet.
            let waiting = waiting_for(&lanes, parent).next();
            let (i, name) = match (nth, waiting) {
                (0, _) => (column, lane.clone()),
                (_, Some(i)) => (i, lanes[i].take().and_then(|l| l.name)),
                (_, None) => (free_slot(&mut lanes), None),
            };
            lanes[i] = Some(Lane {
                waiting: parent.clone(),
                name,
            });
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
            lane,
            forks,
        });
    }
    rows
}

/// One open column of the graph: the oid it is waiting for, and the branch it draws.
struct Lane {
    waiting: String,
    name: Option<String>,
}

/// What a commit's decorations would name a column after: its first branch, or failing that its
/// first tag. A detached HEAD names no line of history.
fn lane_name(refs: &[Ref]) -> Option<String> {
    refs.iter()
        .find(|r| r.kind != RefKind::Head)
        .map(|r| r.name.clone())
}

fn waiting_for<'a>(lanes: &'a [Option<Lane>], oid: &'a str) -> impl Iterator<Item = usize> + 'a {
    lanes
        .iter()
        .enumerate()
        .filter(move |(_, lane)| lane.as_ref().is_some_and(|l| l.waiting == oid))
        .map(|(i, _)| i)
}

fn free_slot(lanes: &mut Vec<Option<Lane>>) -> usize {
    match lanes.iter().position(Option::is_none) {
        Some(i) => i,
        None => {
            lanes.push(None);
            lanes.len() - 1
        }
    }
}

// ----------------------------------------------------------------- submodules

/// The submodules of `repo`, as `git submodule status` reports them.
///
/// A repository with no `.gitmodules` has none, and that is a stat rather than a process: the
/// command costs about as much as `git status` (30 ms on a 40 000-file repository) and the Git
/// pane asks on every refresh, so the common case must not pay for it.
pub fn submodules(repo: &Repo) -> Result<Vec<Submodule>, Error> {
    if !repo.root.join(".gitmodules").exists() {
        return Ok(Vec::new());
    }
    let out = run(&repo.root, &["submodule", "status"], true)?;
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
    match run(&repo.root, &["show", &format!("{rev}:{path}")], true) {
        Ok(bytes) => Ok(Some(Blob::of(&bytes))),
        Err(Error::Git(msg))
            if msg.contains("does not exist") || msg.contains("exists on disk, but not in") =>
        {
            Ok(None)
        }
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
    /// come back. Defaulted, so an answer from a server that does not send it still reads.
    #[serde(default)]
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
pub fn parse_branches(bytes: &[u8]) -> Branches {
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
        true,
    )
    .is_err()
}

pub fn discard(repo: &Repo, paths: &[&str]) -> Result<(), Error> {
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

/// `--` keeps a note called `-f`, or one whose name matches a branch, from being read as an option.
fn write(repo: &Repo, verb: &[&str], paths: &[&str]) -> Result<(), Error> {
    let args = [verb, &["--"][..], paths].concat();
    run(&repo.root, &args, false)?;
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

/// A push can be stopped part way ([`interrupt`]): the remote takes it whole or not at all.
pub fn push(repo: &Repo) -> Result<String, Error> {
    transcript(repo, &["push"], true)
}

/// A pull cannot: its merge rewrites the working tree.
pub fn pull(repo: &Repo) -> Result<String, Error> {
    transcript(repo, &["pull"], false)
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

/// Pull, then push, as one operation with one transcript.
///
/// Both halves run every time: the `behind` count a background [`fetch`] keeps current is a
/// readout and not a decision — it can be a whole fetch interval old — and the pull is also what
/// keeps the push from landing on a history the remote has moved past. A failed pull stops there,
/// and its error is the whole answer.
///
/// A branch that has never been pushed is the one case that is not a pull and a push: there is no
/// upstream to pull from and no ref to push onto, so the whole of a sync there is publishing it.
pub fn sync(repo: &Repo) -> Result<String, Error> {
    if upstream(repo).is_none() {
        return publish(repo);
    }
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
fn transcript(repo: &Repo, args: &[&str], stoppable: bool) -> Result<String, Error> {
    let cmd = network(&repo.root, args);
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
