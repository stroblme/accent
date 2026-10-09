//! The Code pane: what the language layer says holds at the caret, as sections one above the
//! other. A proof's goals today; a debugger's variables, call stack and breakpoints are meant to
//! join them as sections of their own, so the window, not a tab's kind, says when it shows.

use super::widgets::Section;
use crate::widgets::{scroller, status_page};
use accent_api::Goals;
use gtk::pango;
use gtk::prelude::*;
use std::cell::{Cell, RefCell};

/// The pane's icon, and the one its empty state is drawn with.
pub(super) const ICON: &str = "filetype-code-symbolic";

pub(super) struct Code {
    /// "sections"; "checking" while the tab in front is being checked and has no answer yet;
    /// "nothing" while no section has anything to say.
    pub(super) root: gtk::Stack,
    goals: Section,
    /// The goals, a label each.
    goal_labels: gtk::Box,
    expected: Section,
    expected_label: gtk::Label,
    /// The tab the pane is about, so another one coming to the front clears it, and whether it
    /// has had an answer for it yet.
    shown_for: RefCell<String>,
    answered: Cell<bool>,
}

impl Code {
    pub(super) fn new() -> Code {
        let goal_labels = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_start(12)
            .margin_end(12)
            .margin_bottom(12)
            .build();
        let goals = Section::new("Goals", &goal_labels);
        let expected_label = text("");
        expected_label.set_margin_start(12);
        expected_label.set_margin_end(12);
        expected_label.set_margin_bottom(12);
        let expected = Section::new("Expected Type", &expected_label);
        // Two text sections share one scroll: neither is a list that wants its own.
        let sections = gtk::Box::new(gtk::Orientation::Vertical, 0);
        sections.append(&goals.root);
        sections.append(&expected.root);
        let root = gtk::Stack::builder().vexpand(true).build();
        root.add_named(&scroller(&sections), Some("sections"));
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
            goal_labels,
            expected,
            expected_label,
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
        if checking && !self.answered.get() {
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
        self.root
            .set_visible_child_name(match tactic.is_some() || expected.is_some() {
                true => "sections",
                false => "nothing",
            });
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
