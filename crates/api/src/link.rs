//! Prepare an SSH master and server binary for vault `serve` or shell `attach`. The socket is
//! keyed by host, and ControlPersist keeps the master alive. Per-host attempts are serialized so
//! restored tabs share the handshake and upload.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::ssh::{self, Url};
use crate::{Error, Result, locked};

/// Upload chunk size, controlling progress-report frequency.
const CHUNK: usize = 256 * 1024;

/// The musl-static `accent-cli` this machine uploads, one build per architecture, and what it is
/// known by on a host.
#[derive(Debug, Clone)]
pub struct Server {
    /// Combined blake3 hash naming all architecture builds ([`ssh::server_path`]),
    /// so a shell knows the remote path before learning the host architecture.
    pub hash: String,
    builds: Vec<Build>,
}

/// One architecture's server.
#[derive(Debug, Clone)]
struct Build {
    /// As `uname -m` names it: one of [`ssh::SERVER_ARCHES`].
    arch: &'static str,
    path: PathBuf,
    hash: String,
    size: usize,
}

impl Build {
    /// A checked build-time archive when the host can unpack it; otherwise the raw binary.
    fn payload(&self, xz: bool) -> Result<(Vec<u8>, bool)> {
        use sha2::{Digest, Sha256};

        let sha256 = |bytes: &[u8]| -> String {
            Sha256::digest(bytes)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect()
        };
        let raw = std::fs::read(&self.path).map_err(|e| unreadable(&self.path, e))?;
        if ssh::hash_of(&raw) != self.hash {
            return Err(Error::Remote(
                "the server binary changed while connecting; try again".to_string(),
            ));
        }
        // Both digests must match: a rebuild or an interrupted install can leave either
        // sidecar from another build, even when the uncompressed sizes are identical.
        if xz
            && let Ok(sums) = std::fs::read_to_string(self.path.with_extension("xz.sha256"))
            && let Ok(bytes) = std::fs::read(self.path.with_extension("xz"))
            && sums == format!("{}\n{}\n", sha256(&raw), sha256(&bytes))
        {
            return Ok((bytes, true));
        }
        Ok((raw, false))
    }
}

impl Server {
    /// The build for a host whose `uname -sm` says `system`, or why there is none: a host that is
    /// not Linux on one of [`ssh::SERVER_ARCHES`], or a build of this app that left that one out.
    fn for_host(&self, host: &str, system: &str) -> Result<&Build> {
        let arch = match system.split_whitespace().collect::<Vec<_>>()[..] {
            ["Linux", arch] => ssh::SERVER_ARCHES.iter().find(|a| **a == arch),
            _ => None,
        };
        let Some(arch) = arch else {
            return Err(Error::Refused(format!(
                "{host} runs {system}, and accent's server is built for Linux on {} only",
                ssh::SERVER_ARCHES.join(" and ")
            )));
        };
        self.builds.iter().find(|b| b.arch == *arch).ok_or_else(|| {
            let carried: Vec<&str> = self.builds.iter().map(|b| b.arch).collect();
            // The Flatpak carries its own machine's server only: Flathub refuses a build holding
            // binaries of another architecture.
            let fix = if Path::new("/.flatpak-info").exists() {
                ""
            } else {
                ": run `make server`"
            };
            Error::Refused(format!(
                "{host} runs {system}, and this build carries the server for {} hosts only{fix}",
                carried.join(" and ")
            ))
        })
    }
}

/// Cache server hashes per build to avoid hashing megabytes for every tab
/// connection.
pub fn server() -> Result<Server> {
    /// The files as they were when they were hashed: a rebuild changes a length or an mtime.
    type Stamp = Vec<(PathBuf, u64, Option<std::time::SystemTime>)>;
    static HASHED: Mutex<Option<(Stamp, Server)>> = Mutex::new(None);

    let found: Vec<(&'static str, PathBuf)> = ssh::SERVER_ARCHES
        .iter()
        .filter_map(|arch| Some((*arch, ssh::server_binary(arch)?)))
        .collect();
    if found.is_empty() {
        return Err(Error::Remote(
            "no server binary to upload; run `make server` first".to_string(),
        ));
    }
    let mut stamp = Vec::new();
    for (_, path) in &found {
        let meta = std::fs::metadata(path).map_err(|e| unreadable(path, e))?;
        stamp.push((path.clone(), meta.len(), meta.modified().ok()));
    }
    let mut hashed = locked(&HASHED);
    if let Some((seen, server)) = &*hashed
        && *seen == stamp
    {
        return Ok(server.clone());
    }
    let mut builds = Vec::new();
    for (arch, path) in found {
        let bytes = std::fs::read(&path).map_err(|e| unreadable(&path, e))?;
        builds.push(Build {
            arch,
            hash: ssh::hash_of(&bytes),
            size: bytes.len(),
            path,
        });
    }
    let hashes: Vec<&str> = builds.iter().map(|b| b.hash.as_str()).collect();
    let server = Server {
        hash: ssh::hash_of(hashes.join(" ").as_bytes()),
        builds,
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
pub fn prepare(url: &Url, ctl: &Path, quiet: bool, say: &dyn Fn(&str, Option<f64>)) -> Result<()> {
    say(&format!("Connecting to {}", url.host), None);
    once(&url.authority(), ctl, quiet, || {
        // A live master skips the handshake and the round trip `ssh::master` would make to adopt
        // it. A stale socket fails the check, and `ControlMaster=auto` replaces it.
        let adopted = run(&ssh::check(url, ctl)).is_ok();
        if !adopted {
            dial(url, ctl, quiet, say)?;
        }
        let server = server()?;
        let missing = match installed(url, ctl, &server, say) {
            // Up is not usable: the host may no longer open a session on that connection — one
            // too many, or a login that has lapsed since, as a cluster's key unlocked for hours
            // does — and every command over it then falls back to a connection of its own, which
            // `BatchMode` refuses at the first prompt. Adopting it again would repeat that on
            // every attempt, so it is retired and a fresh master dials in its place, asking for
            // the passphrase where this attempt may.
            Err(Error::Remote(why)) if adopted => {
                tracing::debug!("the master for {} opens no session: {why}", url.host);
                let _ = run(&ssh::stop(url, ctl));
                dial(url, ctl, quiet, say)?;
                installed(url, ctl, &server, say)?
            }
            answer => answer?,
        };
        if let Some((build, xz)) = missing {
            upload(url, ctl, &server.hash, build, xz, say)?;
        }
        made_ready(&url.authority(), &server.hash);
        Ok(())
    })
}

/// Which server [`prepare`] last made ready on each host, by its [`Url::authority`].
static PREPARED: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

fn made_ready(authority: &str, hash: &str) {
    let mut prepared = locked(&PREPARED);
    prepared.retain(|(host, _)| host != authority);
    prepared.push((authority.to_string(), hash.to_string()));
}

/// The hash of the server a command on `url`'s host runs ([`ssh::server_path`]): the one
/// [`prepare`] last made ready there, over any master, rather than this machine's build as it is
/// now, which a rebuild since has not put there and whose upload swept the one before; this
/// build's before any prepare.
pub fn prepared(url: &Url) -> Result<String> {
    let authority = url.authority();
    let made = locked(&PREPARED)
        .iter()
        .find(|(host, _)| *host == authority)
        .map(|(_, hash)| hash.clone());
    match made {
        Some(hash) => Ok(hash),
        None => Ok(server()?.hash),
    }
}

/// Start the background master behind `ctl`, which is where a prompt can come up.
fn dial(url: &Url, ctl: &Path, quiet: bool, say: &dyn Fn(&str, Option<f64>)) -> Result<()> {
    say(&format!("Connecting to {}", url.host), None);
    if let Some(dir) = ctl.parent() {
        std::fs::create_dir_all(dir).map_err(|e| unreadable(dir, e))?;
    }
    run(&ssh::master(url, ctl, quiet)).map_err(|e| match e {
        Error::Remote(why) if why.is_empty() => {
            Error::Remote(format!("cannot connect to {}", url.host))
        }
        e => e,
    })
}

/// A local file the link needs and could not have, by its path.
fn unreadable(path: &Path, e: std::io::Error) -> Error {
    Error::Remote(format!("{}: {e}", path.display()))
}

/// Configure SSH prompts to use the app's askpass dialog instead of a terminal.
/// `SSH_ASKPASS` reruns accent with `ACCENT_ASKPASS`; `REQUIRE=force` uses it
/// even if the app was started from a terminal.
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
fn run(argv: &[String]) -> Result<()> {
    let out = command(argv)
        .stdin(Stdio::null())
        .output()
        .map_err(no_ssh)?;
    match out.status.success() {
        true => Ok(()),
        false => Err(Error::Remote(
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        )),
    }
}

/// ssh would not even start.
pub(crate) fn no_ssh(e: std::io::Error) -> Error {
    Error::Remote(format!("cannot run ssh: {e}"))
}

/// The build the host still needs, `None` where it holds it already, or why it cannot have one
/// or a command over the master could not say.
fn installed<'s>(
    url: &Url,
    ctl: &Path,
    server: &'s Server,
    say: &dyn Fn(&str, Option<f64>),
) -> Result<Option<(&'s Build, bool)>> {
    say("Checking the remote server", None);
    let out = command(&ssh::run(url, ctl, &ssh::have_server_cmd(&server.hash)))
        .stdin(Stdio::null())
        .output()
        .map_err(no_ssh)?;
    let said = String::from_utf8_lossy(&out.stdout);
    let mut lines = said.lines().map(str::trim);
    let (Some(system), Some(Ok(size))) = (lines.next(), lines.next().map(str::parse::<usize>))
    else {
        return Err(Error::Remote(
            match String::from_utf8_lossy(&out.stderr).trim() {
                "" => format!("cannot run a command on {}", url.host),
                why => why.to_string(),
            },
        ));
    };
    let build = server.for_host(&url.host, system)?;
    Ok((size != build.size).then_some((build, lines.next() == Some("xz"))))
}

/// Put the server built for the host on it, under the name `hash` every build of it shares.
fn upload(
    url: &Url,
    ctl: &Path,
    hash: &str,
    build: &Build,
    xz: bool,
    say: &dyn Fn(&str, Option<f64>),
) -> Result<()> {
    let (bytes, xz) = build.payload(xz)?;
    let total = bytes.len();
    say(
        &format!("Uploading the server (0 / {} MB)", mb(total)),
        Some(0.0),
    );
    let argv = ssh::run(url, ctl, &ssh::install_server_cmd(hash, build.size, xz));
    let progress = |done, total| match done < total {
        true => say(
            &format!(
                "Uploading the server ({} / {} MB)",
                mb(done as usize),
                mb(total as usize)
            ),
            Some(done as f64 / total as f64),
        ),
        // The bytes are all written, and the host is still unpacking them: without this the
        // bar would sit full for seconds under a message saying the upload had finished.
        false => say("Installing the server", None),
    };
    send(
        &argv,
        bytes.as_slice(),
        total as u64,
        &progress,
        &Stop::default(),
    )
    .map_err(|e| Error::Remote(format!("cannot install the server: {e}")))
}

/// Stops a copy to or from a host from another thread: [`Stop::stop`] kills the ssh child moving
/// the bytes, so the file in flight is cut short and lands nowhere (the host's temporary of
/// [`ssh::put_cmd`], a download's `.part`), and a caller copying several files asks
/// [`Stop::stopped`] between them.
#[derive(Clone, Default)]
pub struct Stop(Arc<Stopping>);

#[derive(Default)]
struct Stopping {
    stopped: AtomicBool,
    /// The child moving bytes right now, if one is.
    child: Mutex<Option<Child>>,
}

impl Stop {
    /// Kill the child moving bytes, and every one held after this.
    pub fn stop(&self) {
        self.0.stopped.store(true, Ordering::SeqCst);
        if let Some(child) = locked(&self.0.child).as_mut() {
            let _ = child.kill();
        }
    }

    pub fn stopped(&self) -> bool {
        self.0.stopped.load(Ordering::SeqCst)
    }

    /// Run `work` while a stop can kill `child`, then hand the child back to be waited for. It is
    /// taken back before it is reaped, so a kill never reaches a process given its pid since.
    pub(crate) fn holding<T>(&self, child: Child, work: impl FnOnce() -> T) -> (Child, T) {
        {
            let mut held = locked(&self.0.child);
            let child = held.insert(child);
            // A stop that came before the child did, and found nothing to kill.
            if self.stopped() {
                let _ = child.kill();
            }
        }
        let answer = work();
        let child = locked(&self.0.child).take().expect("the child held above");
        (child, answer)
    }
}

/// Run `argv` and stream `input` in chunks, reporting bytes written of `total`, until `stop`.
/// On write failure, use SSH stderr rather than the resulting broken-pipe error.
/// SSH exit 255 maps to [`Error::Offline`] and [`ssh::TAKEN`] to `AlreadyExists`, the command
/// naming what was taken; other failures belong to the command.
pub(crate) fn send(
    argv: &[String],
    mut input: impl Read,
    total: u64,
    progress: &dyn Fn(u64, u64),
    stop: &Stop,
) -> Result<()> {
    let mut child = command(argv)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(no_ssh)?;
    let stdin = child.stdin.take();
    let (child, written) = stop.holding(child, || -> std::io::Result<()> {
        // Dropped on the way out, which closes the pipe: the host's `cat` ends there.
        let mut stdin = stdin.ok_or_else(|| std::io::Error::other("ssh has no stdin"))?;
        let (mut buf, mut done) = (vec![0; CHUNK], 0);
        loop {
            let n = input.read(&mut buf)?;
            if n == 0 {
                return Ok(());
            }
            stdin.write_all(&buf[..n])?;
            done += n as u64;
            progress(done, total);
        }
    });
    let out = child.wait_with_output().map_err(no_ssh)?;
    let said = String::from_utf8_lossy(&out.stderr).trim().to_string();
    match (written, out.status.success()) {
        (Ok(()), true) => Ok(()),
        _ if out.status.code() == Some(255) => Err(Error::Offline(said)),
        _ if out.status.code() == Some(ssh::TAKEN) => {
            Err(accent_core::Error::AlreadyExists(said).into())
        }
        (Err(e), _) if said.is_empty() => Err(Error::Remote(e.to_string())),
        _ => Err(Error::Remote(said)),
    }
}

fn mb(bytes: usize) -> String {
    format!("{:.1}", bytes as f64 / (1024.0 * 1024.0))
}

// ------------------------------------------------------------------- one at a time

/// Per-host attempts, serialized by a lock held throughout the attempt.
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
    outcome: Result<()>,
}

impl Ended {
    /// Allow reuse only for the same master. A caller allowed to prompt must
    /// retry a quiet failure, since its passphrase may succeed where BatchMode failed.
    fn answers(&self, ctl: &Path, quiet: bool) -> bool {
        self.ctl == ctl && (self.outcome.is_ok() || quiet || !self.quiet)
    }
}

/// Run `work` unless a compatible attempt completed while this caller waited.
/// Key by host authority, not socket, so separate vault and terminal masters
/// cannot upload over each other.
fn once(authority: &str, ctl: &Path, quiet: bool, work: impl FnOnce() -> Result<()>) -> Result<()> {
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

    /// Commands on a host name the server last made ready there, not this machine's build
    /// as it is now: a rebuild since is not on the host until the next prepare puts it there.
    #[test]
    fn commands_name_the_server_last_prepared_over_their_master() {
        let url = |address: &str| ssh::parse(address).unwrap();
        let (a, b) = (url("ssh://a.test/vault"), url("ssh://b.test/vault"));
        made_ready(&a.authority(), "0123456789abcdef");
        made_ready(&b.authority(), "fedcba9876543210");
        made_ready(&a.authority(), "00000000000000aa");
        // Any path on the host: the servers are the login's, not a vault's.
        assert_eq!(prepared(&url("ssh://a.test/")).unwrap(), "00000000000000aa");
        assert_eq!(prepared(&b).unwrap(), "fedcba9876543210");
    }

    #[test]
    fn a_server_upload_uses_only_a_matching_archive() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("accent-cli");
        let raw = b"a server binary";
        std::fs::write(&path, raw).unwrap();
        let mut build = Build {
            arch: "x86_64",
            path: path.clone(),
            hash: ssh::hash_of(raw),
            size: raw.len(),
        };
        assert_eq!(build.payload(true).unwrap(), (raw.to_vec(), false));
        let script =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../build-aux/compress-server.py");
        assert!(
            Command::new("python3")
                .arg(script)
                .arg(&path)
                .status()
                .unwrap()
                .success()
        );
        let archive = path.with_extension("xz");
        let packed = std::fs::read(&archive).unwrap();
        assert_eq!(build.payload(true).unwrap(), (packed.clone(), true));
        assert_eq!(build.payload(false).unwrap(), (raw.to_vec(), false));

        // A rebuild can leave an older archive beside a binary of the same size.
        let rebuilt = b"a newer binary!";
        assert_eq!(rebuilt.len(), raw.len());
        std::fs::write(&path, rebuilt).unwrap();
        assert!(build.payload(true).is_err());
        build.hash = ssh::hash_of(rebuilt);
        assert_eq!(build.payload(true).unwrap(), (rebuilt.to_vec(), false));

        std::fs::write(&path, raw).unwrap();
        build.hash = ssh::hash_of(raw);
        std::fs::write(&archive, &packed[..packed.len() - 1]).unwrap();
        assert_eq!(build.payload(true).unwrap(), (raw.to_vec(), false));
        std::fs::write(&archive, packed).unwrap();
        std::fs::remove_file(path.with_extension("xz.sha256")).unwrap();
        assert_eq!(build.payload(true).unwrap(), (raw.to_vec(), false));
    }

    /// ssh ending on its own (255) is the link that went; the command failing is the host
    /// refusing.
    #[test]
    fn a_transfer_the_link_dropped_is_offline() {
        let sent = |script: &str| {
            let argv = ["sh", "-c", script].map(String::from);
            send(&argv, &b"bytes"[..], 5, &|_, _| (), &Stop::default()).unwrap_err()
        };
        assert_eq!(
            sent("cat >/dev/null; echo lost >&2; exit 255"),
            Error::Offline("lost".to_string())
        );
        assert_eq!(
            sent("cat >/dev/null; echo no >&2; exit 1"),
            Error::Remote("no".to_string())
        );
    }

    /// A host gets the build for its machine; any other kind of host is refused, naming what it
    /// runs and what the server is built for, and a build that left a machine out says so.
    #[test]
    fn a_host_gets_the_server_built_for_its_machine() {
        let build = |arch| Build {
            arch,
            path: PathBuf::new(),
            hash: String::new(),
            size: 1,
        };
        let server = Server {
            hash: "0123456789abcdef".to_string(),
            builds: vec![build("x86_64"), build("aarch64")],
        };
        for arch in ["x86_64", "aarch64"] {
            let picked = server.for_host("box", &format!("Linux {arch}"));
            assert_eq!(picked.map(|b| b.arch), Ok(arch));
        }
        for other in [
            "Linux armv7l",
            "Linux riscv64",
            "Darwin arm64",
            "FreeBSD amd64",
            "",
        ] {
            assert_eq!(
                server.for_host("box", other).map(|b| b.arch),
                Err(Error::Refused(format!(
                    "box runs {other}, and accent's server is built for Linux on x86_64 and \
                     aarch64 only"
                )))
            );
        }
        let one = Server {
            builds: vec![build("x86_64")],
            ..server
        };
        assert_eq!(
            one.for_host("box", "Linux aarch64").map(|b| b.arch),
            Err(Error::Refused(
                "box runs Linux aarch64, and this build carries the server for x86_64 hosts \
                 only: run `make server`"
                    .to_string()
            ))
        );
    }

    /// Long enough for a caller spawned now to be queued behind the attempt that is running.
    const QUEUED: Duration = Duration::from_millis(100);

    const CTL: &str = "/run/user/1000/accent/0123456789abcdef";

    type Outcome = Result<()>;

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

    /// ssh that has ended before its input is written — refused by the host, as a lapsed login is
    /// — leaves a broken pipe to write into; the reason is what it said.
    #[test]
    fn a_refused_transfer_says_why_rather_than_broken_pipe() {
        let argv = [
            "sh",
            "-c",
            "echo 'Permission denied (publickey).' >&2; exit 255",
        ]
        .map(String::from);
        let err = send(
            &argv,
            &[0u8; 1 << 20][..],
            1 << 20,
            &|_, _| (),
            &Stop::default(),
        )
        .unwrap_err();
        assert_eq!(err.to_string(), "Permission denied (publickey).");
    }

    /// A stop kills the child moving the bytes, so a copy that would run for half a minute ends at
    /// once, mid-file; one stopped before it starts ends at once too.
    #[test]
    fn a_stop_cuts_a_transfer_short_mid_file() {
        // `exec`, so the child is the one holding the pipe, as ssh is.
        let argv = ["sh", "-c", "exec sleep 30"].map(String::from);
        let endless = || std::io::repeat(0);
        let stop = Stop::default();
        let t = std::time::Instant::now();
        std::thread::scope(|s| {
            s.spawn(|| {
                std::thread::sleep(QUEUED);
                stop.stop();
            });
            assert!(send(&argv, endless(), u64::MAX, &|_, _| (), &stop).is_err());
        });
        assert!(stop.stopped());
        assert!(send(&argv, endless(), u64::MAX, &|_, _| (), &stop).is_err());
        assert!(t.elapsed() < Duration::from_secs(10), "{:?}", t.elapsed());
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
            Err(Error::Remote("gone".to_string()))
        });
        assert!(ran);
        assert_eq!(again, Err(Error::Remote("gone".to_string())));

        // A quiet attempt fails where a prompt might have got through, so a caller that may
        // prompt is not handed that failure.
        std::thread::scope(|s| {
            let (release, quiet) = held(s, "a-quiet-host", true);
            let loud = s.spawn(|| once("a-quiet-host", ctl, false, || Ok(())));
            std::thread::sleep(QUEUED);
            release
                .send(Err(Error::Remote("no agent".to_string())))
                .unwrap();
            assert_eq!(
                quiet.join().unwrap(),
                Err(Error::Remote("no agent".to_string()))
            );
            assert_eq!(loud.join().unwrap(), Ok(()));
        });
    }
}
