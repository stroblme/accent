//! A kernel we started: its process in a group of its own, its connection file and sockets in a
//! folder only the user can enter, and every message it sends, as [`Incoming`]s.

use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, BufReader};
use tokio::process::{Child, ChildStderr};
use tokio::sync::{mpsc, watch};

use crate::Error;
use crate::wire::{self, Header, Message, Signer};
use crate::zmtp::{self, Kind, Reader, Writer};

/// How long a kernel may take to answer: nbclient's `startup_timeout`.
const START: Duration = Duration::from_secs(60);
/// How long a kernel asked to shut down may take before it is killed.
const STOP: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    Shell,
    Iopub,
    Stdin,
    Control,
}

/// The channels and the ports the connection file gives them: the writable ones first, in the
/// order of [`Kernel`]'s `out`. The heartbeat's port 5 is bound by the kernel and never used.
const CHANNELS: [(Channel, u16); 4] = [
    (Channel::Shell, 1),
    (Channel::Stdin, 3),
    (Channel::Control, 4),
    (Channel::Iopub, 2),
];

#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    Message(Channel, Box<Message>),
    /// The kernel's process ended, with its exit code when it had one.
    Exited(Option<i32>),
}

pub type Incomings = mpsc::UnboundedReceiver<Incoming>;

pub struct Kernel {
    session: String,
    signer: Arc<Signer>,
    next: AtomicU64,
    /// Each writable channel's frames to send, in [`CHANNELS`]' order.
    out: [mpsc::UnboundedSender<Vec<Vec<u8>>>; 3],
    /// The kernel's process group, which is its process id.
    group: i32,
    folder: PathBuf,
    /// Held until the client ends, so another Accent can identify an abandoned folder.
    _lock: File,
    /// `Some` once its process has ended, with the exit code when it had one.
    ended: watch::Receiver<Option<Option<i32>>>,
    last_words: Arc<Mutex<Option<String>>>,
}

impl Kernel {
    /// Start `argv`, which takes `-f <connection file>` after it, in `cwd` with `env`, its
    /// folder under `under`. Answered once it has answered `kernel_info` and its IOPub reaches
    /// us, as jupyter_client's `wait_for_ready` waits, with that answer's content.
    pub async fn start(
        argv: &[String],
        cwd: &Path,
        env: &[(String, String)],
        under: &Path,
    ) -> Result<(Kernel, Incomings, Value), Error> {
        let key = wire::hex(&random::<32>()?);
        let (folder, lock) = folder(under)?;
        let mut child = match spawn(argv, cwd, env, &folder, &key) {
            Ok(child) => child,
            Err(e) => {
                let _ = std::fs::remove_dir_all(&folder);
                return Err(e.into());
            }
        };
        let last_words = Arc::new(Mutex::new(None));
        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(drain(stderr, last_words.clone()));
        }
        let group = child
            .id()
            .and_then(|id| i32::try_from(id).ok())
            .unwrap_or(0);
        let (incoming, mut incomings) = mpsc::unbounded_channel();
        let (end, ended) = watch::channel(None);
        tokio::spawn(watch(child, end, incoming.clone()));
        let (out, outboxes): (Vec<_>, Vec<_>) = (0..3).map(|_| mpsc::unbounded_channel()).unzip();
        // From here on a start that fails drops the kernel, which kills it and removes its folder.
        let kernel = Kernel {
            session: wire::hex(&random::<16>()?),
            signer: Arc::new(Signer::new(key.as_bytes())),
            next: AtomicU64::new(1),
            out: out.try_into().expect("an outbox per writable channel"),
            group,
            folder,
            _lock: lock,
            ended,
            last_words,
        };
        let started = Instant::now();
        let mut outboxes = outboxes.into_iter();
        for (channel, port) in CHANNELS {
            let kind = match channel {
                Channel::Iopub => Kind::Sub,
                _ => Kind::Dealer,
            };
            let path = kernel.folder.join(format!("k-{port}"));
            // Until it binds its sockets there is nothing to connect to: retried as it starts.
            let (reader, writer) = loop {
                match zmtp::connect(&path, kind, kernel.session.as_bytes()).await {
                    Ok(socket) => break socket,
                    Err(_) if kernel.ended.borrow().is_some() => {
                        return Err(Error::Exited(kernel.why()));
                    }
                    Err(_) if started.elapsed() > START => return Err(Error::Timeout(START)),
                    Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
                }
            };
            let (signer, incoming) = (kernel.signer.clone(), incoming.clone());
            match outboxes.next() {
                Some(outbox) => {
                    tokio::spawn(read(channel, reader, signer, incoming, ()));
                    tokio::spawn(write(writer, outbox));
                }
                // IOPub is only read, but a write half dropped shuts the socket's sending side,
                // which ZeroMQ takes for a disconnect: its reader holds it.
                None => {
                    tokio::spawn(read(channel, reader, signer, incoming, writer));
                }
            }
        }

        // Asked again each second until it answers, and until IOPub says anything at all: a SUB
        // hears nothing published before its subscription reached the kernel.
        let (mut info, mut heard) = (None, false);
        while info.is_none() || !heard {
            if started.elapsed() > START {
                return Err(Error::Timeout(START));
            }
            let asked = kernel.send(Channel::Shell, "kernel_info_request", json!({}), None)?;
            let second = tokio::time::Instant::now() + Duration::from_secs(1);
            while info.is_none() || !heard {
                let Ok(incoming) = tokio::time::timeout_at(second, incomings.recv()).await else {
                    break;
                };
                match incoming {
                    Some(Incoming::Message(Channel::Shell, msg))
                        if msg.parent.as_ref().is_some_and(|p| p.msg_id == asked) =>
                    {
                        info = Some(msg.content)
                    }
                    Some(Incoming::Message(Channel::Iopub, _)) => heard = true,
                    Some(Incoming::Message(..)) => {}
                    Some(Incoming::Exited(_)) | None => return Err(Error::Exited(kernel.why())),
                }
            }
        }
        Ok((kernel, incomings, info.unwrap_or_default()))
    }

    /// Send `msg_type` with `content` on `channel`, answering `parent` if any: its `msg_id`.
    pub fn send(
        &self,
        channel: Channel,
        msg_type: &str,
        content: Value,
        parent: Option<&Header>,
    ) -> Result<String, Error> {
        let at = CHANNELS.iter().position(|(c, _)| *c == channel);
        let Some(out) = at.and_then(|at| self.out.get(at)) else {
            return Err(io::Error::other("IOPub is not written to").into());
        };
        let n = self.next.fetch_add(1, Ordering::Relaxed);
        let header = Header::new(&self.session, n, msg_type);
        let id = header.msg_id.clone();
        let msg = Message {
            header,
            parent: parent.cloned(),
            metadata: json!({}),
            content,
        };
        out.send(wire::encode(&self.signer, &msg))
            .map_err(|_| Error::Closed)?;
        Ok(id)
    }

    /// Interrupt what it runs, as Jupyter's `interrupt_mode: signal` does: SIGINT to its group.
    pub fn interrupt(&self) -> io::Result<()> {
        self.signal(libc::SIGINT)
    }

    /// Ask it to shut down, killing its group past [`STOP`], and remove its folder.
    pub async fn shutdown(&self) {
        let _ = self.send(
            Channel::Control,
            "shutdown_request",
            json!({"restart": false}),
            None,
        );
        let mut ended = self.ended.clone();
        let over = ended.wait_for(Option::is_some);
        if tokio::time::timeout(STOP, over).await.is_err() {
            let _ = self.signal(libc::SIGKILL);
        }
        let _ = std::fs::remove_dir_all(&self.folder);
    }

    /// Remove folders left by a dead local client. A live client holds each folder's lock;
    /// folders without one predate this marker and cannot be judged safely here.
    pub fn sweep(under: &Path) -> io::Result<()> {
        // SAFETY: geteuid has no pointers or mutable process state.
        let owner = unsafe { libc::geteuid() };
        for entry in std::fs::read_dir(under)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir()
                || !entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("accent-kernel-")
                || entry.metadata()?.uid() != owner
            {
                continue;
            }
            let lock = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(entry.path().join("owner.lock"));
            let Ok(lock) = lock else { continue };
            let marker = lock.metadata()?;
            if !marker.is_file() || marker.uid() != owner {
                continue;
            }
            // SAFETY: flock only receives the open file's descriptor and plain lock flags.
            if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                std::fs::remove_dir_all(entry.path())?;
            }
        }
        Ok(())
    }

    /// Its last line on stderr: why one that ended did, such as `No module named ipykernel`.
    pub fn last_words(&self) -> Option<String> {
        self.last_words.lock().ok()?.clone()
    }

    /// Why it ended: its last words, else how it exited.
    fn why(&self) -> String {
        match (self.last_words(), *self.ended.borrow()) {
            (Some(words), _) => words,
            (None, Some(Some(code))) => format!("exit code {code}"),
            (None, _) => "it was killed".to_string(),
        }
    }

    /// Signal the kernel's group while its process runs: once it has ended, the number may be
    /// another's.
    fn signal(&self, signal: i32) -> io::Result<()> {
        if self.ended.borrow().is_some() || self.group <= 0 {
            return Err(io::Error::other("the kernel has ended"));
        }
        // SAFETY: killpg takes plain integers; the group is the kernel's own, made at its spawn.
        match unsafe { libc::killpg(self.group, signal) } {
            0 => Ok(()),
            _ => Err(io::Error::last_os_error()),
        }
    }

    #[cfg(test)]
    fn pid(&self) -> i32 {
        self.group
    }
}

/// A kernel dropped without [`Kernel::shutdown`] is killed, and its folder removed.
impl Drop for Kernel {
    fn drop(&mut self) {
        let _ = self.signal(libc::SIGKILL);
        let _ = std::fs::remove_dir_all(&self.folder);
    }
}

/// Write the connection file into `folder` and start `argv` on it, in a process group of its own
/// so an interrupt reaches what it runs.
fn spawn(
    argv: &[String],
    cwd: &Path,
    env: &[(String, String)],
    folder: &Path,
    key: &str,
) -> io::Result<Child> {
    let Some((exe, args)) = argv.split_first() else {
        return Err(io::Error::other("no kernel command"));
    };
    let file = folder.join("kernel.json");
    let info = json!({
        "transport": "ipc", "ip": folder.join("k"),
        "shell_port": 1, "iopub_port": 2, "stdin_port": 3, "control_port": 4, "hb_port": 5,
        "key": key, "signature_scheme": "hmac-sha256", "kernel_name": "",
    });
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&file)?
        .write_all(info.to_string().as_bytes())?;
    let mut command = tokio::process::Command::new(exe);
    command
        .args(args)
        .arg("-f")
        .arg(&file)
        .current_dir(cwd)
        .envs(env.iter().map(|(k, v)| (k, v)))
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // Killed as we end, whatever the way: a process that exits drops nothing, and ipykernel 6
    // only notices its parent gone once its parent is PID 1, which a subreaper such as
    // `systemd --user` never lets it become. The signal comes when the spawning thread ends: a
    // runtime worker, which lasts as long as the process.
    #[cfg(target_os = "linux")]
    // SAFETY: between fork and exec, prctl alone, which is async-signal-safe.
    unsafe {
        command.pre_exec(
            || match libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) {
                0 => Ok(()),
                _ => Err(io::Error::last_os_error()),
            },
        );
    }
    command.spawn()
}

/// A fresh folder under `under` that only the user can enter.
fn folder(under: &Path) -> io::Result<(PathBuf, File)> {
    loop {
        let folder = under.join(format!("accent-kernel-{}", wire::hex(&random::<4>()?)));
        match std::fs::DirBuilder::new().mode(0o700).create(&folder) {
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
            Ok(()) => {
                let lock = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(folder.join("owner.lock"))
                    .and_then(|lock| {
                        // SAFETY: flock only receives the new file's descriptor and lock flags.
                        match unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } {
                            0 => Ok(lock),
                            _ => Err(io::Error::last_os_error()),
                        }
                    });
                return lock.map(|lock| (folder.clone(), lock)).inspect_err(|_| {
                    let _ = std::fs::remove_dir_all(folder);
                });
            }
        }
    }
}

fn random<const N: usize>() -> io::Result<[u8; N]> {
    let mut bytes = [0; N];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes)
}

/// Kernels log freely on stderr; a full pipe would block them, so it is always drained.
async fn drain(stderr: ChildStderr, last_words: Arc<Mutex<Option<String>>>) {
    let mut lines = BufReader::new(stderr).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        tracing::debug!(target: "accent_jupyter::stderr", "{line}");
        if !line.trim().is_empty()
            && let Ok(mut words) = last_words.lock()
        {
            *words = Some(line);
        }
    }
}

/// Wait for the kernel's end and say so: the one way its death is heard.
async fn watch(
    mut child: Child,
    end: watch::Sender<Option<Option<i32>>>,
    incoming: mpsc::UnboundedSender<Incoming>,
) {
    let code = child.wait().await.ok().and_then(|status| status.code());
    let _ = end.send(Some(code));
    let _ = incoming.send(Incoming::Exited(code));
}

/// Read `channel`'s messages until it closes, holding `_held` as long.
async fn read<R: AsyncRead + Unpin>(
    channel: Channel,
    mut reader: Reader<R>,
    signer: Arc<Signer>,
    incoming: mpsc::UnboundedSender<Incoming>,
    _held: impl Send,
) {
    while let Ok(frames) = reader.recv().await {
        match wire::decode(&signer, frames) {
            Ok(msg) => {
                if incoming
                    .send(Incoming::Message(channel, Box::new(msg)))
                    .is_err()
                {
                    return;
                }
            }
            Err(e) => tracing::debug!("dropped a message on {channel:?}: {e}"),
        }
    }
}

async fn write<W: AsyncWrite + Unpin>(
    mut writer: Writer<W>,
    mut outbox: mpsc::UnboundedReceiver<Vec<Vec<u8>>>,
) {
    while let Some(frames) = outbox.recv().await {
        if writer.send(&frames).await.is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_start_reclaims_abandoned_kernel_folders_without_touching_live_ones() {
        let tmp = tempfile::tempdir().unwrap();
        let (live, live_lock) = folder(tmp.path()).unwrap();
        let (stale, stale_lock) = folder(tmp.path()).unwrap();
        std::fs::write(stale.join("kernel.json"), "secret").unwrap();
        let old = tmp.path().join("accent-kernel-old");
        std::fs::create_dir(&old).unwrap();
        drop(stale_lock);

        Kernel::sweep(tmp.path()).unwrap();
        assert!(live.exists());
        assert!(!stale.exists());
        assert!(old.exists(), "an old folder has no safe liveness marker");

        drop(live_lock);
        // A concurrent fork can briefly inherit the lock before its exec closes it.
        let deadline = Instant::now() + Duration::from_secs(2);
        while live.exists() && Instant::now() < deadline {
            Kernel::sweep(tmp.path()).unwrap();
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!live.exists());
    }

    fn has_ipykernel() -> bool {
        std::process::Command::new("python3")
            .args(["-c", "import ipykernel"])
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    /// The first incoming `want` picks within `within`, past whatever else comes.
    async fn next(
        incomings: &mut Incomings,
        within: u64,
        want: impl Fn(&Incoming) -> bool,
    ) -> Incoming {
        let wait = async {
            loop {
                let incoming = incomings.recv().await.expect("the kernel's news ended");
                if want(&incoming) {
                    return incoming;
                }
            }
        };
        tokio::time::timeout(Duration::from_secs(within), wait)
            .await
            .expect("in time")
    }

    /// A `msg_type` caused by our message `id`.
    fn caused(incoming: &Incoming, id: &str, msg_type: &str) -> bool {
        matches!(incoming, Incoming::Message(_, msg)
            if msg.header.msg_type == msg_type
                && msg.parent.as_ref().is_some_and(|p| p.msg_id == id))
    }

    fn content(incoming: Incoming) -> (Header, Value) {
        match incoming {
            Incoming::Message(_, msg) => (msg.header, msg.content),
            Incoming::Exited(code) => panic!("the kernel exited: {code:?}"),
        }
    }

    /// Against the ipykernel installed here, when there is one; the user's `~/.ipython` is left
    /// alone.
    #[test]
    fn an_ipykernel_runs_code_reads_input_and_is_interrupted() {
        if !has_ipykernel() {
            eprintln!("ipykernel is not installed: skipping the end-to-end test");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let ipython = tmp.path().join("ipython").to_string_lossy().into_owned();
        let env = [("IPYTHONDIR".to_string(), ipython)];
        let argv = ["python3", "-m", "ipykernel_launcher"].map(String::from);
        accent_lsp::runtime().block_on(async {
            let started = Kernel::start(&argv, tmp.path(), &env, tmp.path()).await;
            let (kernel, mut incomings, info) = started.unwrap();
            assert_eq!(info["language_info"]["name"], "python", "{info}");
            let execute = |code: &str| {
                let request = json!({
                    "code": code, "silent": false, "store_history": true,
                    "user_expressions": {}, "allow_stdin": true, "stop_on_error": true,
                });
                kernel
                    .send(Channel::Shell, "execute_request", request, None)
                    .unwrap()
            };

            let id = execute("print(1+1)");
            let printed = next(&mut incomings, 30, |i| caused(i, &id, "stream")).await;
            assert_eq!(content(printed).1["text"], "2\n");

            let id = execute("x = input('? ')");
            let asked = next(&mut incomings, 30, |i| caused(i, &id, "input_request")).await;
            let (asking, prompt) = content(asked);
            assert_eq!(prompt["prompt"], "? ");
            let answer = json!({"value": "abc"});
            kernel
                .send(Channel::Stdin, "input_reply", answer, Some(&asking))
                .unwrap();
            let id = execute("x");
            let result = next(&mut incomings, 30, |i| caused(i, &id, "execute_result")).await;
            assert_eq!(content(result).1["data"]["text/plain"], "'abc'");

            let id = execute("import time; time.sleep(30)");
            next(&mut incomings, 30, |i| caused(i, &id, "execute_input")).await;
            kernel.interrupt().unwrap();
            let error = next(&mut incomings, 5, |i| caused(i, &id, "error")).await;
            assert_eq!(content(error).1["ename"], "KeyboardInterrupt");

            let (pid, folder) = (kernel.pid(), kernel.folder.clone());
            kernel.shutdown().await;
            next(&mut incomings, 5, |i| matches!(i, Incoming::Exited(_))).await;
            assert!(!folder.exists(), "its folder is removed");
            // SAFETY: signal 0 only asks whether the process is there.
            assert_ne!(unsafe { libc::kill(pid, 0) }, 0, "the kernel is gone");
        });
    }
}
