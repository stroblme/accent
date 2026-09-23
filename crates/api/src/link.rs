//! A host made ready for whatever rides its ssh master: a vault's `serve`, a held shell's
//! `attach`.
//!
//! Ready means two things, and both belong to the host rather than to whoever asked: a master is
//! up behind the socket, and the server binary this build uploads is installed there. Functions,
//! not a type, because nothing is left to own once they are: the socket path derives from the
//! address, and the master lives on through ControlPersist and whatever rides it.
//!
//! Attempts on one host take turns, and a caller that waited on one takes its outcome instead of
//! making its own, so the three tabs of a restored terminal session cost one handshake, one check
//! and at most one upload between them.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::locked;
use crate::ssh::{self, Url};

/// How much of the server binary goes out per write, and therefore how often the progress bar
/// moves while it is uploading.
const CHUNK: usize = 256 * 1024;

/// The musl-static `accent-cli` this machine uploads, and what it is known by on a host.
#[derive(Debug, Clone)]
pub struct Server {
    pub path: PathBuf,
    /// Its blake3, which names it on the host: see [`ssh::server_path`].
    pub hash: String,
    pub size: usize,
}

/// The server binary, hashed once per build rather than once per connection: 8 MB of blake3 is
/// not free, and every tab on a host asks for its name.
pub fn server() -> Result<Server, String> {
    /// The file as it was when it was hashed: a rebuild changes its length or its mtime.
    type Stamp = (PathBuf, u64, Option<std::time::SystemTime>);
    static HASHED: Mutex<Option<(Stamp, Server)>> = Mutex::new(None);

    let path = ssh::server_binary()?;
    let meta = std::fs::metadata(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let stamp = (path.clone(), meta.len(), meta.modified().ok());
    let mut hashed = locked(&HASHED);
    if let Some((seen, server)) = &*hashed
        && *seen == stamp
    {
        return Ok(server.clone());
    }
    let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let server = Server {
        hash: ssh::hash_of(&bytes),
        size: bytes.len(),
        path,
    };
    *hashed = Some((stamp, server.clone()));
    Ok(server)
}

/// The host behind `url`, as an address of its own: the key of the master a shell on it rides
/// when no vault window has one there.
pub fn host(url: &Url) -> Url {
    Url {
        path: PathBuf::from("/"),
        ..url.clone()
    }
}

/// Make the host behind `url` ready: a master behind `ctl`, and the server installed. `say` hears
/// each step, with a fraction for the upload, the one step that can measure itself.
///
/// `quiet` is for an attempt nobody asked for: see [`ssh::master`].
pub fn prepare(
    url: &Url,
    ctl: &Path,
    quiet: bool,
    say: &dyn Fn(&str, Option<f64>),
) -> Result<(), String> {
    say(&format!("Connecting to {}", url.host), None);
    once(&url.authority(), ctl, quiet, || {
        // A live master skips the handshake and the round trip `ssh::master` would make to adopt
        // it. A stale socket fails the check, and `ControlMaster=auto` replaces it.
        if run(&ssh::check(url, ctl)).is_err() {
            if let Some(dir) = ctl.parent() {
                std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            }
            run(&ssh::master(url, ctl, quiet)).map_err(|why| match why.is_empty() {
                true => format!("cannot connect to {}", url.host),
                false => why,
            })?;
        }
        provision(url, ctl, say)
    })
}

/// One ssh invocation, with the environment that makes a prompt reach a dialog instead of a
/// terminal nobody is looking at.
///
/// `SSH_ASKPASS` points at accent itself: the app re-runs as its own askpass helper when it
/// sees `ACCENT_ASKPASS`, so there is one binary to install and the dialog looks like the
/// rest of the window. `REQUIRE=force` is what makes ssh use it even when it can see a
/// terminal, which it can — the app was very likely started from one.
pub(crate) fn command(argv: &[String]) -> Command {
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]);
    if let Ok(exe) = std::env::current_exe() {
        cmd.env("SSH_ASKPASS", exe)
            .env("SSH_ASKPASS_REQUIRE", "force")
            .env("ACCENT_ASKPASS", "1");
    }
    cmd
}

/// Run one ssh invocation to its end, answering with what it said on stderr when it failed. It
/// must not read our stdin, and its prompts go through `SSH_ASKPASS`.
fn run(argv: &[String]) -> Result<(), String> {
    let out = command(argv)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run ssh: {e}"))?;
    match out.status.success() {
        true => Ok(()),
        false => Err(String::from_utf8_lossy(&out.stderr).trim().to_string()),
    }
}

/// Put the right server binary on the host, if it is not already there.
fn provision(url: &Url, ctl: &Path, say: &dyn Fn(&str, Option<f64>)) -> Result<(), String> {
    say("Checking the remote server", None);
    let server = server()?;
    let total = server.size;
    if run(&ssh::run(
        url,
        ctl,
        &ssh::have_server_cmd(&server.hash, total),
    ))
    .is_ok()
    {
        return Ok(());
    }

    let bytes =
        std::fs::read(&server.path).map_err(|e| format!("{}: {e}", server.path.display()))?;
    // Rebuilt since it was hashed: these bytes would go up under another build's name.
    if ssh::hash_of(&bytes) != server.hash {
        return Err("the server binary changed while connecting; try again".to_string());
    }
    say(
        &format!("Uploading the server (0 / {} MB)", mb(total)),
        Some(0.0),
    );
    let mut child = command(&ssh::run(
        url,
        ctl,
        &ssh::install_server_cmd(&server.hash, total),
    ))
    .stdin(Stdio::piped())
    .stdout(Stdio::null())
    .stderr(Stdio::piped())
    .spawn()
    .map_err(|e| format!("cannot run ssh: {e}"))?;
    {
        let mut stdin = child.stdin.take().ok_or("ssh has no stdin")?;
        for (n, chunk) in bytes.chunks(CHUNK).enumerate() {
            stdin
                .write_all(chunk)
                .map_err(|e| format!("uploading the server: {e}"))?;
            let done = ((n + 1) * CHUNK).min(total);
            say(
                &format!("Uploading the server ({} / {} MB)", mb(done), mb(total)),
                Some(done as f64 / total as f64),
            );
        }
    }
    // The bytes are all written, and the host is still unpacking them: without this the bar
    // would sit full for seconds under a message saying the upload had finished.
    say("Installing the server", None);
    let out = child
        .wait_with_output()
        .map_err(|e| format!("uploading the server: {e}"))?;
    match out.status.success() {
        true => Ok(()),
        false => Err(format!(
            "cannot install the server: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )),
    }
}

fn mb(bytes: usize) -> String {
    format!("{:.1}", bytes as f64 / (1024.0 * 1024.0))
}

// ------------------------------------------------------------------- one at a time

/// The attempts on one host. Its lock is held for the length of an attempt, which is what makes
/// them take turns.
#[derive(Default)]
struct Host {
    /// How many attempts have ended, which tells a caller whether one ended while it waited.
    ended: AtomicU64,
    last: Mutex<Option<Ended>>,
}

/// How the last attempt on a host went, and for whom.
struct Ended {
    ctl: PathBuf,
    quiet: bool,
    outcome: Result<(), String>,
}

impl Ended {
    /// Whether a caller that waited for this attempt may take its outcome as its own: only for
    /// the same master, and never a quiet failure for a caller that may prompt, whose passphrase
    /// might get through where `BatchMode` gave up.
    fn answers(&self, ctl: &Path, quiet: bool) -> bool {
        self.ctl == ctl && (self.outcome.is_ok() || quiet || !self.quiet)
    }
}

/// Run `work` for the host `authority` names, unless an attempt on it that ends while this caller
/// waits its turn can answer for it.
///
/// Keyed by the authority rather than the socket because the upload is the host's: two masters
/// to one host, a vault window's and a terminal window's, must not upload over each other.
fn once(
    authority: &str,
    ctl: &Path,
    quiet: bool,
    work: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    static HOSTS: Mutex<Vec<(String, Arc<Host>)>> = Mutex::new(Vec::new());
    let host = {
        let mut hosts = locked(&HOSTS);
        match hosts.iter().find(|(a, _)| a == authority) {
            Some((_, host)) => host.clone(),
            None => {
                let host = Arc::new(Host::default());
                hosts.push((authority.to_string(), host.clone()));
                host
            }
        }
    };

    let arrived = host.ended.load(Ordering::SeqCst);
    // A panic in `work` poisons the lock without counting an end, so the next caller simply makes
    // an attempt of its own.
    let mut last = locked(&host.last);
    if host.ended.load(Ordering::SeqCst) != arrived
        && let Some(ended) = &*last
        && ended.answers(ctl, quiet)
    {
        return ended.outcome.clone();
    }
    let outcome = work();
    *last = Some(Ended {
        ctl: ctl.to_path_buf(),
        quiet,
        outcome: outcome.clone(),
    });
    host.ended.fetch_add(1, Ordering::SeqCst);
    outcome
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;

    /// Long enough for a caller spawned now to be queued behind the attempt that is running.
    const QUEUED: Duration = Duration::from_millis(100);

    const CTL: &str = "/run/user/1000/accent/0123456789abcdef";

    type Outcome = Result<(), String>;

    /// Start an attempt on `authority` that holds the host until the test says how it went.
    fn held<'s>(
        s: &'s std::thread::Scope<'s, '_>,
        authority: &'static str,
        quiet: bool,
    ) -> (
        mpsc::Sender<Outcome>,
        std::thread::ScopedJoinHandle<'s, Outcome>,
    ) {
        let (release, outcome) = mpsc::channel();
        let (started, running) = mpsc::channel();
        let attempt = s.spawn(move || {
            once(authority, Path::new(CTL), quiet, || {
                started.send(()).unwrap();
                outcome.recv().unwrap()
            })
        });
        running.recv().unwrap();
        (release, attempt)
    }

    #[test]
    fn a_host_is_made_ready_once_for_everyone_who_waited() {
        let ctl = Path::new(CTL);
        std::thread::scope(|s| {
            let (release, first) = held(s, "a-waited-host", false);
            let second = s.spawn(|| {
                once("a-waited-host", ctl, true, || {
                    panic!("a caller that waited ran an attempt of its own")
                })
            });
            std::thread::sleep(QUEUED);
            release.send(Ok(())).unwrap();
            assert_eq!(first.join().unwrap(), Ok(()));
            assert_eq!(second.join().unwrap(), Ok(()));
        });

        // One that comes after an attempt has ended asks again: the master may be gone since.
        let mut ran = false;
        let again = once("a-waited-host", ctl, false, || {
            ran = true;
            Err("gone".to_string())
        });
        assert!(ran);
        assert_eq!(again, Err("gone".to_string()));

        // A quiet attempt fails where a prompt might have got through, so a caller that may
        // prompt is not handed that failure.
        std::thread::scope(|s| {
            let (release, quiet) = held(s, "a-quiet-host", true);
            let loud = s.spawn(|| once("a-quiet-host", ctl, false, || Ok(())));
            std::thread::sleep(QUEUED);
            release.send(Err("no agent".to_string())).unwrap();
            assert_eq!(quiet.join().unwrap(), Err("no agent".to_string()));
            assert_eq!(loud.join().unwrap(), Ok(()));
        });
    }
}
