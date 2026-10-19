//! Breakpoints as source marks in a tab's gutter, which move with its text as it is edited.

use crate::diff;
use crate::editor::Tab;
use accent_api::debug::Placed;
use gtk::glib;
use gtk::prelude::*;
use sourceview5::prelude::*;
use std::rc::Rc;

/// A breakpoint the program has bound, or is yet to bind in a running session.
pub(crate) const BREAKPOINT: &str = "breakpoint";
/// One the program has not bound.
pub(crate) const UNBOUND: &str = "breakpoint-unbound";
/// The line the program is paused on.
pub(crate) const EXECUTION: &str = "execution";
/// A breakpoint, bound or not, on the paused line: drawn by no icon while the arrow is there.
const COVERED: &str = "breakpoint-covered";
const UNBOUND_COVERED: &str = "breakpoint-unbound-covered";

/// A breakpoint's source mark, and why the program has not bound it, where it said.
pub(crate) struct Breakpoint {
    mark: sourceview5::Mark,
    unbound: Option<String>,
}

impl Breakpoint {
    fn new(tab: &Tab, start: &gtk::TextIter, category: &str, unbound: Option<String>) -> Self {
        let mark = tab.buffer.create_source_mark(None, category, start);
        Breakpoint { mark, unbound }
    }
}

/// Give `tab`'s view the categories, above the diagnostics' priority 2. GtkSourceView draws every
/// icon on a line, the highest last, so a line gives its cell to one of them ([`redraw`]).
pub(crate) fn install(tab: &Rc<Tab>) {
    const UNBOUND_SAYS: &str = "The program has not bound this breakpoint";
    for (category, icon, tooltip, priority) in [
        (BREAKPOINT, Some("dap-breakpoint-symbolic"), "Breakpoint", 3),
        (
            UNBOUND,
            Some("dap-breakpoint-unverified-symbolic"),
            UNBOUND_SAYS,
            3,
        ),
        (COVERED, None, "Breakpoint", 3),
        (UNBOUND_COVERED, None, UNBOUND_SAYS, 3),
        (
            EXECUTION,
            Some("go-next-symbolic"),
            "The program is paused here",
            4,
        ),
    ] {
        let attributes = sourceview5::MarkAttributes::new();
        if let Some(icon) = icon {
            attributes.set_icon_name(icon);
        }
        attributes.connect_query_tooltip_text(glib::clone!(
            #[weak]
            tab,
            #[upgrade_or_default]
            move |_, mark| why_unbound(&tab, mark).unwrap_or_else(|| tooltip.to_string())
        ));
        tab.view
            .set_mark_attributes(category, &attributes, priority);
    }
}

/// Why the program has not bound the breakpoint `mark` is, where it said.
fn why_unbound(tab: &Tab, mark: &sourceview5::Mark) -> Option<String> {
    let breakpoints = tab.breakpoints.borrow();
    let why = breakpoints
        .iter()
        .find(|b| b.mark == *mark)?
        .unbound
        .as_deref()?;
    Some(why.trim().to_string())
}

/// The paused line's wash, the warning's amber mixed with the resolved foreground.
pub(crate) fn restyle(view: &sourceview5::View) {
    // 0 is the only safe priority: the binding hands the number to C as the pointer the
    // category's priority is written through, and 0 is that pointer's NULL.
    if let Some(attributes) = view.mark_attributes(EXECUTION, 0) {
        attributes.set_background(&diff::tint(
            diff::WARNING_HUE,
            view.color(),
            diff::CHANGE_ALPHA,
        ));
    }
}

/// Whether `line` holds a breakpoint or the paused line's arrow, which take its cell from a
/// diagnostic's icon.
pub(crate) fn on_line(buffer: &sourceview5::Buffer, line: i32) -> bool {
    [BREAKPOINT, UNBOUND, COVERED, UNBOUND_COVERED, EXECUTION]
        .into_iter()
        .any(|category| !buffer.source_marks_at_line(line, Some(category)).is_empty())
}

/// The 0-based lines `tab`'s breakpoints are on, sorted, each once: an edit can bring two together.
pub(crate) fn lines(tab: &Tab) -> Vec<u32> {
    let mut lines: Vec<u32> = tab
        .breakpoints
        .borrow()
        .iter()
        .map(|b| tab.buffer.iter_at_mark(&b.mark).line().max(0) as u32)
        .collect();
    lines.sort_unstable();
    lines.dedup();
    lines
}

/// Replace `tab`'s breakpoints with one at the start of each of `lines`, those past the end
/// dropped: the file may have lost them since they were set.
pub(crate) fn place(tab: &Tab, lines: &[u32]) {
    let mut marks = tab.breakpoints.borrow_mut();
    for b in marks.drain(..) {
        tab.buffer.delete_mark(&b.mark);
    }
    for &line in lines {
        if let Some(start) = tab.buffer.iter_at_line(line as i32) {
            marks.push(Breakpoint::new(tab, &start, BREAKPOINT, None));
        }
    }
}

/// The line each of `tab`'s breakpoints is on, in their order, for [`put_back`] once its text is
/// replaced, which moves every mark to line 0.
pub(crate) fn each_line(tab: &Tab) -> Vec<i32> {
    let marks = tab.breakpoints.borrow();
    marks
        .iter()
        .map(|b| tab.buffer.iter_at_mark(&b.mark).line())
        .collect()
}

/// Move each of `tab`'s breakpoints back to its line from [`each_line`], the dot or ring it was;
/// one now past the end is dropped: the file may have lost it.
pub(crate) fn put_back(tab: &Tab, lines: &[i32]) {
    let mut lines = lines.iter();
    tab.breakpoints.borrow_mut().retain(|b| {
        match lines.next().and_then(|&line| tab.buffer.iter_at_line(line)) {
            Some(start) => tab.buffer.move_mark(&b.mark, &start),
            None => tab.buffer.delete_mark(&b.mark),
        }
        !b.mark.is_deleted()
    });
}

/// Replace `tab`'s breakpoints with where the adapter holds them: a dot for each it bound, a ring
/// for each it did not, saying why where it said, on the line it moved each to.
pub(crate) fn bind(tab: &Tab, placed: &[Placed]) {
    let mut marks = tab.breakpoints.borrow_mut();
    for b in marks.drain(..) {
        tab.buffer.delete_mark(&b.mark);
    }
    let mut lines = Vec::new();
    for placed in placed {
        let category = match placed.verified {
            true => BREAKPOINT,
            false => UNBOUND,
        };
        if lines.contains(&placed.line) {
            continue;
        }
        if let Some(start) = tab.buffer.iter_at_line(placed.line as i32) {
            lines.push(placed.line);
            let unbound = placed.message.clone().filter(|_| !placed.verified);
            marks.push(Breakpoint::new(tab, &start, category, unbound));
        }
    }
}

/// The gutter after a breakpoint or the paused line came, went or moved: the arrow takes its
/// line's cell from a breakpoint's dot or ring, which waits under it until the program moves on,
/// and either from a diagnostic's icon (`diagnostics::render`). One drawn over another reads as
/// neither.
pub(crate) fn redraw(tab: &Tab) {
    let mut marks = tab.breakpoints.borrow_mut();
    for Breakpoint { mark, .. } in marks.iter_mut() {
        let at = tab.buffer.iter_at_mark(mark);
        let paused = !tab
            .buffer
            .source_marks_at_line(at.line(), Some(EXECUTION))
            .is_empty();
        let category = match (mark.category().as_str(), paused) {
            (BREAKPOINT, true) => COVERED,
            (UNBOUND, true) => UNBOUND_COVERED,
            (COVERED, false) => BREAKPOINT,
            (UNBOUND_COVERED, false) => UNBOUND,
            _ => continue,
        };
        tab.buffer.delete_mark(mark);
        *mark = tab.buffer.create_source_mark(None, category, &at);
    }
    drop(marks);
    tab.remark_diagnostics();
}

/// A breakpoint on `line`, or none there any more.
pub(crate) fn toggle(tab: &Tab, line: u32) {
    let mut marks = tab.breakpoints.borrow_mut();
    let (here, rest): (Vec<_>, Vec<_>) = marks
        .drain(..)
        .partition(|b| tab.buffer.iter_at_mark(&b.mark).line() == line as i32);
    *marks = rest;
    for b in &here {
        tab.buffer.delete_mark(&b.mark);
    }
    if here.is_empty()
        && let Some(start) = tab.buffer.iter_at_line(line as i32)
    {
        marks.push(Breakpoint::new(tab, &start, BREAKPOINT, None));
    }
}
