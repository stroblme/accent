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

use std::collections::HashMap;
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, RwLock};

use serde_json::json;

use crate::link;
use crate::rpc::{Client, Hello, RpcError};
use crate::ssh::{self, Forward, Url};
use crate::{Event, VaultConfig};

/// A call on the main thread that takes longer than this has cost the windows a frame.
const FRAME: std::time::Duration = std::time::Duration::from_millis(16);

/// Where a call in flight leaves its request id, so a caller that gives up on the answer can
/// cancel it at the server.
pub type Asked = Mutex<Option<u64>>;

/// Where a remote vault has got to. The UI shows the first two as the wait it already knows how
/// to show, and the third as a banner with a way back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    Connecting,
    Connected,
    Disconnected(String),
}

/// Why an attempt to connect failed, which decides whether another attempt is worth making.
enum Failure {
    /// Anything on the way to the vault: ssh, the link, the upload, a server that went quiet.
    Link(String),
    /// The host's `serve` answered the `hello` and will not serve the vault.
    Refused(String),
}

impl From<String> for Failure {
    fn from(why: String) -> Failure {
        Failure::Link(why)
    }
}

impl From<&str> for Failure {
    fn from(why: &str) -> Failure {
        Failure::Link(why.to_string())
    }
}

/// What [`Remote::push`] made of a cached copy that has been written to.
pub enum Pushed {
    /// The bytes are on the host, and the copy is stamped with what the host says now.
    Sent,
    /// The host's file moved on since the copy was fetched, so it was left as it is and what had
    /// been written went up *beside* it under this name in the vault: a conflict copy the tree
    /// lists, the reader opens and either of the two can be deleted.
    Conflict(String),
    /// Neither could go to the host, so what was written is only on this machine — at this path,
    /// for this reason.
    Kept(PathBuf, String),
}

/// How many `(edited)` copies of one document the host may already hold before a refusal gives up
/// and keeps the bytes here instead. Each one costs a `stat` round trip to rule out, and twenty
/// unread copies of the same document is a reader with a different problem.
const EDITED_COPIES: usize = 20;

/// `notes/doc.pdf` -> `notes/doc (edited).pdf`, and `notes/doc (edited 2).pdf` for the next one:
/// where a copy the host would not take over the original goes instead, whether it was drawn on
/// or had pages moved, put in or taken out.
pub fn edited_name(rel: &str, nth: usize) -> String {
    let (stem, ext) = match rel.rsplit_once('.') {
        // A dot in a directory's name is not this file's extension.
        Some((stem, ext)) if !ext.contains('/') => (stem, format!(".{ext}")),
        _ => (rel, String::new()),
    };
    match nth {
        1 => format!("{stem} (edited){ext}"),
        n => format!("{stem} (edited {n}){ext}"),
    }
}

/// Where a cached copy is put when the host will not take it anywhere: beside itself, under a
/// name no fetch writes over, keeping the extension so it still opens in whatever reads that
/// kind. The last resort only — a refusal the host accepts a copy of lands in the vault, where
/// the reader can reach it.
fn kept_path(dest: &Path) -> PathBuf {
    match dest.extension() {
        Some(ext) => dest.with_extension(format!("kept.{}", ext.to_string_lossy())),
        None => dest.with_extension("kept"),
    }
}

/// Copy the written-on cache file somewhere no fetch overwrites, and say why it had to stay here.
fn keep(dest: &Path, why: String) -> Pushed {
    match std::fs::copy(dest, kept_path(dest)) {
        Ok(_) => Pushed::Kept(kept_path(dest), why),
        // Nowhere left to put it: the cached copy itself holds the bytes until the next fetch.
        Err(e) => Pushed::Kept(dest.to_path_buf(), format!("{why}; {e}")),
    }
}

/// One vault on a remote host.
pub struct Remote {
    url: Url,
    ctl: PathBuf,
    /// The vault root as the server canonicalised it. Empty until `hello` answers, which is why
    /// it is behind a lock: every path the UI shows is relative to this.
    root: RwLock<PathBuf>,
    config: Mutex<VaultConfig>,
    /// Ghost text is a global preference rather than a per-vault one, so it travels beside the
    /// config on `hello` instead of inside it, and a reconnect carries it again.
    ghost: Mutex<bool>,
    client: Mutex<Option<Arc<Client>>>,
    /// The documents the window has open, as the server was last told about them: the language
    /// they were opened as, and the text they were last sent with.
    ///
    /// A reconnect reaches a `serve` that has never heard of them, so every `change_document`,
    /// `completion` or `hover` about a tab that is still open would come back "not open" until
    /// that tab was closed and opened again. Kept here rather than in the façade because this is
    /// the only place that knows a connection has been replaced.
    docs: Mutex<HashMap<String, (String, String)>>,
    /// The forwards the window started and has not stopped. They live in the ssh master, so a
    /// link that drops takes them with it and the master a reconnect makes has none of them;
    /// [`connect`](Self::connect) puts them back. Nothing outlives the vault: closing it cancels
    /// every one still here.
    forwards: Mutex<Vec<Forward>>,
    /// Shared with the rpc reader thread, which is the first to know the link has gone.
    state: Arc<Mutex<State>>,
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
            ghost: Mutex::new(true),
            client: Mutex::new(None),
            docs: Mutex::new(HashMap::new()),
            forwards: Mutex::new(Vec::new()),
            state: Arc::new(Mutex::new(State::Connecting)),
            child: Mutex::new(None),
            events,
        });
        remote.clone().start(false);
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
        let ghost = *self.locked(&self.ghost);
        let _ = self.call::<serde_json::Value>("hello", json!([cfg, ghost]));
    }

    pub fn set_ghost(&self, on: bool) {
        *self.locked(&self.ghost) = on;
        let cfg = self.config();
        let _ = self.call::<serde_json::Value>("hello", json!([cfg, on]));
    }

    /// Try again after a failure. The master usually survives whatever killed the server, so the
    /// second attempt is normally the fast one.
    pub fn reconnect(self: &Arc<Self>) {
        self.retry(false);
    }

    /// The same, for an attempt nobody asked for: the window's own retries after a dropped link.
    /// It never prompts, so a key that wants a passphrase fails it instead of raising a dialog;
    /// the attempt that may ask is [`reconnect`](Self::reconnect).
    pub fn reconnect_quietly(self: &Arc<Self>) {
        self.retry(true);
    }

    fn retry(self: &Arc<Self>, quiet: bool) {
        let mut state = self.locked(&self.state);
        if *state == State::Connecting {
            return;
        }
        *state = State::Connecting;
        drop(state);
        self.clone().start(quiet);
    }

    // ------------------------------------------------------------- calling

    /// Ask the server. Waits while a connection is still being made, so the first paint of a
    /// window does not have to be ordered against it.
    pub fn call<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<T, RpcError> {
        self.call_within(method, params, crate::rpc::DEADLINE)
    }

    /// The same, waiting up to `deadline` for the answer: for a method the host itself may take
    /// longer over than [`crate::rpc::DEADLINE`].
    pub fn call_within<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        params: serde_json::Value,
        deadline: std::time::Duration,
    ) -> Result<T, RpcError> {
        self.call_tracked(method, params, &Asked::default(), deadline)
    }

    /// The same, leaving the request id in `asked` for [`cancel`](Self::cancel).
    pub fn call_tracked<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        params: serde_json::Value,
        asked: &Asked,
        deadline: std::time::Duration,
    ) -> Result<T, RpcError> {
        self.remember(method, &params);
        let client = self.wait_for_client()?;
        let asked_at = std::time::Instant::now();
        let answer = client.call_tracked(method, params, asked, deadline);
        // Every window shares the one GTK main thread, so a round trip made there stalls them
        // all. `RUST_LOG=accent_api::remote=debug` names each one that cost a frame.
        let took = asked_at.elapsed();
        if took > FRAME && std::thread::current().name() == Some("main") {
            tracing::debug!(method, ms = took.as_millis() as u64, "held the main thread");
        }
        if answer.is_err() && client.is_dead() {
            self.disconnect(&self.lost(), Event::Disconnected);
        }
        answer
    }

    /// Tell the server nobody is waiting for that request any more. Nothing to do if it never
    /// got as far as being asked, or if the connection has gone since.
    pub fn cancel(&self, asked: &Asked) {
        let Some(id) = *self.locked(asked) else {
            return;
        };
        if let Some(client) = self.locked(&self.client).clone() {
            client.cancel(id);
        }
    }

    /// Keep what the server would have to be told again, so a new one can be.
    ///
    /// Recorded whether or not the call lands: a document opened while the link was down is one
    /// the window has open, and the reconnect is exactly when the server has to hear about it.
    fn remember(&self, method: &str, params: &serde_json::Value) {
        if !method.ends_with("_document") {
            return;
        }
        let at = |n: usize| params.get(n).and_then(serde_json::Value::as_str);
        let Some(rel) = at(0).map(str::to_string) else {
            return;
        };
        let mut docs = self.locked(&self.docs);
        match method {
            "open_document" => {
                if let (Some(language), Some(text)) = (at(1), at(2)) {
                    docs.insert(rel, (language.to_string(), text.to_string()));
                }
            }
            "change_document" => {
                if let (Some(text), Some(held)) = (at(1), docs.get_mut(&rel)) {
                    held.1 = text.to_string();
                }
            }
            "close_document" => {
                docs.remove(&rel);
            }
            _ => {}
        }
    }

    /// Tell a freshly started server about the documents the window still has open.
    ///
    /// All at once, a thread each: the server answers every request on a thread of its own, so
    /// ten tabs cost the reconnect one round trip rather than ten.
    fn reopen(&self, client: &Client) {
        let open: Vec<(String, String, String)> = self
            .locked(&self.docs)
            .iter()
            .map(|(rel, (language, text))| (rel.clone(), language.clone(), text.clone()))
            .collect();
        if open.is_empty() {
            return;
        }
        self.say("Reopening the documents");
        std::thread::scope(|s| {
            for (rel, language, text) in &open {
                s.spawn(move || {
                    // One that will not reopen is one tab without a language, not a failed
                    // connection.
                    if let Err(e) = client
                        .call::<serde_json::Value>("open_document", json!([rel, language, text]))
                    {
                        tracing::warn!("reopening {rel} on the new server: {e}");
                    }
                });
            }
        });
    }

    /// The client, or why there is not one.
    ///
    /// A call made while the link is still being established is refused on the spot rather than
    /// made to wait for it. The wait used to be up to [`crate::rpc::DEADLINE`], and the caller is
    /// as often as not the GTK main loop, so a slow host froze the window it had just opened — and
    /// an upload longer than the deadline made every call fail anyway, saying "not connected"
    /// about a connection that was still being made. The UI hears [`Event::Connected`] and asks
    /// again; there is nothing here worth blocking a frame for.
    fn wait_for_client(&self) -> Result<Arc<Client>, RpcError> {
        match &*self.locked(&self.state) {
            State::Connecting => {
                return Err(RpcError {
                    code: crate::rpc::CONNECTING,
                    message: "still connecting".to_string(),
                    data: None,
                });
            }
            State::Disconnected(why) => return Err(RpcError::disconnected(why)),
            State::Connected => {}
        }
        match self.locked(&self.client).clone() {
            Some(client) => Ok(client),
            None => Err(RpcError::disconnected("not connected")),
        }
    }

    // -------------------------------------------------------------- files

    /// A local path holding this file's current bytes, downloading it when what we have is stale.
    ///
    /// This is how the PDF viewer, the image tab and the preview's assets reach a remote vault:
    /// they need a real file, and the protocol deliberately carries no bytes. The etag decides —
    /// same as on disk, no transfer.
    pub fn fetch(&self, rel: &str) -> std::io::Result<PathBuf> {
        let (Some(dest), Some(stamp)) = (
            ssh::cache_path(&self.url, rel),
            ssh::stamp_path(&self.url, rel),
        ) else {
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
        let cached = std::fs::read(&stamp)
            .ok()
            .and_then(|b| serde_json::from_slice::<crate::Etag>(&b).ok());
        if cached == Some(current) && dest.exists() {
            return Ok(dest);
        }

        for dir in [dest.parent(), stamp.parent()].into_iter().flatten() {
            std::fs::create_dir_all(dir)?;
        }
        let out = self.ssh_output(&format!("cat {}", ssh::quote(&self.remote_path(rel))))?;
        std::fs::write(&dest, out)?;
        let _ = std::fs::write(&stamp, serde_json::to_vec(&current).unwrap_or_default());
        Ok(dest)
    }

    /// Put a cached copy back on the host: the other half of [`fetch`](Self::fetch), for the
    /// readers that write into the file they were handed rather than through the vault — the PDF
    /// pen, Add Page, Export Highlights.
    ///
    /// The host's etag is checked against the one the copy was fetched at, because the copy was
    /// drawn on without the host knowing: a file that moved under it is a conflict for the reader
    /// to settle, not one to overwrite. On a match the copy goes up and the stamp is written
    /// again from what the host says afterwards, so the fetch that follows the host's own watcher
    /// event does not pull our bytes back over a page that is still being drawn on. On a mismatch
    /// it goes [beside](Self::push_beside) the original instead.
    ///
    /// `edited` is the copy an earlier refusal of this same document already left on the host, so
    /// that a reader who keeps drawing writes that one again rather than a numbered copy per
    /// stroke.
    pub fn push(&self, rel: &str, edited: Option<&str>) -> std::io::Result<Pushed> {
        let (Some(dest), Some(stamp)) = (
            ssh::cache_path(&self.url, rel),
            ssh::stamp_path(&self.url, rel),
        ) else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("{rel} is outside the vault"),
            ));
        };
        let current: Option<crate::Etag> = self
            .call("stat", json!([rel]))
            .map_err(RpcError::io_error)?;
        let fetched = std::fs::read(&stamp)
            .ok()
            .and_then(|b| serde_json::from_slice::<crate::Etag>(&b).ok());
        if current != fetched {
            return Ok(self.push_beside(&dest, rel, edited));
        }
        self.upload(&dest, rel)?;
        if let Ok(Some(now)) = self.call::<Option<crate::Etag>>("stat", json!([rel])) {
            let _ = std::fs::write(&stamp, serde_json::to_vec(&now).unwrap_or_default());
        }
        Ok(Pushed::Sent)
    }

    /// The refusal's other half: the written-on copy goes up as `<name> (edited).pdf` in the same
    /// folder, the way a note's conflict copy lands in the vault, and the original is not touched.
    /// It is then a file like any other — the tree lists it, it opens and it syncs — so the reader
    /// can hold the two against each other and delete one, where a copy left in the ssh cache was
    /// reachable only through the text of a toast.
    ///
    /// A second conflict on the same document takes the next free number rather than writing over
    /// `(edited)`, whose changes nobody has looked at yet; only the refusals of one conflict, which
    /// carry `edited`, write the same copy again. If not even the copy can go up, the bytes stay
    /// here — the one place a `.kept.pdf` still appears — and the toast says so.
    fn push_beside(&self, dest: &Path, rel: &str, edited: Option<&str>) -> Pushed {
        let named = match edited {
            Some(name) => Ok(name.to_string()),
            None => self.free_edited_name(rel),
        };
        match named.and_then(|name| self.upload(dest, &name).map(|()| name)) {
            Ok(name) => Pushed::Conflict(name),
            Err(e) => keep(dest, e.to_string()),
        }
    }

    /// The first of `<name> (edited).pdf`, `<name> (edited 2).pdf`, … the host does not hold.
    fn free_edited_name(&self, rel: &str) -> std::io::Result<String> {
        for nth in 1..=EDITED_COPIES {
            let name = edited_name(rel, nth);
            let held: Option<crate::Etag> = self
                .call("stat", json!([&name]))
                .map_err(RpcError::io_error)?;
            if held.is_none() {
                return Ok(name);
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("{EDITED_COPIES} edited copies of it are already there"),
        ))
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

    /// One ssh invocation that may prompt through a dialog: see [`link::command`].
    fn ssh(&self, argv: &[String]) -> Command {
        link::command(argv)
    }

    /// Ask ssh to start forwarding a port, either way, over the master that is already open.
    /// Nothing is spawned: the running master takes the instruction and keeps it.
    pub fn forward(&self, f: Forward) -> Result<(), String> {
        self.control(ssh::forward(&self.url, &self.ctl, f))?;
        let mut forwards = self.locked(&self.forwards);
        if !forwards.contains(&f) {
            forwards.push(f);
        }
        Ok(())
    }

    /// Stop a forward. It is forgotten whatever ssh answers: a master that has died took the
    /// forward with it, and a reconnect must not bring back one the window stopped.
    pub fn cancel_forward(&self, f: Forward) -> Result<(), String> {
        self.locked(&self.forwards).retain(|kept| *kept != f);
        self.control(ssh::cancel(&self.url, &self.ctl, f))
    }

    /// Put the forwards back on the master [`connect`](Self::connect) found or made. A master that
    /// survived the drop still holds them and answers OK to a forward it already has, so this is
    /// the same call either way. One that will not come back is said, not fatal: the vault is up.
    fn restore_forwards(&self) {
        let forwards = self.locked(&self.forwards).clone();
        if forwards.is_empty() {
            return;
        }
        self.say("Restoring the forwards");
        for f in forwards {
            if let Err(e) = self.control(ssh::forward(&self.url, &self.ctl, f)) {
                let _ = self
                    .events
                    .send(Event::Error(format!("Cannot restore a forward: {e}")));
            }
        }
    }

    fn control(&self, argv: Vec<String>) -> Result<(), String> {
        let out = self
            .ssh(&argv)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("cannot run ssh: {e}"))?;
        match out.status.success() {
            true => Ok(()),
            false => Err(match String::from_utf8_lossy(&out.stderr).trim() {
                "" => "ssh refused".to_string(),
                why => why.to_string(),
            }),
        }
    }

    fn ssh_output(&self, command: &str) -> std::io::Result<Vec<u8>> {
        let out = self
            .ssh(&ssh::run(&self.url, &self.ctl, command))
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
        let mut child = self
            .ssh(&ssh::run(&self.url, &self.ctl, command))
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

    fn start(self: Arc<Self>, quiet: bool) {
        let _ = std::thread::Builder::new()
            .name("accent-connect".to_string())
            .spawn(move || match self.connect(quiet) {
                Ok(()) => {
                    // Said under the lock, so a loss the reader reports at once lands after it.
                    // One it saw before now, while the documents were reopened or the forwards
                    // put back, found the state still Connecting, which it does not speak for;
                    // the client it left dead is what says so here instead.
                    let mut state = self.locked(&self.state);
                    let dead = self
                        .locked(&self.client)
                        .as_ref()
                        .is_none_or(|c| c.is_dead());
                    let event = match dead {
                        false => {
                            *state = State::Connected;
                            Event::Connected
                        }
                        true => {
                            *state = State::Disconnected(self.lost());
                            Event::Disconnected(self.lost())
                        }
                    };
                    let _ = self.events.send(event);
                }
                Err(Failure::Link(why)) => self.disconnect(&why, Event::Disconnected),
                Err(Failure::Refused(why)) => self.disconnect(&why, Event::Refused),
            });
    }

    fn say(&self, what: &str) {
        self.step(what, None);
    }

    /// The same message with a measure on it, for the one step of a connection whose length is
    /// known before it starts.
    fn step(&self, what: &str, fraction: Option<f64>) {
        let _ = self.events.send(Event::Connecting {
            what: what.to_string(),
            fraction,
        });
    }

    /// Say the vault is not answering, as `event` says it: a link that went, or a refusal.
    fn disconnect(&self, why: &str, event: fn(String) -> Event) {
        let mut state = self.locked(&self.state);
        if matches!(&*state, State::Disconnected(_)) {
            return;
        }
        *state = State::Disconnected(why.to_string());
        drop(state);
        let _ = self.events.send(event(why.to_string()));
    }

    /// What a dropped link is called, whichever side notices it first.
    fn lost(&self) -> String {
        format!("Lost the connection to {}", self.url.host)
    }

    /// What the rpc reader runs when the server's output ends unasked.
    ///
    /// It holds the state and the channel rather than the `Remote`: were it the last holder,
    /// dropping it there would run [`teardown`](Self::teardown), which joins the very thread it is
    /// on. Only a connection that was up can be lost: one still being made fails its own `hello`
    /// and says why, or is found dead where [`start`](Self::start) would have said it was up.
    fn on_lost(&self) -> Box<dyn FnOnce() + Send> {
        let (state, events, why) = (self.state.clone(), self.events.clone(), self.lost());
        Box::new(move || {
            let mut state = crate::locked(&state);
            if *state == State::Connected {
                *state = State::Disconnected(why.clone());
                let _ = events.send(Event::Disconnected(why));
            }
        })
    }

    fn connect(&self, quiet: bool) -> Result<(), Failure> {
        // Whatever the last attempt left running goes first: `spawn_server` overwrites both slots,
        // so without this a retry would leak an ssh child and a reader thread every time.
        self.teardown();
        link::prepare(&self.url, &self.ctl, quiet, &|what, fraction| {
            self.step(what, fraction)
        })?;
        self.spawn_server(&link::server()?.hash)?;
        self.restore_forwards();
        Ok(())
    }

    fn spawn_server(&self, hash: &str) -> Result<(), Failure> {
        self.say("Opening the vault");
        let command = ssh::serve_cmd(&ssh::server_path(hash), &self.url.path);
        let mut child = self
            .ssh(&ssh::run(&self.url, &self.ctl, &command))
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
            self.on_lost(),
        ));
        let hello: Hello = client
            .call("hello", json!([self.config(), *self.locked(&self.ghost)]))
            // The server's own refusal ("/srv/x is not a folder") or the link's failure.
            .map_err(|e| {
                let why = format!("cannot open the vault on {}: {e}", self.url.host);
                match e.code {
                    crate::rpc::REFUSED => Failure::Refused(why),
                    _ => Failure::Link(why),
                }
            })?;
        *self.root.write().unwrap_or_else(|e| e.into_inner()) = hello.root;
        self.reopen(&client);
        *self.locked(&self.client) = Some(client);
        Ok(())
    }

    fn locked<'a, T>(&self, m: &'a Mutex<T>) -> std::sync::MutexGuard<'a, T> {
        crate::locked(m)
    }

    fn read_lock<'a, T>(&self, m: &'a RwLock<T>) -> std::sync::RwLockReadGuard<'a, T> {
        m.read().unwrap_or_else(|e| e.into_inner())
    }

    /// Let go of the server this connection had: the rpc client, and the ssh process carrying it.
    ///
    /// Both the window closing and a reconnect come through here, which is what stops a retry
    /// leaving a zombie ssh and a reader thread behind for every attempt.
    ///
    /// The order is what keeps a window closable. The writer goes first, so `serve` sees EOF and
    /// exits of its own accord; the child is given a second to follow and killed if it does not —
    /// a `Child` that is never waited for being the zombie this phase exists to avoid. Only then
    /// is the reader thread joined, because it ends when ssh's stdout closes, and a wedged ssh
    /// would otherwise hold the join for the fifteen seconds of a ServerAlive timeout, or forever.
    fn teardown(&self) {
        let client = self.locked(&self.client).take();
        if let Some(client) = &client {
            client.close();
        }
        if let Some(mut child) = self.locked(&self.child).take() {
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
        if let Some(client) = &client {
            client.join();
        }
    }
}

impl Drop for Remote {
    /// Close the server, reap the ssh process, and cancel every forward, but leave the master.
    ///
    /// The master stays up for its ControlPersist minute on purpose, after the app has exited
    /// too, so reopening the vault within it skips the handshake and any passphrase. Nothing of
    /// the vault's rides it meanwhile: `serve` has had its EOF, and the forwards are cancelled one
    /// by one, because a master that is only lingering still holds every forward it was given.
    fn drop(&mut self) {
        self.teardown();
        for f in std::mem::take(&mut *self.locked(&self.forwards)) {
            let _ = self.control(ssh::cancel(&self.url, &self.ctl, f));
        }
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

#[cfg(test)]
mod tests {
    use super::{edited_name, kept_path};
    use std::path::Path;

    /// The copy a refused upload leaves behind keeps its extension, so whatever reads that kind
    /// of file still opens it, and it never lands on the name a fetch writes.
    #[test]
    fn a_kept_copy_is_named_beside_the_one_it_came_from() {
        assert_eq!(kept_path(Path::new("/c/a.pdf")), Path::new("/c/a.kept.pdf"));
        assert_eq!(kept_path(Path::new("/c/a")), Path::new("/c/a.kept"));
        assert_ne!(kept_path(Path::new("/c/a.pdf")), Path::new("/c/a.pdf"));
    }

    /// The conflict copy sits in the original's folder, under the original's extension, so the
    /// tree lists it beside what it came from and it opens in the same reader.
    #[test]
    fn an_edited_copy_is_named_beside_the_original() {
        assert_eq!(edited_name("a/doc.pdf", 1), "a/doc (edited).pdf");
        assert_eq!(edited_name("a/doc.pdf", 2), "a/doc (edited 2).pdf");
        assert_eq!(edited_name("doc", 1), "doc (edited)");
        // A dot in a folder's name is not the file's extension.
        assert_eq!(edited_name("a.d/doc", 1), "a.d/doc (edited)");
    }
}
