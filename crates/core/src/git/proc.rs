//! Git subprocess execution: direct waits for local queries, bounded waits for transfers and
//! user hooks, and network settings that detect dead links.

use super::Error;
use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Run git in `root` and return stdout.
///
/// Queries must set `readonly` to disable optional index writes (`GIT_OPTIONAL_LOCKS=0`),
/// preventing status-refresh loops triggered by the repository watcher. Local reads wait without
/// polling; network operations, commits and switches use [`bounded`] because they can stall or
/// run user programs.
pub(super) fn run(root: &Path, args: &[&str], readonly: bool) -> Result<Vec<u8>, Error> {
    let out = command(root, args, readonly)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| Error::io("git", e))?;
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

/// SSH probes every 5 s and gives up after three unanswered probes (15 s); connection attempts
/// time out after 10 s. This avoids the kernel's long dead-link timeout.
const SSH: &str = "ssh -o ServerAliveInterval=5 -o ServerAliveCountMax=3 -o ConnectTimeout=10";

/// HTTPS keepalive probes start after 5 s idle and repeat every 5 s, stopping after two
/// unanswered probes (15 s). In-flight stalls rely on the call timeout; older git versions may
/// ignore these variables. Corresponding `http.keepAlive*` keys are noted beside each variable.
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

/// Build a fetch, pull or push command that times out on an unresponsive connection ([`SSH`]).
///
/// `ask` permits a passphrase dialog for user-initiated Sync, but not timed autofetch. Git also
/// uses `SSH_ASKPASS` as a fallback for HTTPS credentials.
///
/// Preserve environment and Git configuration for SSH and HTTP keepalive settings; supply defaults
/// only when the user has set neither.
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

/// Background fetch timeout. Noninteractive prompts alone cannot prevent stalled network calls
/// from holding a worker.
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(8);

/// User transfer timeout, longer than [`FETCH_TIMEOUT`] to allow large pushes over slow links
/// while preventing a stalled call from holding the worker and Sync button indefinitely.
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

/// Run git until completion, `cap`, or [`interrupt`] when `stoppable` identifies a repository.
/// `what` names the command in errors.
///
/// Bounds prevent network stalls and hanging hooks from holding workers indefinitely. A separate
/// session disables terminal prompts and groups helpers for [`end`] to signal. Output is drained
/// concurrently so full pipes cannot block git.
pub(super) fn bounded(
    cmd: Command,
    stdin: Option<&[u8]>,
    cap: Duration,
    what: &str,
    stoppable: Option<&Path>,
) -> Result<Output, Error> {
    checked(finished(cmd, stdin, cap, what, stoppable)?)
}

/// [`bounded`] without its [`checked`]: git's output whatever its exit status, for a caller that
/// reads a refusal off stdout.
pub(super) fn finished(
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
    let mut child = cmd.stdin(input).spawn().map_err(|e| Error::io("git", e))?;
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
                break Err(Error::io("git", e));
            }
        }
    };
    registry().retain(|(_, _, s)| !Arc::ptr_eq(s, &stop));
    let until = Instant::now() + LINGER;
    Ok(Output {
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
