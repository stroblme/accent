//! The debugger in the window: the breakpoints a program stops at, kept per file for the session,
//! the expressions it watches, the session itself (`run`), and its Debug Console (`console`).

use super::*;
use accent_api::debug::{Breakpoint, ExceptionFilter, Supports, debuggable};
use std::collections::BTreeMap;

pub(crate) mod breakpoints;
mod console;
mod inline;
mod line;
mod run;

use run::Run;

/// The window's debugging state.
#[derive(Default)]
pub struct Debugger {
    /// The breakpoints of the files no tab has open, by file key; an open tab holds its own as
    /// marks that move with its text.
    kept: RefCell<BTreeMap<String, Vec<Breakpoint>>>,
    /// What the last session's adapter takes beside a breakpoint's line, for the window's life.
    supports: Cell<Option<Supports>>,
    /// The Watch section's expressions, in its order; the vault's session keeps them.
    watches: RefCell<Vec<String>>,
    /// The exceptions the adapter last offered to stop at, as the reader set them; the vault's
    /// session keeps them.
    exceptions: RefCell<Vec<ExceptionFilter>>,
    /// The number the last session went by: the window picks it, so a session's news can come
    /// before its start is answered.
    next: Cell<u64>,
    /// The program the last session ran, and its language, which Start Debugging runs again from
    /// a tab it cannot run. The window's alone, never the vault's session.
    last: RefCell<Option<(String, String)>>,
    /// The session, from Start until its end.
    run: RefCell<Option<Rc<Run>>>,
    /// The tab holding the paused line's mark.
    execution: RefCell<std::rc::Weak<Tab>>,
    /// The values of the call selected, painted after the lines of its tab.
    values: inline::Values,
    console: console::Console,
}

impl Debugger {
    /// Whether a session runs in this window.
    pub fn running(&self) -> bool {
        self.run.borrow().is_some()
    }

    fn run(&self) -> Option<Rc<Run>> {
        self.run.borrow().clone()
    }

    /// Every file's breakpoints: those kept, with each open tab's as its marks stand now.
    pub(crate) fn saved(&self, app: &App) -> BTreeMap<String, Vec<Breakpoint>> {
        let mut saved = self.kept.borrow().clone();
        for tab in app.open_tabs() {
            keep(&mut saved, &tab);
        }
        saved
    }

    /// Hold on to a closing tab's breakpoints, for when its file is opened again.
    pub(crate) fn closing(&self, tab: &Tab) {
        keep(&mut self.kept.borrow_mut(), tab);
    }

    /// Put back the breakpoints a session kept, before any of its tabs opens, its watches and
    /// its exceptions.
    pub(crate) fn restore(&self, session: &accent_core::config::Session) {
        self.kept.borrow_mut().extend(session.breakpoints.clone());
        self.watches.replace(session.watches.clone());
        self.exceptions.replace(session.exceptions.clone());
    }

    /// What the Breakpoints rows offer to set: until an adapter has said, all of it, which
    /// debugpy takes.
    pub(crate) fn supports(&self) -> Supports {
        self.supports.get().unwrap_or(Supports {
            condition: true,
            hit_condition: true,
            log_message: true,
        })
    }

    pub(crate) fn watches(&self) -> Vec<String> {
        self.watches.borrow().clone()
    }

    pub(crate) fn exceptions(&self) -> Vec<ExceptionFilter> {
        self.exceptions.borrow().clone()
    }

    /// The values painted after the lines, each `line:text`, for `ACCENT_BENCH_DEBUG`.
    #[cfg(feature = "bench")]
    pub(crate) fn inline_values(&self) -> Vec<String> {
        self.values.shown()
    }

    /// Put a file's kept breakpoints on its newly opened tab.
    pub(crate) fn place(&self, tab: &Tab) {
        if let Some(kept) = self.kept.borrow().get(&tab.rel()) {
            breakpoints::place(tab, kept);
        }
    }

    /// Follow a file or a folder moved from `from` to `to`.
    pub(crate) fn renamed(&self, from: &str, to: &str) {
        accent_core::config::rename_keys(&mut self.kept.borrow_mut(), from, to);
    }

    /// Forget the breakpoints of a removed file, or of everything under a removed folder.
    pub(crate) fn forget(&self, rel: &str) {
        let under = format!("{rel}/");
        self.kept
            .borrow_mut()
            .retain(|key, _| key != rel && !key.starts_with(&under));
    }
}

/// `tab`'s breakpoints into `map`, or its entry out of it when it has none.
fn keep(map: &mut BTreeMap<String, Vec<Breakpoint>>, tab: &Tab) {
    match breakpoints::saved(tab) {
        saved if saved.is_empty() => map.remove(&tab.rel()),
        saved => map.insert(tab.rel(), saved),
    };
}

impl App {
    /// A breakpoint on `line` of `tab`, or none there any more; only where a debugger can run it.
    /// A running program is told the file's breakpoints, all of them, and the marks move to where
    /// it holds them.
    pub(crate) fn toggle_breakpoint(self: &Rc<Self>, tab: &Rc<Tab>, line: u32) {
        if crate::doc::host_file_path(&tab.rel()).is_none() && debuggable(&lang::language_id(tab)) {
            breakpoints::toggle(tab, line);
            breakpoints::redraw(tab);
            self.save_session_soon();
            self.send_breakpoints(tab.rel(), breakpoints::saved(tab));
            self.refill_breakpoints();
        }
    }

    /// Set what the breakpoint on `edited.line` of file `key` does beside stopping there, from
    /// its Breakpoints row; a running program is told. Nothing when no breakpoint is on that line
    /// any more.
    pub(crate) fn edit_breakpoint(self: &Rc<Self>, key: &str, edited: Breakpoint) {
        let now = match self.tab_for(key) {
            Some(tab) => {
                if !breakpoints::edit(&tab, &edited) {
                    return;
                }
                breakpoints::redraw(&tab);
                breakpoints::saved(&tab)
            }
            None => {
                let mut kept = self.debugger.kept.borrow_mut();
                let Some(file) = kept.get_mut(key) else {
                    return;
                };
                let Some(here) = file.iter_mut().find(|b| b.line == edited.line) else {
                    return;
                };
                *here = edited;
                file.clone()
            }
        };
        self.save_session_soon();
        self.send_breakpoints(key.to_string(), now);
        self.refill_breakpoints();
    }

    /// Watch `expression`, after those watched already; one watched already stays where it is.
    pub(crate) fn add_watch(self: &Rc<Self>, expression: &str) {
        let mut watches = self.debugger.watches.borrow_mut();
        if !watches.iter().any(|w| w == expression) {
            watches.push(expression.to_string());
        }
        drop(watches);
        self.save_session_soon();
        self.refill_watches();
    }

    pub(crate) fn remove_watch(self: &Rc<Self>, expression: &str) {
        self.debugger
            .watches
            .borrow_mut()
            .retain(|w| w != expression);
        self.save_session_soon();
        self.refill_watches();
    }

    /// Add to Watch: the selection in the tab in front, else the name at its caret, or just before
    /// it, with the names it is reached through, as a hover takes it.
    pub(crate) fn watch_at_caret(self: &Rc<Self>) {
        let Some(tab) = self.active() else {
            return;
        };
        let expression = match tab.buffer.selection_bounds() {
            Some((start, end)) => start.text(&end).trim().to_string(),
            None => {
                let caret = editor::caret(&tab.buffer);
                let (mut start, mut end) = (caret, caret);
                start.set_line_offset(0);
                if !end.ends_line() {
                    end.forward_to_line_end();
                }
                let line = start.text(&end).to_string();
                let column = caret.line_offset() as usize;
                let at = |column| expression_at(&line, column);
                let before = || column.checked_sub(1).and_then(at);
                at(column).or_else(before).unwrap_or_default()
            }
        };
        if !expression.is_empty() {
            self.add_watch(&expression);
        }
    }

    /// Stop at the exceptions `filter` names, or no longer; a running program is told at once.
    pub(crate) fn set_exception(self: &Rc<Self>, filter: &str, on: bool) {
        let mut exceptions = self.debugger.exceptions.borrow_mut();
        if let Some(exception) = exceptions.iter_mut().find(|e| e.filter == filter) {
            exception.on = on;
        }
        drop(exceptions);
        self.save_session_soon();
        self.send_exceptions();
    }

    /// Toggle Breakpoint, on the caret's line of the tab in front.
    pub(crate) fn toggle_breakpoint_at_caret(self: &Rc<Self>) {
        if let Some(tab) = self.active() {
            let line = editor::caret(&tab.buffer).line().max(0) as u32;
            self.toggle_breakpoint(&tab, line);
        }
    }

    /// Remove All Breakpoints, in every file of this window, and a running program's with them:
    /// a closed file's went out with its launch.
    pub(crate) fn remove_all_breakpoints(self: &Rc<Self>) {
        let had = self.debugger.saved(self);
        self.debugger.kept.borrow_mut().clear();
        for tab in self.open_tabs() {
            breakpoints::place(&tab, &[]);
            breakpoints::redraw(&tab);
        }
        for key in had.into_keys() {
            self.send_breakpoints(key, Vec::new());
        }
        self.save_session_soon();
        self.refill_breakpoints();
    }
}

/// What a hover at char `column` of `line` asks the paused program for: the name there with the
/// names it is reached through, `self.items` on the `items` of `self.items.append(x)`, as VS
/// Code's hover takes it. `None` off a name and on a number.
pub(crate) fn expression_at(line: &str, column: usize) -> Option<String> {
    let chars: Vec<char> = line.chars().collect();
    let name = |c: &char| c.is_alphanumeric() || *c == '_';
    let run_back = |end: usize| chars[..end].iter().rev().take_while(|c| name(c)).count();
    if !chars.get(column).is_some_and(name) {
        return None;
    }
    let end = column + chars[column..].iter().take_while(|c| name(c)).count();
    let mut start = column - run_back(column);
    while start > 1 && chars[start - 1] == '.' && name(&chars[start - 2]) {
        start -= 1 + run_back(start - 1);
    }
    let expression: String = chars[start..end].iter().collect();
    (!expression.starts_with(|c: char| c.is_ascii_digit())).then_some(expression)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hovered_name_takes_the_names_it_is_reached_through() {
        let line = "    self.items.append(x)";
        let on = |word: &str| expression_at(line, line.find(word).unwrap() + 1);
        assert_eq!(on("items").as_deref(), Some("self.items"));
        assert_eq!(on("append").as_deref(), Some("self.items.append"));
        assert_eq!(on("self").as_deref(), Some("self"));
        assert_eq!(
            expression_at("total = double(21)", 9).as_deref(),
            Some("double")
        );
        assert_eq!(
            expression_at("total = double(21)", 14),
            None,
            "on the bracket"
        );
        assert_eq!(expression_at("total = double(21)", 15), None, "on a number");
    }
}
