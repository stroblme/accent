//! Breakpoints as source marks in a tab's gutter, which move with its text as it is edited.

use crate::diff;
use crate::editor::Tab;
use gtk::prelude::*;
use sourceview5::prelude::*;

/// A breakpoint the program has bound, or is yet to bind in a running session.
pub(crate) const BREAKPOINT: &str = "breakpoint";
/// One the program has not bound.
pub(crate) const UNBOUND: &str = "breakpoint-unbound";
/// The line the program is paused on.
pub(crate) const EXECUTION: &str = "execution";

/// Give `tab`'s view the three categories, above the diagnostics' priority 2. GtkSourceView draws
/// every icon on a line, the highest last, so a breakpoint's line is given no diagnostic mark
/// (`diagnostics::render`).
pub(crate) fn install(tab: &Tab) {
    for (category, icon, tooltip, priority) in [
        (BREAKPOINT, "dap-breakpoint-symbolic", "Breakpoint", 3),
        (
            UNBOUND,
            "dap-breakpoint-unverified-symbolic",
            "The program has not bound this breakpoint",
            3,
        ),
        (
            EXECUTION,
            "go-next-symbolic",
            "The program is paused here",
            4,
        ),
    ] {
        let attributes = sourceview5::MarkAttributes::builder()
            .icon_name(icon)
            .build();
        attributes.connect_query_tooltip_text(move |_, _| tooltip.to_string());
        tab.view
            .set_mark_attributes(category, &attributes, priority);
    }
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

/// Whether `line` holds a breakpoint.
pub(crate) fn on_line(buffer: &sourceview5::Buffer, line: i32) -> bool {
    [BREAKPOINT, UNBOUND]
        .into_iter()
        .any(|category| !buffer.source_marks_at_line(line, Some(category)).is_empty())
}

/// The 0-based lines `tab`'s breakpoints are on, sorted, each once: an edit can bring two together.
pub(crate) fn lines(tab: &Tab) -> Vec<u32> {
    let mut lines: Vec<u32> = tab
        .breakpoints
        .borrow()
        .iter()
        .map(|mark| tab.buffer.iter_at_mark(mark).line().max(0) as u32)
        .collect();
    lines.sort_unstable();
    lines.dedup();
    lines
}

/// Replace `tab`'s breakpoints with one at the start of each of `lines`, those past the end
/// dropped: the file may have lost them since they were set.
pub(crate) fn place(tab: &Tab, lines: &[u32]) {
    let mut marks = tab.breakpoints.borrow_mut();
    for mark in marks.drain(..) {
        tab.buffer.delete_mark(&mark);
    }
    for &line in lines {
        if let Some(start) = tab.buffer.iter_at_line(line as i32) {
            marks.push(tab.buffer.create_source_mark(None, BREAKPOINT, &start));
        }
    }
}

/// A breakpoint on `line`, or none there any more.
pub(crate) fn toggle(tab: &Tab, line: u32) {
    let mut marks = tab.breakpoints.borrow_mut();
    let (here, rest): (Vec<_>, Vec<_>) = marks
        .drain(..)
        .partition(|mark| tab.buffer.iter_at_mark(mark).line() == line as i32);
    *marks = rest;
    for mark in &here {
        tab.buffer.delete_mark(mark);
    }
    if here.is_empty()
        && let Some(start) = tab.buffer.iter_at_line(line as i32)
    {
        marks.push(tab.buffer.create_source_mark(None, BREAKPOINT, &start));
    }
}
