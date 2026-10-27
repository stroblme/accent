//! Running a Jupyter notebook (`Vault::kernel_*`): the tab's kernel, the runs it was sent and
//! what they leave in their cells, and the run commands. A run changes the model outside the
//! history, being the kernel's and not an edit, and the cells it changed are drawn once per turn
//! of the event pump; Clear Outputs is an edit.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};

use accent_api::kernel::{Info, News, Start, Status};
#[cfg(feature = "bench")]
use accent_core::notebook::output::Output;
use accent_core::notebook::{CellId, Edit, Format, Kind, Run};
use serde_json::Value;

use super::NotebookTab;
use super::cell::{Execution, Turn};
use crate::sidebar::KernelState;
use crate::*;

/// Kernel session numbers, unique in the process and so in any vault.
static SESSIONS: AtomicU64 = AtomicU64::new(0);

struct RunJob {
    cell: CellId,
    source: String,
}

fn after_run(last: Execution, status: Status, still_queued: bool) -> (Execution, Execution) {
    let last = match status {
        Status::Ok => Execution::Succeeded,
        Status::Error => Execution::Failed,
        Status::Aborted => last,
    };
    let shown = if still_queued {
        Execution::Running
    } else {
        last
    };
    (last, shown)
}

fn queued_for(kernel: Option<&Kernel>, id: CellId) -> bool {
    kernel.is_some_and(|k| k.runs.borrow().values().any(|job| job.cell == id))
}

pub(super) fn queued_for_source(kernel: Option<&Kernel>, id: CellId, source: &str) -> bool {
    kernel.is_some_and(|k| {
        k.runs
            .borrow()
            .values()
            .any(|job| job.cell == id && job.source == source)
    })
}

/// Where cell `id` stands with `kernel`: the first run not done is the one it is on, once it has
/// started; the rest wait.
fn turn_of(kernel: Option<&Kernel>, id: CellId) -> Turn {
    let Some(kernel) = kernel else {
        return Turn::Free;
    };
    let runs = kernel.runs.borrow();
    let on = runs.values().next().map(|job| job.cell);
    if on == Some(id) && kernel.info.borrow().is_some() {
        Turn::Running
    } else if runs.values().any(|job| job.cell == id) {
        Turn::Waiting
    } else {
        Turn::Free
    }
}

fn output_save_due(kernel: Option<&Kernel>) -> bool {
    kernel.is_none_or(|kernel| kernel.runs.borrow().is_empty())
}

#[cfg(test)]
mod tests {
    use super::super::cell::Execution;
    use super::*;

    #[test]
    fn queued_and_aborted_runs_keep_the_last_settled_result() {
        assert_eq!(Execution::Unrun.stale(), Execution::Unrun);
        assert_eq!(Execution::Succeeded.stale(), Execution::StaleSucceeded);
        assert_eq!(Execution::Failed.stale(), Execution::StaleFailed);
        let (last, shown) = after_run(Execution::Unrun, Status::Ok, true);
        assert_eq!((last, shown), (Execution::Succeeded, Execution::Running));
        assert_eq!(
            after_run(last, Status::Aborted, false),
            (Execution::Succeeded, Execution::Succeeded)
        );
        assert_eq!(
            after_run(Execution::Succeeded, Status::Error, false),
            (Execution::Failed, Execution::Failed)
        );
        assert_eq!(
            after_run(Execution::Succeeded, Status::Aborted, true),
            (Execution::Succeeded, Execution::Running)
        );
        assert_eq!(
            after_run(Execution::StaleFailed, Status::Aborted, false),
            (Execution::StaleFailed, Execution::StaleFailed)
        );
        assert_eq!(
            after_run(Execution::StaleSucceeded, Status::Ok, false),
            (Execution::Succeeded, Execution::Succeeded)
        );
    }

    #[test]
    fn a_queued_run_of_old_source_does_not_mark_the_edited_source_running() {
        let notebook =
            accent_core::notebook::read("cell.ipynb", accent_core::notebook::EMPTY_IPYNB).unwrap();
        let id = notebook.cells()[0].id();
        let kernel = Kernel {
            session: 1,
            info: RefCell::default(),
            runs: RefCell::default(),
            outbox: RefCell::default(),
            sending: Cell::new(false),
            clear_next: RefCell::default(),
            stale: RefCell::default(),
            last_run: Cell::new(0),
        };
        kernel.runs.borrow_mut().insert(
            1,
            RunJob {
                cell: id,
                source: "old".into(),
            },
        );
        assert!(!queued_for_source(Some(&kernel), id, "new"));
        kernel.runs.borrow_mut().insert(
            2,
            RunJob {
                cell: id,
                source: "new".into(),
            },
        );
        assert!(queued_for_source(Some(&kernel), id, "new"));
    }

    #[test]
    fn only_the_run_the_kernel_is_on_can_be_stopped_from_its_cell() {
        let notebook = accent_core::notebook::read("cells.ipynb", TWO_CELLS).unwrap();
        let (a, b) = (notebook.cells()[0].id(), notebook.cells()[1].id());
        let kernel = Kernel {
            session: 1,
            info: RefCell::default(),
            runs: RefCell::default(),
            outbox: RefCell::default(),
            sending: Cell::new(false),
            clear_next: RefCell::default(),
            stale: RefCell::default(),
            last_run: Cell::new(0),
        };
        assert_eq!(turn_of(None, a), Turn::Free);
        assert_eq!(turn_of(Some(&kernel), a), Turn::Free);
        for (run, cell) in [(1, a), (2, b)] {
            let source = String::new();
            kernel
                .runs
                .borrow_mut()
                .insert(run, RunJob { cell, source });
        }
        // A kernel still starting runs nothing yet: every cell waits for it.
        assert_eq!(turn_of(Some(&kernel), a), Turn::Waiting);
        kernel.info.replace(Some(Info {
            interpreter: "python3".into(),
            implementation: "ipython".into(),
            version: "9".into(),
        }));
        assert_eq!(turn_of(Some(&kernel), a), Turn::Running);
        assert_eq!(turn_of(Some(&kernel), b), Turn::Waiting);
        kernel.runs.borrow_mut().remove(&1);
        assert_eq!(turn_of(Some(&kernel), a), Turn::Free);
        assert_eq!(turn_of(Some(&kernel), b), Turn::Running);
    }

    /// Two empty code cells.
    const TWO_CELLS: &str = r#"{"cells": [
        {"cell_type": "code", "id": "a", "metadata": {}, "source": [], "outputs": [],
         "execution_count": null},
        {"cell_type": "code", "id": "b", "metadata": {}, "source": [], "outputs": [],
         "execution_count": null}
    ], "metadata": {}, "nbformat": 4, "nbformat_minor": 5}"#;

    #[test]
    fn stream_updates_save_only_after_the_run_finishes() {
        let notebook =
            accent_core::notebook::read("cell.ipynb", accent_core::notebook::EMPTY_IPYNB).unwrap();
        let kernel = Kernel {
            session: 1,
            info: RefCell::default(),
            runs: RefCell::default(),
            outbox: RefCell::default(),
            sending: Cell::new(false),
            clear_next: RefCell::default(),
            stale: RefCell::default(),
            last_run: Cell::new(0),
        };
        assert!(output_save_due(Some(&kernel)));
        kernel.runs.borrow_mut().insert(
            1,
            RunJob {
                cell: notebook.cells()[0].id(),
                source: String::new(),
            },
        );
        assert!(!output_save_due(Some(&kernel)));
        kernel.runs.borrow_mut().clear();
        assert!(output_save_due(Some(&kernel)));
    }
}

/// A tab's kernel, from its start until it ends.
pub(super) struct Kernel {
    /// The number the vault knows it by.
    session: u64,
    /// What it answered once started; `None` while it starts.
    info: RefCell<Option<Info>>,
    /// The runs not done yet, by number, and their cells.
    runs: RefCell<BTreeMap<u64, RunJob>>,
    /// Runs yet to be sent, with their code: held while it starts, then sent one at a time,
    /// each once the last has reached it, so it takes them in the order they were given.
    outbox: RefCell<VecDeque<(u64, String)>>,
    sending: Cell<bool>,
    /// Runs whose next output clears their cell first, as `clear_output(wait=True)` asks.
    clear_next: RefCell<HashSet<u64>>,
    /// Runs begun whose cell still shows the last run's outputs, dimmed, until their first
    /// output takes their place or they end with none, and the count each began as.
    stale: RefCell<HashMap<u64, Option<u32>>>,
    last_run: Cell<u64>,
}

/// A tab's kernel, if it has one, and what runs changed that is yet to be drawn.
#[derive(Default)]
pub(super) struct Hold {
    kernel: RefCell<Option<Rc<Kernel>>>,
    /// The cells runs changed, each from its first output changed.
    dirty: RefCell<Vec<(CellId, usize)>>,
    /// A redraw is due at the end of this turn.
    due: Cell<bool>,
}

impl NotebookTab {
    pub(super) fn set_execution(&self, id: CellId, state: Execution) {
        if let Some(view) = self.view_of(Some(id)) {
            view.execution.set(state);
            if self.tools.cell.get() == Some(id) {
                self.tools.show_execution(state);
            }
        }
    }

    fn settle_execution(&self, id: CellId, status: Status) {
        let Some(view) = self.view_of(Some(id)) else {
            return;
        };
        let still_queued = self.current_source_queued(id);
        let (last, shown) = after_run(view.last_result.get(), status, still_queued);
        view.last_result.set(last);
        self.set_execution(id, shown);
    }

    fn leave_execution(&self, id: CellId) {
        let Some(view) = self.view_of(Some(id)) else {
            return;
        };
        let still_queued = self.current_source_queued(id);
        let (_, shown) = after_run(view.last_result.get(), Status::Aborted, still_queued);
        self.set_execution(id, shown);
    }

    fn current_source_queued(&self, id: CellId) -> bool {
        self.sync_model();
        let model = self.model.borrow();
        let Some(at) = model.position(id) else {
            return false;
        };
        queued_for_source(self.kernel().as_deref(), id, model.cells()[at].source())
    }

    fn stale_execution(&self) {
        for view in self
            .views
            .borrow()
            .iter()
            .filter(|view| view.kind == Kind::Code)
        {
            let state = view.last_result.get().stale();
            view.last_result.set(state);
            self.set_execution(view.id, state);
        }
    }

    /// Whether a kernel runs for the tab, which then stays in its window.
    pub(crate) fn kernel_runs(&self) -> bool {
        self.kernel.kernel.borrow().is_some()
    }

    pub(super) fn kernel(&self) -> Option<Rc<Kernel>> {
        self.kernel.kernel.borrow().clone()
    }

    /// Cell `id`'s count as the model has it, or `[*]` while a run of it waits or runs; and the
    /// cell buttons' Run Cell or Stop, the run the kernel is on having changed with it.
    fn relabel(&self, id: CellId) {
        self.show_turn();
        let Some(view) = self.view_of(Some(id)) else {
            return;
        };
        let queued = queued_for(self.kernel().as_deref(), id);
        let model = self.model.borrow();
        match (queued, model.position(id)) {
            (true, _) => view.set_count("[*]"),
            (false, Some(at)) => view.set_count(&super::cell::count_label(&model.cells()[at])),
            (false, None) => {}
        }
    }

    /// Cell `id`'s outputs and count drawn afresh, after an edit that changed them.
    fn redraw_cell(&self, id: CellId) {
        let outputs = {
            let model = self.model.borrow();
            model
                .position(id)
                .map(|at| model.cells()[at].outputs().to_vec())
        };
        if let (Some(view), Some(outputs)) = (self.view_of(Some(id)), outputs) {
            view.outputs.show(0, &outputs);
        }
        self.relabel(id);
    }

    /// Undo or Redo made `edits`: the views keep their outputs otherwise (`reconcile`).
    pub(super) fn redraw_outputs(&self, edits: &[Edit]) {
        for edit in edits {
            if let Edit::Outputs { id, .. } = edit {
                self.redraw_cell(*id);
            }
        }
    }

    /// Clear Outputs, the selected cell's, or Clear All Outputs: one edit, which Undo walks back.
    pub(crate) fn clear_outputs(self: &Rc<Self>, all: bool) {
        self.sync_model();
        let ids: Vec<CellId> = {
            let model = self.model.borrow();
            let cleared = |c: &&accent_core::notebook::Cell| {
                (all || self.selected.get() == Some(c.id()))
                    && c.kind() == Kind::Code
                    && (c.count().is_some() || !c.outputs().is_empty())
            };
            model
                .cells()
                .iter()
                .filter(cleared)
                .map(|c| c.id())
                .collect()
        };
        let edits = ids.iter().map(|&id| Edit::Outputs {
            id,
            run: Some(Run::new(None, Vec::new())),
        });
        let edits: Vec<Edit> = edits.collect();
        if self.operate(|nb| Some(nb.apply_all(edits))) {
            for id in ids {
                self.redraw_cell(id);
            }
        }
    }

    /// Select the cell after `id` with the keyboard in it, adding a code cell past the last.
    fn advance(self: &Rc<Self>, id: CellId) {
        let next = {
            let model = self.model.borrow();
            let at = model.position(id);
            at.and_then(|at| model.cells().get(at + 1)).map(|c| c.id())
        };
        match next {
            Some(next) => self.focus_cell(next),
            None => self.insert(Kind::Code, true),
        }
    }

    /// Cell `id`'s outputs go and it counts `count`: as a run begins, and as it clears them.
    fn empty_run(self: &Rc<Self>, id: CellId, count: Option<u32>) {
        let run = Some(Run::new(count, Vec::new()));
        if self
            .model
            .borrow_mut()
            .apply(Edit::Outputs { id, run })
            .is_some()
        {
            self.ran(id, 0);
        }
    }

    fn count_of(&self, id: CellId) -> Option<u32> {
        let model = self.model.borrow();
        model.position(id).and_then(|at| model.cells()[at].count())
    }

    /// One more output of cell `id`'s run, or more of its last stream.
    fn add_output(self: &Rc<Self>, id: CellId, output: Value, display_id: Option<String>) {
        let shown = {
            let mut model = self.model.borrow_mut();
            model.push_output(id, output, display_id).and_then(|_| {
                let at = model.position(id)?;
                Some(model.cells()[at].outputs().len())
            })
        };
        if let Some(shown) = shown {
            self.ran(id, shown.saturating_sub(1));
        }
    }

    /// The run of cell `id` is done, counted `count`.
    fn finish_run(self: &Rc<Self>, id: CellId, count: Option<u32>) {
        if self.model.borrow_mut().set_count(id, count).is_some() {
            self.changed_soon();
            self.save_soon();
        }
    }

    /// Cell `id`'s outputs from the `from`th on changed: drawn at the end of this turn.
    fn ran(self: &Rc<Self>, id: CellId, from: usize) {
        {
            let mut dirty = self.kernel.dirty.borrow_mut();
            match dirty.iter_mut().find(|(cell, _)| *cell == id) {
                Some((_, first)) => *first = (*first).min(from),
                None => dirty.push((id, from)),
            }
        }
        self.changed_soon();
    }

    /// Draw what runs changed and mark the notebook changed, once per turn of the event pump:
    /// a stream flushes every 0.2 s, and a turn may carry many outputs.
    fn changed_soon(self: &Rc<Self>) {
        if self.kernel.due.replace(true) {
            return;
        }
        let weak = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            let Some(tab) = weak.upgrade() else { return };
            tab.kernel.due.set(false);
            for (id, from) in tab.kernel.dirty.take() {
                if let Some(view) = tab.view_of(Some(id)) {
                    let model = tab.model.borrow();
                    if let Some(at) = model.position(id) {
                        let outputs = model.cells()[at].outputs();
                        view.outputs
                            .show(from, outputs.get(from..).unwrap_or_default());
                    }
                    // Dimmed until what took the last run's place is drawn, in the same frame.
                    view.outputs.set_stale(tab.shows_stale(id));
                }
            }
            tab.mark_changed();
            if output_save_due(tab.kernel().as_deref()) {
                tab.save_soon();
            }
        });
    }

    /// Whether cell `id` still shows the outputs of the run before the one it is in.
    fn shows_stale(&self, id: CellId) -> bool {
        self.kernel().is_some_and(|kernel| {
            let runs = kernel.runs.borrow();
            let stale = kernel.stale.borrow();
            stale
                .keys()
                .any(|run| runs.get(run).is_some_and(|job| job.cell == id))
        })
    }

    fn end_ask(&self, id: CellId) {
        if let Some(view) = self.view_of(Some(id)) {
            view.end_ask();
        }
    }

    /// The kernel is over: its runs end where they are, each cell counted as the model has it,
    /// and one still showing the last run's outputs has none.
    fn kernel_over(self: &Rc<Self>) {
        let Some(kernel) = self.kernel.kernel.take() else {
            return;
        };
        self.stale_execution();
        kernel.outbox.take();
        let stale = kernel.stale.take();
        for (run, RunJob { cell: id, .. }) in kernel.runs.take() {
            if let Some(&count) = stale.get(&run) {
                self.empty_run(id, count);
            }
            self.end_ask(id);
            self.relabel(id);
        }
    }

    /// Where cell `id` stands with the kernel.
    pub(super) fn turn(&self, id: CellId) -> Turn {
        turn_of(self.kernel().as_deref(), id)
    }

    /// The cell buttons' Run Cell, or Stop, as the cell they serve stands with the kernel.
    fn show_turn(&self) {
        if let Some(id) = self.tools.cell.get() {
            self.tools.show_turn(self.turn(id));
        }
    }

    /// The kernel as the Code pane's strip shows it.
    fn kernel_state(&self) -> KernelState {
        let Some(kernel) = self.kernel() else {
            return KernelState::Off;
        };
        let info = kernel.info.borrow();
        let Some(info) = info.as_ref() else {
            return KernelState::Starting;
        };
        let what = format!(
            "{} {} in {}",
            info.implementation, info.version, info.interpreter
        );
        let busy = !kernel.runs.borrow().is_empty();
        KernelState::Up { what, busy }
    }

    /// Bring `widget`, just added, into view once it is laid out, a margin inside, as a caret is
    /// kept: the viewport scrolls to nothing on its own (`open`).
    fn bring_into_view(self: &Rc<Self>, widget: &gtk::Widget) {
        let weak = Rc::downgrade(self);
        widget.add_tick_callback(move |widget, _| {
            let Some(tab) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            // Not laid out yet: the frame it was added in lays it out after its ticks.
            if widget.height() == 0 && widget.is_mapped() {
                return glib::ControlFlow::Continue;
            }
            let bounds = tab.content().and_then(|c| widget.compute_bounds(&c));
            if let Some(bounds) = bounds {
                let adjustment = tab.scroller.vadjustment();
                let (top, bottom) = (
                    f64::from(bounds.y()),
                    f64::from(bounds.y() + bounds.height()),
                );
                let margin = 24.0;
                if top < adjustment.value() {
                    adjustment.set_value(top - margin);
                } else if bottom > adjustment.value() + adjustment.page_size() {
                    adjustment.set_value(bottom - adjustment.page_size() + margin);
                }
            }
            glib::ControlFlow::Break
        });
    }
}

impl App {
    /// Why `tab` runs nothing where it is, `None` when it can run: a Jupyter notebook in a vault.
    pub(crate) fn why_not_run(&self, tab: &NotebookTab) -> Option<&'static str> {
        if self.vault().is_none() || doc::is_loose_key(&tab.key()) {
            return Some("it is not in a vault");
        }
        if tab.model.borrow().format() != Format::Ipynb {
            return Some("marimo runs its own notebooks");
        }
        None
    }

    /// `tab`'s cells may run now, else a toast says why not.
    fn may_run(&self, tab: &NotebookTab) -> bool {
        let why = match tab.presenting.get() {
            true => Some("it is being presented"),
            false => self.why_not_run(tab),
        };
        if let Some(why) = why {
            self.kernel_toast(&format!("Cannot run the notebook: {why}"));
        }
        why.is_none()
    }

    /// Run Cell, or Run Cell and Advance (`advance`): the selected cell's code run, then the
    /// next cell selected, a code cell added past the last; a markdown cell only advances.
    pub(crate) fn run_selected(self: &Rc<Self>, tab: &Rc<NotebookTab>, advance: bool) {
        let Some(id) = tab.selected.get() else {
            return;
        };
        if !self.may_run(tab) {
            return;
        }
        self.run_cells(tab, &[id]);
        if advance {
            tab.advance(id);
        }
    }

    /// Run All: every code cell in order, an error stopping the rest.
    pub(crate) fn run_all(self: &Rc<Self>, tab: &Rc<NotebookTab>) {
        if !self.may_run(tab) {
            return;
        }
        let ids: Vec<CellId> = tab.model.borrow().cells().iter().map(|c| c.id()).collect();
        self.run_cells(tab, &ids);
    }

    /// Send the code cells among `ids` to the tab's kernel, started first if need be; the kernel
    /// takes them in turn. A cell with no code is not run, as in JupyterLab.
    fn run_cells(self: &Rc<Self>, tab: &Rc<NotebookTab>, ids: &[CellId]) {
        let Some(vault) = self.vault().cloned() else {
            return;
        };
        tab.sync_model();
        let codes: Vec<(CellId, String)> = {
            let model = tab.model.borrow();
            let cells = ids
                .iter()
                .filter_map(|&id| Some(&model.cells()[model.position(id)?]));
            cells
                .filter(|c| c.kind() == Kind::Code && !c.source().trim().is_empty())
                .map(|c| (c.id(), c.source().to_string()))
                .collect()
        };
        if codes.is_empty() {
            return;
        }
        let kernel = tab
            .kernel()
            .unwrap_or_else(|| self.start_kernel(tab, &vault));
        for (id, code) in codes {
            let run = kernel.last_run.get() + 1;
            kernel.last_run.set(run);
            kernel.runs.borrow_mut().insert(
                run,
                RunJob {
                    cell: id,
                    source: code.clone(),
                },
            );
            kernel.outbox.borrow_mut().push_back((run, code));
            tab.set_execution(id, Execution::Running);
            tab.relabel(id);
        }
        self.send_runs(tab, &kernel);
        self.sync_kernel();
    }

    /// Start a kernel for `tab`; the runs it is given meanwhile wait for its answer.
    fn start_kernel(self: &Rc<Self>, tab: &Rc<NotebookTab>, vault: &Vault) -> Rc<Kernel> {
        tab.stale_execution();
        let kernel = Rc::new(Kernel {
            session: SESSIONS.fetch_add(1, Ordering::Relaxed) + 1,
            info: RefCell::default(),
            runs: RefCell::default(),
            outbox: RefCell::default(),
            sending: Cell::new(false),
            clear_next: RefCell::default(),
            stale: RefCell::default(),
            last_run: Cell::new(0),
        });
        tab.kernel.kernel.replace(Some(kernel.clone()));
        let start = Start {
            notebook: tab.key(),
        };
        let started = vault.kernel_start(kernel.session, &start);
        let (app, weak) = (Rc::downgrade(self), Rc::downgrade(tab));
        let starting = kernel.clone();
        glib::spawn_future_local(async move {
            let started = started.await;
            let Some(app) = app.upgrade() else { return };
            let kernel = starting;
            let tab = weak
                .upgrade()
                .filter(|tab| tab.kernel().is_some_and(|k| Rc::ptr_eq(&k, &kernel)));
            match (started, tab) {
                (Ok(info), Some(tab)) => {
                    kernel.info.replace(Some(info));
                    // Its first run is the one it is on now.
                    tab.show_turn();
                    app.send_runs(&tab, &kernel);
                    app.sync_kernel();
                }
                // Shut down, restarted or closed while it started.
                (Ok(_), None) => app.send_kernel_stop(kernel.session),
                (Err(e), Some(tab)) => {
                    app.kernel_toast(&format!("Cannot start the kernel: {e}"));
                    tab.kernel_over();
                    app.sync_kernel();
                }
                (Err(_), None) => {}
            }
        });
        kernel
    }

    /// Send the kernel's outbox, one run at a time, once it has started: overlapping sends may
    /// reach it in either order. A run that cannot be sent is over unrun.
    fn send_runs(self: &Rc<Self>, tab: &Rc<NotebookTab>, kernel: &Rc<Kernel>) {
        let Some(vault) = self.vault().cloned() else {
            return;
        };
        if kernel.info.borrow().is_none() || kernel.sending.replace(true) {
            return;
        }
        let (app, tab, kernel) = (Rc::downgrade(self), Rc::downgrade(tab), kernel.clone());
        glib::spawn_future_local(async move {
            loop {
                let next = kernel.outbox.borrow_mut().pop_front();
                let Some((run, code)) = next else { break };
                let Err(e) = vault.kernel_execute(kernel.session, run, code).await else {
                    continue;
                };
                let (Some(app), Some(tab)) = (app.upgrade(), tab.upgrade()) else {
                    break;
                };
                // A kernel that ended meanwhile has said so already.
                if !tab.kernel().is_some_and(|k| Rc::ptr_eq(&k, &kernel)) {
                    break;
                }
                let unsent = kernel.runs.borrow_mut().remove(&run);
                if let Some(RunJob { cell: id, .. }) = unsent {
                    tab.leave_execution(id);
                    tab.relabel(id);
                }
                app.kernel_toast(&format!("Cannot run the cell: {e}"));
                app.sync_kernel();
            }
            kernel.sending.set(false);
        });
    }

    /// Interrupt Kernel: SIGINT to what it runs, as Jupyter sends it.
    pub(crate) fn interrupt_kernel(&self, tab: &NotebookTab) {
        let (Some(vault), Some(kernel)) = (self.vault(), tab.kernel()) else {
            return;
        };
        let sent = vault.kernel_interrupt(kernel.session);
        glib::spawn_future_local(async move {
            if let Err(e) = sent.await {
                tracing::warn!("cannot interrupt the kernel: {e}");
            }
        });
    }

    /// Restart Kernel: this one ended and another started, the cells keeping what they hold.
    pub(crate) fn restart_kernel(self: &Rc<Self>, tab: &Rc<NotebookTab>) {
        let Some(vault) = self.vault().cloned().filter(|_| tab.kernel_runs()) else {
            return;
        };
        self.stop_kernel(tab);
        self.start_kernel(tab, &vault);
        self.sync_kernel();
    }

    /// Shut Down Kernel; the next run starts another.
    pub(crate) fn shut_down_kernel(&self, tab: &Rc<NotebookTab>) {
        self.stop_kernel(tab);
        self.sync_kernel();
    }

    /// End `tab`'s kernel, if it has one: Shut Down, Restart and the tab closing.
    pub(crate) fn stop_kernel(&self, tab: &Rc<NotebookTab>) {
        if let Some(kernel) = tab.kernel() {
            tab.kernel_over();
            tab.save_soon();
            self.send_kernel_stop(kernel.session);
        }
    }

    fn send_kernel_stop(&self, session: u64) {
        let Some(vault) = self.vault() else {
            return;
        };
        let stopped = vault.kernel_stop(session);
        glib::spawn_future_local(async move {
            if let Err(e) = stopped.await {
                tracing::warn!("stopping the kernel: {e}");
            }
        });
    }

    /// The vault's link dropped, and every kernel on its host with it: nothing is left to tell.
    pub(crate) fn kernels_lost(&self) {
        for tab in self.notebooks() {
            tab.kernel_over();
            tab.save_soon();
        }
        self.sync_kernel();
    }

    /// What a kernel says on its own; an ended kernel's news is dropped.
    pub(crate) fn on_kernel(self: &Rc<Self>, session: u64, news: News) {
        let found = self.notebooks().into_iter().find_map(|tab| {
            let kernel = tab.kernel().filter(|k| k.session == session)?;
            Some((tab, kernel))
        });
        let Some((tab, kernel)) = found else {
            return;
        };
        let cell = |run: u64| kernel.runs.borrow().get(&run).map(|job| job.cell);
        match news {
            // The last run's outputs stay, dimmed, for the new ones to be held against.
            News::Started { run, count } => {
                if let Some(id) = cell(run) {
                    kernel.stale.borrow_mut().insert(run, count);
                    if let Some(view) = tab.view_of(Some(id)) {
                        view.outputs.set_stale(true);
                    }
                }
            }
            News::Output {
                run,
                output,
                display_id,
            } => {
                if let Some(id) = cell(run) {
                    let stale = kernel.stale.borrow_mut().remove(&run);
                    let wait = kernel.clear_next.borrow_mut().remove(&run);
                    match (stale, wait) {
                        (Some(count), _) => tab.empty_run(id, count),
                        (None, true) => tab.empty_run(id, tab.count_of(id)),
                        (None, false) => {}
                    }
                    tab.add_output(id, output, display_id);
                }
            }
            News::Update { display_id, output } => {
                let changed = tab.model.borrow_mut().update_display(&display_id, output);
                for (id, from) in changed {
                    tab.ran(id, from);
                }
            }
            News::Clear { run, wait: true } => {
                kernel.clear_next.borrow_mut().insert(run);
            }
            News::Clear { run, wait: false } => {
                if let Some(id) = cell(run) {
                    let stale = kernel.stale.borrow_mut().remove(&run);
                    tab.empty_run(id, stale.unwrap_or_else(|| tab.count_of(id)));
                }
            }
            News::Input {
                run,
                prompt,
                password,
            } => {
                if let Some(id) = cell(run) {
                    self.ask(&tab, &kernel, id, &prompt, password);
                }
            }
            News::Done { run, status, count } => {
                kernel.clear_next.borrow_mut().remove(&run);
                let stale = kernel.stale.borrow_mut().remove(&run);
                let Some(RunJob { cell: id, source }) = kernel.runs.borrow_mut().remove(&run)
                else {
                    return;
                };
                // Over with no output: nothing takes the last run's place.
                if let Some(started) = stale {
                    tab.empty_run(id, started);
                }
                // An aborted run never started, and keeps what its cell had.
                if status != Status::Aborted {
                    tab.finish_run(id, count);
                }
                tab.sync_model();
                let same_source = {
                    let model = tab.model.borrow();
                    model
                        .position(id)
                        .is_some_and(|at| model.cells()[at].source() == source)
                };
                if same_source {
                    tab.settle_execution(id, status);
                } else {
                    tab.leave_execution(id);
                }
                tab.end_ask(id);
                tab.relabel(id);
                self.sync_kernel();
            }
            News::Ended { error } => {
                tab.kernel_over();
                tab.save_soon();
                if let Some(why) = error {
                    self.kernel_toast(&format!("The kernel ended: {why}"));
                }
                self.sync_kernel();
            }
        }
    }

    /// `input()` asks under cell `id`, the keyboard going to it in the notebook in front.
    fn ask(
        self: &Rc<Self>,
        tab: &Rc<NotebookTab>,
        kernel: &Kernel,
        id: CellId,
        prompt: &str,
        password: bool,
    ) {
        let (Some(view), Some(vault)) = (tab.view_of(Some(id)), self.vault().cloned()) else {
            return;
        };
        let (app, session) = (Rc::downgrade(self), kernel.session);
        let entry = view.ask(prompt, password, move |value| {
            let answered = vault.kernel_input(session, value);
            let app = app.clone();
            glib::spawn_future_local(async move {
                if let Err(e) = answered.await
                    && let Some(app) = app.upgrade()
                {
                    app.kernel_toast(&format!("Cannot answer the input: {e}"));
                }
            });
        });
        if self.active_notebook().is_some_and(|n| Rc::ptr_eq(&n, tab)) {
            entry.grab_focus();
            tab.bring_into_view(&entry);
        }
    }

    /// The kernel commands as the Code pane's notebook's kernel allows them, and the pane's
    /// strip showing what it does, unless a debug session holds the pane.
    pub(crate) fn sync_kernel(&self) {
        let tab = self.code_doc().and_then(|doc| doc.notebook().cloned());
        let kernel = tab.as_ref().and_then(|tab| tab.kernel());
        let started = kernel.as_ref().is_some_and(|k| k.info.borrow().is_some());
        let busy = started && kernel.as_ref().is_some_and(|k| !k.runs.borrow().is_empty());
        for (action, on) in [
            ("notebook-interrupt", busy),
            ("notebook-restart", started),
            ("notebook-shut-down", kernel.is_some()),
        ] {
            if let Some(action) = self
                .window
                .lookup_action(action)
                .and_downcast::<gio::SimpleAction>()
            {
                action.set_enabled(on);
            }
        }
        if let Some(sidebar) = self.sidebar.get() {
            let shown = tab
                .filter(|_| !self.debugger.running())
                .map(|tab| tab.kernel_state());
            sidebar.set_kernel(shown.as_ref());
        }
    }

    fn kernel_toast(&self, text: &str) {
        self.add_toast(Toast::new(text).key("kernel"));
    }
}

/// What `ACCENT_BENCH_KERNEL` reads.
#[cfg(feature = "bench")]
impl NotebookTab {
    pub fn bench_execution(&self, n: usize) -> Option<String> {
        let view = self.views.borrow().get(n).cloned()?;
        Some(format!("{:?}", view.execution.get()))
    }

    /// What cell `n`'s run button offers with the cell buttons in its rail, as under the pointer:
    /// `run`, `run-greyed` or `stop`. The buttons go back where they were.
    pub fn bench_turn(&self, n: usize) -> &'static str {
        let Some(view) = self.views.borrow().get(n).cloned() else {
            return "missing";
        };
        self.tools.place(Some(&view), self.turn(view.id));
        let (run, stop) = (&self.tools.run, &self.tools.stop);
        let offered = match (stop.get_visible(), run.get_visible(), run.is_sensitive()) {
            (true, ..) => "stop",
            (false, true, true) => "run",
            (false, true, false) => "run-greyed",
            (false, false, _) => "none",
        };
        self.place();
        offered
    }

    /// Cell `n`'s count label, its outputs as the model holds them, and how many its view shows.
    pub fn bench_cell(&self, n: usize) -> Option<(String, Vec<Output>, usize)> {
        let view = self.views.borrow().get(n).cloned()?;
        let model = self.model.borrow();
        let at = model.position(view.id)?;
        let outputs = model.cells()[at].outputs().to_vec();
        Some((view.count_text(), outputs, view.outputs.shown()))
    }

    /// Whether cell `n`'s outputs are dimmed as the last run's.
    pub fn bench_stale(&self, n: usize) -> bool {
        self.views
            .borrow()
            .get(n)
            .is_some_and(|v| v.outputs.is_stale())
    }

    /// Whether a run waits or runs, or what one changed is still to be drawn: the drawing waits
    /// for an idle, which a busy loop runs after the drill's own timeouts.
    pub fn bench_busy(&self) -> bool {
        self.kernel().is_some_and(|k| !k.runs.borrow().is_empty()) || self.kernel.due.get()
    }

    /// The prompt `input()` shows, whether its entry hides what is typed, and the entry.
    pub fn bench_asking(&self) -> Option<(String, bool, gtk::Widget)> {
        self.views.borrow().iter().find_map(|v| v.asking())
    }

    /// How many of the cells' text views are allocated a height other than the one they ask for.
    pub fn bench_unfit(&self) -> usize {
        self.views
            .borrow()
            .iter()
            .flat_map(|v| v.text_views())
            .filter(|v| {
                v.width() > 0 && v.measure(gtk::Orientation::Vertical, v.width()).1 != v.height()
            })
            .count()
    }
}
