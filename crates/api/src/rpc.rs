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

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde_json::{Value, json};

/// Who is waiting for which answer. One entry per call in flight, taken out by the reader thread
/// when the answer arrives and dropped wholesale when the connection dies.
type Waiting = Arc<Mutex<HashMap<u64, Sender<Result<Value, RpcError>>>>>;

use crate::{Event, Local};

use accent_core::fs::{Etag, SaveError};

/// How long a call waits before giving up on the far end. Generous next to a round trip on any
/// link a person would edit over, and short enough that a wedged server is a message rather than
/// a hang.
pub const DEADLINE: Duration = Duration::from_secs(10);

/// A save refused because the file changed under it. Its `data` is the current [`Etag`], so the
/// client can rebuild [`SaveError::ChangedOnDisk`] and the UI can offer the same comparison it
/// offers locally.
pub const CHANGED_ON_DISK: i64 = -32001;
/// Anything that was an `io::Error` on the far side.
pub const IO: i64 = -32002;
/// Everything else, already formatted for a human.
pub const FAILED: i64 = -32000;

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
    fn failed(message: impl std::fmt::Display) -> RpcError {
        RpcError {
            code: FAILED,
            message: message.to_string(),
            data: None,
        }
    }

    /// The `SaveError` this stands for, so a remote save fails exactly as a local one does.
    pub fn save_error(self) -> SaveError {
        match (self.code, self.data) {
            (CHANGED_ON_DISK, Some(data)) => match serde_json::from_value::<Etag>(data) {
                Ok(current) => SaveError::ChangedOnDisk { current },
                Err(_) => SaveError::Io(std::io::Error::other(self.message)),
            },
            _ => SaveError::Io(std::io::Error::other(self.message)),
        }
    }

    pub fn io_error(self) -> std::io::Error {
        std::io::Error::other(self.message)
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
    out: Mutex<Box<dyn Write + Send>>,
    next_id: AtomicU64,
    pending: Waiting,
    dead: Arc<AtomicBool>,
    reader: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl Client {
    /// Start routing. `events` receives the server's notifications; it is the same channel the
    /// local worker would have posted to, so nothing downstream can tell the two apart.
    pub fn new(
        out: Box<dyn Write + Send>,
        input: Box<dyn Read + Send>,
        events: Sender<Event>,
    ) -> Client {
        let pending: Waiting = Arc::new(Mutex::new(HashMap::new()));
        let dead = Arc::new(AtomicBool::new(false));

        let reader = std::thread::Builder::new()
            .name("accent-rpc".to_string())
            .spawn({
                let (pending, dead) = (pending.clone(), dead.clone());
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
                    pending.lock().unwrap_or_else(|e| e.into_inner()).clear();
                }
            })
            .ok();

        Client {
            out: Mutex::new(out),
            next_id: AtomicU64::new(1),
            pending,
            dead,
            reader: Mutex::new(reader),
        }
    }

    pub fn is_dead(&self) -> bool {
        self.dead.load(Ordering::SeqCst)
    }

    /// Ask, and wait for the answer.
    pub fn call<T: DeserializeOwned>(&self, method: &str, params: Value) -> Result<T, RpcError> {
        let value = self.call_value(method, params)?;
        serde_json::from_value(value)
            .map_err(|e| RpcError::failed(format!("{method} answered something unreadable: {e}")))
    }

    fn call_value(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        if self.is_dead() {
            return Err(RpcError::failed("not connected"));
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = channel();
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, tx);

        let line = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        if let Err(e) = self.write(&line) {
            self.pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&id);
            return Err(RpcError::failed(format!("cannot reach the server: {e}")));
        }

        match rx.recv_timeout(DEADLINE) {
            Ok(answer) => answer,
            // The sender was dropped: the reader thread saw EOF and cleared the map.
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                Err(RpcError::failed("the connection closed"))
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                self.pending
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&id);
                Err(RpcError::failed(format!("{method} timed out")))
            }
        }
    }

    fn write(&self, line: &Value) -> std::io::Result<()> {
        let mut out = self.out.lock().unwrap_or_else(|e| e.into_inner());
        serde_json::to_writer(&mut *out, line)?;
        out.write_all(b"\n")?;
        out.flush()
    }

    /// Let go of the writing end, which is what tells `serve` to stop, then wait for the reader
    /// thread to notice the far end has closed.
    pub fn shutdown(&self) {
        // Replacing the writer drops the pipe, and `serve` reads EOF on its stdin.
        {
            let mut out = self.out.lock().unwrap_or_else(|e| e.into_inner());
            *out = Box::new(std::io::sink());
        }
        if let Some(handle) = self.reader.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = handle.join();
        }
    }
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
        if let Some(tx) = pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&id)
        {
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
    input: impl Read,
    output: impl Write + Send + 'static,
) -> anyhow::Result<()> {
    let cfg = crate::VaultConfig::default();
    let (vault, events) = match db {
        Some(db) => Local::open_at(root, db, cfg)?,
        None => Local::open(root, cfg)?,
    };
    serve_local(vault, events, input, output);
    Ok(())
}

/// Answer requests on `input` until it ends, forwarding `events` as notifications.
///
/// Each request runs on a thread of its own: the vault is `Sync` and keeps a second SQLite reader
/// for exactly this, so a whole-vault grep does not hold up the tree the user is clicking through.
/// Every line is written whole under one lock, so two answers can never interleave.
pub(crate) fn serve_local(
    vault: Local,
    events: Receiver<Event>,
    input: impl Read,
    output: impl Write + Send + 'static,
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

    let mut workers = Vec::new();
    for line in BufReader::new(input).lines() {
        let Ok(line) = line else { break };
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

        let (vault, out) = (vault.clone(), out.clone());
        let worker = std::thread::spawn(move || {
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
    let mut out = out.lock().unwrap_or_else(|e| e.into_inner());
    serde_json::to_writer(&mut *out, line)?;
    out.write_all(b"\n")?;
    out.flush()
}

// ----------------------------------------------------------------- dispatch

/// One positional argument, deserialised.
fn arg<T: DeserializeOwned>(params: &Value, n: usize) -> Result<T, RpcError> {
    let value = params.get(n).cloned().unwrap_or(Value::Null);
    serde_json::from_value(value)
        .map_err(|e| RpcError::failed(format!("argument {n} is not what was expected: {e}")))
}

/// Whatever the method returned, as JSON.
fn ok<T: serde::Serialize>(value: T) -> Result<Value, RpcError> {
    serde_json::to_value(value).map_err(|e| RpcError::failed(format!("cannot answer: {e}")))
}

/// An `anyhow` failure, formatted the way the app would have shown it locally.
fn any<T: serde::Serialize>(r: anyhow::Result<T>) -> Result<Value, RpcError> {
    ok(r.map_err(|e| RpcError::failed(format!("{e:#}")))?)
}

fn io<T: serde::Serialize>(r: std::io::Result<T>) -> Result<Value, RpcError> {
    match r {
        Ok(v) => ok(v),
        Err(e) => Err(RpcError {
            code: IO,
            message: e.to_string(),
            data: None,
        }),
    }
}

fn git_result<T: serde::Serialize>(
    r: Result<T, accent_core::git::Error>,
) -> Result<Value, RpcError> {
    ok(r.map_err(RpcError::failed)?)
}

/// The whole remote surface. Everything the UI can ask of a vault it cannot reach is one arm
/// here; anything absent is deliberately local — the session file, the config, and path
/// arithmetic, all of which belong to the machine the window is on.
fn dispatch(vault: &Local, method: &str, p: &Value) -> Result<Value, RpcError> {
    use accent_core::git;
    let repo = |n: usize| arg::<git::Repo>(p, n);
    // `git` takes `&[&str]`, the wire carries owned strings.
    let paths = |n: usize| -> Result<Vec<String>, RpcError> { arg(p, n) };
    fn refs(v: &[String]) -> Vec<&str> {
        v.iter().map(String::as_str).collect()
    }

    match method {
        "hello" => {
            vault.set_config(arg(p, 0)?);
            ok(Hello {
                root: vault.root().to_path_buf(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            })
        }

        // ------------------------------------------------------------ files
        "read" => io(vault.read(&arg::<String>(p, 0)?)),
        "read_text" => io(vault.read_text(&arg::<String>(p, 0)?)),
        "stat" => io(vault.stat(&arg::<String>(p, 0)?)),
        "delete" => io(vault.delete(&arg::<String>(p, 0)?)),
        "save" => match vault.save(&arg::<String>(p, 0)?, &arg::<String>(p, 1)?, arg(p, 2)?) {
            Ok(etag) => ok(etag),
            Err(SaveError::ChangedOnDisk { current }) => Err(RpcError {
                code: CHANGED_ON_DISK,
                message: "file changed on disk since it was read".to_string(),
                data: serde_json::to_value(current).ok(),
            }),
            Err(SaveError::Io(e)) => Err(RpcError {
                code: IO,
                message: e.to_string(),
                data: None,
            }),
        },
        "create_note" => any(vault.create_note(
            &arg::<String>(p, 0)?,
            arg::<Option<String>>(p, 1)?.as_deref(),
        )),
        "create_dir" => io(vault.create_dir(&arg::<String>(p, 0)?)),
        "plan_rename" => any(vault.plan_rename(&arg::<String>(p, 0)?, &arg::<String>(p, 1)?)),
        "rename" => any(vault.rename(&arg(p, 0)?, arg(p, 1)?)),
        "replace_all" => {
            let re = compile(p, 0, 1)?;
            any(vault.replace_all(&re, &arg::<String>(p, 2)?, arg(p, 3)?))
        }
        "adopt_conflict" => any(vault.adopt_conflict(&arg::<String>(p, 0)?, &arg::<String>(p, 1)?)),
        "conflict_diff" => any(vault.conflict_diff(&arg::<String>(p, 0)?, &arg::<String>(p, 1)?)),
        "daily_note" => any(vault.daily_note()),
        "templates" => any(vault.templates()),

        // ------------------------------------------------------------ index
        "list_dir" => any(vault.list_dir(&arg::<String>(p, 0)?)),
        "search" => any(vault.search(&arg::<String>(p, 0)?, arg(p, 1)?, arg(p, 2)?)),
        "grep" => {
            let re = compile(p, 0, 1)?;
            any(vault.grep(&re, arg(p, 2)?, arg(p, 3)?))
        }
        "grep_unindexed" => {
            let re = compile(p, 0, 1)?;
            any(vault.grep_unindexed(&re, arg(p, 2)?))
        }
        "tags" => any(vault.tags()),
        "files_with_tag" => any(vault.files_with_tag(&arg::<String>(p, 0)?)),
        "backlinks" => any(vault.backlinks(&arg::<String>(p, 0)?)),
        "note_paths" => any(vault.note_paths()),
        "file_paths" => any(vault.file_paths(arg(p, 0)?)),
        "set_excluded" => any(vault.set_excluded(&paths(0)?)),
        "recent_notes" => any(vault.recent_notes(arg(p, 0)?)),
        "resolve_link" => any(vault.resolve_link(&arg::<String>(p, 0)?)),
        "conflicts" => any(vault.conflicts()),
        "conflicts_of" => any(vault.conflicts_of(&arg::<String>(p, 0)?)),
        "complete_notes" => any(vault.complete_notes(&arg::<String>(p, 0)?, arg(p, 1)?)),
        "complete_tags" => any(vault.complete_tags(&arg::<String>(p, 0)?, arg(p, 1)?)),
        "rescan" => {
            vault.rescan();
            ok(())
        }

        // --------------------------------------------------------- language
        // Every request runs on its own thread here (`serve_local`), so blocking on the runtime
        // is a wait for one answer and never a nested one.
        "open_document" => any(block(vault.open_document(
            &arg::<String>(p, 0)?,
            &arg::<String>(p, 1)?,
            arg(p, 2)?,
        ))),
        "change_document" => any(block(
            vault.change_document(&arg::<String>(p, 0)?, arg(p, 1)?),
        )),
        "close_document" => any(block(vault.close_document(&arg::<String>(p, 0)?))),
        "completion" => any(block(vault.completion(
            &arg::<String>(p, 0)?,
            arg(p, 1)?,
            arg(p, 2)?,
        ))),
        "resolve_completion" => any(block(
            vault.resolve_completion(&arg::<String>(p, 0)?, arg(p, 1)?),
        )),
        "signature_help" => any(block(
            vault.signature_help(&arg::<String>(p, 0)?, arg(p, 1)?),
        )),
        "hover" => any(block(vault.hover(&arg::<String>(p, 0)?, arg(p, 1)?))),
        "definition" => any(block(vault.definition(&arg::<String>(p, 0)?, arg(p, 1)?))),
        "references" => any(block(vault.references(&arg::<String>(p, 0)?, arg(p, 1)?))),
        "symbols" => any(block(vault.symbols(&arg::<String>(p, 0)?))),
        "folds" => any(block(vault.folds(&arg::<String>(p, 0)?))),

        // -------------------------------------------------------------- git
        "repos" => ok(vault.repos()),
        "git_status" => git_result(git::status(&repo(0)?)),
        "git_log" => git_result(git::log(&repo(0)?, arg(p, 1)?, arg(p, 2)?)),
        "git_show" => git_result(git::show(
            &repo(0)?,
            &arg::<String>(p, 1)?,
            &arg::<String>(p, 2)?,
        )),
        "git_changed_files" => git_result(git::changed_files(&repo(0)?, &arg::<String>(p, 1)?)),
        "git_submodules" => git_result(git::submodules(&repo(0)?)),
        "git_commit" => git_result(git::commit(&repo(0)?, &arg::<String>(p, 1)?, arg(p, 2)?)),
        "git_sync" => git_result(git::sync(&repo(0)?)),
        "git_stage" => git_result(git::stage(&repo(0)?, &refs(&paths(1)?))),
        "git_unstage" => git_result(git::unstage(&repo(0)?, &refs(&paths(1)?))),
        "git_discard" => git_result(git::discard(&repo(0)?, &refs(&paths(1)?))),

        _ => Err(RpcError::failed(format!("no such method: {method}"))),
    }
}

/// Wait for a language request. The task is not dropped before it answers, so nothing is
/// cancelled: the caller on the other end of the pipe is already waiting for this one.
fn block<T>(task: crate::Task<T>) -> anyhow::Result<T> {
    accent_lsp::runtime().block_on(task)
}

/// The two arguments every exact-search method carries, compiled where the files are. A pattern
/// that does not compile is the caller's mistake, not a broken connection, so it comes back as a
/// plain failure with regex's own wording.
fn compile(
    p: &Value,
    query: usize,
    options: usize,
) -> Result<accent_core::search::Regex, RpcError> {
    let (q, o) = (arg::<String>(p, query)?, arg(p, options)?);
    accent_core::search::pattern(&q, o).map_err(RpcError::failed)
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
            let root = tempfile::tempdir().unwrap();
            let cache = tempfile::tempdir().unwrap();
            std::fs::write(root.path().join("a.md"), "hello [[b]]\n").unwrap();
            std::fs::write(root.path().join("b.md"), "#tag\n").unwrap();

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
                serve_local(vault, vault_events, server_in, server_out);
            });

            let (events, event_rx) = channel();
            let client = Client::new(Box::new(client_out), Box::new(client_in), events);
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
        assert_eq!(support.completion_triggers, ['[', '#']);

        let symbols: Vec<crate::Symbol> = w.client.call("symbols", json!(["a.md"])).unwrap();
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "Title");

        assert!(
            w.wait(|e| matches!(e, Event::Diagnostics { rel, items }
                if rel == "a.md" && items.len() == 1)),
            "the dangling link has to reach the client as a notification"
        );
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

    /// The session belongs to the machine the window is on, so it is not on the wire at all.
    #[test]
    fn the_session_is_not_a_remote_method() {
        let w = Wired::open();
        assert!(w.client.call::<Session>("session", json!([])).is_err());
    }
}
