//! Debug sessions: a program run under its language's debug adapter, where the vault is.
//!
//! A façade group of its own (`Vault::debug_*`) rather than the `Language` trait: a session is a
//! program, not a document, and outlives any tab of its files. Sessions are keyed by a number the
//! caller picks, so their news ([`Event::Debug`]) can arrive before [`Vault::debug_start`]
//! answers. Lines are 0-based here and 1-based on the wire; paths are vault-relative inside the
//! vault and absolute outside, as a [`Location`](crate::Location)'s.

mod session;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::Sender;
use std::time::Duration;

pub use accent_core::config::{Breakpoint, ExceptionFilter};

use accent_core::config::DebugConfig;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::kernel::{Cells, Kernels};
use crate::language::{Pos, Task, in_path, remote_task_then};
use crate::registry::Registry;
use crate::vault::Backend;
use crate::{Error, Event, Local, Result, Vault};
use session::Session;

// ---------------------------------------------------------------- the façade's types

/// What to debug: `program` in `language` (a GtkSourceView id), stopping at `breakpoints`, each
/// a file and its own, and at the `exceptions` switched on, as the window last had them; one
/// the window has not seen is as the adapter defaults it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Launch {
    pub program: String,
    pub language: String,
    pub breakpoints: Vec<(String, Vec<Breakpoint>)>,
    pub exceptions: Vec<ExceptionFilter>,
}

/// What to debug a notebook's cells in: the kernel the vault knows by `kernel`, its debugger
/// stopping at `breakpoints` and `exceptions` as [`Launch`]'s does. The cells' breakpoints go once
/// the kernel has their files ([`Vault::debug_dump`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Attach {
    pub kernel: u64,
    pub breakpoints: Vec<(String, Vec<Breakpoint>)>,
    pub exceptions: Vec<ExceptionFilter>,
}

/// A breakpoint as the adapter holds it: maybe on another line than asked, and maybe not bound.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Placed {
    /// On the line the adapter holds it, with what was asked of it beside the line.
    pub breakpoint: Breakpoint,
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

/// What completes the text before a column: `text` written over the `replaces` characters
/// before it, `label` when it is listed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Candidate {
    pub label: String,
    pub text: String,
    pub replaces: u32,
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

/// What an adapter takes beside a breakpoint's line, as its capabilities say.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Supports {
    pub condition: bool,
    pub hit_condition: bool,
    pub log_message: bool,
}

/// Who asks for an expression: the console's prompt runs whatever it is given, a hover and a
/// watch must change nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Context {
    Repl,
    Hover,
    Watch,
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
    /// What the adapter's breakpoints take beside their lines.
    Supports(Supports),
    /// The exceptions the adapter can stop at, each on or off as the program is launched with.
    Exceptions(Vec<ExceptionFilter>),
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
        // By the link itself: a venv's `python` points into the host's `/usr`, which the Flatpak
        // cannot see.
        if python.symlink_metadata().is_ok() {
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
    sessions: Registry<Session>,
}

impl Debugs {
    pub(crate) fn new(root: PathBuf, events: Sender<Event>) -> Debugs {
        Debugs {
            root,
            events,
            sessions: Registry::new("debug session"),
        }
    }

    /// Start the adapter and launch the program; answered once the program runs.
    pub(crate) fn start(self: &Arc<Self>, id: u64, launch: Launch, cfg: &DebugConfig) -> Task<()> {
        let (debugs, cfg) = (self.clone(), cfg.clone());
        Task::spawn(session::start(debugs, id, launch, cfg))
    }

    /// Attach to `attach.kernel`'s debugger; answered once it stops where asked.
    pub(crate) fn attach(
        self: &Arc<Self>,
        id: u64,
        attach: Attach,
        kernels: &Kernels,
    ) -> Task<Cells> {
        let (debugs, debuggee) = (self.clone(), kernels.debugger(attach.kernel));
        Task::spawn(async move { session::attach(debugs, id, attach, debuggee?).await })
    }

    /// End the program and its adapter; a session already over is no error.
    pub(crate) fn stop(&self, id: u64) -> Task<()> {
        let session = self.sessions.remove(id);
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
        self.sessions.shutdown(Session::stop);
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
                let session = self.sessions.get(session);
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
    /// Set `path`'s breakpoints to `breakpoints`, all of them; the answer is where the adapter
    /// holds them.
    debug_breakpoints => breakpoints(path: String, breakpoints: Vec<Breakpoint>) -> Vec<Placed>;
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
    /// Stop at the exceptions `filters` name, and at no others.
    debug_exceptions => exceptions(filters: Vec<String>) -> ();
    /// What completes `text` before char `column` in `frame`, or globally.
    debug_completions => completions(frame: Option<i64>, text: String, column: u32)
        -> Vec<Candidate>;
    /// Answer [`News::Terminal`] `seq`: the program's process id where the window runs it.
    debug_answer => answer(seq: i64, pid: Option<u32>) -> ();
    /// Have a notebook's kernel write `code` to the file it runs it as, which takes breakpoints
    /// once it is there: that file's path.
    debug_dump => dump(code: String) -> String;
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

    /// Debug the cells of a notebook's kernel as `session`: answered once its debugger stops
    /// where asked, with how the kernel names the cells' files, which their frames name. Its
    /// first answer may wait for debugpy to start.
    pub fn debug_kernel(&self, session: u64, attach: &Attach) -> Task<Cells> {
        match &self.backend {
            Backend::Local(v) => v.debug_kernel(session, attach),
            Backend::Remote(r) => remote_task_then(
                r.clone(),
                "debug_kernel",
                json!([session, attach]),
                crate::kernel::START,
                |_, answer| Ok(answer),
            ),
        }
    }

    /// End the program and its adapter; a notebook's kernel lives on.
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

    pub fn debug_kernel(&self, session: u64, attach: &Attach) -> Task<Cells> {
        self.debug.attach(session, attach.clone(), &self.kernels)
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
            exceptions: Vec::new(),
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
        assert!(debugs.sessions.get(1).is_err());
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
            breakpoints: vec![("demo.py".to_string(), vec![Breakpoint::at(1)])],
            exceptions: Vec::new(),
        };
        let started = vault.debug_start(1, &launch);

        let News::Exceptions(filters) = next(|n| matches!(n, News::Exceptions(_))) else {
            unreachable!()
        };
        let on: Vec<_> = filters
            .iter()
            .filter(|f| f.on)
            .map(|f| f.filter.as_str())
            .collect();
        assert_eq!(on, ["uncaught"], "debugpy's defaults");

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
            breakpoint: Breakpoint::at(1),
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
            // Columns in characters: the `é` before is one, whatever it is on the wire.
            let text = "é + x.bi".to_string();
            let offered = vault
                .debug_completions(1, Some(top.id), text, 8)
                .await
                .unwrap();
            let offered: Vec<_> = offered
                .iter()
                .map(|c| (c.text.as_str(), c.replaces))
                .collect();
            assert_eq!(offered, [("bit_count", 2), ("bit_length", 2)]);
            let raised = vault.debug_exceptions(1, vec!["raised".to_string()]);
            raised.await.unwrap();
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
    /// A cell of a notebook debugged in its kernel, the kernel and what it holds living on past
    /// the session, a second session on it, and a kernel that dies under a paused cell ending
    /// its session.
    #[test]
    fn a_notebook_cell_pauses_at_a_breakpoint_and_its_kernel_lives_on() {
        if !crate::tests::has_ipykernel() || !crate::tests::has_debugpy() {
            eprintln!("ipykernel or debugpy is not installed: skipping the end-to-end test");
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("tmp")).unwrap();
        // The kernel's IPython folder, and the folder it writes its cells to, are the test's.
        let python = r#"IPYTHONDIR="$PWD/ipython" TMPDIR="$PWD/tmp" exec python3 "$@""#;
        let config = VaultConfig {
            debug: DebugConfig {
                python: ["sh", "-c", python, "sh"].map(String::from).to_vec(),
                ..Default::default()
            },
            ..Default::default()
        };
        let (vault, events) =
            Vault::open_at(root.path(), &cache.path().join("index.db"), config).unwrap();
        fn wait<T>(task: impl std::future::Future<Output = T>) -> T {
            accent_lsp::runtime().block_on(task)
        }
        let next = |want: &dyn Fn(&Event) -> bool| {
            wait_for(&events, want, Duration::from_secs(60)).expect("the news in time")
        };
        let done = |run: u64| move |e: &Event| matches!(e, Event::Kernel { news: crate::kernel::News::Done { run: r, .. }, .. } if *r == run);
        let stopped = |session: u64| move |e: &Event| matches!(e, Event::Debug { session: s, news: News::Stopped { .. } } if *s == session);
        let double = "def double(x):\n    return x * 2\n";
        let total = "total = double(21)\nprint(\"total\", total)\n";

        let start = crate::kernel::Start {
            notebook: "run.ipynb".to_string(),
            kernel: None,
        };
        wait(vault.kernel_start(1, &start)).unwrap();
        wait(vault.kernel_execute(1, 1, double.to_string())).unwrap();
        next(&done(1));

        let attach = Attach {
            kernel: 1,
            breakpoints: Vec::new(),
            exceptions: Vec::new(),
        };
        let cells = wait(vault.debug_kernel(7, &attach)).unwrap();
        let path = wait(vault.debug_dump(7, double.to_string())).unwrap();
        assert_eq!(path, cells.path(double));
        let placed = wait(vault.debug_breakpoints(7, path.clone(), vec![Breakpoint::at(1)]));
        assert!(placed.unwrap()[0].verified);
        wait(vault.kernel_execute(1, 2, total.to_string())).unwrap();
        let Event::Debug {
            news: News::Stopped { thread, reason, .. },
            ..
        } = next(&stopped(7))
        else {
            unreachable!()
        };
        assert_eq!(reason, "breakpoint");
        let thread = thread.unwrap();
        let top = wait(vault.debug_stack(7, thread)).unwrap().remove(0);
        assert_eq!(
            (top.name.as_str(), top.path.as_deref(), top.pos.line),
            ("double", Some(path.as_str()), 1)
        );
        let scopes = wait(vault.debug_scopes(7, top.id)).unwrap();
        let locals = wait(vault.debug_variables(7, scopes[0].reference)).unwrap();
        assert!(
            locals.iter().any(|v| v.name == "x" && v.value == "21"),
            "{locals:?}"
        );
        let twice = vault.debug_evaluate(7, Some(top.id), "x * 2".to_string(), Context::Repl);
        assert_eq!(wait(twice).unwrap().result, "42");
        wait(vault.debug_continue(7, thread)).unwrap();
        // The cell's print is its own output, as without the debugger.
        let Event::Kernel {
            news: crate::kernel::News::Stream { text, .. },
            ..
        } = next(&|e| {
            matches!(
                e,
                Event::Kernel {
                    news: crate::kernel::News::Stream { run: 2, .. },
                    ..
                }
            )
        })
        else {
            unreachable!()
        };
        assert_eq!(text, "total 42\n");
        next(&done(2));
        wait(vault.debug_stop(7)).unwrap();
        let ended = next(&|e| {
            matches!(
                e,
                Event::Debug {
                    session: 7,
                    news: News::Ended { .. }
                }
            )
        });
        assert!(
            matches!(
                ended,
                Event::Debug {
                    news: News::Ended { error: None, .. },
                    ..
                }
            ),
            "{ended:?}"
        );

        // The kernel lives on, holding what the cells made.
        wait(vault.kernel_execute(1, 3, "total".to_string())).unwrap();
        let result = next(&|e| {
            matches!(
                e,
                Event::Kernel {
                    news: crate::kernel::News::Output { run: 3, .. },
                    ..
                }
            )
        });
        let Event::Kernel {
            news: crate::kernel::News::Output { output, .. },
            ..
        } = result
        else {
            unreachable!()
        };
        assert_eq!(output["data"]["text/plain"], "42");

        // A second session finds the kernel's debugger started.
        let cells = wait(vault.debug_kernel(8, &attach)).unwrap();
        let path = wait(vault.debug_dump(8, double.to_string())).unwrap();
        assert_eq!(path, cells.path(double));
        wait(vault.debug_breakpoints(8, path, vec![Breakpoint::at(1)])).unwrap();
        wait(vault.kernel_execute(1, 4, total.to_string())).unwrap();
        let Event::Debug {
            news: News::Stopped { thread, .. },
            ..
        } = next(&stopped(8))
        else {
            unreachable!()
        };
        // Killed while paused: the session ends quietly with it, the kernel saying why.
        let top = wait(vault.debug_stack(8, thread.unwrap()))
            .unwrap()
            .remove(0);
        let pid = "__import__('os').getpid()".to_string();
        let pid = wait(vault.debug_evaluate(8, Some(top.id), pid, Context::Repl)).unwrap();
        let pid: i32 = pid.result.parse().unwrap();
        // SAFETY: kill takes plain integers; the process is the test's own kernel.
        assert_eq!(unsafe { libc::kill(pid, libc::SIGKILL) }, 0);
        let (mut debug, mut kernel) = (None, None);
        while debug.is_none() || kernel.is_none() {
            match next(&|e| {
                matches!(
                    e,
                    Event::Debug {
                        session: 8,
                        news: News::Ended { .. }
                    } | Event::Kernel {
                        session: 1,
                        news: crate::kernel::News::Ended { .. }
                    }
                )
            }) {
                Event::Debug { news, .. } => debug = Some(news),
                Event::Kernel { news, .. } => kernel = Some(news),
                _ => unreachable!(),
            }
        }
        assert_eq!(
            debug,
            Some(News::Ended {
                code: None,
                error: None
            })
        );
        assert!(
            matches!(kernel, Some(crate::kernel::News::Ended { error: Some(_) })),
            "{kernel:?}"
        );
        assert!(
            wait(vault.debug_stack(8, 1)).is_err(),
            "the session is over"
        );
    }
}
