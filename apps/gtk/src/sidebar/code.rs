//! Show language-layer data at the active caret as independent Code pane sections.
//!
//! Proof goals are the first sections, the debugger's the rest (`debug.rs`), and the notebook in
//! front's kernel has a strip of its own; the window controls visibility, so the debugger's stay
//! up over any tab while a session runs.

use super::debug::{self, Debug};
use super::python::{self, Picker};
use super::sections::Sections;
use super::widgets::Section;
use crate::widgets::{scroller, status_page};
use accent_api::Goals;
use accent_core::config::CodePane;
use gtk::pango;
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// The pane's icon, and the one its empty state is drawn with.
pub(super) const ICON: &str = "filetype-code-symbolic";

pub(super) struct Code {
    /// "sections"; "checking" while the tab in front is being checked and has no answer yet;
    /// "nothing" while no section has anything to say.
    pub(super) root: gtk::Stack,
    goals: Section,
    /// The debugger's strip and sections, in a window with a vault to debug in.
    pub(super) debug: Option<Debug>,
    /// Whether the debugger's strip is up — a session runs, or the tab in front can be debugged —
    /// which keeps the sections' page up.
    debugging: Cell<bool>,
    /// The notebook in front's kernel, up while it is in front and no debug session runs.
    kernel: KernelStrip,
    /// The Python Environment dropdown, in a window with a vault: beside whichever strip is up,
    /// the same for a Python file and a notebook, and away while a debug session runs.
    python: Option<Picker>,
    /// The strips, pushed to the end of their row beside the dropdown and centred without it.
    strips: gtk::Box,
    /// The goals, a label each.
    goal_labels: gtk::Box,
    expected: Section,
    expected_label: gtk::Label,
    /// Goals over Expected Type.
    sections: Rc<Sections>,
    /// The tab the pane is about, so another one coming to the front clears it, and whether it
    /// has had an answer for it yet.
    shown_for: RefCell<String>,
    answered: Cell<bool>,
}

impl Code {
    pub(super) fn new(debug: Option<debug::Data>, python: Option<python::Data>) -> Code {
        let goal_labels = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_start(12)
            .margin_end(12)
            .margin_bottom(12)
            .build();
        let goals = Section::new("Goals", &scroller(&goal_labels));
        let expected_label = text("");
        expected_label.set_margin_start(12);
        expected_label.set_margin_end(12);
        expected_label.set_margin_bottom(12);
        let expected = Section::new("Expected Type", &scroller(&expected_label));
        let python = python.map(|data| Picker::new(&Rc::new(data)));
        let debug = debug.map(Debug::new);
        let mut stacked = vec![&goals.root, &expected.root];
        stacked.extend(debug.iter().flat_map(|d| d.sections().map(|s| &s.root)));
        let sections = Sections::new(&stacked);
        // One row over the sections, outside their dividers: the dropdown, then the strip that
        // is up, as the sidebar is too narrow for two rows to leave either much to say.
        let strips = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .hexpand(true)
            .build();
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        if let Some(python) = &python {
            row.append(&python.button);
        }
        row.append(&strips);
        if let Some(debug) = &debug {
            strips.append(&debug.strip);
        }
        let kernel = KernelStrip::new();
        strips.append(&kernel.root);
        let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
        page.append(&row);
        page.append(&sections.root);
        let root = gtk::Stack::builder().vexpand(true).build();
        root.add_named(&page, Some("sections"));
        root.add_named(
            &status_page(ICON, "No Goals", "Place the caret in a proof"),
            Some("nothing"),
        );
        root.add_named(
            &status_page(
                ICON,
                "Checking…",
                "Goals show once the check reaches the caret",
            ),
            Some("checking"),
        );
        let code = Code {
            root,
            goals,
            debug,
            debugging: Cell::new(false),
            kernel,
            python,
            strips,
            goal_labels,
            expected,
            expected_label,
            sections,
            shown_for: RefCell::default(),
            answered: Cell::new(false),
        };
        code.show(None, None);
        code
    }

    /// Clear the pane if it is about another tab than `key`, the one in front. Until its first
    /// answer, a tab still being checked (`checking`) has none to give, and the pane says so; the
    /// check ending takes nothing down, the answer asked for then does.
    pub(super) fn sync(&self, key: &str, checking: bool) {
        if *self.shown_for.borrow() != key {
            self.shown_for.replace(key.to_string());
            self.answered.set(false);
            self.show(None, None);
        }
        if checking && !self.answered.get() && !self.strip_up() {
            self.root.set_visible_child_name("checking");
        }
    }

    /// Show what holds at the caret of the tab `key`, in `font`, the editor's at the tab's zoom.
    pub(super) fn set(
        &self,
        key: &str,
        goals: Option<&Goals>,
        font: Option<&pango::FontDescription>,
    ) {
        self.shown_for.replace(key.to_string());
        self.answered.set(true);
        self.show(goals, font);
    }

    /// A section with nothing to say is hidden, and with neither the pane says why.
    fn show(&self, goals: Option<&Goals>, font: Option<&pango::FontDescription>) {
        let attrs = font.map(|font| {
            let attrs = pango::AttrList::new();
            attrs.insert(pango::AttrFontDesc::new(font));
            attrs
        });
        let tactic = goals.and_then(|g| g.tactic.as_deref());
        let expected = goals.and_then(|g| g.expected.as_deref());
        while let Some(child) = self.goal_labels.first_child() {
            self.goal_labels.remove(&child);
        }
        for goal in tactic.unwrap_or_default() {
            let label = text(goal);
            label.set_attributes(attrs.as_ref());
            self.goal_labels.append(&label);
        }
        if tactic.is_some_and(<[String]>::is_empty) {
            let done = gtk::Label::builder().label("No goals").xalign(0.0).build();
            done.add_css_class("dimmed");
            self.goal_labels.append(&done);
        }
        self.goals.set_count(tactic.map(<[String]>::len));
        self.goals.root.set_visible(tactic.is_some());
        self.expected_label.set_text(expected.unwrap_or_default());
        self.expected_label.set_attributes(attrs.as_ref());
        self.expected.root.set_visible(expected.is_some());
        self.pick_page();
    }

    /// The sections while any has something to say or a strip is up, else the page saying why
    /// there are none.
    fn pick_page(&self) {
        // Every strip coming or going passes here, and the dropdown stands beside either; a
        // session needs the room for its steps.
        let session = self.debug.as_ref().is_some_and(Debug::in_session);
        let picker = self.python.as_ref().filter(|_| self.strip_up() && !session);
        if let Some(python) = &self.python {
            python.button.set_visible(picker.is_some());
        }
        self.strips.set_halign(match picker {
            Some(_) => gtk::Align::End,
            None => gtk::Align::Center,
        });
        let shown = self.goals.root.is_visible() || self.expected.root.is_visible();
        self.root
            .set_visible_child_name(match shown || self.strip_up() {
                true => "sections",
                false => "nothing",
            });
    }

    /// The debugger's strip up or not, idle or with a session: its sections come and go, and
    /// with them the pane's page and the dropdown.
    pub(super) fn set_debugging(&self, on: bool) {
        self.debugging.set(on);
        self.pick_page();
    }

    /// `get_visible`, not `is_visible`: the strip's own flag, which a hidden sidebar does not
    /// clear, so a strip put away meanwhile is not left up for the sidebar's return.
    fn strip_up(&self) -> bool {
        self.debugging.get() || self.kernel.root.get_visible()
    }

    /// The kernel's strip showing `shown`, or put away (`None`).
    pub(super) fn set_kernel(&self, shown: Option<&KernelState>) {
        if let Some(state) = shown {
            self.kernel.show(state);
        }
        if self.kernel.root.get_visible() != shown.is_some() {
            self.kernel.root.set_visible(shown.is_some());
            self.pick_page();
        }
    }

    /// Name `command`, the vault's Python, on the dropdowns.
    pub(super) fn set_python(&self, command: &[String]) {
        if let Some(python) = &self.python {
            python.set(command);
        }
    }

    /// The dropdown, while it is up beside a strip.
    pub(super) fn python_button(&self) -> Option<gtk::MenuButton> {
        let python = self.python.as_ref().filter(|p| p.button.get_visible())?;
        Some(python.button.clone())
    }

    /// What the kernel's strip says and shows, while it is up, for `ACCENT_BENCH_KERNEL`.
    #[cfg(feature = "bench")]
    pub(super) fn kernel_line(&self) -> Option<String> {
        self.kernel.root.is_visible().then(|| self.kernel.shown())
    }

    pub(super) fn section(&self, name: &str) -> Option<&Section> {
        match name {
            "goals" => Some(&self.goals),
            "expected" => Some(&self.expected),
            "stack" => self.debug.as_ref().map(|d| &d.stack),
            "variables" => self.debug.as_ref().map(|d| &d.variables),
            "watch" => self.debug.as_ref().map(|d| &d.watch),
            "breakpoints" => self.debug.as_ref().map(|d| &d.breakpoints),
            _ => None,
        }
    }

    /// Whether `paned` is one of the pane's dividers, putting it back to an even split if so.
    pub(super) fn reset_divider(&self, paned: &gtk::Paned) -> bool {
        self.sections.reset(paned)
    }

    /// The sections as the session keeps them.
    pub(super) fn saved(&self) -> CodePane {
        // A window with nothing to debug keeps the debugger's sections open for one that has.
        let open = |name| self.section(name).is_none_or(|s| s.root.is_expanded());
        CodePane {
            goals: open("goals"),
            expected: open("expected"),
            stack: open("stack"),
            variables: open("variables"),
            watch: open("watch"),
            breakpoints: open("breakpoints"),
            dividers: self.sections.shares(),
        }
    }

    pub(super) fn restore(&self, saved: &CodePane) {
        self.sections.restore(&saved.dividers);
        for (name, open) in [
            ("goals", saved.goals),
            ("expected", saved.expected),
            ("stack", saved.stack),
            ("variables", saved.variables),
            ("watch", saved.watch),
            ("breakpoints", saved.breakpoints),
        ] {
            if let Some(section) = self.section(name) {
                section.root.set_expanded(open);
            }
        }
        self.sections.fit();
    }

    /// The goals on screen as their labels read, for `ACCENT_BENCH_GOALS`; `None` while the
    /// Goals section is hidden.
    #[cfg(feature = "bench")]
    pub(super) fn goals(&self) -> Option<Vec<String>> {
        let mut shown = Vec::new();
        let mut child = self.goal_labels.first_child();
        while let Some(widget) = child {
            // A goal's label is selectable; the dim "No goals" line is not one.
            if let Some(label) = widget.downcast_ref::<gtk::Label>()
                && label.is_selectable()
            {
                shown.push(label.text().to_string());
            }
            child = widget.next_sibling();
        }
        self.goals.root.is_visible().then_some(shown)
    }
}

/// A selectable, wrapping label for a goal or a type, which can be long and is worth copying out.
fn text(content: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(content)
        .xalign(0.0)
        .wrap(true)
        .wrap_mode(pango::WrapMode::WordChar)
        .selectable(true)
        .build()
}

/// The notebook in front's kernel, as its strip shows it.
pub enum KernelState {
    /// None runs: none was started, or it was shut down or ended.
    Off,
    /// Started, and not answered yet.
    Starting,
    /// Answered as `what` (`ipython 9.10.0 in python3`), with cells running or waiting for it
    /// (`busy`) or not.
    Up { what: String, busy: bool },
}

/// The kernel's strip, VS Code's notebook toolbar (user decision 2026-10-27): after a spinner
/// while the kernel starts or runs cells, Run All, or Stop in its place meanwhile, then Restart
/// Kernel and Clear All Outputs, flat icon buttons on the window's actions. Shut Down Kernel is
/// the palette's, and Stop's while the kernel starts, there being nothing to interrupt yet.
struct KernelStrip {
    root: gtk::Box,
    working: debug::Working,
    run_all: gtk::Button,
    /// Stop while cells run: Interrupt Kernel.
    interrupt: gtk::Button,
    /// Stop while the kernel starts: Shut Down Kernel.
    cancel: gtk::Button,
    restart: gtk::Button,
}

impl KernelStrip {
    fn new() -> KernelStrip {
        let stop = "media-playback-stop-symbolic";
        let run_all = debug::strip_button("media-seek-forward-symbolic", "win.notebook-run-all");
        let interrupt = debug::strip_button(stop, "win.notebook-interrupt");
        let cancel = debug::strip_button(stop, "win.notebook-shut-down");
        let restart = debug::strip_button("view-refresh-symbolic", "win.notebook-restart");
        let root = debug::strip_of(&[
            run_all.clone().upcast(),
            interrupt.clone().upcast(),
            cancel.clone().upcast(),
            restart.clone().upcast(),
            debug::strip_button("edit-clear-all-symbolic", "win.notebook-clear-all-outputs")
                .upcast(),
        ]);
        let working = debug::Working::new(&root);
        root.prepend(&working.slot);
        KernelStrip {
            root,
            working,
            run_all,
            interrupt,
            cancel,
            restart,
        }
    }

    /// Run All or Stop, the spinner while the kernel is at work, and the kernel that answered in
    /// Restart Kernel's tooltip: it keeps its Python, which the dropdown may no longer name, until
    /// it is restarted.
    fn show(&self, state: &KernelState) {
        let (said, working) = match state {
            KernelState::Off => ("No kernel", false),
            KernelState::Starting => ("Starting…", true),
            KernelState::Up { busy: false, .. } => ("Idle", false),
            KernelState::Up { busy: true, .. } => ("Busy", true),
        };
        self.cancel
            .set_visible(matches!(state, KernelState::Starting));
        self.interrupt
            .set_visible(matches!(state, KernelState::Up { busy: true, .. }));
        self.run_all
            .set_visible(!self.cancel.get_visible() && !self.interrupt.get_visible());
        let restart = crate::actions::label_of("win.notebook-restart");
        let tooltip = match state {
            KernelState::Up { what, .. } => format!("{restart} ({what})"),
            _ => restart.to_string(),
        };
        self.restart.set_tooltip_text(Some(&tooltip));
        self.working.say(said, working);
    }

    /// What it says, and which of Run All and the Stops it shows, for the drills.
    #[cfg(feature = "bench")]
    fn shown(&self) -> String {
        let toggle = [
            (&self.run_all, "run-all"),
            (&self.interrupt, "interrupt"),
            (&self.cancel, "shut-down"),
        ];
        let toggle = toggle.into_iter().find(|(button, _)| button.get_visible());
        let name = toggle.map_or("none", |(_, name)| name);
        format!("{} {name}", self.working.said())
    }
}
