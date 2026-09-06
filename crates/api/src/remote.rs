//! A vault on another machine, reached over one ssh connection.
//!
//! The shape is Zed's and VS Code's: a headless copy of this very binary runs on the host as
//! `accent-cli serve`, and the window talks to it over the ssh session's stdio. Everything that
//! needs the files — the index, the watcher, search, git — runs there; the window keeps only what
//! belongs to the machine the person is sitting at, which is the session and the settings.
//!
//! Connecting takes seconds and may ask for a passphrase, so it never blocks the caller. Opening
//! returns at once and the work happens on a thread that reports through the same [`Event`]
//! channel the local worker uses, which is why a remote vault paints its window as fast as a
//! local one.
//!
//! Nothing here parses ssh's output or drives a pty. The system `ssh` binary owns authentication,
//! `~/.ssh/config`, ProxyJump and the agent; a passphrase prompt comes back to us through
//! `SSH_ASKPASS`, which the app answers with a dialog.

use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, Mutex, RwLock};

use serde_json::json;

use crate::rpc::{Client, Hello, RpcError};
use crate::ssh::{self, Url};
use crate::{Event, VaultConfig};

/// How much of the server binary goes out per write, and therefore how often the progress bar
/// moves while it is uploading.
const CHUNK: usize = 256 * 1024;

/// Where a remote vault has got to. The UI shows the first two as the wait it already knows how
/// to show, and the third as a banner with a way back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    Connecting,
    Connected,
    Disconnected(String),
}

/// One vault on a remote host.
pub struct Remote {
    url: Url,
    ctl: PathBuf,
    /// The vault root as the server canonicalised it. Empty until `hello` answers, which is why
    /// it is behind a lock: every path the UI shows is relative to this.
    root: RwLock<PathBuf>,
    config: Mutex<VaultConfig>,
    client: Mutex<Option<Arc<Client>>>,
    state: Mutex<State>,
    /// Woken when `state` changes, so a call made while connecting waits rather than failing.
    ready: Condvar,
    child: Mutex<Option<Child>>,
    events: Sender<Event>,
}

impl Remote {
    /// Start connecting. Returns immediately; watch the event channel for progress.
    pub fn open(url: Url, cfg: VaultConfig, events: Sender<Event>) -> Arc<Remote> {
        let ctl = ssh::control_path(&url);
        let remote = Arc::new(Remote {
            root: RwLock::new(url.path.clone()),
            url,
            ctl,
            config: Mutex::new(cfg),
            client: Mutex::new(None),
            state: Mutex::new(State::Connecting),
            ready: Condvar::new(),
            child: Mutex::new(None),
            events,
        });
        remote.clone().start();
        remote
    }

    pub fn url(&self) -> &Url {
        &self.url
    }

    pub fn control_path(&self) -> &Path {
        &self.ctl
    }

    pub fn root(&self) -> PathBuf {
        self.read_lock(&self.root).clone()
    }

    pub fn state(&self) -> State {
        self.locked(&self.state).clone()
    }

    pub fn config(&self) -> VaultConfig {
        self.locked(&self.config).clone()
    }

    pub fn set_config(&self, cfg: VaultConfig) {
        *self.locked(&self.config) = cfg.clone();
        // Best effort: the server takes it at `hello` too, so a call that fails here is corrected
        // by the next connection rather than lost.
        let _ = self.call::<serde_json::Value>("hello", json!([cfg]));
    }

    /// Try again after a failure. The master usually survives whatever killed the server, so the
    /// second attempt is normally the fast one.
    pub fn reconnect(self: &Arc<Self>) {
        if matches!(self.state(), State::Connecting) {
            return;
        }
        *self.locked(&self.state) = State::Connecting;
        self.clone().start();
    }

    // ------------------------------------------------------------- calling

    /// Ask the server. Waits while a connection is still being made, so the first paint of a
    /// window does not have to be ordered against it.
    pub fn call<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<T, RpcError> {
        let client = self.wait_for_client()?;
        let answer = client.call(method, params);
        if let Err(e) = &answer
            && client.is_dead()
        {
            self.disconnect(&e.message);
        }
        answer
    }

    fn wait_for_client(&self) -> Result<Arc<Client>, RpcError> {
        let mut state = self.locked(&self.state);
        while *state == State::Connecting {
            let (guard, timeout) = self
                .ready
                .wait_timeout(state, crate::rpc::DEADLINE)
                .unwrap_or_else(|e| e.into_inner());
            state = guard;
            if timeout.timed_out() {
                break;
            }
        }
        if let State::Disconnected(why) = &*state {
            return Err(RpcError {
                code: crate::rpc::FAILED,
                message: why.clone(),
                data: None,
            });
        }
        drop(state);
        match self.locked(&self.client).clone() {
            Some(client) => Ok(client),
            None => Err(RpcError {
                code: crate::rpc::FAILED,
                message: "not connected".to_string(),
                data: None,
            }),
        }
    }

    // -------------------------------------------------------------- files

    /// A local path holding this file's current bytes, downloading it when what we have is stale.
    ///
    /// This is how the PDF viewer, the image tab and the preview's assets reach a remote vault:
    /// they need a real file, and the protocol deliberately carries no bytes. The etag decides —
    /// same as on disk, no transfer.
    pub fn fetch(&self, rel: &str) -> std::io::Result<PathBuf> {
        let Some(dest) = ssh::cache_path(&self.url, rel) else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("{rel} is outside the vault"),
            ));
        };
        let current: Option<crate::Etag> = self
            .call("stat", json!([rel]))
            .map_err(RpcError::io_error)?;
        let Some(current) = current else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("{rel} is not in the vault"),
            ));
        };
        // The remote etag against the one the cached copy was written with. Size and mtime are
        // enough here: the inode is the remote's, and it is in the etag we stored.
        let stamp = dest.with_extension("etag");
        let cached = std::fs::read(&stamp)
            .ok()
            .and_then(|b| serde_json::from_slice::<crate::Etag>(&b).ok());
        if cached == Some(current) && dest.exists() {
            return Ok(dest);
        }

        if let Some(dir) = dest.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let out = self.ssh_output(&format!("cat {}", ssh::quote(&self.remote_path(rel))))?;
        std::fs::write(&dest, out)?;
        let _ = std::fs::write(&stamp, serde_json::to_vec(&current).unwrap_or_default());
        Ok(dest)
    }

    /// Copy a local file into the vault. The remote watcher indexes it as it lands.
    pub fn upload(&self, local: &Path, rel: &str) -> std::io::Result<()> {
        let bytes = std::fs::read(local)?;
        self.ssh_input(
            &format!("cat > {}", ssh::quote(&self.remote_path(rel))),
            &bytes,
        )
    }

    /// Copy a file out of the vault to somewhere on this machine.
    pub fn download(&self, rel: &str, dest: &Path) -> std::io::Result<()> {
        let out = self.ssh_output(&format!("cat {}", ssh::quote(&self.remote_path(rel))))?;
        std::fs::write(dest, out)
    }

    fn remote_path(&self, rel: &str) -> String {
        self.root().join(rel).to_string_lossy().into_owned()
    }

    // ---------------------------------------------------------------- ssh

    fn ssh_output(&self, command: &str) -> std::io::Result<Vec<u8>> {
        let argv = ssh::run(&self.url, &self.ctl, command);
        let out = Command::new(&argv[0])
            .args(&argv[1..])
            .stdin(Stdio::null())
            .output()?;
        match out.status.success() {
            true => Ok(out.stdout),
            false => Err(std::io::Error::other(
                String::from_utf8_lossy(&out.stderr).trim().to_string(),
            )),
        }
    }

    fn ssh_input(&self, command: &str, bytes: &[u8]) -> std::io::Result<()> {
        let argv = ssh::run(&self.url, &self.ctl, command);
        let mut child = Command::new(&argv[0])
            .args(&argv[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()?;
        child
            .stdin
            .take()
            .ok_or_else(|| std::io::Error::other("ssh has no stdin"))?
            .write_all(bytes)?;
        let out = child.wait_with_output()?;
        match out.status.success() {
            true => Ok(()),
            false => Err(std::io::Error::other(
                String::from_utf8_lossy(&out.stderr).trim().to_string(),
            )),
        }
    }

    // ----------------------------------------------------------- connect

    fn start(self: Arc<Self>) {
        let _ = std::thread::Builder::new()
            .name("accent-connect".to_string())
            .spawn(move || match self.connect() {
                Ok(()) => {
                    *self.locked(&self.state) = State::Connected;
                    self.ready.notify_all();
                    let _ = self.events.send(Event::Connected);
                }
                Err(e) => self.disconnect(&e),
            });
    }

    fn say(&self, what: &str) {
        let _ = self.events.send(Event::Connecting(what.to_string()));
    }

    fn disconnect(&self, why: &str) {
        let mut state = self.locked(&self.state);
        if matches!(&*state, State::Disconnected(_)) {
            return;
        }
        *state = State::Disconnected(why.to_string());
        drop(state);
        self.ready.notify_all();
        let _ = self.events.send(Event::Disconnected(why.to_string()));
    }

    fn connect(&self) -> Result<(), String> {
        self.say(&format!("Connecting to {}", self.url.host));
        if let Some(dir) = self.ctl.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let argv = ssh::master(&self.url, &self.ctl);
        let out = Command::new(&argv[0])
            .args(&argv[1..])
            // The master must not read our stdin, and its own prompts go through SSH_ASKPASS.
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("cannot run ssh: {e}"))?;
        if !out.status.success() {
            let why = String::from_utf8_lossy(&out.stderr).trim().to_string();
            return Err(match why.is_empty() {
                true => format!("cannot connect to {}", self.url.host),
                false => why,
            });
        }

        self.provision()?;
        self.spawn_server()
    }

    /// Put the right server binary on the host, if it is not already there.
    fn provision(&self) -> Result<(), String> {
        self.say("Checking the remote server");
        let local = ssh::server_binary().map_err(|e| e.to_string())?;
        let bytes = std::fs::read(&local).map_err(|e| format!("{}: {e}", local.display()))?;
        let hash = ssh::hash_of(&bytes);

        if self.ssh_output(&ssh::have_server_cmd(&hash)).is_ok() {
            return Ok(());
        }

        let total = bytes.len();
        self.say(&format!("Uploading the server (0 / {} MB)", mb(total)));
        let argv = ssh::run(&self.url, &self.ctl, &ssh::install_server_cmd(&hash));
        let mut child = Command::new(&argv[0])
            .args(&argv[1..])
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
                self.say(&format!(
                    "Uploading the server ({} / {} MB)",
                    mb(done),
                    mb(total)
                ));
            }
        }
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

    fn spawn_server(&self) -> Result<(), String> {
        self.say("Opening the vault");
        let local = ssh::server_binary().map_err(|e| e.to_string())?;
        let bytes = std::fs::read(&local).map_err(|e| format!("{}: {e}", local.display()))?;
        let command = ssh::serve_cmd(&ssh::server_path(&ssh::hash_of(&bytes)), &self.url.path);
        let argv = ssh::run(&self.url, &self.ctl, &command);

        let mut child = Command::new(&argv[0])
            .args(&argv[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("cannot run ssh: {e}"))?;
        let stdin = child.stdin.take().ok_or("ssh has no stdin")?;
        let stdout = child.stdout.take().ok_or("ssh has no stdout")?;
        if let Some(stderr) = child.stderr.take() {
            drain(stderr);
        }
        *self.locked(&self.child) = Some(child);

        let client = Arc::new(Client::new(
            Box::new(stdin),
            Box::new(stdout),
            self.events.clone(),
        ));
        let hello: Hello = client
            .call("hello", json!([self.config()]))
            .map_err(|e| format!("the server did not answer: {e}"))?;
        *self.root.write().unwrap_or_else(|e| e.into_inner()) = hello.root;
        *self.locked(&self.client) = Some(client);
        Ok(())
    }

    fn locked<'a, T>(&self, m: &'a Mutex<T>) -> std::sync::MutexGuard<'a, T> {
        m.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn read_lock<'a, T>(&self, m: &'a RwLock<T>) -> std::sync::RwLockReadGuard<'a, T> {
        m.read().unwrap_or_else(|e| e.into_inner())
    }
}

impl Drop for Remote {
    /// Close the server, reap the ssh process, then take the master down with it.
    ///
    /// The order matters and so does the `wait`: a `Child` that is never waited for is the zombie
    /// this phase exists to avoid, and `-O exit` is what takes the shells and the port forwards
    /// with it rather than leaving them behind on the host.
    fn drop(&mut self) {
        if let Some(client) = self.locked(&self.client).take() {
            client.shutdown();
        }
        if let Some(mut child) = self.locked(&self.child).take() {
            // ssh exits on its own once the server sees EOF; kill it if it does not.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => break,
                    Ok(None) if std::time::Instant::now() < deadline => {
                        std::thread::sleep(std::time::Duration::from_millis(20))
                    }
                    _ => {
                        let _ = child.kill();
                        let _ = child.wait();
                        break;
                    }
                }
            }
        }
        let argv = ssh::exit(&self.url, &self.ctl);
        let _ = Command::new(&argv[0])
            .args(&argv[1..])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// Read a child's stderr into the log rather than letting it fill its pipe, which would wedge the
/// process it belongs to.
fn drain(stream: impl Read + Send + 'static) {
    let _ = std::thread::Builder::new()
        .name("accent-ssh-log".to_string())
        .spawn(move || {
            for line in std::io::BufReader::new(stream)
                .lines()
                .map_while(Result::ok)
            {
                tracing::debug!(target: "accent_api::remote", "ssh: {line}");
            }
        });
}

fn mb(bytes: usize) -> String {
    format!("{:.1}", bytes as f64 / (1024.0 * 1024.0))
}
