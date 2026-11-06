//! Notebook kernels: a Jupyter notebook's code cells run on a kernel where the vault is.
//!
//! A façade group of its own (`Vault::kernel_*`), keyed by a session number the caller picks as
//! the debug sessions are, so a kernel's news ([`Event::Kernel`]) can arrive before
//! [`Vault::kernel_start`] answers; each run's news comes under the run number the caller gave
//! it. A Python notebook's kernel is the Python the debugger would run the notebook in
//! ([`crate::debug::interpreter`]) with `-m ipykernel_launcher`; another language's is the
//! kernelspec the caller names ([`Vault::kernel_specs`]). Either runs in the notebook's folder,
//! as Jupyter's and VS Code's does.

mod session;
mod spec;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::Sender;
use std::time::Duration;

use accent_core::config::DebugConfig;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::language::{Task, remote_task_then};
use crate::registry::Registry;
use crate::vault::Backend;
use crate::{Error, Event, Local, Result, Vault};
use session::Session;
pub use spec::{Spec, Specs, of_language, pick};

/// What to start a kernel for: a notebook, vault-relative, and the kernelspec it runs on by its
/// name, `None` for the vault's Python with ipykernel.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Start {
    pub notebook: String,
    pub kernel: Option<String>,
}

/// The kernel that answered: the interpreter it runs in, or its kernelspec's display name, and
/// what it is (`ipython`, `9.6.0`).
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
    /// One output as nbformat 4 stores it, with its kernel-only display ID; never a stream.
    Output {
        run: u64,
        output: Value,
        display_id: Option<String>,
    },
    /// The run's stream of `name` as a notebook keeps it ([`accent_core::notebook::bound`]),
    /// sent at most every 100 ms: in the last output, a stream of `name`, the `replaced` bytes
    /// from byte `at` on give way to `text`; with no `at`, a new output holding `text`.
    Stream {
        run: u64,
        name: String,
        at: Option<usize>,
        replaced: usize,
        text: String,
    },
    /// Replace every output this kernel gave the ID, in any cell.
    Update { display_id: String, output: Value },
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
    sessions: Registry<Session>,
}

impl Kernels {
    pub(crate) fn new(root: PathBuf, events: Sender<Event>) -> Kernels {
        Kernels {
            root,
            events,
            sessions: Registry::new("kernel"),
        }
    }

    fn start(self: &Arc<Self>, id: u64, start: &Start, cfg: &DebugConfig) -> Task<Info> {
        let notebook = self.root.join(&start.notebook);
        let (kernels, kernel, python) = (self.clone(), start.kernel.clone(), cfg.python.clone());
        Task::spawn(async move {
            // Off the caller's thread: a kernelspec is looked for in the vault's environments too.
            let root = kernels.root.clone();
            let launch = {
                let notebook = notebook.clone();
                tokio::task::spawn_blocking(move || launch(&root, &notebook, kernel, &python))
            };
            session::start(kernels, id, notebook, launch.await??).await
        })
    }

    fn execute(&self, id: u64, run: u64, code: String) -> Task<()> {
        let session = self.sessions.get(id);
        Task::spawn(async move { session?.execute(run, code) })
    }

    fn input(&self, id: u64, value: String) -> Task<()> {
        let session = self.sessions.get(id);
        Task::spawn(async move { session?.input(value) })
    }

    fn interrupt(&self, id: u64) -> Task<()> {
        let session = self.sessions.get(id);
        Task::spawn(async move { session?.interrupt() })
    }

    /// End the kernel; one already over is no error. It stays registered until it is over, so a
    /// shutdown of the vault meanwhile waits for it rather than ending the process under it.
    fn stop(self: &Arc<Self>, id: u64) -> Task<()> {
        let (session, kernels) = (self.sessions.get(id).ok(), self.clone());
        Task::spawn(async move {
            if let Some(session) = session {
                session.stopping.store(true, Ordering::SeqCst);
                session.stop().await;
                kernels.sessions.remove(id);
            }
            Ok(())
        })
    }

    /// Stop every kernel within 2 s, past which the rest are killed as they are dropped; one
    /// still starting stops itself as it would register. For `Local::drop`, and for `serve` once
    /// its window has gone. Blocks on the runtime, and starts none when nothing runs.
    pub(crate) fn shutdown(&self) {
        self.sessions.shutdown(Session::stop);
    }
}

/// How a kernel is started: its command, `{connection_file}` in it, its environment, what it is
/// called, and how it is interrupted.
pub(crate) struct Launch {
    argv: Vec<String>,
    env: Vec<(String, String)>,
    /// The interpreter, or the kernelspec's display name: [`Info::interpreter`].
    called: String,
    /// A kernelspec's, which may not take the `ipc` transport.
    kernelspec: bool,
    message_interrupt: bool,
}

/// The vault's Python with ipykernel for `notebook`, or the kernelspec named `kernel`.
fn launch(
    root: &Path,
    notebook: &Path,
    kernel: Option<String>,
    python: &[String],
) -> Result<Launch> {
    let Some(name) = kernel else {
        let mut argv = crate::debug::interpreter(root, notebook, python);
        let called = argv.first().cloned().unwrap_or_default();
        argv.extend(["-m", "ipykernel_launcher", "-f", "{connection_file}"].map(String::from));
        return Ok(Launch {
            argv,
            env: Vec::new(),
            called,
            kernelspec: false,
            message_interrupt: false,
        });
    };
    let specs = spec::find(root).specs;
    let Some(spec) = specs.iter().find(|s| s.name == name) else {
        return Err(Error::Language(format!(
            "the {name} kernel is not installed"
        )));
    };
    Ok(Launch {
        argv: spec::argv(spec),
        env: spec.env.clone().into_iter().collect(),
        called: spec.display_name.clone(),
        kernelspec: true,
        message_interrupt: spec.message_interrupt,
    })
}

/// `argv` as it runs, and the folder its sockets go in. In the Flatpak the kernel is the host's,
/// through `flatpak-spawn`, which hands it `env` itself, its sockets in the one folder the
/// sandbox and the host share.
fn command(
    argv: Vec<String>,
    env: &[(String, String)],
    notebook: &Path,
) -> (Vec<String>, PathBuf, bool) {
    let sandbox = std::env::var("FLATPAK_ID").ok();
    let sandbox = sandbox.filter(|_| Path::new("/.flatpak-info").exists());
    match (sandbox, std::env::var_os("XDG_RUNTIME_DIR")) {
        (Some(id), Some(runtime)) => {
            let cwd = notebook.parent().unwrap_or(notebook);
            let host = ["flatpak-spawn", "--host", "--watch-bus"].map(String::from);
            let directory = format!("--directory={}", cwd.display());
            let env = env.iter().map(|(k, v)| format!("--env={k}={v}"));
            let argv = host.into_iter().chain([directory]).chain(env).chain(argv);
            (
                argv.collect(),
                Path::new(&runtime).join("app").join(id),
                true,
            )
        }
        _ => (argv, std::env::temp_dir(), false),
    }
}

impl Vault {
    /// The kernelspecs on the vault's machine, for a notebook in another language than Python.
    pub fn kernel_specs(&self) -> Task<Specs> {
        match &self.backend {
            Backend::Local(v) => {
                let root = v.root().to_path_buf();
                Task::blocking(move || Ok(spec::find(&root)))
            }
            Backend::Remote(r) => remote_task_then(
                r.clone(),
                "kernel_specs",
                json!([]),
                crate::rpc::DEADLINE,
                |_, answer| Ok(answer),
            ),
        }
    }

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

    /// Interrupt what the kernel runs, as Jupyter does: SIGINT to its process group, or an
    /// `interrupt_request` for a kernelspec that asks for one.
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
        "kernel_specs" => answer(Ok::<_, Error>(spec::find(vault.root()))),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::poll_until;

    /// A vault closed while a kernel is still starting: the start holds `Kernels` itself, so it
    /// must find it shut down rather than register past the vault's end.
    #[test]
    fn a_start_the_vault_closed_under_leaves_no_kernel() {
        if !crate::tests::has_ipykernel() {
            eprintln!("ipykernel is not installed: skipping the end-to-end test");
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let dir = root.path();
        let (events, _news) = std::sync::mpsc::channel();
        let kernels = Arc::new(Kernels::new(dir.to_path_buf(), events));
        std::fs::write(dir.join("hold"), "").unwrap();
        // The kernel's Python says its process id and waits while `hold` is there, its IPython
        // folder the test's rather than the user's.
        let held = r#"echo $$ > pid.tmp && mv pid.tmp pid
while [ -e hold ]; do sleep 0.01; done
IPYTHONDIR="$PWD/ipython" exec python3 "$@""#;
        let cfg = DebugConfig {
            python: ["sh", "-c", held, "sh"].map(String::from).to_vec(),
            ..Default::default()
        };
        let start = Start {
            notebook: "run.ipynb".to_string(),
            kernel: None,
        };
        let started = kernels.start(1, &start, &cfg);
        let pid = dir.join("pid");
        let spawned = poll_until(|| pid.exists(), Duration::from_secs(10));
        assert!(spawned, "the kernel never started");

        kernels.shutdown();
        std::fs::remove_file(dir.join("hold")).unwrap();
        let started = accent_lsp::runtime()
            .block_on(async { tokio::time::timeout(Duration::from_secs(60), started).await });
        let cancelled = matches!(started, Ok(Err(Error::Core(accent_core::Error::Cancelled))));
        assert!(cancelled, "{started:?}");
        assert!(kernels.sessions.get(1).is_err());
        let pid = std::fs::read_to_string(pid).unwrap();
        let pid: i32 = pid.trim().parse().unwrap();
        // SAFETY: signal 0 only asks whether the process is there.
        let gone = || unsafe { libc::kill(pid, 0) } != 0;
        let gone = poll_until(gone, Duration::from_secs(5));
        assert!(gone, "the kernel outlived its vault");
    }
}
