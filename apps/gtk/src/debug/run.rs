//! A debug session in the window: Start, Stop and the steps, what the session says on its own
//! (`on_debug`), the program's terminal, and where the program is paused.

use super::*;
use accent_api::debug::{Category, Context, Evaluated, Frame, Launch, News, Step, Variable};
use sourceview5::prelude::BufferExt as _;

/// One session, from Start until its end.
#[derive(Default)]
pub(super) struct Run {
    id: u64,
    program: String,
    language: String,
    state: Cell<State>,
    /// The frame Variables is about.
    frame: Cell<Option<i64>>,
    /// Bumped each time the program moves on, so an answer about where it was is dropped.
    generation: Cell<u32>,
    /// What the strip says while paused, and the exception's text in its tooltip.
    paused: RefCell<(String, Option<String>)>,
    /// Restart Debugging: start the program again once this session has ended.
    restart: Cell<bool>,
    /// The files whose breakpoints changed while the program started, which it is told once it
    /// runs: it was launched with them as they were at Start.
    changed: RefCell<std::collections::BTreeSet<String>>,
}

#[derive(Clone, Copy, PartialEq, Debug, Default)]
enum State {
    #[default]
    Starting,
    Running,
    Paused {
        thread: i64,
    },
    Stopping,
}

impl App {
    /// Start Debugging: the Python file in front, else the program last debugged here.
    pub(crate) fn start_debugging(self: &Rc<Self>) {
        // The adapter and the program would both have to be the host's: debugpy's launcher calls
        // back over a loopback the sandbox does not share.
        if Path::new("/.flatpak-info").exists() {
            return self.debug_cannot(
                "start debugging",
                "the Flatpak cannot run a debugger on this machine yet",
            );
        }
        if self.debugger.running() {
            return self.debug_cannot("start debugging", "a program is already being debugged");
        }
        let last = self.debugger.last.borrow().clone();
        match self.debuggable_in_front().or(last) {
            Some((program, language)) => self.launch_program(program, language),
            None => self.debug_cannot("start debugging", "no Python file is in front"),
        }
    }

    /// The file in front and its language, where Start Debugging can run it: a vault's file a
    /// debugger runs.
    pub(crate) fn debuggable_in_front(&self) -> Option<(String, String)> {
        self.active()
            .filter(|tab| self.vault().is_some() && !doc::is_loose_key(&tab.rel()))
            .map(|tab| (tab.rel(), lang::language_id(&tab)))
            .filter(|(_, language)| debuggable(language))
    }

    /// Debug `program`, its edits written first as VS Code's `debug.saveBeforeStart` does: the
    /// program runs what is on disk.
    fn launch_program(self: &Rc<Self>, program: String, language: String) {
        let Some(vault) = self.vault().cloned() else {
            return;
        };
        for tab in self.open_tabs() {
            if !doc::is_loose_key(&tab.rel()) {
                self.save_tab_now(&tab);
            }
        }
        let id = self.debugger.next.get() + 1;
        self.debugger.next.set(id);
        let launch = Launch {
            program: program.clone(),
            language: language.clone(),
            breakpoints: self.debugger.saved(self).into_iter().collect(),
        };
        self.debugger
            .last
            .replace(Some((program.clone(), language.clone())));
        let run = Rc::new(Run {
            id,
            program,
            language,
            ..Run::default()
        });
        self.debugger.run.replace(Some(run.clone()));
        self.sync_debug();
        self.sync_code_pane();
        if let Some(sidebar) = self
            .sidebar
            .get()
            .filter(|_| self.sidebar_column.is_visible())
        {
            sidebar.show_pane("code");
        }
        let started = vault.debug_start(id, &launch);
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let started = started.await;
            let Some(app) = weak.upgrade() else {
                return;
            };
            match (started, run.state.get()) {
                (Ok(()), State::Starting) => {
                    run.state.set(State::Running);
                    app.sync_debug();
                    let now = app.debugger.saved(&app);
                    for key in run.changed.take() {
                        let lines = now.get(&key).cloned().unwrap_or_default();
                        app.send_breakpoints(key, lines);
                    }
                }
                // A Stop sent before the session was registered reached nothing.
                (Ok(()), State::Stopping) => app.send_stop(&run),
                (Ok(()), _) => {}
                // Stopping while starting fails the start: the reader's doing, not news.
                (Err(_), State::Stopping) => app.debug_ended(run.id),
                (Err(e), _) => {
                    app.debug_cannot("start debugging", e);
                    app.debug_ended(run.id);
                }
            }
        });
    }

    /// Stop Debugging: the program and its adapter end, and the session with them.
    pub(crate) fn stop_debugging(self: &Rc<Self>) {
        let Some(run) = self
            .debugger
            .run()
            .filter(|run| run.state.get() != State::Stopping)
        else {
            return;
        };
        run.state.set(State::Stopping);
        self.moved_on(&run);
        self.sync_debug();
        self.send_stop(&run);
    }

    fn send_stop(&self, run: &Run) {
        let Some(vault) = self.vault() else {
            return;
        };
        let stopped = vault.debug_stop(run.id);
        glib::spawn_future_local(async move {
            if let Err(e) = stopped.await {
                tracing::warn!("stopping the debug session: {e}");
            }
        });
    }

    /// Restart Debugging: Stop, then Start with the same program once the session has ended.
    /// debugpy has no `restart` of its own.
    pub(crate) fn restart_debugging(self: &Rc<Self>) {
        if let Some(run) = self.debugger.run().filter(|run| run.started()) {
            run.restart.set(true);
            self.stop_debugging();
        }
    }

    /// Continue: every thread runs on.
    pub(crate) fn debug_resume(self: &Rc<Self>) {
        self.move_on("continue", |vault, id, thread| {
            vault.debug_continue(id, thread)
        });
    }

    pub(crate) fn debug_step(self: &Rc<Self>, step: Step) {
        self.move_on("step", move |vault, id, thread| {
            vault.debug_step(id, thread, step)
        });
    }

    /// Ask the paused thread to `ask`; what the pane shows of where it was goes at once.
    fn move_on(
        self: &Rc<Self>,
        what: &'static str,
        ask: impl FnOnce(&Vault, u64, i64) -> accent_api::Task<()>,
    ) {
        let Some(run) = self.debugger.run() else {
            return;
        };
        let (State::Paused { thread }, Some(vault)) = (run.state.get(), self.vault()) else {
            return;
        };
        run.state.set(State::Running);
        self.moved_on(&run);
        self.sync_debug();
        let asked = ask(vault, run.id, thread);
        glib::spawn_future_local(async move {
            if let Err(e) = asked.await {
                tracing::warn!("cannot {what}: {e}");
            }
        });
    }

    /// Pause: pydevd suspends the thread named, the first one `threads` lists.
    pub(crate) fn debug_pause(self: &Rc<Self>) {
        let Some(run) = self
            .debugger
            .run()
            .filter(|run| run.state.get() == State::Running)
        else {
            return;
        };
        let (Some(vault), id) = (self.vault().cloned(), run.id) else {
            return;
        };
        glib::spawn_future_local(async move {
            let threads =
                crate::work::or_empty("listing the threads", vault.debug_threads(id).await);
            if let Some(thread) = threads.first()
                && let Err(e) = vault.debug_pause(id, thread.id).await
            {
                tracing::warn!("cannot pause: {e}");
            }
        });
    }

    /// The program moved on from where it was paused: Call Stack, Variables and the execution
    /// line go, and answers about where it was are dropped.
    fn moved_on(&self, run: &Run) {
        run.generation.set(run.generation.get() + 1);
        run.frame.set(None);
        self.clear_execution();
    }

    /// Whether `run` is still the session, at `generation`.
    fn current(&self, run: &Rc<Run>, generation: u32) -> bool {
        self.debugger
            .run()
            .is_some_and(|now| Rc::ptr_eq(&now, run) && now.generation.get() == generation)
    }

    /// What the session says on its own; another session's news is an ended one's, and dropped.
    pub(crate) fn on_debug(self: &Rc<Self>, session: u64, news: News) {
        let Some(run) = self.debugger.run().filter(|run| run.id == session) else {
            return;
        };
        let stopping = run.state.get() == State::Stopping;
        match news {
            // A Stop sent before the session was registered reached nothing: this is the session
            // it missed.
            News::Terminal { .. } if stopping => self.send_stop(&run),
            News::Terminal {
                seq,
                args,
                cwd,
                env,
            } => {
                self.open_program(&run, seq, args, cwd, env);
                self.open_console(run.id, &run.program);
            }
            News::Stopped { .. } | News::Continued if stopping => {}
            News::Stopped {
                thread,
                reason,
                text,
                description,
            } => {
                self.moved_on(&run);
                let line = match (reason.as_str(), text) {
                    ("breakpoint", _) => "Paused on breakpoint".to_string(),
                    ("exception", Some(text)) => format!("Paused on exception: {text}"),
                    ("exception", None) => "Paused on exception".to_string(),
                    _ => "Paused".to_string(),
                };
                run.paused.replace((line, description));
                if let Some(thread) = thread {
                    run.state.set(State::Paused { thread });
                }
                self.sync_debug();
                self.show_stop(&run, thread);
            }
            News::Continued => {
                if matches!(run.state.get(), State::Paused { .. }) {
                    run.state.set(State::Running);
                    self.moved_on(&run);
                    self.sync_debug();
                }
            }
            News::Output { category, text } => {
                tracing::info!(target: "accent::debug", "{category:?}: {}", text.trim_end());
                if category == Category::Important {
                    self.debug_toast(text.trim());
                }
                self.console_output(category, &text);
            }
            // The adapter answering for the lines it was launched with, which the reader has
            // changed since: theirs go once the program runs.
            News::Breakpoints { path, .. } if run.changed.borrow().contains(&path) => {}
            News::Breakpoints { path, placed } => {
                if let Some(tab) = self.tab_for(&path) {
                    breakpoints::bind(&tab, &placed);
                    breakpoints::redraw(&tab);
                }
                self.refill_breakpoints();
            }
            News::Ended { code, error } => {
                self.console_end(session, code);
                self.debug_ended(session);
                if let Some(error) = error {
                    self.debug_toast(&format!("The debug session ended: {error}"));
                }
            }
        }
    }

    /// Where the program stopped: its threads with the paused one open on its calls, and the
    /// innermost of those selected. A stop that names no thread is the first one's.
    fn show_stop(self: &Rc<Self>, run: &Rc<Run>, thread: Option<i64>) {
        let Some(vault) = self.vault().cloned() else {
            return;
        };
        let (weak, run, generation) = (Rc::downgrade(self), run.clone(), run.generation.get());
        glib::spawn_future_local(async move {
            let threads = vault.debug_threads(run.id).await;
            let threads = crate::work::or_empty("listing the threads", threads);
            let Some(thread) = thread.or_else(|| threads.first().map(|t| t.id)) else {
                return;
            };
            let frames = vault.debug_stack(run.id, thread).await;
            let frames = crate::work::or_empty("listing the calls", frames);
            let Some(app) = weak.upgrade().filter(|app| app.current(&run, generation)) else {
                return;
            };
            if run.state.get() != (State::Paused { thread }) {
                run.state.set(State::Paused { thread });
                app.sync_debug();
            }
            if let Some(sidebar) = app.sidebar.get() {
                sidebar.set_debug_threads(&threads, Some((thread, frames.clone())));
            }
            if let Some(top) = frames.first() {
                app.select_frame(top);
            }
        });
    }

    /// Show `frame`: its line in its tab, marked, and its variables. A file in the vault comes to
    /// the front in the preview tab, one outside it on a local vault in a kept tab (read-only
    /// where a package manager put it); a remote vault's, and code with no file, open nothing.
    pub(crate) fn select_frame(self: &Rc<Self>, frame: &Frame) {
        let Some(run) = self
            .debugger
            .run()
            .filter(|run| matches!(run.state.get(), State::Paused { .. }))
        else {
            return;
        };
        let Some(vault) = self.vault().cloned() else {
            return;
        };
        run.frame.set(Some(frame.id));
        self.clear_execution();
        if let Some(sidebar) = self.sidebar.get() {
            sidebar.select_debug_frame(frame.id);
        }
        let (generation, id) = (run.generation.get(), frame.id);
        let shown = move |app: &Rc<App>, run: &Rc<Run>| {
            app.current(run, generation) && run.frame.get() == Some(id)
        };
        let how = match frame.path.as_deref() {
            Some(path) if !doc::is_loose_key(path) => Some(Opened::Preview),
            Some(_) if !vault.is_remote() => Some(Opened::Kept),
            _ => None,
        };
        if let (Some(path), Some(how)) = (&frame.path, how) {
            let (run, pos) = (run.clone(), frame.pos);
            let show = move |app: &Rc<App>, tab: &Rc<Tab>| {
                if shown(app, &run) {
                    tab.show_at(&lang::iter_at(&tab.buffer, pos), 0.25);
                    app.mark_execution(tab, pos.line);
                }
            };
            match self.tab_for(path) {
                // In front of its own pane, the keyboard and the active pane staying where they
                // are, as for a tab brought forward behind the reader's back (Tabs).
                Some(tab) => {
                    if let Some(pane) = self.pane_of(&tab.page) {
                        pane.tabs.set_selected_page(&tab.page);
                    }
                    show(self, &tab);
                }
                None => self.with_tab(path, how, "show where the program paused", show),
            }
        }
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let scopes =
                crate::work::or_empty("listing the scopes", vault.debug_scopes(run.id, id).await);
            if let Some(app) = weak.upgrade().filter(|app| shown(app, &run))
                && let Some(sidebar) = app.sidebar.get()
            {
                sidebar.set_debug_scopes(&scopes);
            }
        });
    }

    /// A thread's calls, for Call Stack opening one; dropped once the program has moved on.
    pub(crate) fn debug_frames(self: &Rc<Self>, thread: i64, then: Box<dyn FnOnce(Vec<Frame>)>) {
        self.ask_paused("listing the calls", then, move |vault, id| {
            vault.debug_stack(id, thread)
        });
    }

    /// What a value holds, for Variables opening it.
    pub(crate) fn debug_children(
        self: &Rc<Self>,
        reference: i64,
        then: Box<dyn FnOnce(Vec<Variable>)>,
    ) {
        self.ask_paused("listing the variables", then, move |vault, id| {
            vault.debug_variables(id, reference)
        });
    }

    /// `expression` in the call selected, while the program is paused: what the Debug Console
    /// and a hover ask.
    pub(crate) fn debug_evaluate(
        &self,
        expression: String,
        context: Context,
    ) -> Option<accent_api::Task<Evaluated>> {
        let run = self
            .debugger
            .run()
            .filter(|run| matches!(run.state.get(), State::Paused { .. }))?;
        let vault = self.vault()?;
        Some(vault.debug_evaluate(run.id, run.frame.get(), expression, context))
    }

    fn ask_paused<T: Default + 'static>(
        self: &Rc<Self>,
        what: &'static str,
        then: Box<dyn FnOnce(T)>,
        ask: impl FnOnce(&Vault, u64) -> accent_api::Task<T>,
    ) {
        let (Some(run), Some(vault)) = (self.debugger.run(), self.vault()) else {
            return;
        };
        let asked = ask(vault, run.id);
        let (weak, generation) = (Rc::downgrade(self), run.generation.get());
        glib::spawn_future_local(async move {
            let answer = crate::work::or_empty(what, asked.await);
            if weak
                .upgrade()
                .is_some_and(|app| app.current(&run, generation))
            {
                then(answer);
            }
        });
    }

    /// The paused line, marked in its tab: one in the window at a time.
    fn mark_execution(&self, tab: &Rc<Tab>, line: u32) {
        self.clear_execution();
        if let Some(start) = tab.buffer.iter_at_line(line as i32) {
            let mark = tab
                .buffer
                .create_source_mark(None, breakpoints::EXECUTION, &start);
            tab.execution.replace(Some(mark));
            self.debugger.execution.replace(Rc::downgrade(tab));
            breakpoints::redraw(tab);
        }
    }

    fn clear_execution(&self) {
        if let Some(tab) = self.debugger.execution.take().upgrade()
            && let Some(mark) = tab.execution.take()
        {
            tab.buffer.delete_mark(&mark);
            breakpoints::redraw(&tab);
        }
    }

    /// The session is over: its marks and its sections go, the rings are dots again, which a
    /// next session is yet to bind, and a Restart starts it again.
    fn debug_ended(self: &Rc<Self>, id: u64) {
        let Some(run) = self.debugger.run().filter(|run| run.id == id) else {
            return;
        };
        self.debugger.run.replace(None);
        self.console_end(id, None);
        self.clear_execution();
        for tab in self.open_tabs() {
            if !tab.breakpoints.borrow().is_empty() {
                breakpoints::place(&tab, &breakpoints::lines(&tab));
                breakpoints::redraw(&tab);
            }
        }
        self.sync_debug();
        self.sync_code_pane();
        if run.restart.get() {
            self.launch_program(run.program.clone(), run.language.clone());
        }
    }

    /// The vault's link dropped: the session ended with it, and there is no host to tell.
    pub(crate) fn debug_lost(self: &Rc<Self>) {
        if let Some(run) = self.debugger.run() {
            run.restart.set(false);
            self.debug_ended(run.id);
        }
    }

    /// The session's controls as its state allows, and the strip saying what that is.
    pub(crate) fn sync_debug(&self) {
        // A session's chords are the window's while it runs (`actions::installed`).
        if let Some((shell, gtk_app)) = self.shell.upgrade().zip(self.window.application()) {
            shell.sync_accels(&gtk_app);
        }
        let run = self.debugger.run();
        let state = run.as_ref().map(|run| run.state.get());
        let paused = matches!(state, Some(State::Paused { .. }));
        let running = state == Some(State::Running);
        let live = run.as_ref().is_some_and(|run| run.started());
        for (action, on) in [
            ("debug-continue", paused),
            ("debug-step-over", paused),
            ("debug-step-into", paused),
            ("debug-step-out", paused),
            ("debug-pause", running),
            ("debug-restart", live),
            (
                "debug-stop",
                run.is_some() && state != Some(State::Stopping),
            ),
        ] {
            if let Some(action) = self
                .window
                .lookup_action(action)
                .and_downcast::<gio::SimpleAction>()
            {
                action.set_enabled(on);
            }
        }
        let Some(sidebar) = self.sidebar.get() else {
            return;
        };
        let (Some(run), Some(state)) = (run, state) else {
            return sidebar.clear_debug(self.debuggable_in_front().is_some());
        };
        let said = run.paused.borrow();
        let (line, why) = match state {
            State::Starting => ("Starting…", None),
            State::Running => ("Running", None),
            State::Paused { .. } => (said.0.as_str(), said.1.as_deref()),
            State::Stopping => ("Stopping…", None),
        };
        sidebar.set_debug_state(line, why, paused);
    }

    /// The program's terminal, as the adapter asked: in the active pane, in front with the
    /// keyboard, as a new shell is; or where the last session's was, in its place.
    fn open_program(
        self: &Rc<Self>,
        run: &Run,
        seq: i64,
        args: Vec<String>,
        cwd: String,
        env: Vec<(String, String)>,
    ) {
        let (weak, id) = (Rc::downgrade(self), run.id);
        let program = terminal::Program {
            args,
            cwd,
            env,
            title: doc::file_name(&run.program).to_string(),
            link: self
                .vault()
                .and_then(|v| v.remote())
                .map(|r| r.url().clone()),
            started: Box::new(move |started| {
                let Some(app) = weak.upgrade() else {
                    return;
                };
                match started {
                    Ok(pid) => app.answer_terminal(id, seq, pid),
                    // debugpy would wait a minute for a program that is not coming.
                    Err(why) if app.debugger.run().is_some_and(|run| run.id == id) => {
                        app.debug_cannot("start debugging", why);
                        app.stop_debugging();
                    }
                    Err(_) => {}
                }
            }),
        };
        let old = self
            .docs()
            .into_iter()
            .find(|doc| doc.terminal().is_some_and(|term| term.is_program()));
        let place = old.as_ref().and_then(|old| {
            let pane = self.pane_of(old.page())?;
            let at = pane.tabs.page_position(old.page());
            Some((pane, at))
        });
        // Put in its place, not moved there: AdwTabBar's reorder animation stalls on a tab that
        // closes while it runs, and leaves the new tab undrawn.
        let shell = terminal::Shell::Program(Rc::new(program));
        self.open_shell_at(terminal::program_key(), shell, Opened::Kept, place);
        // A turn later: closed in the turn the new tab went in beside it, with the console after
        // it, the new tab was left undrawn all the same (ISSUES.md).
        if let Some(old) = old {
            let (weak, page) = (Rc::downgrade(self), old.page().clone());
            glib::idle_add_local_once(move || {
                if let Some(app) = weak.upgrade() {
                    app.close_page(&page);
                }
            });
        }
    }

    /// Tell the adapter the program runs.
    fn answer_terminal(&self, id: u64, seq: i64, pid: Option<u32>) {
        let Some(vault) = self.vault() else {
            return;
        };
        let answered = vault.debug_answer(id, seq, pid);
        glib::spawn_future_local(async move {
            if let Err(e) = answered.await {
                tracing::warn!("answering the debug adapter: {e}");
            }
        });
    }

    /// Tell a running program file `key`'s breakpoints, all of them, and move its tab's marks to
    /// where the program holds them. One still starting is told once it runs; an answer that
    /// does not come leaves the marks as the reader set them.
    pub(super) fn send_breakpoints(self: &Rc<Self>, key: String, lines: Vec<u32>) {
        let (Some(run), Some(vault)) = (self.debugger.run(), self.vault()) else {
            return;
        };
        if !run.takes_breakpoints(&key) {
            return;
        }
        let asked = vault.debug_breakpoints(run.id, key.clone(), lines);
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let placed = match asked.await {
                Ok(placed) => placed,
                Err(e) => return tracing::warn!("setting the breakpoints of {key}: {e}"),
            };
            let Some(app) = weak.upgrade() else {
                return;
            };
            if app.debugger.run().is_some_and(|now| Rc::ptr_eq(&now, &run)) {
                if let Some(tab) = app.tab_for(&key) {
                    breakpoints::bind(&tab, &placed);
                    breakpoints::redraw(&tab);
                }
                app.refill_breakpoints();
            }
        });
    }

    /// The Breakpoints section's rows, `path:line` as References' are.
    pub(crate) fn refill_breakpoints(&self) {
        let Some(sidebar) = self.sidebar.get() else {
            return;
        };
        let rows: Vec<String> = self
            .debugger
            .saved(self)
            .iter()
            .flat_map(|(key, lines)| lines.iter().map(move |line| format!("{key}:{}", line + 1)))
            .collect();
        sidebar.set_breakpoint_rows(&rows);
    }

    /// A debugging toast: a newer one takes its place.
    fn debug_toast(&self, text: &str) {
        self.add_toast(Toast::new(text).key("debug"));
    }

    fn debug_cannot(&self, what: &str, why: impl std::fmt::Display) {
        self.debug_toast(&format!("Cannot {what}: {why}"));
    }
}

impl Run {
    /// Whether the program runs, paused or not: a session past its start and not stopping.
    fn started(&self) -> bool {
        matches!(self.state.get(), State::Running | State::Paused { .. })
    }

    /// Whether the program takes `key`'s breakpoints now. One still starting is told once it
    /// runs, so the file is noted; one stopping needs none.
    fn takes_breakpoints(&self, key: &str) -> bool {
        if self.state.get() == State::Starting {
            self.changed.borrow_mut().insert(key.to_string());
        }
        self.started()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn breakpoints_changed_while_starting_wait_for_the_program() {
        let run = Run::default();
        assert!(!run.takes_breakpoints("lib.py"));
        assert!(run.changed.borrow().contains("lib.py"));
        run.state.set(State::Paused { thread: 1 });
        assert!(run.takes_breakpoints("demo.py"));
        run.state.set(State::Stopping);
        assert!(!run.takes_breakpoints("demo.py"));
        assert_eq!(run.changed.take().len(), 1, "noted only while starting");
    }
}
