//! Notebook kernels: a Jupyter notebook's code cells run on an ipykernel where the vault is.
//!
//! A façade group of its own (`Vault::kernel_*`), keyed by a session number the caller picks as
//! the debug sessions are, so a kernel's news ([`Event::Kernel`]) can arrive before
//! [`Vault::kernel_start`] answers; each run's news comes under the run number the caller gave
//! it. The kernel is the Python the debugger would run the notebook in
//! ([`crate::debug::interpreter`]) with `-m ipykernel_launcher`, in the notebook's folder, as
//! Jupyter's and VS Code's is.

mod session;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use accent_core::config::DebugConfig;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::language::{Task, remote_task_then};
use crate::vault::Backend;
use crate::{Error, Event, Local, Result, Vault, locked};
use session::Session;

/// What to start a kernel for: a notebook, vault-relative.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Start {
    pub notebook: String,
}

/// The kernel that answered: the interpreter it runs in, and what it is (`ipython`, `9.6.0`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Info {
    pub interpreter: String,
    pub implementation: String,
    pub version: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Status {
    Ok,
    Error,
    /// Not run: an error before it stopped the runs queued behind it.
    Aborted,
}

/// What a kernel has to say unasked, as [`Event::Kernel`]: of a run, under the number
/// [`Vault::kernel_execute`] was given, or of itself.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum News {
    /// The run began, its cell's earlier outputs going.
    Started { run: u64, count: Option<u32> },
    /// One output as nbformat 4 stores it (nbformat's `output_from_msg`).
    Output { run: u64, output: Value },
    /// The run's outputs go; with `wait`, as its next output comes.
    Clear { run: u64, wait: bool },
    /// `input()` asks; answered with [`Vault::kernel_input`].
    Input {
        run: u64,
        prompt: String,
        password: bool,
    },
    Done {
        run: u64,
        status: Status,
        count: Option<u32>,
    },
    /// Over: on its own with `error` its last words, or stopped with `None`.
    Ended { error: Option<String> },
}

/// How long a start on a host may take: the kernel's own minute, and the round trip.
const START: Duration = Duration::from_secs(90);

/// A vault's kernels, each a notebook's.
pub(crate) struct Kernels {
    root: PathBuf,
    events: Sender<Event>,
    sessions: Mutex<HashMap<u64, Arc<Session>>>,
    /// Raised by [`Kernels::shutdown`] before it takes the sessions: a start still under way
    /// holds `Kernels` itself, so it outlives the vault's end and must not register after it.
    closed: AtomicBool,
}

impl Kernels {
    pub(crate) fn new(root: PathBuf, events: Sender<Event>) -> Kernels {
        Kernels {
            root,
            events,
            sessions: Mutex::default(),
            closed: AtomicBool::new(false),
        }
    }

    fn start(self: &Arc<Self>, id: u64, start: &Start, cfg: &DebugConfig) -> Task<Info> {
        let notebook = self.root.join(&start.notebook);
        let python = crate::debug::interpreter(&self.root, &notebook, &cfg.python);
        let interpreter = python.first().cloned().unwrap_or_default();
        let (argv, under) = command(python, &notebook);
        let start = session::start(self.clone(), id, notebook, argv, under, interpreter);
        Task::spawn(start)
    }

    fn execute(&self, id: u64, run: u64, code: String) -> Task<()> {
        let session = self.session(id);
        Task::spawn(async move { session?.execute(run, code) })
    }

    fn input(&self, id: u64, value: String) -> Task<()> {
        let session = self.session(id);
        Task::spawn(async move { session?.input(value) })
    }

    fn interrupt(&self, id: u64) -> Task<()> {
        let session = self.session(id);
        Task::spawn(async move { session?.interrupt() })
    }

    /// End the kernel; one already over is no error. It stays registered until it is over, so a
    /// shutdown of the vault meanwhile waits for it rather than ending the process under it.
    fn stop(self: &Arc<Self>, id: u64) -> Task<()> {
        let (session, kernels) = (self.session(id).ok(), self.clone());
        Task::spawn(async move {
            if let Some(session) = session {
                session.stopping.store(true, Ordering::SeqCst);
                session.stop().await;
                kernels.remove(id);
            }
            Ok(())
        })
    }

    /// Stop every kernel within 2 s, past which the rest are killed as they are dropped; one
    /// still starting stops itself as it would register. For `Local::drop`, and for `serve` once
    /// its window has gone. Blocks on the runtime, and starts none when nothing runs.
    pub(crate) fn shutdown(&self) {
        self.closed.store(true, Ordering::SeqCst);
        let sessions: Vec<Arc<Session>> = locked(&self.sessions).drain().map(|(_, s)| s).collect();
        if sessions.is_empty() {
            return;
        }
        accent_lsp::runtime().block_on(async {
            let all = async {
                for session in &sessions {
                    session.stop().await;
                }
            };
            let _ = tokio::time::timeout(Duration::from_secs(2), all).await;
        });
    }

    /// Keep `session` as `id`; false once the vault has shut down, the caller then stopping it.
    fn register(&self, id: u64, session: &Arc<Session>) -> bool {
        let mut sessions = locked(&self.sessions);
        // Read under the lock `shutdown` drains through, so a session is either drained or refused.
        if self.closed.load(Ordering::SeqCst) {
            return false;
        }
        sessions.insert(id, session.clone());
        true
    }

    fn session(&self, id: u64) -> Result<Arc<Session>> {
        locked(&self.sessions)
            .get(&id)
            .cloned()
            .ok_or_else(|| Error::Language(format!("no kernel {id}")))
    }

    fn remove(&self, id: u64) -> Option<Arc<Session>> {
        locked(&self.sessions).remove(&id)
    }
}

/// The kernel's command, `python -m ipykernel_launcher`, and the folder its sockets go in. In
/// the Flatpak the kernel is the host's, through `flatpak-spawn`, its sockets in the one folder
/// the sandbox and the host share.
fn command(python: Vec<String>, notebook: &Path) -> (Vec<String>, PathBuf) {
    let mut argv = python;
    argv.extend(["-m", "ipykernel_launcher"].map(String::from));
    let sandbox = std::env::var("FLATPAK_ID").ok();
    let sandbox = sandbox.filter(|_| Path::new("/.flatpak-info").exists());
    match (sandbox, std::env::var_os("XDG_RUNTIME_DIR")) {
        (Some(id), Some(runtime)) => {
            let cwd = notebook.parent().unwrap_or(notebook);
            let host = ["flatpak-spawn", "--host", "--watch-bus"].map(String::from);
            let directory = format!("--directory={}", cwd.display());
            let argv = host.into_iter().chain([directory]).chain(argv).collect();
            (argv, Path::new(&runtime).join("app").join(id))
        }
        _ => (argv, std::env::temp_dir()),
    }
}

impl Vault {
    /// Start a kernel for `start.notebook` as `session`, a number the caller picks; answered once
    /// the kernel answers.
    pub fn kernel_start(&self, session: u64, start: &Start) -> Task<Info> {
        self.kernel_call(
            |v| v.kernels.start(session, start, &v.config().debug),
            "kernel_start",
            json!([session, start]),
            START,
        )
    }

    /// Run `code` as run `run`, its news under that number; answered once sent, the kernel
    /// taking the runs in turn. Calls that overlap may reach it in either order, so a caller with
    /// several runs sends each once the last is answered.
    pub fn kernel_execute(&self, session: u64, run: u64, code: String) -> Task<()> {
        let params = json!([session, run, code]);
        self.kernel_call(
            |v| v.kernels.execute(session, run, code),
            "kernel_execute",
            params,
            crate::rpc::DEADLINE,
        )
    }

    /// Answer the `input()` that asks ([`News::Input`]).
    pub fn kernel_input(&self, session: u64, value: String) -> Task<()> {
        let params = json!([session, value]);
        self.kernel_call(
            |v| v.kernels.input(session, value),
            "kernel_input",
            params,
            crate::rpc::DEADLINE,
        )
    }

    /// Interrupt what the kernel runs: SIGINT to its process group, as Jupyter does.
    pub fn kernel_interrupt(&self, session: u64) -> Task<()> {
        self.kernel_call(
            |v| v.kernels.interrupt(session),
            "kernel_interrupt",
            json!([session]),
            crate::rpc::DEADLINE,
        )
    }

    /// End the kernel; one already over is no error.
    pub fn kernel_stop(&self, session: u64) -> Task<()> {
        self.kernel_call(
            |v| v.kernels.stop(session),
            "kernel_stop",
            json!([session]),
            crate::rpc::DEADLINE,
        )
    }

    /// End every kernel now, within 2 s, for a window that is closing: its last window closing
    /// ends the process before the vault is dropped. A remote vault's end with `serve`, as the
    /// link closes.
    pub fn kernel_shutdown(&self) {
        if let Backend::Local(v) = &self.backend {
            v.kernels.shutdown();
        }
    }

    fn kernel_call<T: DeserializeOwned + Send + 'static>(
        &self,
        local: impl FnOnce(&Local) -> Task<T>,
        method: &'static str,
        params: Value,
        deadline: Duration,
    ) -> Task<T> {
        match &self.backend {
            Backend::Local(v) => local(v),
            Backend::Remote(r) => {
                remote_task_then(r.clone(), method, params, deadline, |_, answer| Ok(answer))
            }
        }
    }
}

/// Dispatch the kernel calls on host request threads in [`crate::rpc::serve_local`].
pub(crate) fn dispatch(vault: &Local, method: &str, p: &Value) -> Option<Result<Value>> {
    use crate::rpc::{answer, args, block};
    let kernels = &vault.kernels;
    Some(match method {
        "kernel_start" => {
            args!(p; session: u64, start: Start);
            answer(block(kernels.start(session, &start, &vault.config().debug)))
        }
        "kernel_execute" => {
            args!(p; session: u64, run: u64, code: String);
            answer(block(kernels.execute(session, run, code)))
        }
        "kernel_input" => {
            args!(p; session: u64, value: String);
            answer(block(kernels.input(session, value)))
        }
        "kernel_interrupt" => {
            args!(p; session: u64);
            answer(block(kernels.interrupt(session)))
        }
        "kernel_stop" => {
            args!(p; session: u64);
            answer(block(kernels.stop(session)))
        }
        _ => return None,
    })
}
