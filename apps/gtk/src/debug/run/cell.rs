//! Debug Cell (user decisions 2026-11-19): the selected code cell of a Python notebook run under
//! the debugger in the notebook's own kernel, which the window attaches to as the cell's run is
//! sent and lets go once it is done, the kernel and what it holds living on. The session is any
//! other's in the window; the kernel names each cell's file by the code it last ran, which is how
//! a stop is found in its cell and a cell's breakpoints reach the kernel.

use super::*;
use crate::notebook::NotebookTab;
use accent_api::debug::{Attach, Placed};
use accent_api::kernel::Cells;
use accent_core::notebook::CellId;
use std::rc::Weak;

/// What a Debug Cell session debugs: a cell of a notebook, its run on the notebook's kernel, and
/// how the kernel names the cells' files once the debugger is attached.
pub(super) struct CellRun {
    notebook: Weak<NotebookTab>,
    cell: CellId,
    run: u64,
    cells: RefCell<Option<Cells>>,
}

/// Where Call Stack says a cell's call is, in the place of the file the kernel ran the cell as:
/// the notebook and the cell's place, `run.ipynb cell 2`, which the window finds the cell by.
fn place(key: &str, number: usize) -> String {
    format!("{key} cell {number}")
}

impl App {
    /// Why `tab`'s cells cannot be debugged here, `None` when they can: a Jupyter notebook of
    /// Python's in a vault (user decision 2026-11-19).
    pub(crate) fn why_not_debug(&self, tab: &NotebookTab) -> Option<&'static str> {
        let python =
            || (!tab.is_python()).then_some("only a Python notebook's cells can be debugged");
        self.why_not_run(tab).or_else(python)
    }

    /// The Code pane's notebook, where Debug Cell debugs its selected cell.
    pub(crate) fn code_notebook(&self) -> Option<Rc<NotebookTab>> {
        let tab = self.code_doc()?.notebook().cloned()?;
        let reads = self.previews_notebook();
        (!reads && self.why_not_debug(&tab).is_none()).then_some(tab)
    }

    /// Debug Cell: the selected code cell.
    pub(crate) fn debug_cell(self: &Rc<Self>, tab: &Rc<NotebookTab>) {
        match tab.selected_code() {
            Some(id) => self.debug_the_cell(tab, id),
            None => self.debug_cannot("debug the cell", "no code cell is selected"),
        }
    }

    /// Run cell `id` under the debugger: its kernel started first if need be, its run held until
    /// the debugger is attached, refused while the kernel is busy (user decision 2026-11-19).
    fn debug_the_cell(self: &Rc<Self>, tab: &Rc<NotebookTab>, id: CellId) {
        self.debugger.again.take();
        let why = self
            .why_not_debug(tab)
            .or_else(|| tab.presented().then_some("it is being presented"))
            .or_else(|| (self.debugger.running()).then_some("a program is already being debugged"))
            .or_else(|| tab.kernel_busy().then_some("the kernel is busy"));
        if let Some(why) = why {
            return self.debug_cannot("debug the cell", why);
        }
        let Some(code) = tab.code_of(id) else {
            return self.debug_cannot("debug the cell", "it has no code");
        };
        let Some(held) = self.hold_run(tab, id, &code) else {
            return;
        };
        let session = self.debugger.next.get() + 1;
        self.debugger.next.set(session);
        let run = Rc::new(Run {
            id: session,
            program: tab.key(),
            language: "python".to_string(),
            cell: Some(CellRun {
                notebook: Rc::downgrade(tab),
                cell: id,
                run: held,
                cells: RefCell::default(),
            }),
            ..Run::default()
        });
        self.debugger.run.replace(Some(run.clone()));
        self.sync_debug();
        self.sync_code_pane();
        let weak = Rc::downgrade(self);
        self.kernel_ready(tab, move |started| {
            if let Some(app) = weak.upgrade() {
                app.attach_cell(&run, started);
            }
        });
    }

    /// Attach the debugger to the started kernel, stopping at the vault's files' breakpoints and
    /// the cells', then send the cell's run. One stopped meanwhile sends nothing.
    fn attach_cell(self: &Rc<Self>, run: &Rc<Run>, started: bool) {
        let Some(cell) = &run.cell else {
            return;
        };
        let tab = cell.notebook.upgrade();
        let vault = self.vault().cloned();
        let session = tab.as_ref().and_then(|tab| tab.kernel_session());
        let (Some(tab), Some(vault), Some(session)) = (tab, vault, session) else {
            return self.debug_ended(run.id);
        };
        if !started || run.state.get() == State::Stopping {
            self.drop_run(&tab, cell.run);
            return self.debug_ended(run.id);
        }
        let attach = Attach {
            kernel: session,
            breakpoints: self.debugger.saved(self).into_iter().collect(),
            exceptions: self.debugger.exceptions(),
        };
        let attached = vault.debug_kernel(run.id, &attach);
        let (weak, run) = (Rc::downgrade(self), run.clone());
        glib::spawn_future_local(async move {
            let attached = attached.await;
            let (Some(app), Some(cell)) = (weak.upgrade(), run.cell.as_ref()) else {
                return;
            };
            let Some(tab) = cell.notebook.upgrade() else {
                return app.send_stop(&run);
            };
            let cells = match attached {
                Ok(cells) => cells,
                Err(e) => {
                    if run.state.get() != State::Stopping {
                        app.debug_cannot("debug the cell", e);
                    }
                    app.drop_run(&tab, cell.run);
                    return app.debug_ended(run.id);
                }
            };
            cell.cells.replace(Some(cells));
            // Each cell the kernel ran with breakpoints, this one among them, before it runs.
            for (id, _) in tab.cell_breakpoints() {
                if let Err(e) = app.send_cell(&tab, id).await {
                    app.debug_cannot("debug the cell", e);
                    app.drop_run(&tab, cell.run);
                    return app.stop_debugging();
                }
            }
            if run.state.get() != State::Starting {
                app.drop_run(&tab, cell.run);
                return app.send_stop(&run);
            }
            run.state.set(State::Running);
            app.sync_debug();
            let number = tab.cell_number(cell.cell).unwrap_or(1);
            let title = format!("{}, cell {number}", doc::file_name(&tab.key()));
            app.open_console(run.id, &title);
            app.send_held(&tab, cell.run);
        });
    }

    /// Tell the session cell `id`'s breakpoints, all of them, in the file the kernel ran its code
    /// as, and mark them where it holds them; nothing for a cell it never ran.
    async fn send_cell(self: &Rc<Self>, tab: &Rc<NotebookTab>, id: CellId) -> Result<(), String> {
        let (Some(run), Some(vault)) = (self.debugger.run(), self.vault().cloned()) else {
            return Ok(());
        };
        let sent = tab.sent_code().into_iter().find(|(cell, _)| *cell == id);
        let Some((_, code)) = sent.filter(|_| run.cell.is_some()) else {
            return Ok(());
        };
        let path = vault.debug_dump(run.id, code).await;
        let path = path.map_err(|e| e.to_string())?;
        let set = tab.breakpoints_of(id);
        let placed = vault.debug_breakpoints(run.id, path, set).await;
        let placed = placed.map_err(|e| e.to_string())?;
        if self
            .debugger
            .run()
            .is_some_and(|now| Rc::ptr_eq(&now, &run))
        {
            tab.bind_breakpoints(id, &placed);
        }
        Ok(())
    }

    /// A breakpoint on 0-based `line` of `tab`'s cell `id`, or none there any more; a session
    /// debugging the notebook's cells is told.
    pub(crate) fn toggle_cell_breakpoint(
        self: &Rc<Self>,
        tab: &Rc<NotebookTab>,
        id: CellId,
        line: u32,
    ) {
        tab.toggle_breakpoint(id, line);
        self.cell_breakpoints_changed(tab, id);
    }

    /// Cell `id`'s breakpoints changed: the session debugging its notebook is told, once it runs.
    pub(crate) fn cell_breakpoints_changed(self: &Rc<Self>, tab: &Rc<NotebookTab>, id: CellId) {
        let ours = self.debugger.run().is_some_and(|run| {
            run.started()
                && (run.cell.as_ref()).is_some_and(|c| c.notebook.ptr_eq(&Rc::downgrade(tab)))
        });
        if ours {
            let (app, tab) = (self.clone(), tab.clone());
            glib::spawn_future_local(async move {
                if let Err(e) = app.send_cell(&tab, id).await {
                    tracing::warn!("setting a cell's breakpoints: {e}");
                }
            });
        }
        self.refill_breakpoints();
    }

    /// Debug Cell's run `done` is done: the debugger lets the kernel go, and a cell to debug
    /// again once it was is debugged.
    pub(crate) fn debugged_run_done(self: &Rc<Self>, tab: &Rc<NotebookTab>, done: u64) {
        let ours = |run: &Rc<Run>| run.cell.as_ref().is_some_and(|c| c.run == done);
        if let Some(run) = self.debugger.run().filter(ours)
            && run.state.get() != State::Stopping
        {
            run.state.set(State::Stopping);
            self.moved_on(&run);
            self.sync_debug();
            self.send_stop(&run);
        }
        let again = self.debugger.again.take();
        if let Some((notebook, id)) = again
            && notebook
                .upgrade()
                .is_some_and(|again| Rc::ptr_eq(&again, tab))
        {
            self.debug_the_cell(tab, id);
        }
    }

    /// Stop Debugging a cell (user decision 2026-11-19): the debugger lets go, then the kernel is
    /// interrupted while the cell's run is still out, which ends it with `KeyboardInterrupt`, the
    /// kernel and what it holds kept. In that order: a SIGINT while debugpy holds the cell at a
    /// breakpoint leaves the kernel answering nothing.
    pub(super) fn stop_cell(self: &Rc<Self>, run: &Rc<Run>, sent: bool) {
        let (Some(vault), Some(cell)) = (self.vault().cloned(), run.cell.as_ref()) else {
            return;
        };
        let stopped = vault.debug_stop(run.id);
        let (tab, held) = (cell.notebook.clone(), cell.run);
        glib::spawn_future_local(async move {
            if let Err(e) = stopped.await {
                tracing::warn!("stopping the debug session: {e}");
            }
            let tab = tab.upgrade().filter(|tab| sent && tab.runs_debugged(held));
            if let Some(session) = tab.and_then(|tab| tab.kernel_session())
                && let Err(e) = vault.kernel_interrupt(session).await
            {
                tracing::warn!("cannot interrupt the kernel: {e}");
            }
        });
    }

    /// Debug the cell again once its run is done, Restart Debugging's second half.
    pub(super) fn debug_again(self: &Rc<Self>, cell: &CellRun) {
        let Some(tab) = cell.notebook.upgrade() else {
            return;
        };
        match tab.kernel_busy() {
            true => {
                let again = (cell.notebook.clone(), cell.cell);
                self.debugger.again.replace(Some(again));
            }
            false => self.debug_the_cell(&tab, cell.cell),
        }
    }

    /// The notebook cell the kernel ran as `path`, by the code each cell last ran.
    fn cell_of(&self, run: &Run, path: &str) -> Option<(Rc<NotebookTab>, CellId)> {
        let cell = run.cell.as_ref()?;
        let tab = cell.notebook.upgrade()?;
        let cells = cell.cells.borrow().clone()?;
        let sent = tab.sent_code().into_iter();
        let id = sent
            .into_iter()
            .find(|(_, code)| cells.path(code) == path)?
            .0;
        Some((tab, id))
    }

    /// `frames` as Call Stack shows them: a cell's call at its cell's place.
    pub(super) fn cell_frames(&self, run: &Run, frames: Vec<Frame>) -> Vec<Frame> {
        frames
            .into_iter()
            .map(|mut frame| {
                let found = frame.path.as_deref().and_then(|p| self.cell_of(run, p));
                if let Some((tab, id)) = found {
                    let number = tab.cell_number(id).unwrap_or(1);
                    frame.path = Some(place(&tab.key(), number));
                }
                frame
            })
            .collect()
    }

    /// Show `frame` in its cell, where it is one of the debugged notebook's: the notebook to the
    /// front of its pane, the caret on the line with the keyboard in the cell as a stop in a file
    /// takes it, and the line marked. False for any other frame.
    pub(super) fn show_cell_frame(self: &Rc<Self>, run: &Run, frame: &Frame) -> bool {
        let Some(cell) = &run.cell else {
            return false;
        };
        let Some((key, number)) = frame.path.as_deref().and_then(|p| p.rsplit_once(" cell "))
        else {
            return false;
        };
        let tab = cell.notebook.upgrade().filter(|tab| tab.key() == key);
        let number = number.parse::<usize>().ok();
        let (Some(tab), Some(number)) = (tab, number) else {
            return false;
        };
        let Some(id) = tab.cell_at(number) else {
            return false;
        };
        if let Some(pane) = self.pane_of(&tab.page) {
            pane.tabs.set_selected_page(&tab.page);
        }
        let (line, column) = (frame.pos.line as i32 + 1, frame.pos.character as i32 + 1);
        tab.goto_place(number - 1, line, column);
        if let Some(marks) = tab.pause_at(id, frame.pos.line) {
            self.debugger.paused_cell.replace(Rc::downgrade(&marks));
        }
        true
    }

    /// Where the session holds the breakpoints of a cell's file: `false` where `path` is no cell's.
    pub(super) fn bind_cell(&self, run: &Run, path: &str, placed: &[Placed]) -> bool {
        let Some((tab, id)) = self.cell_of(run, path) else {
            return false;
        };
        tab.bind_breakpoints(id, placed);
        true
    }
}
