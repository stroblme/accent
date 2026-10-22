//! Breakpoints as source marks in a tab's gutter, which move with its text as it is edited.

use crate::diff;
use crate::editor::Tab;
use accent_api::debug::{self, Placed};
use gtk::glib;
use gtk::prelude::*;
use sourceview5::prelude::*;
use std::rc::Rc;

/// A breakpoint the program has bound, or is yet to bind in a running session.
const BREAKPOINT: &str = "breakpoint";
/// One the program has not bound.
const UNBOUND: &str = "breakpoint-unbound";
/// One with a condition or a hit count, bound or not.
const CONDITIONAL: &str = "breakpoint-conditional";
const CONDITIONAL_UNBOUND: &str = "breakpoint-conditional-unbound";
/// One with a log message, bound or not.
const LOGPOINT: &str = "logpoint";
const LOGPOINT_UNBOUND: &str = "logpoint-unbound";
/// The line the program is paused on.
pub(crate) const EXECUTION: &str = "execution";
/// A breakpoint, bound or not, on the paused line: drawn by no icon while the arrow is there.
const COVERED: &str = "breakpoint-covered";
const UNBOUND_COVERED: &str = "breakpoint-unbound-covered";

/// Each kind of breakpoint's category and glyph, bound and not: VS Code's, a plain dot, a dot
/// with `=` for a condition or a hit count, a diamond for a log message.
const KINDS: [[(&str, &str); 2]; 3] = [
    [
        (BREAKPOINT, "dap-breakpoint-symbolic"),
        (UNBOUND, "dap-breakpoint-unverified-symbolic"),
    ],
    [
        (CONDITIONAL, "dap-breakpoint-conditional-symbolic"),
        (
            CONDITIONAL_UNBOUND,
            "dap-breakpoint-conditional-unverified-symbolic",
        ),
    ],
    [
        (LOGPOINT, "dap-logpoint-symbolic"),
        (LOGPOINT_UNBOUND, "dap-logpoint-unverified-symbolic"),
    ],
];

/// Which of [`KINDS`] `set` is: a log message wins, as it never stops.
fn kind(set: &debug::Breakpoint) -> usize {
    match set {
        debug::Breakpoint {
            log_message: Some(_),
            ..
        } => 2,
        debug::Breakpoint {
            condition: None,
            hit_condition: None,
            ..
        } => 0,
        _ => 1,
    }
}

/// The category a breakpoint's mark is in: by its kind and whether it is bound, or with no icon
/// while it is `covered` by the paused line's arrow.
fn category(set: &debug::Breakpoint, unbound: bool, covered: bool) -> &'static str {
    match (covered, unbound) {
        (true, true) => UNBOUND_COVERED,
        (true, false) => COVERED,
        (false, _) => KINDS[kind(set)][unbound as usize].0,
    }
}

/// `set`'s glyph, bound, for its Breakpoints row.
pub(crate) fn icon(set: &debug::Breakpoint) -> &'static str {
    KINDS[kind(set)][0].1
}

/// What `set` does beside stopping, a line each, for a tooltip; empty for a plain one.
pub(crate) fn described(set: &debug::Breakpoint) -> Vec<String> {
    [
        ("Condition", &set.condition),
        ("Hit count", &set.hit_condition),
        ("Log message", &set.log_message),
    ]
    .into_iter()
    .filter_map(|(what, value)| Some(format!("{what}: {}", value.as_deref()?)))
    .collect()
}

/// A breakpoint's source mark, what was set beside its line, and whether the program has not
/// bound it, with why where it said.
pub(crate) struct Breakpoint {
    mark: sourceview5::Mark,
    /// Its `line` is where it was set; the mark says where it is now.
    set: debug::Breakpoint,
    unbound: bool,
    why: Option<String>,
}

impl Breakpoint {
    fn new(
        tab: &Tab,
        start: &gtk::TextIter,
        set: debug::Breakpoint,
        unbound: bool,
        why: Option<String>,
    ) -> Self {
        let category = category(&set, unbound, false);
        let mark = tab.buffer.create_source_mark(None, category, start);
        Breakpoint {
            mark,
            set,
            unbound,
            why,
        }
    }

    /// As it stands now: on its mark's line.
    fn now(&self, tab: &Tab) -> debug::Breakpoint {
        debug::Breakpoint {
            line: tab.buffer.iter_at_mark(&self.mark).line().max(0) as u32,
            ..self.set.clone()
        }
    }
}

/// Give `tab`'s view the categories, above the diagnostics' priority 2. GtkSourceView draws every
/// icon on a line, the highest last, so a line gives its cell to one of them ([`redraw`]).
pub(crate) fn install(tab: &Rc<Tab>) {
    let breakpoints = KINDS
        .into_iter()
        .flatten()
        .map(|(c, icon)| (c, Some(icon), 3));
    let covered = [(COVERED, None, 3), (UNBOUND_COVERED, None, 3)];
    let paused = (EXECUTION, Some("go-next-symbolic"), 4);
    for (category, icon, priority) in breakpoints.chain(covered).chain([paused]) {
        let attributes = sourceview5::MarkAttributes::new();
        if let Some(icon) = icon {
            attributes.set_icon_name(icon);
        }
        attributes.connect_query_tooltip_text(glib::clone!(
            #[weak]
            tab,
            #[upgrade_or_default]
            move |_, mark| tooltip(&tab, mark)
        ));
        tab.view
            .set_mark_attributes(category, &attributes, priority);
    }
}

/// What the mark says under the pointer: the paused line, or the breakpoint's settings and why
/// the program has not bound it, where it said.
fn tooltip(tab: &Tab, mark: &sourceview5::Mark) -> String {
    let breakpoints = tab.breakpoints.borrow();
    let Some(b) = breakpoints.iter().find(|b| b.mark == *mark) else {
        return "The program is paused here".to_string();
    };
    let mut lines = described(&b.set);
    if lines.is_empty() {
        lines.push("Breakpoint".to_string());
    }
    if b.unbound {
        let why = b.why.as_deref().map(str::trim);
        lines.push(
            why.unwrap_or("The program has not bound this breakpoint")
                .to_string(),
        );
    }
    lines.join("\n")
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
    let breakpoints = KINDS.into_iter().flatten().map(|(category, _)| category);
    breakpoints
        .chain([COVERED, UNBOUND_COVERED, EXECUTION])
        .any(|category| !buffer.source_marks_at_line(line, Some(category)).is_empty())
}

/// `tab`'s breakpoints as they stand, by line, one a line: an edit can bring two together, and
/// the first keeps the line.
pub(crate) fn saved(tab: &Tab) -> Vec<debug::Breakpoint> {
    let mut saved: Vec<debug::Breakpoint> = tab
        .breakpoints
        .borrow()
        .iter()
        .map(|b| b.now(tab))
        .collect();
    saved.sort_by_key(|b| b.line);
    saved.dedup_by_key(|b| b.line);
    saved
}

/// Replace `tab`'s breakpoints with `breakpoints`, those past the end dropped: the file may have
/// lost their lines since they were set.
pub(crate) fn place(tab: &Tab, breakpoints: &[debug::Breakpoint]) {
    let mut marks = tab.breakpoints.borrow_mut();
    for b in marks.drain(..) {
        tab.buffer.delete_mark(&b.mark);
    }
    for b in breakpoints {
        if let Some(start) = tab.buffer.iter_at_line(b.line as i32) {
            marks.push(Breakpoint::new(tab, &start, b.clone(), false, None));
        }
    }
}

/// Set what the breakpoint on `edited.line` does beside stopping there; false when none is.
pub(crate) fn edit(tab: &Tab, edited: &debug::Breakpoint) -> bool {
    let mut marks = tab.breakpoints.borrow_mut();
    match marks.iter_mut().find(|b| b.now(tab).line == edited.line) {
        Some(b) => {
            b.set = edited.clone();
            true
        }
        None => false,
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
        let line = placed.breakpoint.line;
        if lines.contains(&line) {
            continue;
        }
        if let Some(start) = tab.buffer.iter_at_line(line as i32) {
            lines.push(line);
            let why = placed.message.clone().filter(|_| !placed.verified);
            let set = placed.breakpoint.clone();
            marks.push(Breakpoint::new(tab, &start, set, !placed.verified, why));
        }
    }
}

/// The gutter after a breakpoint or the paused line came, went or moved: the arrow takes its
/// line's cell from a breakpoint's dot or ring, which waits under it until the program moves on,
/// and either from a diagnostic's icon (`diagnostics::render`). One drawn over another reads as
/// neither.
pub(crate) fn redraw(tab: &Tab) {
    let mut marks = tab.breakpoints.borrow_mut();
    for b in marks.iter_mut() {
        let at = tab.buffer.iter_at_mark(&b.mark);
        let paused = !tab
            .buffer
            .source_marks_at_line(at.line(), Some(EXECUTION))
            .is_empty();
        let category = category(&b.set, b.unbound, paused);
        if b.mark.category() != category {
            tab.buffer.delete_mark(&b.mark);
            b.mark = tab.buffer.create_source_mark(None, category, &at);
        }
    }
    drop(marks);
    tab.remark_diagnostics();
}

/// Each breakpoint's 1-based line and its mark's category, as a drill reads them.
#[cfg(feature = "bench")]
pub(crate) fn shown(tab: &Tab) -> Vec<(u32, String)> {
    let marks = tab.breakpoints.borrow();
    let mut shown: Vec<_> = marks
        .iter()
        .map(|b| (b.now(tab).line + 1, b.mark.category().to_string()))
        .collect();
    shown.sort();
    shown
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
        let set = debug::Breakpoint::at(line);
        marks.push(Breakpoint::new(tab, &start, set, false, None));
    }
}
