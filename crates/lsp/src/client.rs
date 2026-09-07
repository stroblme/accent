//! The near end of a language server: one child process, one reader task, one writer task.
//!
//! The shape follows `accent-api`'s JSON-RPC client (`rpc.rs`) — an id per request, a map of who
//! is waiting, a deadline so a wedged server is a message rather than a hang — with two things
//! LSP adds. A server may ask *us* questions, which the reader answers inline because none of
//! them need anything but the client's own state. And a request whose future is dropped sends
//! `$/cancelRequest`: the editor supersedes a completion on every keystroke, and telling the
//! server to stop is what keeps a big project's analysis from queueing behind work nobody wants.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};

use crate::runtime::runtime;
use crate::types::{InitializeResult, ServerCapabilities};
use crate::{codec, from_uri, to_uri};

/// How long a request waits before giving up. The same number and the same reasoning as
/// `rpc.rs`: longer than any answer a person would wait for, shorter than a hang.
pub const DEADLINE: Duration = Duration::from_secs(10);

/// A message the server sent to nobody in particular and this client does not handle itself —
/// `textDocument/publishDiagnostics`, above all.
#[derive(Debug, Clone)]
pub struct Notification {
    pub method: String,
    pub params: Value,
}

/// Where those arrive. Unbounded: the reader task must never block on a slow consumer.
pub type Notifications = mpsc::UnboundedReceiver<Notification>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("the language server exited")]
    Closed,
    #[error("the language server did not answer")]
    Timeout,
    #[error("{message}")]
    Response { code: i64, message: String },
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// A running language server.
pub struct Client {
    inner: Arc<Inner>,
    /// `None` when the transport is not a process, which is how the tests here are wired.
    child: Mutex<Option<tokio::process::Child>>,
}

/// Everything the reader task and the request futures share.
struct Inner {
    out: mpsc::UnboundedSender<Vec<u8>>,
    pending: Mutex<HashMap<u64, oneshot::Sender<Result<Value, Error>>>>,
    next_id: AtomicU64,
    dead: AtomicBool,
    /// Set by `initialize`, and the answer to `workspace/workspaceFolders` afterwards.
    root_uri: OnceLock<String>,
}

impl Client {
    /// Start `argv` in `cwd` and talk to it over its stdio.
    pub fn spawn(argv: &[String], cwd: &Path) -> std::io::Result<(Client, Notifications)> {
        let Some((exe, args)) = argv.split_first() else {
            return Err(std::io::Error::other("no language server command"));
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
            return Err(std::io::Error::other("the language server has no stdio"));
        };

        // Servers log freely on stderr; a full pipe would block them, so it is always drained.
        runtime().spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::debug!(target: "accent_lsp::stderr", "{line}");
            }
        });

        let (client, notifications) = Client::new(stdout, stdin);
        *client.child.lock().unwrap_or_else(|e| e.into_inner()) = Some(child);
        Ok((client, notifications))
    }

    /// Talk to whatever is on the other end of these two streams.
    pub fn new(
        reader: impl AsyncRead + Send + Unpin + 'static,
        writer: impl AsyncWrite + Send + Unpin + 'static,
    ) -> (Client, Notifications) {
        let (out, outbox) = mpsc::unbounded_channel();
        let (notifier, notifications) = mpsc::unbounded_channel();
        let inner = Arc::new(Inner {
            out,
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            dead: AtomicBool::new(false),
            root_uri: OnceLock::new(),
        });

        runtime().spawn(write_loop(outbox, writer));
        runtime().spawn(read_loop(inner.clone(), reader, notifier));

        let client = Client {
            inner,
            child: Mutex::new(None),
        };
        (client, notifications)
    }

    pub fn is_dead(&self) -> bool {
        self.inner.dead.load(Ordering::SeqCst)
    }

    /// Tell the server something. Synchronous, because the writer task owns the stream and this
    /// only hands it bytes — which is what lets `Drop` send `$/cancelRequest` and `exit`.
    pub fn notify(&self, method: &str, params: impl Serialize) -> Result<(), Error> {
        if self.is_dead() {
            return Err(Error::Closed);
        }
        self.inner.send(&json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": serde_json::to_value(params)?,
        }))
    }

    /// Ask the server something and wait for its answer.
    ///
    /// Dropping the returned future — a superseded completion, an aborted `Task` — cancels the
    /// request at the server instead of letting it finish into nothing.
    pub async fn request<R: DeserializeOwned>(
        &self,
        method: &str,
        params: impl Serialize,
    ) -> Result<R, Error> {
        if self.is_dead() {
            return Err(Error::Closed);
        }
        let id = self.inner.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.inner.waiting().insert(id, tx);

        let message = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": serde_json::to_value(params)?,
        });
        if let Err(e) = self.inner.send(&message) {
            self.inner.waiting().remove(&id);
            return Err(e);
        }

        let cancel = Cancel {
            inner: self.inner.clone(),
            id,
        };
        let answer = match tokio::time::timeout(DEADLINE, rx).await {
            Ok(Ok(answer)) => answer?,
            // The reader task cleared the map: the server is gone.
            Ok(Err(_)) => return Err(Error::Closed),
            Err(_) => return Err(Error::Timeout),
        };
        drop(cancel);
        Ok(serde_json::from_value(answer)?)
    }

    /// The handshake, and the only place accent says what it can do.
    ///
    /// Everything announced here has a caller on the other side of `accent-api`: no
    /// `linkSupport`, because a plain [`crate::types::Location`] is all the UI jumps to.
    pub async fn initialize(&self, root: &Path) -> Result<ServerCapabilities, Error> {
        let uri = to_uri(root);
        let name = name_of(&uri);
        // Set before asking, so a server that wants the folders while initializing gets them.
        let _ = self.inner.root_uri.set(uri.clone());

        let params = json!({
            "processId": std::process::id(),
            "rootUri": uri,
            "rootPath": root.to_string_lossy(),
            "workspaceFolders": [{"uri": uri, "name": name}],
            "clientInfo": {"name": "accent", "version": env!("CARGO_PKG_VERSION")},
            "capabilities": {
                "general": {"positionEncodings": ["utf-8", "utf-16"]},
                "textDocument": {
                    "synchronization": {"didSave": false},
                    "completion": {
                        "completionItem": {
                            "snippetSupport": true,
                            "insertReplaceSupport": true,
                            "documentationFormat": ["markdown", "plaintext"],
                            "resolveSupport": {
                                "properties": ["documentation", "detail", "additionalTextEdits"]
                            }
                        },
                        "contextSupport": true
                    },
                    "hover": {"contentFormat": ["markdown", "plaintext"]},
                    "signatureHelp": {
                        "signatureInformation": {
                            "documentationFormat": ["markdown", "plaintext"],
                            "parameterInformation": {"labelOffsetSupport": true}
                        }
                    },
                    "definition": {},
                    "references": {},
                    "documentSymbol": {"hierarchicalDocumentSymbolSupport": true},
                    "foldingRange": {"lineFoldingOnly": true},
                    "publishDiagnostics": {}
                },
                "workspace": {"configuration": true, "workspaceFolders": true},
                "window": {"workDoneProgress": true}
            }
        });

        let result: InitializeResult = self.request("initialize", params).await?;
        self.notify("initialized", json!({}))?;
        Ok(result.capabilities)
    }

    /// The orderly goodbye, on a short leash: a server that will not stop is killed rather than
    /// left behind when the window closes.
    pub async fn shutdown(&self) {
        let _ = tokio::time::timeout(
            Duration::from_secs(1),
            self.request::<Value>("shutdown", Value::Null),
        )
        .await;
        let _ = self.notify("exit", Value::Null);

        let child = self.child.lock().unwrap_or_else(|e| e.into_inner()).take();
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
    fn waiting(
        &self,
    ) -> std::sync::MutexGuard<'_, HashMap<u64, oneshot::Sender<Result<Value, Error>>>> {
        self.pending.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn send(&self, message: &Value) -> Result<(), Error> {
        let body = serde_json::to_vec(message)?;
        self.out
            .send(codec::frame(&body))
            .map_err(|_| Error::Closed)
    }

    /// Answer a question the server asked us. Everything it can ask is either a formality or
    /// something this client already knows, so nothing here goes back to the caller.
    fn answer(&self, id: &Value, method: &str, params: Option<&Value>) {
        let outcome = match method {
            "client/registerCapability"
            | "client/unregisterCapability"
            | "window/workDoneProgress/create"
            | "window/showMessageRequest" => Ok(Value::Null),
            // accent has no per-server settings, but the answer must have the shape the server
            // asked for: one entry per item.
            "workspace/configuration" => {
                let items = params
                    .and_then(|p| p.get("items"))
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len);
                Ok(Value::Array(vec![Value::Null; items]))
            }
            "workspace/workspaceFolders" => Ok(match self.root_uri.get() {
                Some(uri) => json!([{"uri": uri, "name": name_of(uri)}]),
                None => Value::Null,
            }),
            _ => Err(json!({"code": -32601, "message": "method not found"})),
        };
        let message = match outcome {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
            Err(error) => json!({"jsonrpc": "2.0", "id": id, "error": error}),
        };
        let _ = self.send(&message);
    }
}

/// Cancels its request unless the answer already arrived. One guard covers both ways a request
/// can end early: the deadline, and a dropped future.
struct Cancel {
    inner: Arc<Inner>,
    id: u64,
}

impl Drop for Cancel {
    fn drop(&mut self) {
        if self.inner.waiting().remove(&self.id).is_some() {
            let _ = self.inner.send(&json!({
                "jsonrpc": "2.0",
                "method": "$/cancelRequest",
                "params": {"id": self.id},
            }));
        }
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

async fn read_loop(
    inner: Arc<Inner>,
    reader: impl AsyncRead + Unpin,
    notifier: mpsc::UnboundedSender<Notification>,
) {
    let mut reader = BufReader::new(reader);
    loop {
        match codec::read(&mut reader).await {
            Ok(Some(body)) => match serde_json::from_slice(&body) {
                Ok(message) => route(&inner, message, &notifier),
                Err(e) => tracing::warn!("unreadable message from the language server: {e}"),
            },
            Ok(None) => break,
            Err(e) => {
                tracing::debug!("the language server's output ended: {e}");
                break;
            }
        }
    }
    // The server is gone. Everyone still waiting is told at once by having their sender dropped,
    // rather than each of them spending the full deadline.
    inner.dead.store(true, Ordering::SeqCst);
    inner.waiting().clear();
}

/// One message from the server: an answer, a question, or news.
fn route(inner: &Arc<Inner>, message: Value, notifier: &mpsc::UnboundedSender<Notification>) {
    let method = message.get("method").and_then(Value::as_str);
    match (message.get("id"), method) {
        (Some(id), None) => {
            let Some(id) = id.as_u64() else { return };
            let answer = match message.get("error") {
                Some(e) => Err(Error::Response {
                    code: e.get("code").and_then(Value::as_i64).unwrap_or(0),
                    message: e
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("the language server refused")
                        .to_string(),
                }),
                None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
            };
            if let Some(tx) = inner.waiting().remove(&id) {
                let _ = tx.send(answer);
            }
        }
        (Some(id), Some(method)) => inner.answer(id, method, message.get("params")),
        (None, Some(method)) => {
            let params = message.get("params").cloned().unwrap_or(Value::Null);
            match method {
                "$/progress" | "telemetry/event" => tracing::debug!("{method}: {params}"),
                "window/logMessage" | "window/showMessage" => {
                    let text = params
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    match params.get("type").and_then(Value::as_u64) {
                        Some(1) => tracing::warn!("{method}: {text}"),
                        _ => tracing::debug!("{method}: {text}"),
                    }
                }
                _ => {
                    let _ = notifier.send(Notification {
                        method: method.to_string(),
                        params,
                    });
                }
            }
        }
        (None, None) => tracing::warn!("a message that is neither an answer nor a notification"),
    }
}

/// The last segment of a `file://` URI, which is what a workspace folder is called.
fn name_of(uri: &str) -> String {
    from_uri(uri)
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Position;
    use std::time::Instant;
    use tokio::io::{AsyncBufRead, ReadHalf, WriteHalf, split};

    type Duplex = tokio::io::DuplexStream;

    /// A client and a fake server joined by one pipe, so the tests exercise the real framing,
    /// the real reader task and the real cancellation instead of a mock.
    fn wired() -> (
        Client,
        Notifications,
        BufReader<ReadHalf<Duplex>>,
        WriteHalf<Duplex>,
    ) {
        let (ours, theirs) = tokio::io::duplex(64 * 1024);
        let (read, write) = split(ours);
        let (client, notifications) = Client::new(read, write);
        let (their_read, their_write) = split(theirs);
        (
            client,
            notifications,
            BufReader::new(their_read),
            their_write,
        )
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
    fn an_answer_comes_back_typed() {
        runtime().block_on(async {
            let (client, _notifications, mut theirs, mut their_write) = wired();
            let server = tokio::spawn(async move {
                let ask = recv(&mut theirs).await;
                assert_eq!(ask["method"], "textDocument/hover");
                send(
                    &mut their_write,
                    json!({"jsonrpc": "2.0", "id": ask["id"], "result": {"line": 3, "character": 7}}),
                )
                .await;
            });

            let pos: Position = sendable(client.request("textDocument/hover", json!({})))
                .await
                .unwrap();
            assert_eq!(
                pos,
                Position {
                    line: 3,
                    character: 7
                }
            );
            server.await.unwrap();
        });
    }

    #[test]
    fn a_dropped_request_is_cancelled() {
        runtime().block_on(async {
            let (client, _notifications, mut theirs, _their_write) = wired();
            let server = tokio::spawn(async move {
                let ask = recv(&mut theirs).await;
                (ask, recv(&mut theirs).await)
            });

            let out = tokio::time::timeout(
                Duration::from_millis(50),
                client.request::<Value>("textDocument/completion", json!({})),
            )
            .await;
            assert!(out.is_err(), "the fake server never answers");

            let (ask, cancel) = server.await.unwrap();
            assert_eq!(cancel["method"], "$/cancelRequest");
            assert_eq!(cancel["params"]["id"], ask["id"]);
        });
    }

    #[test]
    fn the_servers_own_questions_are_answered() {
        runtime().block_on(async {
            let (_client, _notifications, mut theirs, mut their_write) = wired();

            send(
                &mut their_write,
                json!({"jsonrpc": "2.0", "id": 1, "method": "workspace/configuration",
                       "params": {"items": [{"section": "a"}, {"section": "b"}]}}),
            )
            .await;
            assert_eq!(recv(&mut theirs).await["result"], json!([null, null]));

            send(
                &mut their_write,
                json!({"jsonrpc": "2.0", "id": 2, "method": "workspace/nonsense"}),
            )
            .await;
            assert_eq!(recv(&mut theirs).await["error"]["code"], -32601);
        });
    }

    #[test]
    fn a_dead_server_frees_its_waiters_at_once() {
        runtime().block_on(async {
            let (client, _notifications, mut theirs, their_write) = wired();
            tokio::spawn(async move {
                recv(&mut theirs).await;
                drop(theirs);
                drop(their_write);
            });

            let started = Instant::now();
            let error = client
                .request::<Value>("textDocument/definition", json!({}))
                .await
                .unwrap_err();
            assert!(matches!(error, Error::Closed), "{error}");
            assert!(started.elapsed() < Duration::from_secs(1));
        });
    }

    #[test]
    fn initialize_reads_the_capabilities_and_says_initialized() {
        runtime().block_on(async {
            let (client, _notifications, mut theirs, mut their_write) = wired();
            let server = tokio::spawn(async move {
                let ask = recv(&mut theirs).await;
                assert_eq!(ask["method"], "initialize");
                send(
                    &mut their_write,
                    json!({"jsonrpc": "2.0", "id": ask["id"], "result": {"capabilities": {
                        "positionEncoding": "utf-8",
                        "completionProvider": {"triggerCharacters": [".", ":"]},
                        "hoverProvider": true
                    }}}),
                )
                .await;
                recv(&mut theirs).await
            });

            let caps = client.initialize(Path::new("/tmp/a vault")).await.unwrap();
            assert_eq!(caps.position_encoding.as_deref(), Some("utf-8"));
            assert_eq!(
                caps.completion_provider.map(|c| c.trigger_characters),
                Some(vec![".".to_string(), ":".to_string()])
            );
            assert!(crate::types::on(&caps.hover_provider));
            assert_eq!(server.await.unwrap()["method"], "initialized");
        });
    }
}
