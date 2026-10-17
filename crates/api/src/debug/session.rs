//! One debug session's protocol: the adapter's start, its news turned into [`News`], and the
//! requests the window makes of a paused program.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, Weak};

use accent_core::config::DebugConfig;
use accent_dap::types::{
    Breakpoint, BreakpointEventBody, EvaluateResponse, ExitedEventBody, OutputEventBody,
    RunInTerminalRequestArguments, ScopesResponse, SetBreakpointsResponse, StackTraceResponse,
    StoppedEventBody, ThreadsResponse, VariablesResponse,
};
use accent_dap::{Client, Incoming, Incomings};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use super::{
    Category, Context, Debugs, Evaluated, Frame, Launch, News, Placed, Scope, Step, Thread,
    Variable,
};
use crate::language::{Pos, session_root};
use crate::{Error, Event, Result, locked};

/// A running adapter and what it holds of the breakpoints.
pub(super) struct Session {
    client: Client,
    root: PathBuf,
    /// Each file's breakpoints as the adapter last said.
    placed: Mutex<HashMap<String, Held>>,
}

/// A file's breakpoints, each with the id the adapter's `breakpoint` events name it by.
type Held = Vec<(Option<i64>, Placed)>;

/// Start the adapter, register the session and launch the program: answered once the program
/// runs. The program's working directory, and the adapter's, is the nearest checkout inside the
/// vault, else the vault root.
pub(super) async fn start(
    debugs: Arc<Debugs>,
    id: u64,
    launch: Launch,
    cfg: DebugConfig,
) -> Result<()> {
    let root = &debugs.root;
    let program = root.join(&launch.program);
    let (adapter_id, argv) = super::adapter(&cfg, &launch.language)?;
    let cwd = session_root(root, &program, &[]);
    let (client, incomings) = Client::spawn(&argv, &cwd)
        .map_err(|e| Error::Language(format!("cannot start {}: {e}", argv[0])))?;
    if let Err(e) = client.initialize(adapter_id).await {
        return Err(failed(&argv[0], &client, e));
    }
    let session = Arc::new(Session {
        client,
        root: root.clone(),
        placed: Mutex::default(),
    });
    // Before `launch`: the adapter asks for the program to be run while it is out. A vault shut
    // down meanwhile stopped only the sessions it held, so this one stops its own adapter.
    if !debugs.register(id, &session) {
        session.client.shutdown().await;
        return Err(accent_core::Error::Cancelled.into());
    }
    accent_lsp::runtime().spawn(forward(
        id,
        incomings,
        Arc::downgrade(&session),
        Arc::downgrade(&debugs),
        debugs.events.clone(),
        launch.breakpoints,
    ));
    let arguments = json!({
        "name": "accent",
        "program": program,
        "cwd": cwd,
        "console": "integratedTerminal",
        "justMyCode": true,
        "subProcess": false,
        "python": super::interpreter(root, &program, &cfg.python),
    });
    let launched = session
        .client
        .request_unbounded::<Value>("launch", arguments);
    if let Err(e) = launched.await {
        // Nothing runs, so nothing is kept.
        debugs.remove(id);
        session.stop().await;
        return Err(failed(&argv[0], &session.client, e));
    }
    Ok(())
}

/// Why the adapter did not answer: one that exited says why on stderr, which is how
/// `No module named debugpy` reaches the reader.
fn failed(adapter: &str, client: &Client, e: accent_dap::Error) -> Error {
    match (e, client.last_words()) {
        (accent_dap::Error::Closed, Some(words)) => {
            Error::Language(format!("{adapter} exited: {words}"))
        }
        (e, _) => e.into(),
    }
}

/// Tell the window what the adapter says, until it ends; then say the session is over, once,
/// and let the adapter go. Holds the session weakly, so a stopped one is dropped at once.
async fn forward(
    id: u64,
    mut incomings: Incomings,
    session: Weak<Session>,
    debugs: Weak<Debugs>,
    events: Sender<Event>,
    breakpoints: Vec<(String, Vec<u32>)>,
) {
    let emit = |news| {
        let _ = events.send(Event::Debug { session: id, news });
    };
    let mut breakpoints = Some(breakpoints);
    let (mut code, mut terminated) = (None, false);
    while let Some(incoming) = incomings.recv().await {
        let (event, body) = match incoming {
            Incoming::Event { event, body } => (event, body),
            Incoming::Request {
                seq,
                command,
                arguments,
            } => {
                let run = match command.as_str() {
                    "runInTerminal" => read::<RunInTerminalRequestArguments>(arguments),
                    _ => None,
                };
                match (run, session.upgrade()) {
                    (Some(run), _) => emit(terminal(seq, run)),
                    (None, Some(session)) => {
                        let refused = Err("not supported".to_string());
                        let _ = session.client.respond(seq, &command, refused);
                    }
                    (None, None) => {}
                }
                continue;
            }
        };
        match event.as_str() {
            "initialized" => {
                if let (Some(session), Some(files)) = (session.upgrade(), breakpoints.take()) {
                    session.configure(files, &emit).await;
                }
            }
            "stopped" => {
                if let Some(stopped) = read::<StoppedEventBody>(body) {
                    emit(News::Stopped {
                        thread: stopped.thread_id,
                        reason: stopped.reason,
                        text: stopped.text,
                        description: stopped.description,
                    });
                }
            }
            "continued" => emit(News::Continued),
            "output" => {
                let Some(output) = read::<OutputEventBody>(body) else {
                    continue;
                };
                let category = match output.category.as_deref() {
                    Some("telemetry") => continue,
                    Some("important") => Category::Important,
                    Some("stdout") => Category::Stdout,
                    Some("stderr") => Category::Stderr,
                    _ => Category::Console,
                };
                emit(News::Output {
                    category,
                    text: output.output,
                });
            }
            "breakpoint" => {
                let changed = session
                    .upgrade()
                    .zip(read::<BreakpointEventBody>(body))
                    .and_then(|(session, event)| session.changed(event));
                if let Some((path, placed)) = changed {
                    emit(News::Breakpoints { path, placed });
                }
            }
            "exited" => code = read::<ExitedEventBody>(body).map(|e| e.exit_code),
            "terminated" => {
                terminated = true;
                break;
            }
            // `process`, `thread`, `module`, `debugpySockets`: nothing the window shows.
            _ => {}
        }
    }
    // Still registered, it was neither stopped nor did it end: the adapter died.
    let gone = debugs.upgrade().and_then(|d| d.remove(id));
    let error = match (&gone, terminated) {
        (Some(session), false) => Some(match session.client.last_words() {
            Some(words) => format!("the debug adapter exited: {words}"),
            None => "the debug adapter exited".to_string(),
        }),
        _ => None,
    };
    emit(News::Ended { code, error });
    if let Some(session) = gone {
        session.client.shutdown().await;
    }
}

/// The adapter's `runInTerminal`, for the window to run; a `null` value would unset a variable,
/// which debugpy never asks.
fn terminal(seq: i64, run: RunInTerminalRequestArguments) -> News {
    let mut env: Vec<(String, String)> = run
        .env
        .into_iter()
        .filter_map(|(key, value)| Some((key, value?)))
        .collect();
    env.sort();
    News::Terminal {
        seq,
        args: run.args,
        cwd: run.cwd,
        env,
    }
}

/// An adapter's body, or nothing for one that does not read as the protocol says.
fn read<T: DeserializeOwned>(body: Value) -> Option<T> {
    serde_json::from_value(body)
        .inspect_err(|e| tracing::debug!("unreadable from the debug adapter: {e}"))
        .ok()
}

impl Session {
    /// What the adapter waits for after `initialized`: the breakpoints, each file's answer told
    /// as it comes, the exception filters, then `configurationDone`, which lets the program run.
    async fn configure(&self, files: Vec<(String, Vec<u32>)>, emit: &impl Fn(News)) {
        for (path, lines) in files {
            match self.breakpoints(path.clone(), lines).await {
                Ok(placed) => emit(News::Breakpoints { path, placed }),
                Err(e) => tracing::warn!("cannot set the breakpoints of {path}: {e}"),
            }
        }
        let filters = json!({"filters": ["uncaught", "userUnhandled"]});
        for (command, arguments) in [
            ("setExceptionBreakpoints", filters),
            ("configurationDone", json!({})),
        ] {
            if let Err(e) = self.client.request::<Value>(command, arguments).await {
                tracing::warn!("the debug adapter refused {command}: {e}");
            }
        }
    }

    pub(super) async fn breakpoints(&self, path: String, lines: Vec<u32>) -> Result<Vec<Placed>> {
        let asked: Vec<Value> = lines.iter().map(|l| json!({"line": l + 1})).collect();
        let arguments = json!({"source": {"path": self.root.join(&path)}, "breakpoints": asked});
        let answer: SetBreakpointsResponse =
            self.client.request("setBreakpoints", arguments).await?;
        let held: Held = answer
            .breakpoints
            .iter()
            .zip(lines)
            .map(|(b, asked)| (b.id, placed(b, asked)))
            .collect();
        let now = held.iter().map(|(_, p)| p.clone()).collect();
        locked(&self.placed).insert(path, held);
        Ok(now)
    }

    /// A `breakpoint` event's change to one of ours, found by its id: that file's list now.
    fn changed(&self, event: BreakpointEventBody) -> Option<(String, Vec<Placed>)> {
        let id = event.breakpoint.id?;
        let mut files = locked(&self.placed);
        let (path, held, at) = files.iter_mut().find_map(|(path, held)| {
            let at = held.iter().position(|(i, _)| *i == Some(id))?;
            Some((path, held, at))
        })?;
        match event.reason.as_str() {
            "removed" => drop(held.remove(at)),
            _ => held[at].1 = placed(&event.breakpoint, held[at].1.line),
        }
        Some((path.clone(), held.iter().map(|(_, p)| p.clone()).collect()))
    }

    pub(super) async fn resume(&self, thread: i64) -> Result<()> {
        self.on_thread("continue", thread).await
    }

    pub(super) async fn pause(&self, thread: i64) -> Result<()> {
        self.on_thread("pause", thread).await
    }

    pub(super) async fn step(&self, thread: i64, step: Step) -> Result<()> {
        let command = match step {
            Step::Over => "next",
            Step::Into => "stepIn",
            Step::Out => "stepOut",
        };
        self.on_thread(command, thread).await
    }

    async fn on_thread(&self, command: &str, thread: i64) -> Result<()> {
        let arguments = json!({"threadId": thread});
        self.client.request::<Value>(command, arguments).await?;
        Ok(())
    }

    pub(super) async fn threads(&self) -> Result<Vec<Thread>> {
        let answer: ThreadsResponse = self.client.request("threads", json!({})).await?;
        let threads = answer.threads.into_iter().map(|t| Thread {
            id: t.id,
            name: t.name,
        });
        Ok(threads.collect())
    }

    pub(super) async fn stack(&self, thread: i64) -> Result<Vec<Frame>> {
        let arguments = json!({"threadId": thread});
        let answer: StackTraceResponse = self.client.request("stackTrace", arguments).await?;
        let frames = answer.stack_frames.into_iter().map(|f| Frame {
            id: f.id,
            name: f.name,
            path: f.source.and_then(|s| s.path).map(|p| self.ours(&p)),
            pos: Pos {
                line: f.line.saturating_sub(1),
                character: f.column.saturating_sub(1),
            },
        });
        Ok(frames.collect())
    }

    pub(super) async fn scopes(&self, frame: i64) -> Result<Vec<Scope>> {
        let arguments = json!({"frameId": frame});
        let answer: ScopesResponse = self.client.request("scopes", arguments).await?;
        let scopes = answer.scopes.into_iter().map(|s| Scope {
            name: s.name,
            reference: s.variables_reference,
            expensive: s.expensive,
        });
        Ok(scopes.collect())
    }

    pub(super) async fn variables(&self, reference: i64) -> Result<Vec<Variable>> {
        let arguments = json!({"variablesReference": reference});
        let answer: VariablesResponse = self.client.request("variables", arguments).await?;
        let variables = answer.variables.into_iter().map(|v| Variable {
            name: v.name,
            value: v.value,
            kind: v.kind,
            reference: v.variables_reference,
        });
        Ok(variables.collect())
    }

    /// The REPL waits as long as the reader's code runs; a hover is a request like any other.
    pub(super) async fn evaluate(
        &self,
        frame: Option<i64>,
        expression: String,
        context: Context,
    ) -> Result<Evaluated> {
        let context_name = match context {
            Context::Repl => "repl",
            Context::Hover => "hover",
        };
        let mut arguments = json!({"expression": expression, "context": context_name});
        if let Some(frame) = frame {
            arguments["frameId"] = frame.into();
        }
        let answer: EvaluateResponse = match context {
            Context::Repl => self.client.request_unbounded("evaluate", arguments).await?,
            Context::Hover => self.client.request("evaluate", arguments).await?,
        };
        Ok(Evaluated {
            result: answer.result,
            kind: answer.kind,
            reference: answer.variables_reference,
        })
    }

    /// debugpy never reads the answer, but the protocol asks for one.
    pub(super) async fn answer(&self, seq: i64, pid: Option<u32>) -> Result<()> {
        let body = match pid {
            Some(pid) => json!({"processId": pid}),
            None => json!({}),
        };
        self.client.respond(seq, "runInTerminal", Ok(body))?;
        Ok(())
    }

    /// End the program, then the adapter, which exits as its input closes.
    pub(super) async fn stop(&self) {
        let arguments = json!({"terminateDebuggee": true});
        let _ = self.client.request::<Value>("disconnect", arguments).await;
        self.client.shutdown().await;
    }

    /// A path from the adapter as the façade gives it: vault-relative inside the vault.
    fn ours(&self, path: &str) -> String {
        match Path::new(path).strip_prefix(&self.root) {
            Ok(rel) => rel.to_string_lossy().into_owned(),
            Err(_) => path.to_string(),
        }
    }
}

/// Where the adapter put a breakpoint asked for on `asked`.
fn placed(b: &Breakpoint, asked: u32) -> Placed {
    Placed {
        line: b.line.map_or(asked, |line| line.saturating_sub(1)),
        verified: b.verified,
        message: b.message.clone(),
    }
}
