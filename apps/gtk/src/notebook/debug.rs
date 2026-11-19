//! A notebook's side of debugging its cells (`debug::cell`): a marks column on every code cell
//! of a notebook that can be debugged (user decision 2026-11-19), the breakpoints in it, kept per
//! cell while its editor is yet to be built, and the line a paused run stops on.

use std::collections::HashMap;
use std::rc::Rc;

use accent_api::debug::{Breakpoint, Evaluated, Placed};
use accent_core::notebook::{CellId, Kind};
use adw::prelude::*;
use sourceview5::prelude::{BufferExt as _, ViewExt as _};

use super::NotebookTab;
use super::cell::{self, Editor, Marks};
use crate::debug::breakpoints::{self, Gutter};
use crate::widgets::Hook;

/// What a notebook holds for its debugger.
#[derive(Default)]
pub(super) struct State {
    /// How wide the code cells' marks column is; 0 while the notebook cannot be debugged.
    marks: std::cell::Cell<i32>,
    /// The breakpoints of code cells whose editors are yet to be built.
    kept: std::cell::RefCell<HashMap<CellId, Vec<Breakpoint>>>,
    /// A click in a cell's marks column, on its 0-based line.
    on_breakpoint: Hook<Toggled>,
    /// What a hover asks the paused program, while its cells are debugged.
    on_evaluate: Hook<Evaluate>,
}

/// A breakpoint toggled on a 0-based line of a notebook's cell.
type Toggled = dyn Fn(&Rc<NotebookTab>, CellId, u32);

/// A hover's expression, asked of the program paused in a notebook's cells.
type Evaluate = dyn Fn(&Rc<NotebookTab>, String) -> Option<accent_api::Task<Evaluated>>;

impl State {
    pub(super) fn marks(&self) -> i32 {
        self.marks.get()
    }
}

impl NotebookTab {
    /// Give every code cell a marks column while the notebook can be debugged, and its markdown
    /// cells' text the same room, so the cells' text lines up.
    pub(crate) fn set_debuggable(&self, on: bool) {
        let width = match on {
            true => cell::marks_width(&self.scroller, self.zoom.get()),
            false => 0,
        };
        if self.debug.marks.replace(width) != width {
            for view in self.views.borrow().iter() {
                view.set_marks(width);
            }
        }
    }

    /// Whether its cells can be debugged, and so have a marks column.
    pub(crate) fn debuggable(&self) -> bool {
        self.debug.marks.get() > 0
    }

    /// The marks column measured again in the code font at the notebook's zoom.
    pub(super) fn remeasure_marks(&self) {
        if self.debug.marks.get() > 0 {
            self.set_debuggable(true);
        }
    }

    pub fn connect_breakpoint(&self, f: impl Fn(&Rc<NotebookTab>, CellId, u32) + 'static) {
        self.debug.on_breakpoint.set(Rc::new(f));
    }

    pub fn connect_evaluate(
        &self,
        f: impl Fn(&Rc<NotebookTab>, String) -> Option<accent_api::Task<Evaluated>> + 'static,
    ) {
        self.debug.on_evaluate.set(Rc::new(f));
    }

    /// What `expression` holds in the paused program, while its cells are debugged.
    pub(super) fn evaluate(
        self: &Rc<Self>,
        expression: String,
    ) -> Option<accent_api::Task<Evaluated>> {
        (self.debug.on_evaluate.get()?)(self, expression)
    }

    /// Wire code cell `id`'s editor, just built: a click in its marks column is a breakpoint, and
    /// those kept for it go on.
    pub(super) fn attach_marks(self: &Rc<Self>, id: CellId, editor: &Editor) {
        let Some(marks) = &editor.marks else {
            return;
        };
        let weak = Rc::downgrade(self);
        editor
            .view
            .connect_line_mark_activated(move |_, at, button, _, presses| {
                if button == gtk::gdk::BUTTON_PRIMARY
                    && presses == 1
                    && let Some(tab) = weak.upgrade()
                    && let Some(f) = tab.debug.on_breakpoint.get()
                {
                    f(&tab, id, at.line().max(0) as u32);
                }
            });
        if let Some(kept) = self.debug.kept.borrow_mut().remove(&id) {
            breakpoints::place(marks, &kept);
            breakpoints::redraw(marks);
        }
    }

    /// Cell `id`'s marks, its editor built for them.
    pub(crate) fn marks_of(self: &Rc<Self>, id: CellId) -> Option<Rc<Marks>> {
        self.editor(id)?.1.marks
    }

    /// Cell `id`'s breakpoints as they stand.
    pub(crate) fn breakpoints_of(&self, id: CellId) -> Vec<Breakpoint> {
        let view = self.view_of(Some(id));
        match view.as_ref().and_then(|v| v.editor()?.marks.clone()) {
            Some(marks) => breakpoints::saved(&marks),
            None => self
                .debug
                .kept
                .borrow()
                .get(&id)
                .cloned()
                .unwrap_or_default(),
        }
    }

    /// Every code cell with breakpoints, in the notebook's order, and its breakpoints.
    pub(crate) fn cell_breakpoints(&self) -> Vec<(CellId, Vec<Breakpoint>)> {
        let ids: Vec<CellId> = (self.views.borrow().iter())
            .filter(|v| v.kind == Kind::Code)
            .map(|v| v.id)
            .collect();
        ids.into_iter()
            .map(|id| (id, self.breakpoints_of(id)))
            .filter(|(_, set)| !set.is_empty())
            .collect()
    }

    /// Its cells' breakpoints as lines of its code cells joined (`Notebook::program`), as the
    /// vault's session keeps them (user decision 2026-11-19).
    pub(crate) fn program_breakpoints(&self) -> Vec<Breakpoint> {
        self.sync_model();
        let program = self.model.borrow().program();
        let mut all = Vec::new();
        for (id, set) in self.cell_breakpoints() {
            if let Some(lines) = program.lines(id) {
                let at = |b: Breakpoint| Breakpoint {
                    line: lines.start + b.line,
                    ..b
                };
                all.extend(set.into_iter().map(at));
            }
        }
        all
    }

    /// Put `set`, lines of its code cells joined, on its cells, in the place of what they hold; a
    /// line past the cells is dropped, as a file's past its end is.
    pub(crate) fn place_program_breakpoints(&self, set: &[Breakpoint]) {
        self.sync_model();
        let program = self.model.borrow().program();
        let mut cells: HashMap<CellId, Vec<Breakpoint>> = HashMap::new();
        for b in set {
            if let Some((id, line)) = program.cell_at(b.line) {
                let at = Breakpoint { line, ..b.clone() };
                cells.entry(id).or_default().push(at);
            }
        }
        let ids: Vec<CellId> = (self.views.borrow().iter())
            .filter(|v| v.kind == Kind::Code)
            .map(|v| v.id)
            .collect();
        for id in ids {
            self.place_breakpoints(id, cells.get(&id).map_or(&[][..], Vec::as_slice));
        }
    }

    /// The cell, from 1, and its line, from 0, that line `line` of its code cells joined is.
    pub(crate) fn program_place(&self, line: u32) -> Option<(usize, u32)> {
        let (id, at) = self.model.borrow().program().cell_at(line)?;
        Some((self.cell_number(id)?, at))
    }

    /// A breakpoint on 0-based `line` of cell `id`, or none there any more.
    pub(crate) fn toggle_breakpoint(self: &Rc<Self>, id: CellId, line: u32) {
        if let Some(marks) = self.marks_of(id) {
            breakpoints::toggle(&marks, line);
            breakpoints::redraw(&marks);
        }
    }

    /// Cell `id`'s breakpoints, all of them, now `set`.
    pub(crate) fn place_breakpoints(&self, id: CellId, set: &[Breakpoint]) {
        let view = self.view_of(Some(id));
        match view.as_ref().and_then(|v| v.editor()?.marks.clone()) {
            Some(marks) => {
                breakpoints::place(&marks, set);
                breakpoints::redraw(&marks);
            }
            None if set.is_empty() => drop(self.debug.kept.borrow_mut().remove(&id)),
            None => drop(self.debug.kept.borrow_mut().insert(id, set.to_vec())),
        }
    }

    /// Cell `id`'s marks where the debugger holds its breakpoints, a ring for one it did not bind.
    pub(crate) fn bind_breakpoints(self: &Rc<Self>, id: CellId, placed: &[Placed]) {
        if let Some(marks) = self.marks_of(id) {
            breakpoints::bind(&marks, placed);
            breakpoints::redraw(&marks);
        }
    }

    /// The session is over: every cell's breakpoints are dots again, which a next one is yet to
    /// bind.
    pub(crate) fn unbind_breakpoints(&self) {
        for view in self.views.borrow().iter() {
            if let Some(marks) = view.editor().and_then(|e| e.marks.clone()) {
                breakpoints::place(&marks, &breakpoints::saved(&marks));
                breakpoints::redraw(&marks);
            }
        }
    }

    /// Mark 0-based `line` of cell `id` as where its run is paused: the marks that hold it.
    pub(crate) fn pause_at(self: &Rc<Self>, id: CellId, line: u32) -> Option<Rc<Marks>> {
        let marks = self.marks_of(id)?;
        let start = marks.buffer().iter_at_line(line as i32)?;
        let mark = (marks.buffer()).create_source_mark(None, breakpoints::EXECUTION, &start);
        marks.execution.replace(Some(mark));
        breakpoints::redraw(&marks);
        Some(marks)
    }

    /// Cell `id`'s place in the notebook, from 1, as its title on the console counts it.
    pub(crate) fn cell_number(&self, id: CellId) -> Option<usize> {
        self.position_of(id).map(|at| at + 1)
    }

    /// The cell at place `number`, from 1.
    pub(crate) fn cell_at(&self, number: usize) -> Option<CellId> {
        let model = self.model.borrow();
        Some(model.cells().get(number.checked_sub(1)?)?.id())
    }

    /// The selected cell, where it is a code cell.
    pub(crate) fn selected_code(&self) -> Option<CellId> {
        let id = self.selected.get()?;
        let model = self.model.borrow();
        let at = model.position(id)?;
        (model.cells()[at].kind() == Kind::Code).then_some(id)
    }

    /// Code cell `id`'s code as it is now, where it has some.
    pub(crate) fn code_of(&self, id: CellId) -> Option<String> {
        self.sync_model();
        let model = self.model.borrow();
        let cell = &model.cells()[model.position(id)?];
        let code = cell.source();
        (cell.kind() == Kind::Code && !code.trim().is_empty()).then(|| code.to_string())
    }

    /// Whether the notebook is presented, which runs nothing.
    pub(crate) fn presented(&self) -> bool {
        self.presenting.get()
    }

    /// The 0-based line of the selected cell's caret, its editor built for it.
    pub(crate) fn selected_line(self: &Rc<Self>) -> Option<(CellId, u32)> {
        let id = self.selected.get()?;
        let (_, editor) = self.editor(id)?;
        let caret = editor.buffer.iter_at_mark(&editor.buffer.get_insert());
        Some((id, caret.line().max(0) as u32))
    }
}
