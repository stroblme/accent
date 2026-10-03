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

use std::collections::{BTreeSet, HashMap};
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, RwLock};

use serde_json::json;

use crate::link::{self, Failure};
use crate::rpc::{Client, Hello, RpcError};
use crate::ssh::{self, Forward, Url};
use crate::{Event, VaultConfig};

/// A call on the main thread that takes longer than this has cost the windows a frame.
const FRAME: std::time::Duration = std::time::Duration::from_millis(16);

/// How much of a file a transfer moves between two reports of how far it has got.
const CHUNK: usize = 256 * 1024;

/// How many documents a reconnect reopens at once: see [`Remote::reopen`].
const REOPENING: usize = 8;

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

/// What [`Remote::push`] made of a cached copy that has been written to.
pub enum Pushed {
    /// The bytes are on the host, and the copy is stamped with what the host says now.
    Sent,
    /// The host's file moved on since the copy was fetched, so it was left as it is and what had
    /// been written went up *beside* it under this name in the vault: a conflict copy the tree
    /// lists, and either of the two can be deleted. The cached copy is that file's now, so the
    /// reader moves onto it, as onto a renamed file, and draws on there.
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

/// Move a cached file or folder to where the host moved its original, making the folders it goes
/// into. Nothing cached there is nothing to move, and a move that fails costs the next fetch a
/// download.
fn carry(from: &Path, to: &Path) {
    if !from.exists() {
        return;
    }
    let moved = to
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| std::fs::rename(from, to));
    if let Err(e) = moved {
        tracing::debug!("moving the cached {}: {e}", from.display());
    }
}

/// What a cached copy was when it was last fetched or pushed: the host's etag then, and the
/// copy's own. A copy whose own etag has moved since was written here — the pen, a page edit —
/// and holds what the host has not had.
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
struct Stamped {
    host: crate::Etag,
    copy: crate::Etag,
}

/// Whether the copy at `dest` was written here since `stamped`, so its bytes are on this machine
/// only. A copy never stamped is taken as fetched: there is nothing to tell it by.
fn unsent(dest: &Path, stamped: Option<Stamped>) -> bool {
    stamped.is_some_and(|s| crate::Etag::of(dest).is_ok_and(|now| now != s.copy))
}

/// The stamp of one cached copy, locked for as long as this is held: see [`Stamped`].
///
/// A fetch and a push each hold it from the `stat` that decides what to do until the stamp is
/// written, so two of one file take turns. Two fetches racing on a burst of watcher events could
/// otherwise land the older bytes under the newer stamp, and the fetch the host's report of a push
/// sets off could pull the pushed bytes back over a page still being drawn on.
struct Stamp(std::fs::File);

impl Stamp {
    fn hold(path: &Path) -> std::io::Result<Stamp> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        file.lock()?;
        Ok(Stamp(file))
    }

    /// `None` for a copy never fetched. Read once per hold: it reads from the file's position.
    fn read(&self) -> Option<Stamped> {
        serde_json::from_reader(&self.0).ok()
    }

    /// Written in place: a file renamed over it would be one the next holder is not waiting on.
    fn set(&self, stamped: &Stamped) -> std::io::Result<()> {
        use std::os::unix::fs::FileExt;
        self.0.set_len(0)?;
        self.0.write_all_at(&serde_json::to_vec(stamped)?, 0)
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
    /// config on `hello` instead of inside it, and a reconnect carries it again. So do word
    /// suggestions.
    ghost: Mutex<bool>,
    words: Mutex<bool>,
    /// Held while a preference change's `hello` is read and sent ([`Remote::send_hello`]).
    hellos: Mutex<()>,
    client: Mutex<Option<Arc<Client>>>,
    /// The documents the window has open, as the server was last told about them: the language
    /// they were opened as, and the text they were last sent with.
    ///
    /// A reconnect reaches a `serve` that has never heard of them, so every `change_document`,
    /// `completion` or `hover` about a tab that is still open would come back "not open" until
    /// that tab was closed and opened again. Kept here rather than in the façade because this is
    /// the only place that knows a connection has been replaced.
    docs: Mutex<HashMap<String, (String, String)>>,
    /// The unindexed folders the window asked to have watched, which a new server has to be
    /// asked again for the same reason. See [`Vault::watch_unindexed`](crate::Vault::watch_unindexed).
    unindexed: Mutex<BTreeSet<String>>,
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
            words: Mutex::new(true),
            hellos: Mutex::new(()),
            client: Mutex::new(None),
            docs: Mutex::new(HashMap::new()),
            unindexed: Mutex::new(BTreeSet::new()),
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

    pub fn set_config(self: &Arc<Self>, cfg: VaultConfig) {
        *self.locked(&self.config) = cfg;
        self.send_hello();
    }

    /// Told on every preference change, so only a change is sent on.
    pub fn set_ghost(self: &Arc<Self>, on: bool) {
        if std::mem::replace(&mut *self.locked(&self.ghost), on) != on {
            self.send_hello();
        }
    }

    /// As [`Self::set_ghost`].
    pub fn set_words(self: &Arc<Self>, on: bool) {
        if std::mem::replace(&mut *self.locked(&self.words), on) != on {
            self.send_hello();
        }
    }

    /// Tell the server the preferences as they now stand, from a thread of its own: the setters
    /// above are the main loop's, and a `hello` is a round trip. Recorded there in the order they
    /// were made, they are sent one `hello` at a time, each reading the values when its turn
    /// comes, so the last to reach the host carries the latest: Ghost Text switched off and
    /// straight on again ends on. Best effort: the server takes them at `hello` on connecting
    /// too, so one that fails is corrected by the next connection rather than lost.
    fn send_hello(self: &Arc<Self>) {
        let remote = self.clone();
        let _ = std::thread::Builder::new()
            .name("accent-hello".into())
            .spawn(move || {
                let _turn = remote.locked(&remote.hellos);
                let _ = remote.call::<serde_json::Value>("hello", remote.hello());
            });
    }

    /// What `hello` tells the server: the vault's config, then the global preferences it acts on.
    fn hello(&self) -> serde_json::Value {
        json!([
            self.config(),
            *self.locked(&self.ghost),
            *self.locked(&self.words)
        ])
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
        if method.ends_with("watch_unindexed") {
            let dirs: Vec<String> = params
                .get(0)
                .and_then(|d| serde_json::from_value(d.clone()).ok())
                .unwrap_or_default();
            let mut watched = self.locked(&self.unindexed);
            for dir in dirs {
                match method {
                    "watch_unindexed" => watched.insert(dir),
                    _ => watched.remove(&dir),
                };
            }
            return;
        }
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

    /// Tell a freshly started server about the documents the window still has open, and the
    /// folders it is watching.
    ///
    /// [`REOPENING`] at a time: the server answers every request on a thread of its own, so eight
    /// tabs cost the reconnect one round trip rather than eight, and a hundred cost thirteen
    /// rather than a hundred threads on each end.
    fn reopen(&self, client: &Client) {
        let unindexed: Vec<String> = self.locked(&self.unindexed).iter().cloned().collect();
        if !unindexed.is_empty()
            && let Err(e) = client.call::<serde_json::Value>("watch_unindexed", json!([unindexed]))
        {
            tracing::warn!("watching the unindexed folders on the new server: {e}");
        }
        let open: Vec<(String, String, String)> = self
            .locked(&self.docs)
            .iter()
            .map(|(rel, (language, text))| (rel.clone(), language.clone(), text.clone()))
            .collect();
        if open.is_empty() {
            return;
        }
        self.say("Reopening the documents");
        let next = Mutex::new(open.iter());
        std::thread::scope(|s| {
            for _ in 0..REOPENING.min(open.len()) {
                s.spawn(|| {
                    loop {
                        let Some((rel, language, text)) = self.locked(&next).next() else {
                            break;
                        };
                        // One that will not reopen is one tab without a language, not a failed
                        // connection.
                        if let Err(e) = client.call::<serde_json::Value>(
                            "open_document",
                            json!([rel, language, text]),
                        ) {
                            tracing::warn!("reopening {rel} on the new server: {e}");
                        }
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
    ///
    /// A copy written here since it was fetched or pushed is never fetched over: what the pen drew
    /// is in it and nowhere else. It is handed back as it is, for a [`push`](Self::push) to send,
    /// beside the host's file if that has moved on.
    pub fn fetch(&self, rel: &str) -> std::io::Result<PathBuf> {
        self.fetch_with(rel, &|_, _| ())
    }

    /// [`fetch`](Self::fetch), telling `progress` the bytes received so far and how many there
    /// are, as they arrive. Nothing is told when the cached copy is current.
    pub fn fetch_with(&self, rel: &str, progress: &dyn Fn(u64, u64)) -> std::io::Result<PathBuf> {
        let (Some(dest), Some(stamp)) = (
            ssh::cache_path(&self.url, rel),
            ssh::stamp_path(&self.url, rel),
        ) else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("{rel} is outside the vault"),
            ));
        };
        let stamp = Stamp::hold(&stamp)?;
        let stamped = stamp.read();
        if unsent(&dest, stamped) {
            return Ok(dest);
        }
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
        if stamped.map(|s| s.host) == Some(current) && dest.exists() {
            return Ok(dest);
        }

        if let Some(dir) = dest.parent() {
            std::fs::create_dir_all(dir)?;
        }
        self.receive(rel, &dest, current.size, progress)?;
        if let Ok(copy) = crate::Etag::of(&dest) {
            let _ = stamp.set(&Stamped {
                host: current,
                copy,
            });
        }
        Ok(dest)
    }

    /// Put a cached copy back on the host: the other half of [`fetch`](Self::fetch), for the
    /// readers that write into the file they were handed rather than through the vault — the PDF
    /// pen, the page edits, Export Highlights.
    ///
    /// A copy not written since it was fetched or last pushed has nothing to send, and costs no
    /// round trip. Otherwise the host's etag is checked against the one the copy was fetched at,
    /// because the copy was drawn on without the host knowing: a file that moved under it is a
    /// conflict for the reader to settle, not one to overwrite. On a match the copy goes up and
    /// the stamp is written again from what the host says afterwards and what was sent, so the
    /// fetch that follows the host's own watcher event does not pull our bytes back over a page
    /// that is still being drawn on, and a save that landed during the upload is still unsent. On
    /// a mismatch it goes [beside](Self::push_beside) the original instead.
    ///
    /// A file no longer on the host — moved or deleted there, while the copy was being written or
    /// was on its way — is `NotFound`, and nothing goes up: under the old name it would come back.
    /// The reader that follows a rename sends it again under the new one.
    pub fn push(&self, rel: &str) -> std::io::Result<Pushed> {
        let (Some(dest), Some(stamp)) = (
            ssh::cache_path(&self.url, rel),
            ssh::stamp_path(&self.url, rel),
        ) else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("{rel} is outside the vault"),
            ));
        };
        let stamp = Stamp::hold(&stamp)?;
        let stamped = stamp.read();
        if stamped.is_some() && !unsent(&dest, stamped) {
            return Ok(Pushed::Sent);
        }
        let current: Option<crate::Etag> = self
            .call("stat", json!([rel]))
            .map_err(RpcError::io_error)?;
        let Some(current) = current else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("{rel} was moved or deleted"),
            ));
        };
        if Some(current) != stamped.map(|s| s.host) {
            return Ok(self.push_beside(&dest, rel));
        }
        let copy = self.send(&dest, rel, true, &|_, _| ())?;
        if let Ok(Some(host)) = self.call::<Option<crate::Etag>>("stat", json!([rel])) {
            let _ = stamp.set(&Stamped { host, copy });
        }
        Ok(Pushed::Sent)
    }

    /// The cached copy of `from` and its stamp go to `to`, where the host has just moved the file
    /// or folder. A rename keeps the host's etag, so the copy is as current under the new name as
    /// under the old: a reader that follows the rename reads it there, and its next push finds the
    /// stamp rather than taking the renamed file for somebody else's change.
    pub fn moved(&self, from: &str, to: &str) {
        for place in [ssh::cache_path, ssh::stamp_path] {
            if let (Some(from), Some(to)) = (place(&self.url, from), place(&self.url, to)) {
                carry(&from, &to);
            }
        }
    }

    /// The refusal's other half: the written-on copy goes up as `<name> (edited).pdf` in the same
    /// folder, the way a note's conflict copy lands in the vault, and the original is not touched.
    /// It is then a file like any other — the tree lists it, it opens and it syncs — so the reader
    /// can hold the two against each other and delete one, where a copy left in the ssh cache was
    /// reachable only through the text of a toast. The cached copy goes with it, under its name
    /// and its stamp, as a rename carries one: the reader moves onto it and draws on there, and
    /// the original is fetched afresh when it is opened again.
    ///
    /// A second conflict takes the next free number rather than writing over `(edited)`, whose
    /// changes nobody has looked at yet. If not even the copy can go up, the bytes stay here — the
    /// one place a `.kept.pdf` still appears — and the toast says so.
    fn push_beside(&self, dest: &Path, rel: &str) -> Pushed {
        let sent = self.free_edited_name(rel).and_then(|name| {
            let copy = self.send(dest, &name, false, &|_, _| ())?;
            Ok((name, copy))
        });
        match sent {
            Ok((name, copy)) => {
                self.adopt(dest, &name, copy);
                Pushed::Conflict(name)
            }
            Err(e) => keep(dest, e.to_string()),
        }
    }

    /// The cached copy at `dest`, which went up to `name` as it was at `copy`, becomes `name`'s.
    /// A copy whose host etag cannot be had is left unstamped, which costs its next push another
    /// copy beside it rather than anything drawn.
    fn adopt(&self, dest: &Path, name: &str, copy: crate::Etag) {
        let (Some(to), Some(stamp)) = (
            ssh::cache_path(&self.url, name),
            ssh::stamp_path(&self.url, name),
        ) else {
            return;
        };
        let Ok(stamp) = Stamp::hold(&stamp) else {
            return;
        };
        carry(dest, &to);
        if let Ok(Some(host)) = self.call::<Option<crate::Etag>>("stat", json!([name])) {
            let _ = stamp.set(&Stamped { host, copy });
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
        self.upload_with(local, rel, &|_, _| ())
    }

    /// [`upload`](Self::upload), read and sent a chunk at a time, telling `progress` the bytes
    /// sent so far and how many there are.
    pub fn upload_with(
        &self,
        local: &Path,
        rel: &str,
        progress: &dyn Fn(u64, u64),
    ) -> std::io::Result<()> {
        self.send(local, rel, false, progress).map(|_| ())
    }

    /// A local file to `rel` on the host, replacing only a file still there when `replace` is
    /// set: see [`ssh::put_cmd`]. Answers with the etag of the file that was sent, which a save
    /// renaming another over `local` meanwhile does not change.
    fn send(
        &self,
        local: &Path,
        rel: &str,
        replace: bool,
        progress: &dyn Fn(u64, u64),
    ) -> std::io::Result<crate::Etag> {
        let file = std::fs::File::open(local)?;
        let sent = crate::Etag::from_meta(&file.metadata()?);
        link::send(
            &self.put_into(rel, sent.size, replace),
            file,
            sent.size,
            progress,
        )?;
        Ok(sent)
    }

    /// Write `bytes` to `rel` over the master, the way an upload goes: the host's shell creates a
    /// new file with the mode its umask gives a new note there. The host's index is then told, as
    /// a local write tells its own, so an image pasted into a note is found by its name for the
    /// embed and the preview at once, not on the host watcher's debounce, which raced the
    /// preview's. Its watcher takes the file in all the same, so a telling that fails is no
    /// failed write.
    pub fn write_file(&self, rel: &str, bytes: &[u8]) -> std::io::Result<()> {
        let size = bytes.len() as u64;
        link::send(&self.put_into(rel, size, false), bytes, size, &|_, _| ())?;
        if let Err(e) = self.call::<()>("wrote", json!([rel])) {
            tracing::debug!("telling the host's index of {rel}: {e}");
        }
        Ok(())
    }

    /// The command line that writes its stdin, `size` bytes of it, to `rel` on the host.
    fn put_into(&self, rel: &str, size: u64, replace: bool) -> Vec<String> {
        let command = ssh::put_cmd(&self.remote_path(rel), size, replace);
        ssh::run(&self.url, &self.ctl, &command)
    }

    /// Copy a file out of the vault to somewhere on this machine.
    pub fn download(&self, rel: &str, dest: &Path) -> std::io::Result<()> {
        self.download_with(rel, dest, &|_, _| ())
    }

    /// [`download`](Self::download), telling `progress` the bytes received so far and how many
    /// there are, as they arrive.
    pub fn download_with(
        &self,
        rel: &str,
        dest: &Path,
        progress: &dyn Fn(u64, u64),
    ) -> std::io::Result<()> {
        let size: Option<crate::Etag> = self
            .call("stat", json!([rel]))
            .map_err(RpcError::io_error)?;
        let size = size.map_or(0, |etag| etag.size);
        self.receive(rel, dest, size, progress)
    }

    /// Stream `rel` from the host into `dest` a chunk at a time, so a large file is never held
    /// whole here, telling `progress` the bytes so far of `total`. The bytes go to a `.part` file
    /// beside `dest` that takes its name once whole: a transfer cut off leaves the last good copy,
    /// and a reader holding the old one open keeps reading what it opened.
    fn receive(
        &self,
        rel: &str,
        dest: &Path,
        total: u64,
        progress: &dyn Fn(u64, u64),
    ) -> std::io::Result<()> {
        let mut child = self
            .ssh(&ssh::run(
                &self.url,
                &self.ctl,
                &format!("cat {}", ssh::quote(&self.remote_path(rel))),
            ))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let mut part = dest.as_os_str().to_owned();
        part.push(".part");
        let part = PathBuf::from(part);
        let received = (|| {
            let mut stdout = child
                .stdout
                .take()
                .ok_or_else(|| std::io::Error::other("ssh has no stdout"))?;
            let mut out = std::fs::File::create(&part)?;
            let (mut buf, mut done) = (vec![0; CHUNK], 0);
            loop {
                let n = stdout.read(&mut buf)?;
                if n == 0 {
                    return Ok(());
                }
                out.write_all(&buf[..n])?;
                done += n as u64;
                progress(done, total);
            }
        })();
        let out = child.wait_with_output()?;
        let finished = received.and_then(|()| match out.status.success() {
            true => std::fs::rename(&part, dest),
            false => Err(std::io::Error::other(
                String::from_utf8_lossy(&out.stderr).trim().to_string(),
            )),
        });
        if finished.is_err() {
            let _ = std::fs::remove_file(&part);
        }
        finished
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

    /// The forwards the window started and has not stopped, which is what the Ports pane lists.
    pub fn forwards(&self) -> Vec<Forward> {
        self.locked(&self.forwards).clone()
    }

    /// Put the forwards back on the master [`connect`](Self::connect) found or made. A master that
    /// survived the drop still holds them and answers OK to a forward it already has, so this is
    /// the same call either way. One that will not come back — its port taken meanwhile — is said
    /// and dropped, not fatal and not tried again: the vault is up, and the Ports pane, which
    /// lists [`forwards`](Self::forwards), shows what is.
    fn restore_forwards(&self) {
        let forwards = self.forwards();
        if forwards.is_empty() {
            return;
        }
        self.say("Restoring the forwards");
        for f in forwards {
            if let Err(e) = self.control(ssh::forward(&self.url, &self.ctl, f)) {
                self.locked(&self.forwards).retain(|kept| *kept != f);
                // ssh's first line, "… Port forwarding failed": a toast is one line.
                let why = e.lines().next().unwrap_or_default();
                let _ = self.events.send(Event::Error(format!(
                    "Cannot restore the forward {f}: {why}"
                )));
            }
        }
    }

    fn control(&self, argv: Vec<String>) -> Result<(), String> {
        control(&argv)
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
    /// It holds the state and the channel rather than the `Remote`, which would then never close:
    /// the reader ends only once the close has ended `serve`. Only a connection that was up can be
    /// lost: one still being made fails its own `hello`
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
        teardown(
            self.locked(&self.client).take(),
            self.locked(&self.child).take(),
        );
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
            .call("hello", self.hello())
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
}

impl Drop for Remote {
    /// Close the server, reap the ssh process, and cancel every forward, but leave the master.
    ///
    /// The master stays up for its ControlPersist minute on purpose, after the app has exited
    /// too, so reopening the vault within it skips the handshake and any passphrase. Nothing of
    /// the vault's rides it meanwhile: `serve` has had its EOF, and the forwards are cancelled one
    /// by one, because a master that is only lingering still holds every forward it was given.
    ///
    /// On a thread of its own, which [`finish_closing`] joins: the last holder is usually a window
    /// closing on the GTK main loop, and the close is a second's wait for a wedged server and an
    /// ssh process per forward, 120–140 ms with one forward on a 60 ms link.
    fn drop(&mut self) {
        let (client, child) = (
            self.locked(&self.client).take(),
            self.locked(&self.child).take(),
        );
        let cancels: Vec<Vec<String>> = std::mem::take(&mut *self.locked(&self.forwards))
            .into_iter()
            .map(|f| ssh::cancel(&self.url, &self.ctl, f))
            .collect();
        let closing = std::thread::Builder::new()
            .name("accent-close".to_string())
            .spawn(move || {
                teardown(client, child);
                for argv in cancels {
                    let _ = control(&argv);
                }
            });
        match closing {
            Ok(handle) => {
                let mut closing = crate::locked(&CLOSING);
                closing.retain(|h| !h.is_finished());
                closing.push(handle);
            }
            Err(e) => tracing::warn!("closing {}: {e}", self.url),
        }
    }
}

/// The closes of [`Remote`]s still running on their threads.
static CLOSING: Mutex<Vec<std::thread::JoinHandle<()>>> = Mutex::new(Vec::new());

/// Wait for every remote vault dropped so far to have closed: for the app's way out, which would
/// otherwise end the process before the forwards were cancelled and leave them on the lingering
/// master for its minute.
pub fn finish_closing() {
    for handle in std::mem::take(&mut *crate::locked(&CLOSING)) {
        let _ = handle.join();
    }
}

/// Let go of the server a connection had: the rpc client, and the ssh process carrying it.
///
/// Both a vault closing and a reconnect come through here, which is what stops a retry leaving a
/// zombie ssh and a reader thread behind for every attempt.
///
/// The order is what keeps a close finite. The writer goes first, so `serve` sees EOF and exits of
/// its own accord; the child is given a second to follow and killed if it does not — a `Child`
/// that is never waited for being the zombie this phase exists to avoid. Only then is the reader
/// thread joined, because it ends when ssh's stdout closes, and a wedged ssh would otherwise hold
/// the join for the fifteen seconds of a ServerAlive timeout, or forever.
fn teardown(client: Option<Arc<Client>>, child: Option<Child>) {
    if let Some(client) = &client {
        client.close();
    }
    if let Some(mut child) = child {
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

/// Run one `ssh -O` instruction to the master, saying why it was refused.
fn control(argv: &[String]) -> Result<(), String> {
    let out = link::command(argv)
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
    use super::{Stamp, Stamped, carry, edited_name, kept_path, unsent};
    use std::path::Path;
    use std::time::Duration;

    /// A renamed file's cached copy goes where the host put the file, into a folder the cache
    /// does not have yet; nothing cached is nothing to move.
    #[test]
    fn a_cached_copy_follows_a_rename() {
        let dir = tempfile::tempdir().unwrap();
        let (from, to) = (dir.path().join("a.pdf"), dir.path().join("new/b.pdf"));
        std::fs::write(&from, "pdf").unwrap();
        carry(&from, &to);
        assert_eq!(std::fs::read_to_string(&to).unwrap(), "pdf");
        assert!(!from.exists());

        let never = dir.path().join("elsewhere/c.pdf");
        carry(&from, &never);
        assert!(!never.parent().unwrap().exists());
    }

    /// A second fetch or push of one file waits for the first to write its stamp, and then reads
    /// that stamp rather than the one before it.
    #[test]
    fn a_stamp_is_held_by_one_transfer_at_a_time() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes/a.pdf");
        let first = Stamp::hold(&path).unwrap();
        assert_eq!(first.read(), None);

        let (tx, rx) = std::sync::mpsc::channel();
        let waiting = path.clone();
        std::thread::spawn(move || {
            let second = Stamp::hold(&waiting).unwrap();
            tx.send(second.read()).unwrap();
        });
        assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());

        let etag = |n| crate::Etag {
            mtime_ns: n,
            size: n as u64,
            ino: n as u64,
        };
        let long = Stamped {
            host: etag(1_000_000_000_000),
            copy: etag(1_000_000_000_000),
        };
        let short = Stamped {
            host: etag(1),
            copy: etag(2),
        };
        first.set(&long).unwrap();
        // Written in place, so a shorter one leaves nothing of the longer behind.
        first.set(&short).unwrap();
        drop(first);
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            Some(short)
        );
    }

    /// A copy written since it was stamped holds what the host has not had, which no fetch may
    /// overwrite; one moved along with its stamp, as a rename carries it, is still the same copy.
    #[test]
    fn a_copy_written_since_its_stamp_is_unsent() {
        let dir = tempfile::tempdir().unwrap();
        let (dest, moved) = (dir.path().join("a.pdf"), dir.path().join("b/a.pdf"));
        std::fs::write(&dest, "fetched").unwrap();
        let fetched = crate::Etag::of(&dest).unwrap();
        let stamped = Some(Stamped {
            host: fetched,
            copy: fetched,
        });
        assert!(!unsent(&dest, stamped));
        assert!(!unsent(&dest, None));

        carry(&dest, &moved);
        assert!(!unsent(&moved, stamped));

        // A save replaces the file by a rename, as the render thread's does.
        let saved = dir.path().join("b/.a.pdf.tmp");
        std::fs::write(&saved, "drawn on").unwrap();
        std::fs::rename(&saved, &moved).unwrap();
        assert!(unsent(&moved, stamped));
    }

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
