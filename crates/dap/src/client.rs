//! Debug-adapter transport with a child process, reader and writer tasks.
//!
//! The language client's sibling: the same framing on the same runtime, with DAP's envelope.
//! Every message carries a `seq`, an answer names the request's, and the adapter's events and its
//! own requests (`runInTerminal`) go to the caller, which alone knows what to do with them.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use accent_lsp::{codec, runtime};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::process::ChildStderr;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

/// Request timeout, long enough for normal replies while bounding a stalled adapter.
pub const DEADLINE: Duration = Duration::from_secs(10);

/// What the adapter says unasked: an event, or a request of its own for [`Client::respond`].
#[derive(Debug, Clone)]
pub enum Incoming {
    Event {
        event: String,
        body: Value,
    },
    Request {
        seq: i64,
        command: String,
        arguments: Value,
    },
}

/// Unbounded so a slow consumer cannot block the reader; it ends when the adapter does.
pub type Incomings = mpsc::UnboundedReceiver<Incoming>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("the debug adapter exited")]
    Closed,
    #[error("the debug adapter did not answer")]
    Timeout,
    /// A `success: false` answer, with the adapter's reason.
    #[error("{message}")]
    Response { command: String, message: String },
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// A running debug adapter.
pub struct Client {
    inner: Arc<Inner>,
    /// `None` for test transports without a child process.
    child: Mutex<Option<tokio::process::Child>>,
}

/// State shared by the reader and pending requests.
struct Inner {
    /// Taken by [`Client::shutdown`]: the writer then ends, closing the adapter's stdin.
    out: Mutex<Option<mpsc::UnboundedSender<Vec<u8>>>>,
    pending: Mutex<HashMap<i64, oneshot::Sender<Result<Value, Error>>>>,
    /// Numbers our requests and our answers alike: DAP has one sequence per sender.
    seq: AtomicI64,
    dead: AtomicBool,
    /// The adapter's last line on stderr.
    last_words: Mutex<Option<String>>,
}

impl Client {
    /// Start `argv` in `cwd` and talk to it over its stdio.
    pub fn spawn(argv: &[String], cwd: &Path) -> std::io::Result<(Client, Incomings)> {
        let Some((exe, args)) = argv.split_first() else {
            return Err(std::io::Error::other("no debug adapter command"));
        };
        // `tokio::process` registers the child with the reactor, so the spawn has to happen
        // inside the runtime even though this function is not async.
        let _guard = runtime().enter();
        let mut child = tokio::process::Command::new(exe)
            .args(args)
            .current_dir(cwd)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            return Err(std::io::Error::other("the debug adapter has no stdio"));
        };
        let (client, incomings) = Client::wire(stdout, stdin, Some(stderr));
        *lock(&client.child) = Some(child);
        Ok((client, incomings))
    }

    /// Talk to whatever is on the other end of these two streams.
    pub fn new(
        reader: impl AsyncRead + Send + Unpin + 'static,
        writer: impl AsyncWrite + Send + Unpin + 'static,
    ) -> (Client, Incomings) {
        Client::wire(reader, writer, None)
    }

    fn wire(
        reader: impl AsyncRead + Send + Unpin + 'static,
        writer: impl AsyncWrite + Send + Unpin + 'static,
        stderr: Option<ChildStderr>,
    ) -> (Client, Incomings) {
        let (out, outbox) = mpsc::unbounded_channel();
        let (incoming, incomings) = mpsc::unbounded_channel();
        let inner = Arc::new(Inner {
            out: Mutex::new(Some(out)),
            pending: Mutex::new(HashMap::new()),
            seq: AtomicI64::new(1),
            dead: AtomicBool::new(false),
            last_words: Mutex::new(None),
        });

        // Adapters log freely on stderr; a full pipe would block them, so it is always drained.
        let drained = stderr.map(|stderr| runtime().spawn(drain(inner.clone(), stderr)));
        runtime().spawn(write_loop(outbox, writer));
        runtime().spawn(read_loop(inner.clone(), reader, incoming, drained));

        let client = Client {
            inner,
            child: Mutex::new(None),
        };
        (client, incomings)
    }

    /// Ask the adapter something and wait up to [`DEADLINE`] for its answer.
    pub async fn request<R: DeserializeOwned>(
        &self,
        command: &str,
        arguments: impl Serialize,
    ) -> Result<R, Error> {
        tokio::time::timeout(DEADLINE, self.request_unbounded(command, arguments))
            .await
            .unwrap_or(Err(Error::Timeout))
    }

    /// [`Client::request`] without a deadline: `launch` answers once the program runs, and a
    /// REPL `evaluate` runs the reader's code. A dropped future only forgets its waiter; the
    /// adapter is not told, debugpy having no `cancel`.
    pub async fn request_unbounded<R: DeserializeOwned>(
        &self,
        command: &str,
        arguments: impl Serialize,
    ) -> Result<R, Error> {
        let seq = self.inner.next_seq();
        let (tx, rx) = oneshot::channel();
        self.inner.waiting().insert(seq, tx);
        let _waiter = Waiter {
            inner: &self.inner,
            seq,
        };
        // Looked at after the waiter is in, so a reader ending meanwhile either clears it or has
        // already said so here.
        if self.inner.dead.load(Ordering::SeqCst) {
            return Err(Error::Closed);
        }
        self.inner.send(&json!({
            "seq": seq,
            "type": "request",
            "command": command,
            "arguments": serde_json::to_value(arguments)?,
        }))?;
        // The reader dropping the sender means the adapter is gone.
        let body = rx.await.map_err(|_| Error::Closed)??;
        Ok(serde_json::from_value(body)?)
    }

    /// Answer the adapter's request `request_seq`: a body, or why not.
    pub fn respond(
        &self,
        request_seq: i64,
        command: &str,
        outcome: Result<Value, String>,
    ) -> Result<(), Error> {
        let mut message = json!({
            "seq": self.inner.next_seq(),
            "type": "response",
            "request_seq": request_seq,
            "command": command,
            "success": outcome.is_ok(),
        });
        match outcome {
            Ok(body) => message["body"] = body,
            Err(why) => message["message"] = why.into(),
        }
        self.inner.send(&message)
    }

    /// The handshake, and the only place accent says what it can do. The adapter's capabilities go
    /// unread: debugpy, the only adapter yet, has all accent asks for.
    pub async fn initialize(&self, adapter_id: &str) -> Result<(), Error> {
        let arguments = json!({
            "clientID": "accent",
            "clientName": "accent",
            "adapterID": adapter_id,
            "linesStartAt1": true,
            "columnsStartAt1": true,
            "pathFormat": "path",
            "supportsRunInTerminalRequest": true,
            "supportsVariableType": true,
        });
        self.request::<Value>("initialize", arguments).await?;
        Ok(())
    }

    /// The adapter's last line on stderr: why one that exited before answering did, such as
    /// `No module named debugpy`. In before any waiter hears of the exit.
    pub fn last_words(&self) -> Option<String> {
        lock(&self.inner.last_words).clone()
    }

    /// Close the adapter's stdin, which ends it (debugpy exits on EOF), and kill it after a
    /// second, so a session's end leaves no process behind.
    pub async fn shutdown(&self) {
        lock(&self.inner.out).take();
        let child = lock(&self.child).take();
        let Some(mut child) = child else { return };
        if tokio::time::timeout(Duration::from_secs(1), child.wait())
            .await
            .is_err()
        {
            let _ = child.start_kill();
        }
    }
}

impl Inner {
    fn waiting(&self) -> MutexGuard<'_, HashMap<i64, oneshot::Sender<Result<Value, Error>>>> {
        lock(&self.pending)
    }

    fn next_seq(&self) -> i64 {
        self.seq.fetch_add(1, Ordering::SeqCst)
    }

    fn send(&self, message: &Value) -> Result<(), Error> {
        let body = serde_json::to_vec(message)?;
        match &*lock(&self.out) {
            Some(out) => out.send(codec::frame(&body)).map_err(|_| Error::Closed),
            None => Err(Error::Closed),
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Forgets its request's waiter when the request ends, whether answered, failed or dropped.
struct Waiter<'a> {
    inner: &'a Inner,
    seq: i64,
}

impl Drop for Waiter<'_> {
    fn drop(&mut self) {
        self.inner.waiting().remove(&self.seq);
    }
}

async fn write_loop(
    mut outbox: mpsc::UnboundedReceiver<Vec<u8>>,
    mut writer: impl AsyncWrite + Unpin,
) {
    while let Some(bytes) = outbox.recv().await {
        if writer.write_all(&bytes).await.is_err() || writer.flush().await.is_err() {
            break;
        }
    }
}

async fn drain(inner: Arc<Inner>, stderr: ChildStderr) {
    let mut lines = BufReader::new(stderr).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        tracing::debug!(target: "accent_dap::stderr", "{line}");
        *lock(&inner.last_words) = Some(line);
    }
}

async fn read_loop(
    inner: Arc<Inner>,
    reader: impl AsyncRead + Unpin,
    incoming: mpsc::UnboundedSender<Incoming>,
    drained: Option<JoinHandle<()>>,
) {
    let mut reader = BufReader::new(reader);
    loop {
        match codec::read(&mut reader).await {
            Ok(Some(body)) => match serde_json::from_slice(&body) {
                Ok(message) => route(&inner, message, &incoming),
                Err(e) => tracing::warn!("unreadable message from the debug adapter: {e}"),
            },
            Ok(None) => break,
            Err(e) => {
                tracing::debug!("the debug adapter's output ended: {e}");
                break;
            }
        }
    }
    // An adapter that dies says why on stderr, which closes with stdout. Its last line is read in
    // before anyone hears of the end, briefly, since a child of the adapter may hold the pipe.
    if let Some(drained) = drained {
        let _ = tokio::time::timeout(Duration::from_secs(1), drained).await;
    }
    // Everyone still waiting is told at once by having their sender dropped.
    inner.dead.store(true, Ordering::SeqCst);
    inner.waiting().clear();
}

/// One message from the adapter: an answer, an event, or a request of its own.
fn route(inner: &Inner, message: Value, incoming: &mpsc::UnboundedSender<Incoming>) {
    let text = |key: &str| message.get(key).and_then(Value::as_str).map(str::to_string);
    let take = |key: &str| message.get(key).cloned().unwrap_or(Value::Null);
    match message.get("type").and_then(Value::as_str) {
        Some("response") => {
            let Some(seq) = message.get("request_seq").and_then(Value::as_i64) else {
                return;
            };
            let answer = match message.get("success").and_then(Value::as_bool) {
                Some(true) => Ok(take("body")),
                _ => Err(Error::Response {
                    command: text("command").unwrap_or_default(),
                    message: message
                        .pointer("/body/error/format")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .or_else(|| text("message"))
                        .unwrap_or_else(|| "the debug adapter refused".to_string()),
                }),
            };
            if let Some(tx) = inner.waiting().remove(&seq) {
                let _ = tx.send(answer);
            }
        }
        Some("event") => {
            let _ = incoming.send(Incoming::Event {
                event: text("event").unwrap_or_default(),
                body: take("body"),
            });
        }
        Some("request") => {
            let _ = incoming.send(Incoming::Request {
                seq: message.get("seq").and_then(Value::as_i64).unwrap_or(0),
                command: text("command").unwrap_or_default(),
                arguments: take("arguments"),
            });
        }
        _ => tracing::warn!("a message that is neither an answer, an event nor a request"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{RunInTerminalRequestArguments, StoppedEventBody, ThreadsResponse};
    use accent_lsp::{codec, runtime};
    use serde_json::json;
    use std::time::Instant;
    use tokio::io::{AsyncBufRead, AsyncWriteExt, BufReader, ReadHalf, WriteHalf, split};

    type Duplex = tokio::io::DuplexStream;

    /// A client and a fake adapter joined by one pipe, so the tests exercise the real framing and
    /// the real reader task instead of a mock.
    fn wired() -> (
        Client,
        Incomings,
        BufReader<ReadHalf<Duplex>>,
        WriteHalf<Duplex>,
    ) {
        let (ours, theirs) = tokio::io::duplex(64 * 1024);
        let (read, write) = split(ours);
        let (client, incomings) = Client::new(read, write);
        let (their_read, their_write) = split(theirs);
        (client, incomings, BufReader::new(their_read), their_write)
    }

    async fn recv(r: &mut (impl AsyncBufRead + Unpin)) -> Value {
        let body = codec::read(r).await.unwrap().unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    async fn send(w: &mut (impl AsyncWrite + Unpin), message: Value) {
        let body = serde_json::to_vec(&message).unwrap();
        w.write_all(&codec::frame(&body)).await.unwrap();
        w.flush().await.unwrap();
    }

    /// `accent-api` spawns these futures on the runtime, so they have to be `Send`.
    fn sendable<T: Send>(t: T) -> T {
        t
    }

    #[test]
    fn an_answer_comes_back_typed_and_a_refusal_is_an_error() {
        runtime().block_on(async {
            let (client, _incomings, mut theirs, mut their_write) = wired();
            let adapter = tokio::spawn(async move {
                let ask = recv(&mut theirs).await;
                assert_eq!(
                    (&ask["type"], &ask["command"]),
                    (&json!("request"), &json!("threads"))
                );
                send(
                    &mut their_write,
                    json!({"seq": 1, "type": "response", "request_seq": ask["seq"],
                           "command": "threads", "success": true,
                           "body": {"threads": [{"id": 1, "name": "MainThread"}]}}),
                )
                .await;
                // debugpy says why in `message`; other adapters in `body.error.format`.
                let ask = recv(&mut theirs).await;
                send(
                    &mut their_write,
                    json!({"seq": 2, "type": "response", "request_seq": ask["seq"],
                           "command": "evaluate", "success": false, "message": "evaluate failed",
                           "body": {"error": {"id": 1, "format": "name 'y' is not defined"}}}),
                )
                .await;
            });

            let answer: ThreadsResponse = sendable(client.request("threads", json!({})))
                .await
                .unwrap();
            assert_eq!(answer.threads[0].id, 1);
            assert_eq!(answer.threads[0].name, "MainThread");

            let error = client
                .request::<Value>("evaluate", json!({"expression": "y"}))
                .await
                .unwrap_err();
            let Error::Response { command, message } = error else {
                panic!("{error}")
            };
            assert_eq!(command, "evaluate");
            assert_eq!(message, "name 'y' is not defined");
            adapter.await.unwrap();
        });
    }

    #[test]
    fn events_and_the_adapters_requests_reach_the_caller_and_are_answered() {
        runtime().block_on(async {
            let (client, mut incomings, mut theirs, mut their_write) = wired();
            send(
                &mut their_write,
                json!({"seq": 6, "type": "event", "event": "stopped",
                       "body": {"reason": "breakpoint", "threadId": 1}}),
            )
            .await;
            send(
                &mut their_write,
                json!({"seq": 7, "type": "request", "command": "runInTerminal",
                       "arguments": {"kind": "integrated", "cwd": "/tmp/a vault",
                                     "args": ["python3", "demo.py"], "env": {}}}),
            )
            .await;

            let Some(Incoming::Event { event, body }) = incomings.recv().await else {
                panic!("no event")
            };
            assert_eq!(event, "stopped");
            let stopped: StoppedEventBody = serde_json::from_value(body).unwrap();
            assert_eq!(
                (stopped.reason.as_str(), stopped.thread_id),
                ("breakpoint", Some(1))
            );

            let Some(Incoming::Request {
                seq,
                command,
                arguments,
            }) = incomings.recv().await
            else {
                panic!("no request")
            };
            assert_eq!((seq, command.as_str()), (7, "runInTerminal"));
            let run: RunInTerminalRequestArguments = serde_json::from_value(arguments).unwrap();
            assert_eq!(run.args, ["python3", "demo.py"]);

            client
                .respond(7, "runInTerminal", Ok(json!({"processId": 4242})))
                .unwrap();
            let answer = recv(&mut theirs).await;
            assert_eq!(answer["type"], "response");
            assert_eq!(answer["request_seq"], 7);
            assert_eq!(answer["command"], "runInTerminal");
            assert_eq!(answer["success"], true);
            assert_eq!(answer["body"]["processId"], 4242);
            assert!(answer["seq"].is_i64());
        });
    }

    #[test]
    fn a_dead_adapter_frees_its_waiters_at_once() {
        runtime().block_on(async {
            let (client, _incomings, mut theirs, their_write) = wired();
            tokio::spawn(async move {
                recv(&mut theirs).await;
                drop(theirs);
                drop(their_write);
            });

            // Unbounded, so only the adapter's end can free it.
            let started = Instant::now();
            let error = client
                .request_unbounded::<Value>("launch", json!({}))
                .await
                .unwrap_err();
            assert!(matches!(error, Error::Closed), "{error}");
            assert!(started.elapsed() < Duration::from_secs(1));
        });
    }
}
