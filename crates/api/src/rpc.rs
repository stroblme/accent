//! The façade as JSON-RPC 2.0 over stdio, so a vault can live on another machine.
//!
//! One JSON object per line, in both directions: requests and responses carry an `id`, and the
//! server pushes the vault's [`Event`]s as `event` notifications, which have none. Newline framing
//! rather than `Content-Length` headers because the whole protocol then reads in a terminal and
//! `serve` can be driven by hand while debugging.
//!
//! Two rules keep it honest. File bytes never travel here — a note's text does, but a PDF or an
//! image is fetched over its own `cat`, because base64 through a JSON parser is neither fast nor
//! debuggable. And every call has a deadline: a request whose answer never comes must fail the one
//! caller waiting for it, not freeze the window.
//!
//! A link can also die without closing: the host never hears the TCP connection go, and `serve`
//! would sit on a stdin that neither ends nor speaks. So the client pings every [`PING`], and a
//! server that has been pinged once takes [`SILENCE`] without a word as the window gone.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;
use serde_json::{Value, json};

/// Who is waiting for which answer. One entry per call in flight, taken out by the reader thread
/// when the answer arrives and dropped wholesale when the connection dies.
type Waiting = Arc<Mutex<HashMap<u64, Sender<Result<Value, RpcError>>>>>;

use crate::{Event, Local, locked};

use accent_core::fs::{Etag, SaveError};

/// How long a call waits before giving up on the far end. Generous next to a round trip on any
/// link a person would edit over, and short enough that a wedged server is a message rather than
/// a hang. A method the host itself allows longer — a commit whose hooks run for a minute — is
/// given its own bound on top of this (the `methods!` table in `vault.rs` says which).
pub const DEADLINE: Duration = Duration::from_secs(10);

/// How often the client says it is still there.
pub const PING: Duration = Duration::from_secs(10);

/// How long a pinged server waits without hearing anything before it stops. Nine missed pings:
/// well past the 10–17 s a flaky VPN drops out for, and past ssh's own 15 s ServerAlive give-up,
/// after which the window has let go of this server anyway.
pub const SILENCE: Duration = Duration::from_secs(90);

/// How long a server that has stopped hearing from its client still waits for the requests it is
/// running: the longest a git call may take on the host — a sync, a pull and a push each under
/// `TRANSFER_TIMEOUT` — and a little over for the process group to be stopped.
const LINGER: Duration = Duration::from_secs(2 * accent_core::git::TRANSFER_TIMEOUT.as_secs() + 10);

/// A call the host bounds must finish inside [`LINGER`], or a server whose client has gone would
/// stop while still rewriting the vault. Checked where the two are written rather than left to a
/// run that would only show it on a slow link.
const _: () = assert!(
    crate::vault::REPLACE_BOUND.as_secs() <= LINGER.as_secs(),
    "a rewrite may outlast the wait a server gives the requests it is running"
);

/// A save refused because the file changed under it. Its `data` is the current [`Etag`], so the
/// client can rebuild [`SaveError::ChangedOnDisk`] and the UI can offer the same comparison it
/// offers locally.
pub const CHANGED_ON_DISK: i64 = -32001;
/// Anything that was an `io::Error` on the far side.
pub const IO: i64 = -32002;
/// Everything else, already formatted for a human.
pub const FAILED: i64 = -32000;
/// There is no link: it went away, or it was never made. Nothing was asked of anything, which is
/// what tells a failed save apart from one the disk refused.
pub const DISCONNECTED: i64 = -32004;
/// The link is still being made, so there is nobody to ask yet. Not a failure of the call: the
/// same call answers once [`Event::Connected`](crate::Event::Connected) has arrived.
pub const CONNECTING: i64 = -32003;
/// The server has no vault to serve — its root is not a folder, or its index would not open — and
/// says so to the `hello`. Unlike a link that failed, another attempt meets the same answer until
/// someone changes the host.
pub const REFUSED: i64 = -32005;

/// What the far end said instead of an answer.
#[derive(Debug, Clone)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    pub data: Option<Value>,
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for RpcError {}

impl RpcError {
    pub(crate) fn failed(message: impl std::fmt::Display) -> RpcError {
        RpcError {
            code: FAILED,
            message: message.to_string(),
            data: None,
        }
    }

    /// Nothing reached the far end, because there is no far end to reach.
    pub(crate) fn disconnected(message: impl std::fmt::Display) -> RpcError {
        RpcError {
            code: DISCONNECTED,
            message: message.to_string(),
            data: None,
        }
    }

    /// Whether the link, rather than the call, is what failed.
    pub fn is_offline(&self) -> bool {
        matches!(self.code, DISCONNECTED | CONNECTING)
    }

    /// The `SaveError` this stands for, so a remote save fails exactly as a local one does.
    pub fn save_error(self) -> SaveError {
        if self.is_offline() {
            return SaveError::Offline;
        }
        if self.code == CHANGED_ON_DISK
            && let Some(data) = self.data.clone()
            && let Ok(current) = serde_json::from_value::<Etag>(data)
        {
            return SaveError::ChangedOnDisk { current };
        }
        SaveError::Io(self.io_error())
    }

    /// The `io::Error` this stands for, kind and all.
    ///
    /// The kind travels in `data` because callers branch on it — the open path treats `NotFound`
    /// as "offer to create it" rather than as a failure — and everything used to arrive as
    /// `Other`, so the same file missing matched one way locally and another way remotely. No
    /// link at all is `NotConnected`, which the window already shows rather than reports.
    pub fn io_error(self) -> std::io::Error {
        if self.is_offline() {
            return std::io::Error::new(std::io::ErrorKind::NotConnected, self.message);
        }
        match self.data.as_ref().and_then(Value::as_str).and_then(kind_of) {
            Some(kind) => std::io::Error::new(kind, self.message),
            None => std::io::Error::other(self.message),
        }
    }
}

/// The `io::ErrorKind`s worth carrying, by name.
///
/// `ErrorKind` is neither serialisable nor exhaustively matchable, so the ones the app actually
/// branches on are named and everything else crosses as `Other` — which is what all of them used
/// to cross as.
const KINDS: &[(&str, std::io::ErrorKind)] = &[
    ("NotFound", std::io::ErrorKind::NotFound),
    ("PermissionDenied", std::io::ErrorKind::PermissionDenied),
    ("AlreadyExists", std::io::ErrorKind::AlreadyExists),
    ("InvalidInput", std::io::ErrorKind::InvalidInput),
    ("InvalidData", std::io::ErrorKind::InvalidData),
];

fn kind_name(kind: std::io::ErrorKind) -> Option<&'static str> {
    KINDS
        .iter()
        .find(|(_, k)| *k == kind)
        .map(|(name, _)| *name)
}

fn kind_of(name: &str) -> Option<std::io::ErrorKind> {
    KINDS.iter().find(|(n, _)| *n == name).map(|(_, k)| *k)
}

/// An `io::Error` as it crosses: the message a person reads, and the kind a caller matches on.
pub(crate) fn io_failure(e: &std::io::Error) -> RpcError {
    RpcError {
        code: IO,
        message: e.to_string(),
        data: kind_name(e.kind()).map(Value::from),
    }
}

/// What `hello` answers: who is on the other end, and where the vault really is.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Hello {
    /// The vault root as the *server* canonicalised it. Every `rel` is relative to this, and the
    /// UI needs the real spelling for the git pane and for anything it shows the user.
    pub root: std::path::PathBuf,
    pub version: String,
}

// ------------------------------------------------------------------- client

/// The near end of a `serve`: writes requests, routes answers back to whoever is waiting.
pub struct Client {
    /// Shared with the heartbeat, which writes its pings through the same lock.
    out: Arc<Mutex<Box<dyn Write + Send>>>,
    next_id: AtomicU64,
    pending: Waiting,
    dead: Arc<AtomicBool>,
    /// Set by [`close`](Self::close) before the pipe goes, so the reader can tell the end it was
    /// asked for from a link that was lost.
    closing: Arc<AtomicBool>,
    /// Stops the heartbeat. The reader sends on it too: a server that is gone needs no pings.
    stop: Sender<()>,
    /// The reader and the heartbeat.
    threads: Mutex<Vec<std::thread::JoinHandle<()>>>,
}

impl Client {
    /// Start routing. `events` receives the server's notifications; it is the same channel the
    /// local worker would have posted to, so nothing downstream can tell the two apart.
    ///
    /// `on_lost` runs once, on the reader thread, when the server's output ends without
    /// [`close`](Self::close) having asked for it: the link dropped, or the server died. It must
    /// not join this client, whose reader is the thread it runs on.
    pub fn new(
        out: Box<dyn Write + Send>,
        input: Box<dyn Read + Send>,
        events: Sender<Event>,
        on_lost: Box<dyn FnOnce() + Send>,
    ) -> Client {
        let out = Arc::new(Mutex::new(out));
        let pending: Waiting = Arc::new(Mutex::new(HashMap::new()));
        let dead = Arc::new(AtomicBool::new(false));
        let closing = Arc::new(AtomicBool::new(false));
        let (stop, stopped) = channel();

        let reader = std::thread::Builder::new()
            .name("accent-rpc".to_string())
            .spawn({
                let (pending, dead) = (pending.clone(), dead.clone());
                let (closing, stop) = (closing.clone(), stop.clone());
                move || {
                    for line in BufReader::new(input).lines() {
                        let Ok(line) = line else { break };
                        if line.trim().is_empty() {
                            continue;
                        }
                        match serde_json::from_str::<Value>(&line) {
                            Ok(msg) => route(msg, &pending, &events),
                            Err(e) => tracing::warn!("unreadable line from the server: {e}"),
                        }
                    }
                    // EOF: the server is gone. Everyone still waiting is told at once by having
                    // their sender dropped, rather than each of them spending the full deadline.
                    dead.store(true, Ordering::SeqCst);
                    locked(&pending).clear();
                    let _ = stop.send(());
                    if !closing.load(Ordering::SeqCst) {
                        on_lost();
                    }
                }
            })
            .ok();

        // The first ping goes at once: a link that drops before the second must still leave a
        // server that knows to stop.
        let heartbeat = std::thread::Builder::new()
            .name("accent-ping".to_string())
            .spawn({
                let out = out.clone();
                move || {
                    let ping = json!({"jsonrpc": "2.0", "method": "ping"});
                    while emit(&out, &ping).is_ok() {
                        if !matches!(stopped.recv_timeout(PING), Err(RecvTimeoutError::Timeout)) {
                            break;
                        }
                    }
                }
            })
            .ok();

        Client {
            out,
            next_id: AtomicU64::new(1),
            pending,
            dead,
            closing,
            stop,
            threads: Mutex::new(reader.into_iter().chain(heartbeat).collect()),
        }
    }

    pub fn is_dead(&self) -> bool {
        self.dead.load(Ordering::SeqCst)
    }

    /// Ask, and wait up to [`DEADLINE`] for the answer.
    pub fn call<T: DeserializeOwned>(&self, method: &str, params: Value) -> Result<T, RpcError> {
        self.call_tracked(method, params, &Mutex::new(None), DEADLINE)
    }

    /// The same, waiting up to `deadline` and leaving the request id in `asked`, so a caller that
    /// gives up on the answer can [`cancel`](Self::cancel) it at the server.
    pub fn call_tracked<T: DeserializeOwned>(
        &self,
        method: &str,
        params: Value,
        asked: &Mutex<Option<u64>>,
        deadline: Duration,
    ) -> Result<T, RpcError> {
        let value = self.call_value(method, params, asked, deadline)?;
        serde_json::from_value(value)
            .map_err(|e| RpcError::failed(format!("{method} answered something unreadable: {e}")))
    }

    /// Tell the server this request has no reader left. A notification, with no id of its own:
    /// there is no answer to it, and a request that has already finished simply is not there.
    pub fn cancel(&self, id: u64) {
        let _ = emit(
            &self.out,
            &json!({"jsonrpc": "2.0", "method": "cancel", "params": [id]}),
        );
    }

    fn call_value(
        &self,
        method: &str,
        params: Value,
        asked: &Mutex<Option<u64>>,
        deadline: Duration,
    ) -> Result<Value, RpcError> {
        if self.is_dead() {
            return Err(RpcError::disconnected("not connected"));
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        *locked(asked) = Some(id);
        let (tx, rx) = channel();
        locked(&self.pending).insert(id, tx);

        let line = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        if let Err(e) = emit(&self.out, &line) {
            locked(&self.pending).remove(&id);
            return Err(RpcError::disconnected(format!(
                "cannot reach the server: {e}"
            )));
        }

        match rx.recv_timeout(deadline) {
            Ok(answer) => answer,
            // The sender was dropped: the reader thread saw EOF and cleared the map.
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                Err(RpcError::disconnected("the connection closed"))
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                locked(&self.pending).remove(&id);
                // Nobody is left to read this one, and the server is still working on it: the
                // same notification a dropped `Task` sends, said here because a caller that gives
                // up on a timeout has no `Asked` of its own to cancel through.
                self.cancel(id);
                Err(RpcError::failed(format!("{method} timed out")))
            }
        }
    }

    /// Let go of the writing end, which is what tells `serve` to stop.
    ///
    /// Replacing the writer drops the pipe, and `serve` reads EOF on its stdin. Separate from
    /// [`join`](Self::join) because the caller owning the process in between has to be able to
    /// kill it: the reader thread ends when that process's output closes, and a wedged one would
    /// never close it. The end that follows is the one asked for, so `on_lost` stays quiet.
    pub fn close(&self) {
        self.closing.store(true, Ordering::SeqCst);
        let _ = self.stop.send(());
        *locked(&self.out) = Box::new(std::io::sink());
    }

    /// Wait for the reader thread to notice the far end has closed, and for the heartbeat to stop.
    pub fn join(&self) {
        for handle in std::mem::take(&mut *locked(&self.threads)) {
            let _ = handle.join();
        }
    }

    /// Both, for a caller with no process of its own to reap.
    pub fn shutdown(&self) {
        self.close();
        self.join();
    }
}

impl Drop for Client {
    /// Let go of the pipe even when nobody closed it, as a connection whose `hello` failed does.
    /// The heartbeat shares the writer, so dropping the client alone would leave it open.
    fn drop(&mut self) {
        self.close();
    }
}

/// The language requests a server has running, by the request id that asked for them.
///
/// A `cancel` takes one out and aborts it, which drops the future and so tells the language
/// server the same thing. Without it a completion the user typed past kept a thread busy to the
/// deadline and the answer went into a pipe nobody was reading.
type InFlight = Arc<Mutex<HashMap<u64, tokio::task::AbortHandle>>>;

thread_local! {
    /// The request this worker thread is answering, and where to register what it waits on.
    /// Set by [`serve_local`], read by [`block`]; unset everywhere else, which is what makes a
    /// vault used directly rather than served register nothing.
    static SERVING: std::cell::RefCell<Option<(u64, InFlight)>> =
        const { std::cell::RefCell::new(None) };
}

/// One message from the server: an answer for somebody, or an event for everybody.
fn route(msg: Value, pending: &Waiting, events: &Sender<Event>) {
    if let Some(id) = msg.get("id").and_then(Value::as_u64) {
        let answer = match msg.get("error") {
            Some(e) => Err(RpcError {
                code: e.get("code").and_then(Value::as_i64).unwrap_or(FAILED),
                message: e
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("the server refused")
                    .to_string(),
                data: e.get("data").cloned(),
            }),
            None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
        };
        if let Some(tx) = locked(pending).remove(&id) {
            let _ = tx.send(answer);
        }
        return;
    }
    if msg.get("method").and_then(Value::as_str) == Some("event")
        && let Some(params) = msg.get("params").cloned()
    {
        match serde_json::from_value::<Event>(params) {
            Ok(event) => {
                let _ = events.send(event);
            }
            Err(e) => tracing::warn!("unreadable event from the server: {e}"),
        }
    }
}

// ------------------------------------------------------------------- server

/// Open the vault at `root` and answer requests on `input` until it ends.
///
/// This is `accent-cli serve`: the whole of what runs on a remote host. The config arrives in the
/// `hello` the client opens with, rather than as an argument, because it belongs to the window
/// and can change while the vault is open.
pub fn serve(
    root: &std::path::Path,
    db: Option<&std::path::Path>,
    input: impl Read + Send + 'static,
    output: impl Write + Send + 'static,
) -> anyhow::Result<()> {
    let cfg = crate::VaultConfig::default();
    let opened = match db {
        Some(db) => Local::open_at(root, db, cfg),
        None => Local::open(root, cfg),
    };
    let (vault, events) = match opened {
        Ok(opened) => opened,
        Err(e) => {
            refuse(&e, input, output);
            return Err(e);
        }
    };
    serve_local(vault, events, input, output, SILENCE);
    Ok(())
}

/// Answer the first request — the client's `hello` — with why there is no vault to serve.
/// Exiting alone would only close the pipe, and the window would say the link closed.
fn refuse(e: &anyhow::Error, input: impl Read, mut output: impl Write) {
    for line in BufReader::new(input).lines().map_while(Result::ok) {
        // The client's pings come without an id, and one may arrive before the `hello`.
        let id = serde_json::from_str::<Value>(&line)
            .ok()
            .and_then(|msg| msg.get("id").cloned());
        if let Some(id) = id {
            let answer = json!({"jsonrpc": "2.0", "id": id, "error": {
                "code": REFUSED, "message": format!("{e:#}"),
            }});
            let _ = writeln!(output, "{answer}").and_then(|()| output.flush());
            return;
        }
    }
}

/// Answer requests on `input` until it ends, forwarding `events` as notifications.
///
/// Each request runs on a thread of its own: the vault is `Sync` and keeps a second SQLite reader
/// for exactly this, so a whole-vault grep does not hold up the tree the user is clicking through.
/// Every line is written whole under one lock, so two answers can never interleave.
///
/// Once the client has pinged, `silence` without a line from it ends the server too. Until then
/// it waits for as long as it is left, so `serve` driven by hand still works.
pub(crate) fn serve_local(
    vault: Local,
    events: Receiver<Event>,
    input: impl Read + Send + 'static,
    output: impl Write + Send + 'static,
    silence: Duration,
) {
    let vault = Arc::new(vault);
    let out: Arc<Mutex<Box<dyn Write + Send>>> = Arc::new(Mutex::new(Box::new(output)));

    let forwarder = std::thread::Builder::new()
        .name("accent-events".to_string())
        .spawn({
            let out = out.clone();
            move || {
                for event in events {
                    let line = json!({"jsonrpc": "2.0", "method": "event", "params": event});
                    if emit(&out, &line).is_err() {
                        break;
                    }
                }
            }
        })
        .ok();

    // stdin on a thread of its own, so the loop below can notice it has gone quiet.
    let (tx, lines) = channel();
    let _ = std::thread::Builder::new()
        .name("accent-stdin".to_string())
        .spawn(move || {
            for line in BufReader::new(input).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });

    let live: InFlight = Arc::new(Mutex::new(HashMap::new()));
    let mut workers: Vec<std::thread::JoinHandle<()>> = Vec::new();
    let mut pinged = false;
    loop {
        let line = match pinged {
            true => lines.recv_timeout(silence),
            false => lines.recv().map_err(RecvTimeoutError::from),
        };
        let line = match line {
            Ok(line) => line,
            Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {
                // The link died without closing. Nobody reads what the threads still have to
                // write, so joining them could wait on a full pipe for good. A commit or a sync
                // still running is the user's, though, so they are given until the longest git
                // bound to finish; then the vault goes, and the process exits around the rest.
                tracing::info!("nothing from the client in {silence:?}; stopping");
                let until = Instant::now() + LINGER;
                while workers.iter().any(|w| !w.is_finished()) && Instant::now() < until {
                    std::thread::sleep(Duration::from_millis(50));
                }
                drop(vault);
                return;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            tracing::warn!("unreadable request");
            continue;
        };
        let id = msg.get("id").and_then(Value::as_u64);
        let method = msg
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let params = msg.get("params").cloned().unwrap_or(json!([]));

        // The one method with nothing to answer: whoever asked has stopped waiting, so the work
        // is dropped rather than finished.
        if method == "cancel" {
            if let Some(target) = params.get(0).and_then(Value::as_u64)
                && let Some(handle) = locked(&live).remove(&target)
            {
                handle.abort();
            }
            continue;
        }
        // Nothing to answer either: the client saying it is still there.
        if method == "ping" {
            pinged = true;
            continue;
        }

        let (vault, out, live) = (vault.clone(), out.clone(), live.clone());
        let worker = std::thread::spawn(move || {
            if let Some(id) = id {
                SERVING.with_borrow_mut(|serving| *serving = Some((id, live)));
            }
            let answer = dispatch(&vault, &method, &params);
            let Some(id) = id else { return };
            let line = match answer {
                Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
                Err(e) => json!({"jsonrpc": "2.0", "id": id, "error": {
                    "code": e.code, "message": e.message, "data": e.data,
                }}),
            };
            let _ = emit(&out, &line);
        });
        workers.push(worker);
        // Threads that have finished are reaped as we go, so a long session does not keep every
        // handle it ever made.
        workers.retain(|w| !w.is_finished());
    }

    // stdin closed: the window went away. Drop the vault, which stops its worker, and let the
    // event forwarder end with the channel.
    drop(vault);
    for worker in workers {
        let _ = worker.join();
    }
    if let Some(forwarder) = forwarder {
        let _ = forwarder.join();
    }
}

fn emit(out: &Mutex<Box<dyn Write + Send>>, line: &Value) -> std::io::Result<()> {
    let mut out = locked(out);
    serde_json::to_writer(&mut *out, line)?;
    out.write_all(b"\n")?;
    out.flush()
}

// ----------------------------------------------------------------- dispatch

/// One positional argument, deserialised.
pub(crate) fn arg<T: DeserializeOwned>(params: &Value, n: usize) -> Result<T, RpcError> {
    let value = params.get(n).cloned().unwrap_or(Value::Null);
    serde_json::from_value(value)
        .map_err(|e| RpcError::failed(format!("argument {n} is not what was expected: {e}")))
}

/// Bind each named argument to its position in `p`, in order, answering the dispatch with the
/// first that does not read: what the tables in `vault.rs` and `language.rs` write for every
/// method they carry. The position is counted by shadowing rather than by hand.
macro_rules! args {
    ($p:ident; $($arg:ident : $t:ty),*) => {
        let _i = 0usize;
        $(
            let $arg: $t = match $crate::rpc::arg($p, _i) {
                Ok(value) => value,
                Err(e) => return Some(Err(e)),
            };
            let _i = _i + 1;
        )*
    };
}
pub(crate) use args;

/// Whatever the method returned, as JSON.
pub(crate) fn ok<T: serde::Serialize>(value: T) -> Result<Value, RpcError> {
    serde_json::to_value(value).map_err(|e| RpcError::failed(format!("cannot answer: {e}")))
}

/// An `anyhow` failure, formatted the way the app would have shown it locally.
pub(crate) fn any<T: serde::Serialize>(r: anyhow::Result<T>) -> Result<Value, RpcError> {
    ok(r.map_err(|e| RpcError::failed(format!("{e:#}")))?)
}

pub(crate) fn io<T: serde::Serialize>(r: std::io::Result<T>) -> Result<Value, RpcError> {
    match r {
        Ok(v) => ok(v),
        Err(e) => Err(io_failure(&e)),
    }
}

pub(crate) fn git_result<T: serde::Serialize>(
    r: Result<T, accent_core::git::Error>,
) -> Result<Value, RpcError> {
    ok(r.map_err(RpcError::failed)?)
}

/// The whole remote surface. Most of it is one line of the [`methods!`](crate::vault) table and
/// is answered by [`crate::vault::dispatch`]; what is left here is what that table cannot say —
/// the methods whose two sides differ, and the document's own lifecycle. Anything absent from
/// both is deliberately local: the session file, the config, and path arithmetic, all of which
/// belong to the machine the window is on.
fn dispatch(vault: &Local, method: &str, p: &Value) -> Result<Value, RpcError> {
    if let Some(answer) = crate::vault::dispatch(vault, method, p)
        .or_else(|| crate::language::dispatch(vault, method, p))
    {
        return answer;
    }

    match method {
        "hello" => {
            vault.set_config(arg(p, 0)?);
            // Ghost text is a global preference, so it rides `hello` rather than `VaultConfig`.
            // An older client sends nothing and gets the default.
            vault.set_ghost(arg::<Option<bool>>(p, 1)?.unwrap_or(true));
            ok(Hello {
                root: vault.root().to_path_buf(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            })
        }

        // A save has an error of its own.
        "save" => match vault.save(&arg::<String>(p, 0)?, &arg::<String>(p, 1)?, arg(p, 2)?) {
            Ok(etag) => ok(etag),
            Err(SaveError::ChangedOnDisk { current }) => Err(RpcError {
                code: CHANGED_ON_DISK,
                message: "file changed on disk since it was read".to_string(),
                data: serde_json::to_value(current).ok(),
            }),
            Err(SaveError::Io(e)) => Err(io_failure(&e)),
            // The server is the far end; it has a disk, so it is never the one that is offline.
            Err(SaveError::Offline) => Err(RpcError::disconnected("the vault is not connected")),
        },
        "create_note" => any(vault.create_note(
            &arg::<String>(p, 0)?,
            arg::<Option<String>>(p, 1)?.as_deref(),
        )),
        // The caller's `stop` stays with the caller: the host's walk runs to its budget.
        "grep_unindexed" => {
            any(vault.grep_unindexed(&arg::<String>(p, 0)?, arg(p, 1)?, arg(p, 2)?, &|| false))
        }
        "rescan" => {
            vault.rescan();
            ok(())
        }
        // Answered on a thread of its own while the host's worker walks, which is the whole point
        // of it: it raises the flag that walk reads and never waits for the walk.
        "stop_indexing" => {
            vault.stop_indexing();
            ok(())
        }
        "resume_indexing" => {
            vault.resume_indexing();
            ok(())
        }
        "repos" => ok(vault.repos()),

        // The document's lifecycle. Opening one is the only call that reads the vault's config,
        // and closing one forgets it rather than telling a provider, so neither fits the table
        // the other three go through.
        "open_document" => any(block(vault.open_document(
            &arg::<String>(p, 0)?,
            &arg::<String>(p, 1)?,
            arg(p, 2)?,
        ))),
        "close_document" => any(block(vault.close_document(&arg::<String>(p, 0)?))),

        _ => Err(RpcError::failed(format!("no such method: {method}"))),
    }
}

/// Wait for a language request, registered so that a `cancel` for it can drop it.
///
/// The registration is what makes cancellation real on the far side: aborting the task drops the
/// future, which is what sends `$/cancelRequest` to the language server underneath it. A vault
/// that is not being served registers nothing and simply waits.
pub(crate) fn block<T>(task: crate::Task<T>) -> anyhow::Result<T> {
    let serving = SERVING.with_borrow(Clone::clone);
    if let Some((id, live)) = &serving {
        locked(live).insert(*id, task.abort_handle());
    }
    let answer = accent_lsp::runtime().block_on(task);
    if let Some((id, live)) = &serving {
        locked(live).remove(id);
    }
    answer
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Session, VaultConfig};
    use std::time::{Duration, Instant};

    /// A client and a server joined by two pipes, exactly as ssh joins them, with no ssh in the
    /// way. `std::io::pipe` is what makes this a real end-to-end test of the protocol rather than
    /// a test of a mock.
    struct Wired {
        client: Client,
        server: Option<std::thread::JoinHandle<()>>,
        _events: Receiver<Event>,
        _root: tempfile::TempDir,
        _cache: tempfile::TempDir,
    }

    impl Wired {
        fn open() -> Wired {
            Wired::seeded(&|root| {
                std::fs::write(root.join("a.md"), "hello [[b]]\n").unwrap();
                std::fs::write(root.join("b.md"), "#tag\n").unwrap();
            })
        }

        /// [`open`](Self::open) over a vault the caller fills, for a test that needs a walk still
        /// running when the first request lands.
        fn seeded(write: &dyn Fn(&std::path::Path)) -> Wired {
            let root = tempfile::tempdir().unwrap();
            let cache = tempfile::tempdir().unwrap();
            write(root.path());

            let (vault, vault_events) = Local::open_at(
                root.path(),
                &cache.path().join("index.db"),
                VaultConfig::default(),
            )
            .unwrap();

            // to_server: client writes, server reads. to_client: the other way round.
            let (server_in, client_out) = std::io::pipe().unwrap();
            let (client_in, server_out) = std::io::pipe().unwrap();
            let server = std::thread::spawn(move || {
                serve_local(vault, vault_events, server_in, server_out, SILENCE);
            });

            let (events, event_rx) = channel();
            let client = Client::new(
                Box::new(client_out),
                Box::new(client_in),
                events,
                Box::new(|| {}),
            );
            let hello: Hello = client
                .call("hello", json!([VaultConfig::default()]))
                .unwrap();
            assert_eq!(hello.version, env!("CARGO_PKG_VERSION"));

            Wired {
                client,
                server: Some(server),
                _events: event_rx,
                _root: root,
                _cache: cache,
            }
        }

        /// Drain until `f` is happy, or give up. The reconcile is asynchronous, so a test that
        /// asks about the index has to wait for it the way the UI does.
        fn wait(&self, f: impl Fn(&Event) -> bool) -> bool {
            let deadline = Instant::now() + Duration::from_secs(10);
            while Instant::now() < deadline {
                match self._events.recv_timeout(Duration::from_millis(200)) {
                    Ok(e) if f(&e) => return true,
                    Ok(_) => {}
                    Err(_) => {}
                }
            }
            false
        }
    }

    impl Drop for Wired {
        fn drop(&mut self) {
            self.client.shutdown();
            if let Some(server) = self.server.take() {
                let _ = server.join();
            }
        }
    }

    #[test]
    fn a_call_crosses_the_wire_and_comes_back_typed() {
        let w = Wired::open();
        assert!(w.wait(|e| matches!(e, Event::Reconciled(_))));

        let rows: Vec<crate::FileRow> = w.client.call("list_dir", json!([""])).unwrap();
        let mut names: Vec<&str> = rows.iter().map(|r| r.rel_path.as_str()).collect();
        names.sort();
        assert_eq!(names, ["a.md", "b.md"]);

        let target: Option<String> = w.client.call("resolve_link", json!(["b"])).unwrap();
        assert_eq!(target.as_deref(), Some("b.md"));
    }

    #[test]
    fn a_save_that_lost_the_race_comes_back_as_the_same_refusal() {
        let w = Wired::open();
        assert!(w.wait(|e| matches!(e, Event::Reconciled(_))));

        let (_, etag): (String, Etag) = w.client.call("read", json!(["a.md"])).unwrap();
        let fresh: Etag = w
            .client
            .call("save", json!(["a.md", "first\n", etag]))
            .unwrap();
        assert_ne!(fresh, etag, "the file was rewritten");

        // The stale etag is exactly what a second window would still be holding.
        let refused = w
            .client
            .call::<Etag>("save", json!(["a.md", "second\n", etag]))
            .unwrap_err();
        assert_eq!(refused.code, CHANGED_ON_DISK);
        assert!(
            matches!(refused.save_error(), SaveError::ChangedOnDisk { current } if current == fresh),
            "the client has to be able to rebuild the local error"
        );
    }

    #[test]
    fn an_event_on_the_far_side_arrives_as_a_notification() {
        let w = Wired::open();
        assert!(w.wait(|e| matches!(e, Event::Reconciled(_))));

        w.client
            .call::<Etag>("save", json!(["c.md", "new note\n", Option::<Etag>::None]))
            .unwrap();
        assert!(
            w.wait(|e| matches!(e, Event::DirsChanged(dirs) if dirs.iter().any(|d| d.is_empty()))),
            "the vault's own worker has to reach the client"
        );
    }

    /// A language request runs where the files are, and what the provider has to say about the
    /// document comes back the way any other event does.
    #[test]
    fn a_document_is_opened_and_answered_for_across_the_wire() {
        let w = Wired::open();
        assert!(w.wait(|e| matches!(e, Event::Reconciled(_))));

        let text = "# Title\n\nsee [[nope]]\n";
        let support: crate::Support = w
            .client
            .call("open_document", json!(["a.md", "markdown", text]))
            .unwrap();
        assert_eq!(support.completion_triggers, ['[', '#', '(']);

        let symbols: Vec<crate::Symbol> = w.client.call("symbols", json!(["a.md"])).unwrap();
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "Title");

        // Ghost text crosses the wire like everything else. Whether merl is installed on the
        // host decides whether there is an answer, so only the round trip is asserted here.
        let at = crate::Pos {
            line: 2,
            character: 3,
        };
        let ghost: Option<String> = w
            .client
            .call("inline_completion", json!(["a.md", at]))
            .unwrap();
        assert_eq!(ghost, None, "a two-line note has nothing to suggest");
        let _: () = w.client.call("settle", json!(["a.md"])).unwrap();

        assert!(
            w.wait(|e| matches!(e, Event::Diagnostics { rel, items }
                if rel == "a.md" && items.len() == 1)),
            "the dangling link has to reach the client as a notification"
        );
    }

    /// A caller branches on the kind — the open path offers to create a note that is not there —
    /// so the kind has to survive the wire rather than flattening to `Other`.
    #[test]
    fn a_missing_file_comes_back_as_not_found() {
        let w = Wired::open();
        let refused = w
            .client
            .call::<(String, Etag)>("read", json!(["nope.md"]))
            .unwrap_err();
        assert_eq!(refused.code, IO);
        assert_eq!(refused.io_error().kind(), std::io::ErrorKind::NotFound);
    }

    /// An exact search crosses as what was typed plus its toggles and is compiled on the host, so
    /// case-insensitivity survives the wire and a pattern that does not compile says why.
    #[test]
    fn a_search_pattern_is_compiled_where_the_files_are() {
        let w = Wired::open();
        assert!(w.wait(|e| matches!(e, Event::Reconciled(_))));

        let (rows, total): (Vec<crate::Match>, usize) = w
            .client
            .call(
                "grep",
                json!(["HELLO", crate::Options::default(), 10, false]),
            )
            .unwrap();
        assert_eq!((rows.len(), total), (1, 1));

        let regex = crate::Options {
            regex: true,
            ..crate::Options::default()
        };
        let refused = w
            .client
            .call::<(Vec<crate::Match>, usize)>("grep", json!(["(", regex, 10, false]))
            .unwrap_err();
        assert_eq!(refused.code, FAILED);
        assert!(refused.message.contains("unclosed group"), "{refused}");
    }

    /// A cancel is a notification: it is taken, nothing comes back for it, and the connection
    /// carries on answering. One for a request that has already finished is simply not there.
    #[test]
    fn a_cancel_is_taken_and_leaves_the_connection_answering() {
        let w = Wired::open();
        assert!(w.wait(|e| matches!(e, Event::Reconciled(_))));

        w.client.cancel(9999);
        let tags: Vec<(String, i64)> = w.client.call("tags", json!([])).unwrap();
        assert!(tags.iter().any(|(tag, _)| tag == "tag"), "{tags:?}");
    }

    #[test]
    fn a_missing_method_is_an_error_and_not_a_dropped_call() {
        let w = Wired::open();
        let e = w.client.call::<()>("nonesuch", json!([])).unwrap_err();
        assert!(e.message.contains("nonesuch"), "{e}");
    }

    /// The window closed: `serve` must return rather than sit on a dead pipe, and the calls that
    /// were in flight must fail at once rather than each spending the full deadline.
    #[test]
    fn closing_the_connection_ends_the_server_and_frees_every_caller() {
        let mut w = Wired::open();
        assert!(w.wait(|e| matches!(e, Event::Reconciled(_))));

        w.client.shutdown();
        let server = w.server.take().expect("still running");
        let t = Instant::now();
        server.join().expect("serve must return on EOF");
        assert!(t.elapsed() < DEADLINE, "it waited instead of noticing");

        let t = Instant::now();
        let e = w.client.call::<()>("tags", json!([])).unwrap_err();
        assert!(
            t.elapsed() < Duration::from_secs(1),
            "a dead client must fail immediately, not after {DEADLINE:?}"
        );
        assert!(e.message.contains("connect"), "{e}");
    }

    /// A call waits as long as its caller gave it: a method the host may take longer over is
    /// answered, where the same call given less gives up at its own deadline — and the one that
    /// gave up tells the host so, or the host works on for an answer nobody will read.
    #[test]
    fn a_call_waits_for_as_long_as_it_was_given() {
        let (server_in, client_out) = std::io::pipe().unwrap();
        let (client_in, mut server_out) = std::io::pipe().unwrap();
        let cancelled: Arc<Mutex<Vec<Value>>> = Arc::default();
        // A host that takes 300 ms over every answer, and ignores the pings.
        let heard = cancelled.clone();
        let server = std::thread::spawn(move || {
            for line in BufReader::new(server_in).lines().map_while(Result::ok) {
                let message = serde_json::from_str::<Value>(&line).unwrap();
                let Some(id) = message.get("id").cloned() else {
                    if message.get("method") == Some(&json!("cancel")) {
                        locked(&heard).push(message["params"][0].clone());
                    }
                    continue;
                };
                std::thread::sleep(Duration::from_millis(300));
                let answer = json!({"jsonrpc": "2.0", "id": id, "result": "done"});
                if writeln!(server_out, "{answer}").is_err() {
                    break;
                }
            }
        });
        let client = Client::new(
            Box::new(client_out),
            Box::new(client_in),
            channel().0,
            Box::new(|| {}),
        );
        let ask = |deadline| {
            client.call_tracked::<String>("slow", json!([]), &Mutex::new(None), deadline)
        };

        let e = ask(Duration::from_millis(100)).unwrap_err();
        assert_eq!(e.message, "slow timed out");
        // The next answer proves the host has read past the cancel the timeout sent.
        assert_eq!(ask(Duration::from_secs(2)).unwrap(), "done");
        assert_eq!(
            *locked(&cancelled),
            vec![json!(1)],
            "the request given up on"
        );
        client.shutdown();
        server.join().unwrap();
    }

    /// The reader is the first to know the link has gone, and says so once. An end the client
    /// asked for is not a lost link, or every window close would read as one.
    #[test]
    fn a_lost_link_is_said_once_and_a_closed_one_not_at_all() {
        let ends = |closed: bool| {
            let (client_in, server_out) = std::io::pipe().unwrap();
            let (_server_in, client_out) = std::io::pipe().unwrap();
            let lost = Arc::new(AtomicU64::new(0));
            let client = Client::new(
                Box::new(client_out),
                Box::new(client_in),
                channel().0,
                Box::new({
                    let lost = lost.clone();
                    move || {
                        lost.fetch_add(1, Ordering::SeqCst);
                    }
                }),
            );
            if closed {
                client.close();
            }
            drop(server_out);
            client.join();
            lost.load(Ordering::SeqCst)
        };
        assert_eq!(ends(false), 1, "the server went away unasked");
        assert_eq!(ends(true), 0, "the client closed it");
    }

    /// A link that dies without closing leaves the server a stdin that neither ends nor speaks.
    /// Pinged once, the server takes the silence as the window gone; never pinged, as when it is
    /// driven by hand, it waits for its stdin to close.
    #[test]
    fn a_pinged_server_stops_when_the_pings_do_and_an_unpinged_one_waits() {
        let silence = Duration::from_millis(200);
        let serve = || {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("vault");
            std::fs::create_dir(&root).unwrap();
            let db = dir.path().join("index.db");
            let (vault, events) = Local::open_at(&root, &db, VaultConfig::default()).unwrap();
            let (server_in, input) = std::io::pipe().unwrap();
            let (output, server_out) = std::io::pipe().unwrap();
            let server = std::thread::spawn(move || {
                serve_local(vault, events, server_in, server_out, silence);
            });
            (server, input, (output, dir))
        };
        let (unpinged, quiet, _kept) = serve();
        let (pinged, mut input, _also_kept) = serve();

        writeln!(input, r#"{{"jsonrpc":"2.0","method":"ping"}}"#).unwrap();
        let t = Instant::now();
        while !pinged.is_finished() && t.elapsed() < silence + Duration::from_secs(1) {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            pinged.is_finished(),
            "still serving {:?} after the ping",
            t.elapsed()
        );

        std::thread::sleep(3 * silence);
        assert!(!unpinged.is_finished(), "nothing armed the silence");
        drop(quiet);
        unpinged.join().expect("stdin closing still ends it");
    }

    /// A link that goes quiet in the middle of a commit leaves the commit to finish on the host:
    /// the server waits for it before letting the vault go, rather than exiting around it.
    #[test]
    fn a_server_gone_quiet_still_finishes_the_commit_it_was_running() {
        use std::os::unix::fs::PermissionsExt;
        let run = |root: &std::path::Path, args: &[&str]| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(root)
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}");
        };
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("vault");
        std::fs::create_dir(&root).unwrap();
        run(&root, &["init", "-q", "-b", "main"]);
        for (key, value) in [
            ("user.name", "Accent Test"),
            ("user.email", "test@accent.invalid"),
            ("commit.gpgsign", "false"),
            ("core.hooksPath", "hooks"),
        ] {
            run(&root, &["config", key, value]);
        }
        let hook = root.join("hooks/pre-commit");
        std::fs::create_dir(root.join("hooks")).unwrap();
        std::fs::write(&hook, "#!/bin/sh\nsleep 1\n").unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(root.join("a.md"), "one\n").unwrap();
        run(&root, &["add", "a.md"]);
        let repo = crate::git::toplevel(&root).unwrap().unwrap();

        let silence = Duration::from_millis(200);
        let (vault, events) =
            Local::open_at(&root, &dir.path().join("index.db"), VaultConfig::default()).unwrap();
        let (server_in, mut input) = std::io::pipe().unwrap();
        let (_output, server_out) = std::io::pipe().unwrap();
        let t = Instant::now();
        let server = std::thread::spawn(move || {
            serve_local(vault, events, server_in, server_out, silence);
        });
        writeln!(input, r#"{{"jsonrpc":"2.0","method":"ping"}}"#).unwrap();
        let commit = json!({"jsonrpc": "2.0", "id": 1, "method": "git_commit",
            "params": [repo, "first", false]});
        writeln!(input, "{commit}").unwrap();

        while !server.is_finished() && t.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            server.is_finished(),
            "still serving after {:?}",
            t.elapsed()
        );
        assert!(
            crate::git::status(&repo).unwrap().branch.oid.is_some(),
            "the server stopped before the commit landed"
        );
    }

    /// A mistyped root is refused rather than served as an empty vault, and the refusal is the
    /// answer to the `hello` the window opens with, so the window's banner can say why.
    #[test]
    fn serve_refuses_a_root_that_is_not_a_folder_and_says_so_to_hello() {
        let dir = tempfile::tempdir().unwrap();
        let (root, db) = (dir.path().join("nonesuch"), dir.path().join("index.db"));
        let (server_in, client_out) = std::io::pipe().unwrap();
        let (client_in, server_out) = std::io::pipe().unwrap();
        let server = std::thread::spawn({
            let (root, db) = (root.clone(), db.clone());
            move || serve(&root, Some(&db), server_in, server_out)
        });
        let client = Client::new(
            Box::new(client_out),
            Box::new(client_in),
            channel().0,
            Box::new(|| {}),
        );

        let e = client
            .call::<Hello>("hello", json!([VaultConfig::default()]))
            .unwrap_err();
        assert_eq!(e.message, format!("{} is not a folder", root.display()));
        // Its own code, so the window can tell it from a link that failed and stop retrying.
        assert_eq!(e.code, REFUSED);
        assert!(server.join().unwrap().is_err(), "serve must exit failing");
        assert!(!db.exists(), "no index for a vault that is not there");
    }

    /// A remote vault's walk runs on the host, so Stop has to reach the worker there while it is
    /// going. It does: every request is served on a thread of its own, and these two raise a flag
    /// rather than queueing behind the walk. Enough notes that the walk outlives the `hello`
    /// round trip `Wired::open` ends with.
    #[test]
    fn stop_and_resume_reach_the_hosts_worker_over_the_wire() {
        let w = Wired::seeded(&|root| {
            for i in 0..4000 {
                std::fs::write(root.join(format!("n{i}.md")), "body").unwrap();
            }
        });
        w.client.call::<()>("stop_indexing", json!([])).unwrap();
        assert!(
            w.wait(|e| matches!(e, Event::Reconciled(s) if s.stopped)),
            "the host's walk was not stopped"
        );
        w.client.call::<()>("resume_indexing", json!([])).unwrap();
        assert!(
            w.wait(|e| matches!(e, Event::Reconciled(s) if !s.stopped)),
            "the host's walk did not finish after a resume"
        );
    }

    /// The session belongs to the machine the window is on, so it is not on the wire at all.
    #[test]
    fn the_session_is_not_a_remote_method() {
        let w = Wired::open();
        assert!(w.client.call::<Session>("session", json!([])).is_err());
    }
}
