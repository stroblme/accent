//! How every call in the `git` module runs `git`: the plain wait a local query gets, the bounded
//! one a transfer, a commit and a switch get, and what a command that talks to a remote is told so
//! it gives up on a dead link.

use super::Error;
use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Run one `git` command in `root` and hand back its stdout.
///
/// `readonly` is not a hint: it sets `GIT_OPTIONAL_LOCKS=0`, and without it `git status` refreshes
/// and rewrites `.git/index`. The GTK layer watches the repository, sees that write, asks for a
/// status to explain it, and the two chase each other forever. Every query passes `true`.
///
/// It waits for ever, which is right for reading a local repository: this is a `git status` per
/// save, and the poll interval [`bounded`] wakes on would be latency on every one of them.
/// Everything that talks to a network — and the commit and the switches, which run the user's own
/// hooks and filters — goes through [`bounded`] instead.
pub(super) fn run(root: &Path, args: &[&str], readonly: bool) -> Result<Vec<u8>, Error> {
    let out = command(root, args, readonly)
        .stdin(Stdio::null())
        .output()?;
    Ok(checked(out)?.stdout)
}

/// The command every call here runs, configured but not spawned. Its own function because
/// [`fetch`](super::fetch) has to wait on the child itself rather than let `Command` wait
/// forever.
pub(super) fn command(root: &Path, args: &[&str], readonly: bool) -> Command {
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

/// What a transfer the user asked for adds to [`SSH`]: a key whose passphrase was just typed goes
/// into the agent, so the push after the pull does not ask for the same one again.
const ADD_KEYS: &str = " -o AddKeysToAgent=yes";

/// The program ssh asks a passphrase through, printing the answer on stdout — the GTK app's own
/// askpass mode (`askpass.rs`), handed over at startup by [`set_askpass`].
///
/// Handed over rather than found, because this module also runs inside `accent-cli serve` on a
/// host, whose binary has no dialog to show: `current_exe` here would point ssh at a program that
/// cannot ask.
static ASKPASS: OnceLock<PathBuf> = OnceLock::new();

/// Let the transfers the user asks for raise `exe` when ssh wants a passphrase. Called once, at
/// startup; a process that never calls it never prompts.
pub fn set_askpass(exe: PathBuf) {
    let _ = ASKPASS.set(exe);
}

/// The helper, unless the user has an askpass of their own: theirs wins, as `core.sshCommand`
/// does.
fn askpass() -> Option<&'static Path> {
    if std::env::var_os("SSH_ASKPASS").is_some() {
        return None;
    }
    ASKPASS.get().map(PathBuf::as_path)
}

/// [`command`] for the three that talk to a remote — fetch, pull and push — told to give up on a
/// link that has stopped answering (see [`SSH`]).
///
/// `ask` is whether ssh may put the passphrase dialog on screen, which is true of a Sync's pull
/// and push and false of the autofetch: that one runs on a five-minute timer, so its dialog would
/// arrive over whatever is being typed with nobody having asked for it, and its own shorter bound
/// would kill it mid-answer. Note that git reads `SSH_ASKPASS` as the last fallback for
/// `GIT_ASKPASS` too, so an https remote's username and password come through the same dialog.
///
/// The user's own settings win. Each variable here would override the configuration, so it is set
/// only where neither the environment nor the configuration — `core.sshCommand`, `SSH_ASKPASS`,
/// and `http.keepAlive*` including its per-URL `http.<url>.keepAlive*` form — has one of its own.
pub(super) fn network(root: &Path, args: &[&str], ask: bool) -> Command {
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
        match ask.then(askpass).flatten() {
            Some(exe) => {
                cmd.env("GIT_SSH_COMMAND", format!("{SSH}{ADD_KEYS}"))
                    .env("SSH_ASKPASS", exe)
                    // ssh prompts on its terminal unless told not to, and accent was very likely
                    // started from one.
                    .env("SSH_ASKPASS_REQUIRE", "force")
                    .env("ACCENT_ASKPASS", "1");
            }
            None => {
                cmd.env("GIT_SSH_COMMAND", SSH);
            }
        }
    }
    for (key, name, value) in KEEPALIVE {
        if !set(key) && !names.iter().any(|n| n.ends_with(name)) {
            cmd.env(key, value);
        }
    }
    cmd
}

/// A refusal is whatever git put on stderr, which is the only thing worth reporting.
pub(super) fn checked(out: Output) -> Result<Output, Error> {
    if !out.status.success() {
        return Err(Error::Git(
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        ));
    }
    Ok(out)
}

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
pub(super) fn bounded(
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
