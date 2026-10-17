//! The debugger in the window: the breakpoints a program stops at, kept per file for the session,
//! the session itself (`run`), and its Debug Console (`console`).

use super::*;
use accent_api::debug::debuggable;
use std::collections::BTreeMap;

pub(crate) mod breakpoints;
mod console;
mod line;
mod run;

use run::Run;

/// The window's debugging state.
#[derive(Default)]
pub struct Debugger {
    /// The breakpoints of the files no tab has open, by file key; an open tab holds its own as
    /// marks that move with its text.
    kept: RefCell<BTreeMap<String, Vec<u32>>>,
    /// The number the last session went by: the window picks it, so a session's news can come
    /// before its start is answered.
    next: Cell<u64>,
    /// The program the last session ran, and its language, which Start Debugging runs again from
    /// a tab it cannot run. The window's alone, never the vault's session.
    last: RefCell<Option<(String, String)>>,
    /// The session, from Start until its end.
    run: RefCell<Option<Rc<Run>>>,
    /// The paused line's mark, and the tab it is in.
    execution: RefCell<Option<(std::rc::Weak<Tab>, sourceview5::Mark)>>,
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
    pub(crate) fn saved(&self, app: &App) -> BTreeMap<String, Vec<u32>> {
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

    /// Put back the breakpoints a session kept, before any of its tabs opens.
    pub(crate) fn restore(&self, kept: &BTreeMap<String, Vec<u32>>) {
        self.kept.borrow_mut().extend(kept.clone());
    }

    /// Put a file's kept breakpoints on its newly opened tab.
    pub(crate) fn place(&self, tab: &Tab) {
        if let Some(lines) = self.kept.borrow().get(&tab.rel()) {
            breakpoints::place(tab, lines);
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
fn keep(map: &mut BTreeMap<String, Vec<u32>>, tab: &Tab) {
    match breakpoints::lines(tab) {
        lines if lines.is_empty() => map.remove(&tab.rel()),
        lines => map.insert(tab.rel(), lines),
    };
}

impl App {
    /// A breakpoint on `line` of `tab`, or none there any more; only where a debugger can run it.
    /// A running program is told the file's breakpoints, all of them, and the marks move to where
    /// it holds them.
    pub(crate) fn toggle_breakpoint(self: &Rc<Self>, tab: &Rc<Tab>, line: u32) {
        if debuggable(&lang::language_id(tab)) {
            breakpoints::toggle(tab, line);
            breakpoints::redraw(tab);
            self.save_session_soon();
            self.send_breakpoints(tab);
            self.refill_breakpoints();
        }
    }

    /// Toggle Breakpoint, on the caret's line of the tab in front.
    pub(crate) fn toggle_breakpoint_at_caret(self: &Rc<Self>) {
        if let Some(tab) = self.active() {
            let line = editor::caret(&tab.buffer).line().max(0) as u32;
            self.toggle_breakpoint(&tab, line);
        }
    }

    /// Remove All Breakpoints, in every file of this window.
    pub(crate) fn remove_all_breakpoints(self: &Rc<Self>) {
        self.debugger.kept.borrow_mut().clear();
        for tab in self.open_tabs() {
            let had = !tab.breakpoints.borrow().is_empty();
            breakpoints::place(&tab, &[]);
            breakpoints::redraw(&tab);
            if had {
                self.send_breakpoints(&tab);
            }
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
