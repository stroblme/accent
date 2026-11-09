//! JSON-RPC 2.0 over stdio for remote vaults, one JSON object per line. Requests and responses
//! have IDs; vault [`Event`]s are notifications. File bytes travel separately over SSH. Calls have
//! deadlines, and clients ping every [`PING`]; after the first ping, a server exits on [`SILENCE`]
//! without input. Both are counted from the last byte either side read, not the last whole line:
//! a 16 MiB note crossing a slow link is minutes of one line, and the link is busy, not gone.

use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, SyncSender, channel, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;
use serde_json::{Value, json};

/// In-flight calls by request id. The reader removes answered calls;
/// a lost connection clears all waiters.
type Waiting = Arc<Mutex<HashMap<u64, Sender<Result<Value>>>>>;

use crate::{Error, Event, Local, Result, locked};

/// Default remote-call deadline, bounding stalled servers. Host-bounded
/// operations add their own allowance, as listed in `vault.rs`'s `methods!` table.
pub const DEADLINE: Duration = Duration::from_secs(10);

/// How often the client says it is still there.
pub const PING: Duration = Duration::from_secs(10);

/// Silence timeout after the first ping: nine missed pings, allowing transient
/// VPN outages and exceeding SSH's 15 s ServerAlive timeout.
pub const SILENCE: Duration = Duration::from_secs(90);

/// How often a host says a request line is still arriving (`receiving`). The client's writer
/// returns while ssh still holds up to its 2 MB channel window of the line, minutes over a slow
/// link, and the call's deadline starts there: without word from the host it would run out.
const ARRIVING: Duration = Duration::from_secs(1);

/// The longest line a host reads, its newline included: four times the longest note a tab opens,
/// which leaves room for JSON's escapes in a save's text (two bytes for a newline or a quote). A
/// client refuses a longer request before sending it, since a host skips such a line without
/// learning which request it was.
const MAX_LINE: usize = 4 * accent_core::fs::MAX_TEXT as usize;

/// The requests a host runs at once, each on a thread that takes the next waiting one when done.
const WORKERS: usize = 64;

/// The requests that wait for a worker, at most, and the bytes their lines may hold together.
/// One past either is answered at once that the host is busy.
const QUEUED: usize = 1024;
const QUEUED_BYTES: usize = MAX_LINE;

/// Answers and events waiting for the writer, beyond which their threads wait.
const UNWRITTEN: usize = 64;

/// Wait for in-flight work after losing the client. Covers the longest git
/// sync (pull and push under `TRANSFER_TIMEOUT`) plus process-group shutdown.
const LINGER: Duration = Duration::from_secs(2 * accent_core::git::TRANSFER_TIMEOUT.as_secs() + 10);

/// Assert host-bounded calls finish within [`LINGER`], preventing server
/// shutdown during a vault rewrite after client loss.
const _: () = assert!(
    crate::vault::REPLACE_BOUND.as_secs() <= LINGER.as_secs(),
    "a rewrite may outlast the wait a server gives the requests it is running"
);

/// JSON-RPC's own code for a method the server does not have.
const NO_METHOD: i64 = -32601;
/// JSON-RPC's own code for arguments that do not read.
const BAD_PARAMS: i64 = -32602;
/// Every other failure. Whichever the code, the error itself is the `data`.
const FAILED: i64 = -32000;

/// Wire error: JSON-RPC code, display message and the original error in `data`.
pub(crate) fn to_wire(e: &Error) -> Value {
    let code = match e {
        Error::UnknownMethod(_) => NO_METHOD,
        Error::Protocol(_) => BAD_PARAMS,
        _ => FAILED,
    };
    json!({"code": code, "message": e.to_string(), "data": e})
}

/// Recover the remote error from `data`, falling back to the message
/// for peers that do not provide it.
fn from_wire(e: &Value) -> Error {
    e.get("data")
        .and_then(|data| serde_json::from_value(data.clone()).ok())
        .unwrap_or_else(|| {
            let said = e.get("message").and_then(Value::as_str);
            Error::Protocol(said.unwrap_or("the server refused").to_string())
        })
}

/// What `hello` answers: who is on the other end, and where the vault really is.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Hello {
    /// Server-canonicalized root. All `rel` paths use it; the UI needs this
    /// spelling for git and displayed paths.
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
    /// What the heartbeat writes besides its pings, a cancel the writer was busy for, and `None`,
    /// which stops it. The reader sends that too: a server that is gone needs no pings.
    notes: Sender<Option<Vec<u8>>>,
    /// The reader and the heartbeat.
    threads: Mutex<Vec<std::thread::JoinHandle<()>>>,
    /// When the reader last read a byte, which a call's deadline counts from.
    heard: Arc<Mutex<Instant>>,
}

impl Client {
    /// Route responses and send remote notifications to the local-style `events` channel.
    /// `on_lost` runs once on the reader thread for an unsolicited output close.
    /// It must not join this client, since it runs on the thread being joined.
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
        let (notes, noted) = channel::<Option<Vec<u8>>>();
        let heard = Arc::new(Mutex::new(Instant::now()));
        let input = Heard {
            input,
            at: heard.clone(),
            arriving: || {},
        };

        let reader = std::thread::Builder::new()
            .name("accent-rpc".to_string())
            .spawn({
                let (pending, dead) = (pending.clone(), dead.clone());
                let (closing, stop) = (closing.clone(), notes.clone());
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
                    let _ = stop.send(None);
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
                    let ping = encode(&json!({"jsonrpc": "2.0", "method": "ping"}));
                    let mut due = Instant::now();
                    loop {
                        if due <= Instant::now() {
                            if emit(&out, &ping).is_err() {
                                break;
                            }
                            due = Instant::now() + PING;
                        }
                        match noted.recv_timeout(due.saturating_duration_since(Instant::now())) {
                            Ok(Some(line)) => {
                                if emit(&out, &line).is_err() {
                                    break;
                                }
                            }
                            Ok(None) | Err(RecvTimeoutError::Disconnected) => break,
                            Err(RecvTimeoutError::Timeout) => {}
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
            notes,
            threads: Mutex::new(reader.into_iter().chain(heartbeat).collect()),
            heard,
        }
    }

    pub fn is_dead(&self) -> bool {
        self.dead.load(Ordering::SeqCst)
    }

    /// How long nothing has come from the server: no answer, no event, no word of a request
    /// still arriving.
    pub fn quiet(&self) -> Duration {
        locked(&self.heard).elapsed()
    }

    /// Ask, and wait up to [`DEADLINE`] for the answer.
    pub fn call<T: DeserializeOwned>(&self, method: &str, params: Value) -> Result<T> {
        self.call_tracked(method, params, &Mutex::new(None), DEADLINE)
    }

    /// Wait until the link has been quiet for `deadline` since the request went, storing the
    /// request id in `asked` so a caller can [`cancel`](Self::cancel) when the answer becomes
    /// stale. Any byte from the server counts: an answer still arriving, and the answers and
    /// events its one writer sends ahead of this one, which this answer waits behind.
    pub fn call_tracked<T: DeserializeOwned>(
        &self,
        method: &str,
        params: Value,
        asked: &Mutex<Option<u64>>,
        deadline: Duration,
    ) -> Result<T> {
        let value = self.call_value(method, params, asked, deadline)?;
        serde_json::from_value(value)
            .map_err(|e| Error::Protocol(format!("{method} answered something unreadable: {e}")))
    }

    /// Notify the server that the request is cancelled; finished requests are ignored.
    /// Release the caller immediately with "cancelled", avoiding accumulated blocking
    /// threads for stale ghost-text requests on slow links.
    ///
    /// Never waits on the link: a request is dropped on whatever thread lets go of it, the
    /// window's own among them, and a 16 MiB save can hold the writer for minutes, so a cancel
    /// that finds it busy goes after it from the heartbeat's thread.
    pub fn cancel(&self, id: u64) {
        if let Some(waiter) = locked(&self.pending).remove(&id) {
            let _ = waiter.send(Err(accent_core::Error::Cancelled.into()));
        }
        let line = encode(&json!({"jsonrpc": "2.0", "method": "cancel", "params": [id]}));
        match self.out.try_lock() {
            Ok(mut out) => {
                let _ = out.write_all(&line).and_then(|()| out.flush());
            }
            Err(_) => {
                let _ = self.notes.send(Some(line));
            }
        }
    }

    fn call_value(
        &self,
        method: &str,
        params: Value,
        asked: &Mutex<Option<u64>>,
        deadline: Duration,
    ) -> Result<Value> {
        if self.is_dead() {
            return Err(Error::Offline("not connected".to_string()));
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let line = encode(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        if line.len() > MAX_LINE {
            return Err(Error::Protocol(format!(
                "{method} is {} MiB, more than the {} MiB a host reads in one request",
                line.len() >> 20,
                MAX_LINE >> 20
            )));
        }
        let (tx, rx) = channel();
        locked(&self.pending).insert(id, tx);
        // Only once there is a waiter to let go: a cancel in between would find none.
        *locked(asked) = Some(id);

        if let Err(e) = emit(&self.out, &line) {
            locked(&self.pending).remove(&id);
            return Err(Error::Offline(format!("cannot reach the server: {e}")));
        }

        let sent = Instant::now();
        let quiet = || (*locked(&self.heard)).max(sent).elapsed();
        loop {
            match rx.recv_timeout(deadline.saturating_sub(quiet())) {
                Ok(answer) => return answer,
                // The sender was dropped: the reader thread saw EOF and cleared the map.
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(Error::Lost("the connection closed".to_string()));
                }
                // Something came meanwhile: the deadline starts again from it.
                Err(RecvTimeoutError::Timeout) if quiet() < deadline => {}
                Err(RecvTimeoutError::Timeout) => {
                    locked(&self.pending).remove(&id);
                    // Nobody is left to read this one, and the server is still working on it:
                    // the same notification a dropped `Task` sends, said here because a caller
                    // that gives up on a timeout has no `Asked` of its own to cancel through.
                    self.cancel(id);
                    return Err(Error::Remote(format!("{method} timed out")));
                }
            }
        }
    }

    /// Close the writer so `serve` reads EOF, suppressing `on_lost` for this shutdown.
    /// Separate from [`join`](Self::join) so the process owner can kill a wedged
    /// child before joining the reader, which needs stdout to close.
    pub fn close(&self) {
        self.closing.store(true, Ordering::SeqCst);
        let _ = self.notes.send(None);
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
    /// Close the pipe on drop, including failed handshakes. The heartbeat shares
    /// the writer, so dropping the client alone would leave it open.
    fn drop(&mut self) {
        self.close();
    }
}

/// Active host language requests by request id. `cancel` removes and aborts
/// a task, dropping its future to cancel the underlying language-server request
/// and release the waiting thread.
type InFlight = Arc<Mutex<HashMap<u64, tokio::task::AbortHandle>>>;

thread_local! {
    /// Current host request and task registry, set by [`serve_local`] and read
    /// by [`block`]. Direct vault calls leave this unset and register nothing.
    static SERVING: std::cell::RefCell<Option<(u64, InFlight)>> =
        const { std::cell::RefCell::new(None) };
}

/// One message from the server: an answer for somebody, or an event for everybody.
fn route(msg: Value, pending: &Waiting, events: &Sender<Event>) {
    if let Some(id) = msg.get("id").and_then(Value::as_u64) {
        let answer = match msg.get("error") {
            Some(e) => Err(from_wire(e)),
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

/// Serve `root` over `input` until EOF (`accent-cli serve`). The opening
/// `hello` supplies window-owned config, which may change while the vault is open.
pub fn serve(
    root: &std::path::Path,
    db: Option<&std::path::Path>,
    input: impl Read + Send + 'static,
    output: impl Write + Send + 'static,
) -> Result<()> {
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

/// Reply to the opening `hello` with the vault failure. Exiting alone would
/// report only a closed link and lose the actionable reason.
fn refuse(e: &crate::Error, input: impl Read, mut output: impl Write) {
    for line in capped_lines(input) {
        // The client's pings come without an id, and one may arrive before the `hello`.
        let id = serde_json::from_slice::<Value>(&line)
            .ok()
            .and_then(|msg| msg.get("id").cloned());
        if let Some(id) = id {
            let refused = Error::Refused(e.to_string());
            let answer = json!({"jsonrpc": "2.0", "id": id, "error": to_wire(&refused)});
            let _ = writeln!(output, "{answer}").and_then(|()| output.flush());
            return;
        }
    }
}

/// Serve requests until EOF and forward `events` as notifications.
/// Each request runs on one of [`WORKERS`] threads with its own search reader, so a vault-wide
/// grep does not block tree reads, and the rest wait for one in the order they came. One writer
/// takes every line out, and the reading never waits for it or for a worker, so pings and
/// cancels are read however busy the host or slow the link. After the first ping, exit on
/// `silence` without input; unpinged manual sessions wait for EOF.
pub(crate) fn serve_local(
    vault: Local,
    events: Receiver<Event>,
    input: impl Read + Send + 'static,
    mut output: impl Write + Send + 'static,
    silence: Duration,
) {
    let vault = Arc::new(vault);
    let (out, unwritten) = sync_channel::<Vec<u8>>(UNWRITTEN);
    let writer = std::thread::Builder::new()
        .name("accent-out".to_string())
        .spawn(move || {
            for line in unwritten {
                if output
                    .write_all(&line)
                    .and_then(|()| output.flush())
                    .is_err()
                {
                    break;
                }
            }
        })
        .ok();

    let forwarder = std::thread::Builder::new()
        .name("accent-events".to_string())
        .spawn({
            let out = out.clone();
            move || {
                for event in events {
                    let line = json!({"jsonrpc": "2.0", "method": "event", "params": event});
                    if out.send(encode(&line)).is_err() {
                        break;
                    }
                }
            }
        })
        .ok();

    // stdin on a thread of its own, so the loop below can notice it has gone quiet. A request
    // line still arriving is the client being there, and is told to it once a second.
    let (tx, lines) = sync_channel(1);
    let heard = Arc::new(Mutex::new(Instant::now()));
    let mut told: Option<Instant> = None;
    let receiving = encode(&json!({"jsonrpc": "2.0", "method": "receiving"}));
    let input = Heard {
        input,
        at: heard.clone(),
        arriving: {
            let out = out.clone();
            move || {
                if told.is_none_or(|told| told.elapsed() >= ARRIVING) {
                    told = Some(Instant::now());
                    // Never waited for: the reading must not wait on the writer.
                    let _ = out.try_send(receiving.clone());
                }
            }
        },
    };
    let _ = std::thread::Builder::new()
        .name("accent-stdin".to_string())
        .spawn(move || {
            for line in capped_lines(input) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });

    let live: InFlight = Arc::new(Mutex::new(HashMap::new()));
    let backlog: Arc<Mutex<Backlog>> = Arc::default();
    let mut workers: Vec<std::thread::JoinHandle<()>> = Vec::new();
    let mut pinged = false;
    loop {
        let quiet = || locked(&heard).elapsed();
        let line = match pinged {
            true => lines.recv_timeout(silence.saturating_sub(quiet())),
            false => lines.recv().map_err(RecvTimeoutError::from),
        };
        let line = match line {
            Ok(line) => line,
            Err(RecvTimeoutError::Disconnected) => break,
            // Part of a line came meanwhile: the silence starts again from it.
            Err(RecvTimeoutError::Timeout) if quiet() < silence => continue,
            Err(RecvTimeoutError::Timeout) => {
                // The link died without closing. Nobody reads what the threads still have to
                // write, so joining them could wait on a full pipe for good. A commit or a sync
                // still running is the user's, though, so they are given until the longest git
                // bound to finish; then the vault goes, and the process exits around the rest.
                // The debug sessions and kernels are nobody's any more and end first, as after EOF
                // below.
                tracing::info!("nothing from the client in {silence:?}; stopping");
                vault.debug.shutdown();
                vault.kernels.shutdown();
                let until = Instant::now() + LINGER;
                while workers.iter().any(|w| !w.is_finished()) && Instant::now() < until {
                    std::thread::sleep(Duration::from_millis(50));
                }
                drop(vault);
                return;
            }
        };
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let Ok(msg) = serde_json::from_slice::<Value>(&line) else {
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
        // is dropped rather than finished, and one still waiting never starts.
        if method == "cancel" {
            if let Some(target) = params.get(0).and_then(Value::as_u64) {
                if let Some(handle) = locked(&live).remove(&target) {
                    handle.abort();
                }
                let mut backlog = locked(&backlog);
                if let Some(at) = backlog.waiting.iter().position(|r| r.id == Some(target))
                    && let Some(gone) = backlog.waiting.remove(at)
                {
                    backlog.bytes -= gone.bytes;
                }
            }
            continue;
        }
        // Nothing to answer either: the client saying it is still there.
        if method == "ping" {
            pinged = true;
            continue;
        }

        let request = Request {
            id,
            method,
            params,
            bytes: line.len(),
        };
        let mut held = locked(&backlog);
        if held.running < WORKERS {
            held.running += 1;
            drop(held);
            let (vault, out, live, backlog) =
                (vault.clone(), out.clone(), live.clone(), backlog.clone());
            workers.push(std::thread::spawn(move || {
                work(&vault, request, &out, &live, &backlog);
            }));
            // Threads that have finished are reaped as we go, so a long session does not keep
            // every handle it ever made.
            workers.retain(|w| !w.is_finished());
        } else if held.waiting.len() < QUEUED && held.bytes + request.bytes <= QUEUED_BYTES {
            held.bytes += request.bytes;
            held.waiting.push_back(request);
        } else if let Some(id) = request.id {
            drop(held);
            let busy = Error::Remote(format!(
                "the host is busy: {WORKERS} requests are running and more are waiting"
            ));
            // Never waited for: a link too slow to take it leaves its caller to time out.
            let _ = out.try_send(encode(
                &json!({"jsonrpc": "2.0", "id": id, "error": to_wire(&busy)}),
            ));
        }
    }

    // stdin closed: the window went away. Its debug sessions and kernels end first: a console's
    // evaluation runs the reader's code, which may never return, and the thread waiting on it
    // holds the vault. Then drop the vault, which stops its worker, and let the event forwarder
    // end with the channel, and the writer once it has written what they all said.
    vault.debug.shutdown();
    vault.kernels.shutdown();
    drop(vault);
    for worker in workers {
        let _ = worker.join();
    }
    if let Some(forwarder) = forwarder {
        let _ = forwarder.join();
    }
    drop(out);
    if let Some(writer) = writer {
        let _ = writer.join();
    }
}

/// A request read off the wire, waiting for a worker or running on one.
struct Request {
    id: Option<u64>,
    method: String,
    params: Value,
    /// Its line's length, which it holds of [`QUEUED_BYTES`] while it waits.
    bytes: usize,
}

/// The requests waiting for one of the [`WORKERS`], in the order they came, and how many run.
#[derive(Default)]
struct Backlog {
    waiting: VecDeque<Request>,
    bytes: usize,
    running: usize,
}

/// Answer `request` on this thread, then each request waiting in `backlog`, until none is.
fn work(
    vault: &Local,
    mut request: Request,
    out: &SyncSender<Vec<u8>>,
    live: &InFlight,
    backlog: &Mutex<Backlog>,
) {
    loop {
        SERVING.with_borrow_mut(|serving| *serving = request.id.map(|id| (id, live.clone())));
        let answer = dispatch(vault, &request.method, &request.params);
        if let Some(id) = request.id {
            let line = match answer {
                Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
                Err(e) => json!({"jsonrpc": "2.0", "id": id, "error": to_wire(&e)}),
            };
            let _ = out.send(encode(&line));
        }
        let mut held = locked(backlog);
        match held.waiting.pop_front() {
            Some(next) => {
                held.bytes -= next.bytes;
                request = next;
            }
            None => {
                held.running -= 1;
                return;
            }
        }
    }
}

/// The lines of `input`, each without its newline, until it ends or a read fails. A line longer
/// than [`MAX_LINE`] is read past rather than held, and the line after it reads as usual.
fn capped_lines(input: impl Read) -> impl Iterator<Item = Vec<u8>> {
    let mut input = BufReader::new(input);
    std::iter::from_fn(move || {
        loop {
            let mut line = Vec::new();
            match (&mut input)
                .take(MAX_LINE as u64)
                .read_until(b'\n', &mut line)
            {
                Ok(0) | Err(_) => return None,
                Ok(read) if read == MAX_LINE && line.last() != Some(&b'\n') => {
                    tracing::warn!("skipped a line longer than {} MiB", MAX_LINE >> 20);
                    input.skip_until(b'\n').ok()?;
                }
                Ok(_) => {
                    if line.last() == Some(&b'\n') {
                        line.pop();
                    }
                    return Some(line);
                }
            }
        }
    })
}

/// A reader that stamps `at` with every read that brings bytes, and runs `arriving` when one
/// ends inside a line: the peer is there while any of its line is still coming.
struct Heard<R, F> {
    input: R,
    at: Arc<Mutex<Instant>>,
    arriving: F,
}

impl<R: Read, F: FnMut()> Read for Heard<R, F> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let read = self.input.read(buf)?;
        if read > 0 {
            *locked(&self.at) = Instant::now();
            if buf[read - 1] != b'\n' {
                (self.arriving)();
            }
        }
        Ok(read)
    }
}

/// `msg` as the line it is sent as.
fn encode(msg: &Value) -> Vec<u8> {
    let mut line = serde_json::to_vec(msg).expect("a JSON value always serialises");
    line.push(b'\n');
    line
}

fn emit(out: &Mutex<Box<dyn Write + Send>>, line: &[u8]) -> std::io::Result<()> {
    let mut out = locked(out);
    out.write_all(line)?;
    out.flush()
}

// ----------------------------------------------------------------- dispatch

/// One positional argument, deserialised.
pub(crate) fn arg<T: DeserializeOwned>(params: &Value, n: usize) -> Result<T> {
    let value = params.get(n).cloned().unwrap_or(Value::Null);
    serde_json::from_value(value)
        .map_err(|e| Error::Protocol(format!("argument {n} is not what was expected: {e}")))
}

/// Deserialize positional arguments into named bindings for the vault and
/// language tables. Shadow the position counter and return the first error.
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
pub(crate) fn ok<T: serde::Serialize>(value: T) -> Result<Value> {
    serde_json::to_value(value).map_err(|e| Error::Protocol(format!("cannot answer: {e}")))
}

/// A method's answer, whatever error it failed with: the vault's own, or the core's.
pub(crate) fn answer<T: serde::Serialize>(r: Result<T, impl Into<Error>>) -> Result<Value> {
    ok(r.map_err(Into::into)?)
}

/// Dispatch the remote surface through [`crate::vault::dispatch`] plus
/// operations the method table cannot express, including document lifecycle.
/// Session files, local config and path arithmetic stay on the window's machine.
fn dispatch(vault: &Local, method: &str, p: &Value) -> Result<Value> {
    if let Some(answer) = crate::vault::dispatch(vault, method, p)
        .or_else(|| crate::language::dispatch(vault, method, p))
        .or_else(|| crate::debug::dispatch(vault, method, p))
        .or_else(|| crate::kernel::dispatch(vault, method, p))
        .or_else(|| crate::python::dispatch(vault, method, p))
    {
        return answer;
    }

    match method {
        "copy_page" => answer(vault.copy_page(
            &arg::<String>(p, 0)?,
            &arg::<String>(p, 1)?,
            arg::<Option<Vec<u8>>>(p, 2)?.as_deref(),
            arg(p, 3)?,
        )),
        "hello" => {
            vault.set_config(arg(p, 0)?);
            // Ghost text and word suggestions are global preferences, so they ride `hello` rather
            // than `VaultConfig`.
            vault.set_ghost(arg(p, 1)?);
            vault.set_words(arg(p, 2)?);
            ok(Hello {
                root: vault.root().to_path_buf(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            })
        }

        "save" => answer(vault.save(&arg::<String>(p, 0)?, &arg::<String>(p, 1)?, arg(p, 2)?)),
        "commit" => answer(vault.commit(
            &arg::<String>(p, 0)?,
            &arg::<String>(p, 1)?,
            arg(p, 2)?,
            arg(p, 3)?,
        )),
        "create_note" => answer(vault.create_note(
            &arg::<String>(p, 0)?,
            arg::<Option<String>>(p, 1)?.as_deref(),
        )),
        // The caller's `stop` stays with the caller: the host's walk runs to its budget.
        "grep_unindexed" => {
            answer(vault.grep_unindexed(&arg::<String>(p, 0)?, arg(p, 1)?, arg(p, 2)?, &|| false))
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
        // What a test holds a worker with: answered once the file at its path exists.
        #[cfg(test)]
        "test_wait" => {
            let release: std::path::PathBuf = arg(p, 0)?;
            while !release.exists() {
                std::thread::sleep(Duration::from_millis(10));
            }
            ok(())
        }

        // The document's lifecycle. Opening one is the only call that reads the vault's config,
        // and closing one forgets it rather than telling a provider, so neither fits the table
        // the other three go through.
        "open_document" => answer(block(vault.open_document(
            &arg::<String>(p, 0)?,
            &arg::<String>(p, 1)?,
            arg(p, 2)?,
        ))),
        "close_document" => answer(block(vault.close_document(&arg::<String>(p, 0)?))),
        // A debug session's ends, likewise: starting reads the vault's config, and stopping one
        // already over is no error.
        "debug_start" => answer(block(
            vault.debug_start(arg(p, 0)?, &arg::<crate::debug::Launch>(p, 1)?),
        )),
        "debug_stop" => answer(block(vault.debug_stop(arg(p, 0)?))),

        _ => Err(Error::UnknownMethod(method.to_string())),
    }
}

/// Wait for a language request, registering it for remote `cancel`. Aborting
/// drops its future and sends `$/cancelRequest` to the language server.
/// Direct vault calls wait without registration.
pub(crate) fn block<T>(task: crate::Task<T>) -> crate::Result<T> {
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
mod tests;
