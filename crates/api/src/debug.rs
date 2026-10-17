//! Debug sessions: a program run under its language's debug adapter, where the vault is.
//!
//! A façade group of its own (`Vault::debug_*`) rather than the `Language` trait: a session is a
//! program, not a document, and outlives any tab of its files. Sessions are keyed by a number the
//! caller picks, so their news ([`Event::Debug`]) can arrive before [`Vault::debug_start`]
//! answers. Lines are 0-based here and 1-based on the wire; paths are vault-relative inside the
//! vault and absolute outside, as a [`Location`](crate::Location)'s.

mod session;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use accent_core::config::DebugConfig;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::language::{Pos, Task, in_path, remote_task_then};
use crate::vault::Backend;
use crate::{Error, Event, Local, Result, Vault, locked};
use session::Session;

// ---------------------------------------------------------------- the façade's types

/// What to debug: `program` in `language` (a GtkSourceView id), stopping at `breakpoints`, each
/// a file and its lines.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Launch {
    pub program: String,
    pub language: String,
    pub breakpoints: Vec<(String, Vec<u32>)>,
}

/// A breakpoint as the adapter holds it: maybe on another line than asked, and maybe not bound.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Placed {
    pub line: u32,
    pub verified: bool,
    /// Why it is not bound, for the reader.
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Thread {
    pub id: i64,
    pub name: String,
}

/// A call on a thread's stack. `path` is `None` for code the adapter holds no file of.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Frame {
    pub id: i64,
    pub name: String,
    pub path: Option<String>,
    pub pos: Pos,
}

/// A frame's variables, grouped (`Locals`, `Globals`); `expensive` ones are not opened unasked.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Scope {
    pub name: String,
    pub reference: i64,
    pub expensive: bool,
}

/// A value; one with a `reference` above 0 has children, listed by [`Vault::debug_variables`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Variable {
    pub name: String,
    pub value: String,
    pub kind: Option<String>,
    pub reference: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Evaluated {
    pub result: String,
    pub kind: Option<String>,
    pub reference: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Step {
    Over,
    Into,
    Out,
}

/// Who asks for an expression: the console's prompt runs whatever it is given, a hover must
/// change nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Context {
    Repl,
    Hover,
}

/// Whose output it is: the adapter's (`Console`, `Important` for what must be seen) or the
/// program's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Category {
    Console,
    Important,
    Stdout,
    Stderr,
}

/// What a session has to say unasked, as [`Event::Debug`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum News {
    /// The adapter asks for the program to be run in a terminal; answer with
    /// [`Vault::debug_answer`].
    Terminal {
        seq: i64,
        args: Vec<String>,
        cwd: String,
        env: Vec<(String, String)>,
    },
    Stopped {
        thread: Option<i64>,
        reason: String,
        text: Option<String>,
        description: Option<String>,
    },
    Continued,
    Output {
        category: Category,
        text: String,
    },
    /// What the adapter holds of `path`'s breakpoints now: a configuration answer or a
    /// `breakpoint` event.
    Breakpoints {
        path: String,
        placed: Vec<Placed>,
    },
    /// Over: the program's exit code if it said one; `error` when the adapter died.
    Ended {
        code: Option<i64>,
        error: Option<String>,
    },
}

// ---------------------------------------------------------------- adapters

/// Built-in adapters: GtkSourceView id, the adapter's protocol id and its command. A row is what
/// makes a language debuggable; `config.toml` replaces only the command.
const ADAPTERS: &[(&str, &str, &[&str])] = &[
    ("python3", "debugpy", DEBUGPY),
    ("python", "debugpy", DEBUGPY),
];

/// debugpy serves any interpreter: the program loads debugpy from the adapter's install.
const DEBUGPY: &[&str] = &["python3", "-m", "debugpy.adapter"];

/// A day stands for no deadline: the REPL runs the reader's code, and a dropped link or the
/// session's end frees the wait first.
const UNBOUNDED: Duration = Duration::from_secs(24 * 60 * 60);

/// Whether a file in `language` (a GtkSourceView id) can be debugged.
pub fn debuggable(language: &str) -> bool {
    ADAPTERS.iter().any(|(id, ..)| *id == language)
}

/// The adapter's protocol id and command for `language`, the config's command winning.
pub(crate) fn adapter(cfg: &DebugConfig, language: &str) -> Result<(&'static str, Vec<String>)> {
    let Some((_, id, argv)) = ADAPTERS.iter().find(|(l, ..)| *l == language) else {
        return Err(Error::Language(format!("{language} cannot be debugged")));
    };
    let argv = match cfg.adapters.get(language) {
        Some(argv) => argv.clone(),
        None => argv.iter().map(|s| s.to_string()).collect(),
    };
    let exe = argv.first().map_or("", String::as_str);
    if !in_path(exe) {
        return Err(Error::Language(format!("{exe} is not installed")));
    }
    Ok((id, argv))
}

/// The interpreter `program` runs in: the configured one, a relative path with a `/` read from
/// `root`; else the nearest `.venv` or `venv` above the program inside the vault; else `python3`.
pub(crate) fn interpreter(root: &Path, program: &Path, configured: &[String]) -> Vec<String> {
    if let Some((exe, rest)) = configured.split_first() {
        let exe = match exe.contains('/') && Path::new(exe).is_relative() {
            true => root.join(exe).to_string_lossy().into_owned(),
            false => exe.clone(),
        };
        return std::iter::once(exe).chain(rest.iter().cloned()).collect();
    }
    let venvs = program
        .ancestors()
        .skip(1)
        .take_while(|dir| dir.starts_with(root))
        .flat_map(|dir| [dir.join(".venv/bin/python"), dir.join("venv/bin/python")]);
    for python in venvs {
        if python.is_file() {
            return vec![python.to_string_lossy().into_owned()];
        }
    }
    vec!["python3".to_string()]
}

// ---------------------------------------------------------------- sessions

/// A vault's debug sessions, each a running adapter.
pub(crate) struct Debugs {
    root: PathBuf,
    events: Sender<Event>,
    sessions: Mutex<HashMap<u64, Arc<Session>>>,
    /// Raised by [`Debugs::shutdown`] before it takes the sessions: a start still under way holds
    /// `Debugs` itself, so it outlives the vault's end and must not register after it.
    closed: AtomicBool,
}

impl Debugs {
    pub(crate) fn new(root: PathBuf, events: Sender<Event>) -> Debugs {
        Debugs {
            root,
            events,
            sessions: Mutex::default(),
            closed: AtomicBool::new(false),
        }
    }

    /// Start the adapter and launch the program; answered once the program runs.
    pub(crate) fn start(self: &Arc<Self>, id: u64, launch: Launch, cfg: &DebugConfig) -> Task<()> {
        let (debugs, cfg) = (self.clone(), cfg.clone());
        Task::spawn(session::start(debugs, id, launch, cfg))
    }

    /// End the program and its adapter; a session already over is no error.
    pub(crate) fn stop(&self, id: u64) -> Task<()> {
        let session = self.remove(id);
        Task::spawn(async move {
            if let Some(session) = session {
                session.stop().await;
            }
            Ok(())
        })
    }

    /// Stop every session within 2 s, past which the adapters are killed; one still starting
    /// stops itself as it would register. For `Local::drop`, and for `serve` once its window has
    /// gone.
    /// Blocks on the runtime, as `Languages::shutdown` does, and starts none when nothing runs.
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
            .ok_or_else(|| Error::Language(format!("no debug session {id}")))
    }

    fn remove(&self, id: u64) -> Option<Arc<Session>> {
        locked(&self.sessions).remove(&id)
    }
}

/// Generate the requests to a running session for [`Debugs`], [`Vault`] and host RPC, as
/// `language::requests!` does for documents, keyed by session instead of file. A remote call
/// waits [`crate::rpc::DEADLINE`] unless its row says `within`.
macro_rules! sessions {
    (@deadline) => { crate::rpc::DEADLINE };
    (@deadline $deadline:expr) => { $deadline };
    ($(
        $(#[$doc:meta])*
        $name:ident => $inner:ident ( $($arg:ident : $ty:ty),* ) -> $ret:ty
            $(, within $deadline:expr)?;
    )*) => {
        impl Debugs { $(
            pub(crate) fn $inner(&self, session: u64, $($arg: $ty),*) -> Task<$ret> {
                let session = self.session(session);
                Task::spawn(async move { session?.$inner($($arg),*).await })
            }
        )* }

        impl Vault { $(
            $(#[$doc])*
            pub fn $name(&self, session: u64, $($arg: $ty),*) -> Task<$ret> {
                match &self.backend {
                    Backend::Local(v) => v.debug.$inner(session, $($arg),*),
                    Backend::Remote(r) => remote_task_then(
                        r.clone(),
                        stringify!($name),
                        json!([session, $($arg),*]),
                        sessions!(@deadline $($deadline)?),
                        |_, answer| Ok(answer),
                    ),
                }
            }
        )* }

        /// Dispatch these requests on host request threads in [`crate::rpc::serve_local`].
        pub(crate) fn dispatch(vault: &Local, method: &str, p: &Value) -> Option<Result<Value>> {
            $( if method == stringify!($name) {
                crate::rpc::args!(p; session: u64 $(, $arg: $ty)*);
                return Some(crate::rpc::answer(crate::rpc::block(
                    vault.debug.$inner(session, $($arg),*),
                )));
            } )*
            None
        }
    };
}

sessions! {
    /// Set `path`'s breakpoints to `lines`, all of them; the answer is where the adapter holds
    /// them.
    debug_breakpoints => breakpoints(path: String, lines: Vec<u32>) -> Vec<Placed>;
    /// Run on; the adapter resumes every thread.
    debug_continue => resume(thread: i64) -> ();
    debug_pause => pause(thread: i64) -> ();
    debug_step => step(thread: i64, step: Step) -> ();
    debug_threads => threads() -> Vec<Thread>;
    /// The thread's calls, innermost first.
    debug_stack => stack(thread: i64) -> Vec<Frame>;
    debug_scopes => scopes(frame: i64) -> Vec<Scope>;
    debug_variables => variables(reference: i64) -> Vec<Variable>;
    /// `expression` in `frame`, or globally. A REPL evaluation waits as long as the reader's code
    /// runs.
    debug_evaluate => evaluate(frame: Option<i64>, expression: String, context: Context)
        -> Evaluated, within UNBOUNDED;
    /// Answer [`News::Terminal`] `seq`: the program's process id where the window runs it.
    debug_answer => answer(seq: i64, pid: Option<u32>) -> ();
}

/// The two calls the macro does not write: starting reads the vault's config, and stopping a
/// session already over is no error.
impl Vault {
    /// Debug `launch.program` as `session`, a number the caller picks. Answered once the program
    /// runs, which is after [`News::Terminal`] has been answered and the breakpoints are set.
    pub fn debug_start(&self, session: u64, launch: &Launch) -> Task<()> {
        match &self.backend {
            Backend::Local(v) => v.debug_start(session, launch),
            Backend::Remote(r) => remote_task_then(
                r.clone(),
                "debug_start",
                json!([session, launch]),
                UNBOUNDED,
                |_, answer| Ok(answer),
            ),
        }
    }

    /// End the program and its adapter.
    pub fn debug_stop(&self, session: u64) -> Task<()> {
        match &self.backend {
            Backend::Local(v) => v.debug_stop(session),
            Backend::Remote(r) => remote_task_then(
                r.clone(),
                "debug_stop",
                json!([session]),
                crate::rpc::DEADLINE,
                |_, answer| Ok(answer),
            ),
        }
    }
}

impl Local {
    pub fn debug_start(&self, session: u64, launch: &Launch) -> Task<()> {
        self.debug
            .start(session, launch.clone(), &self.config().debug)
    }

    pub fn debug_stop(&self, session: u64) -> Task<()> {
        self.debug.stop(session)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::VaultConfig;
    use crate::tests::wait_for;
    use std::process::{Command, Stdio};

    #[test]
    fn the_interpreter_is_the_nearest_venv_inside_the_vault() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("vault");
        let python = |venv: &Path| {
            let python = venv.join("bin/python");
            std::fs::create_dir_all(python.parent().unwrap()).unwrap();
            std::fs::write(&python, "").unwrap();
            python.to_string_lossy().into_owned()
        };
        // Above the vault is not the vault's.
        python(&tmp.path().join(".venv"));
        assert_eq!(interpreter(&root, &root.join("x.py"), &[]), ["python3"]);

        let outer = python(&root.join(".venv"));
        let inner = python(&root.join("sub/venv"));
        assert_eq!(
            interpreter(&root, &root.join("sub/deep/x.py"), &[]),
            [inner.as_str()]
        );
        assert_eq!(
            interpreter(&root, &root.join("x.py"), &[]),
            [outer.as_str()]
        );

        let configured = [".venv/bin/python".to_string(), "-X".to_string()];
        let program = root.join("sub/x.py");
        assert_eq!(
            interpreter(&root, &program, &configured),
            [outer.as_str(), "-X"]
        );
    }

    /// A vault closed while an adapter is still starting: the start holds `Debugs` itself, so it
    /// must find it shut down rather than register past the vault's end.
    #[test]
    fn a_start_the_vault_closed_under_leaves_no_adapter() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path();
        let (events, _news) = std::sync::mpsc::channel();
        let debugs = Arc::new(Debugs::new(dir.to_path_buf(), events));
        std::fs::write(dir.join("hold"), "").unwrap();
        let launch = Launch {
            program: "demo.py".to_string(),
            language: "python3".to_string(),
            breakpoints: Vec::new(),
        };
        let started = debugs.start(1, launch, &crate::tests::fake_adapter());
        let appears = |name: &str| {
            crate::tests::poll_until(|| dir.join(name).exists(), Duration::from_secs(10))
        };
        assert!(appears("initialize"), "the adapter never started");

        debugs.shutdown();
        std::fs::remove_file(dir.join("hold")).unwrap();
        let started = accent_lsp::runtime()
            .block_on(async { tokio::time::timeout(Duration::from_secs(10), started).await });
        assert!(matches!(started, Ok(Err(_))), "{started:?}");
        assert!(locked(&debugs.sessions).is_empty());
        assert!(appears("ended"), "the adapter outlived its vault");
    }

    #[test]
    fn debugpy_pauses_at_a_breakpoint_and_answers_for_the_frame() {
        if !crate::tests::has_debugpy() {
            eprintln!("debugpy is not installed: skipping the end-to-end test");
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let demo = "def double(x):\n    y = x * 2\n    return y\n\ntotal = double(21)\nprint(\"total\", total)\n";
        std::fs::write(root.path().join("demo.py"), demo).unwrap();
        let (vault, events) = Vault::open_at(
            root.path(),
            &cache.path().join("index.db"),
            VaultConfig::default(),
        )
        .unwrap();
        // The session's news in order, whatever the vault's worker says between.
        let next = |want: fn(&News) -> bool| {
            let event = wait_for(
                &events,
                |e| matches!(e, Event::Debug { session: 1, news } if want(news)),
                Duration::from_secs(30),
            );
            match event {
                Some(Event::Debug { news, .. }) => news,
                e => panic!("{e:?}"),
            }
        };

        let launch = Launch {
            program: "demo.py".to_string(),
            language: "python3".to_string(),
            breakpoints: vec![("demo.py".to_string(), vec![1])],
        };
        let started = vault.debug_start(1, &launch);

        let News::Terminal {
            seq,
            args,
            cwd,
            env,
        } = next(|n| matches!(n, News::Terminal { .. }))
        else {
            unreachable!()
        };
        let mut program = Command::new(&args[0])
            .args(&args[1..])
            .current_dir(cwd)
            .envs(env)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let answered = vault.debug_answer(1, seq, Some(program.id()));
        accent_lsp::runtime().block_on(answered).unwrap();

        let placed = next(|n| matches!(n, News::Breakpoints { .. }));
        let line_1 = Placed {
            line: 1,
            verified: true,
            message: None,
        };
        assert_eq!(
            placed,
            News::Breakpoints {
                path: "demo.py".to_string(),
                placed: vec![line_1]
            }
        );
        let News::Stopped { thread, reason, .. } = next(|n| matches!(n, News::Stopped { .. }))
        else {
            unreachable!()
        };
        assert_eq!(reason, "breakpoint");

        accent_lsp::runtime().block_on(async {
            started.await.unwrap();
            let thread = thread.unwrap();
            let top = vault.debug_stack(1, thread).await.unwrap().remove(0);
            assert_eq!(
                (top.name.as_str(), top.path.as_deref(), top.pos.line),
                ("double", Some("demo.py"), 1)
            );
            let scopes = vault.debug_scopes(1, top.id).await.unwrap();
            let locals = vault.debug_variables(1, scopes[0].reference).await;
            let locals = locals.unwrap();
            assert!(
                locals.iter().any(|v| v.name == "x" && v.value == "21"),
                "{locals:?}"
            );
            let twice = vault.debug_evaluate(1, Some(top.id), "x * 2".to_string(), Context::Repl);
            assert_eq!(twice.await.unwrap().result, "42");
            vault.debug_continue(1, thread).await.unwrap();
        });

        let ended = next(|n| matches!(n, News::Ended { .. }));
        assert_eq!(
            ended,
            News::Ended {
                code: Some(0),
                error: None
            }
        );
        program.wait().unwrap();
    }
}
