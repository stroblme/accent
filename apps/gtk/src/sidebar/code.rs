//! Show language-layer data at the active caret as independent Code pane sections.
//!
//! Proof goals are the first sections, the debugger's the rest (`debug.rs`); the window
//! controls visibility, so the debugger's stay up over any tab while a session runs.

use super::debug::{self, Debug};
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
    pub(super) fn new(debug: Option<debug::Data>) -> Code {
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
        let debug = debug.map(Debug::new);
        let mut stacked = vec![&goals.root, &expected.root];
        stacked.extend(debug.iter().flat_map(|d| d.sections().map(|s| &s.root)));
        let sections = Sections::new(&stacked);
        // The strip stands over the sections, outside their dividers.
        let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
        if let Some(debug) = &debug {
            page.append(&debug.strip);
        }
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
        if checking && !self.answered.get() && !self.debugging.get() {
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
            done.add_css_class("dim-label");
            self.goal_labels.append(&done);
        }
        self.goals.set_count(tactic.map(<[String]>::len));
        self.goals.root.set_visible(tactic.is_some());
        self.expected_label.set_text(expected.unwrap_or_default());
        self.expected_label.set_attributes(attrs.as_ref());
        self.expected.root.set_visible(expected.is_some());
        self.pick_page();
    }

    /// The sections while any has something to say or the debugger's strip is up, else the page
    /// saying why there are none.
    fn pick_page(&self) {
        let shown = self.goals.root.is_visible() || self.expected.root.is_visible();
        self.root
            .set_visible_child_name(match shown || self.debugging.get() {
                true => "sections",
                false => "nothing",
            });
    }

    /// The debugger's strip up or not: its sections come and go, and with them the pane's page.
    pub(super) fn set_debugging(&self, on: bool) {
        if self.debugging.replace(on) != on {
            self.pick_page();
        }
    }

    pub(super) fn section(&self, name: &str) -> Option<&Section> {
        match name {
            "goals" => Some(&self.goals),
            "expected" => Some(&self.expected),
            "stack" => self.debug.as_ref().map(|d| &d.stack),
            "variables" => self.debug.as_ref().map(|d| &d.variables),
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
