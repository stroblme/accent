//! The Info pane's Details section: what the file in front is, as rows of a name and a value
//! under a heading per group.

use crate::widgets::scroller;
use gtk::pango;
use gtk::prelude::*;
use std::cell::RefCell;

/// One row: what is known, and its value as shown.
#[derive(Clone, PartialEq)]
pub struct Fact {
    pub name: &'static str,
    pub value: String,
}

/// The rows under one heading: the file's, or what its kind adds.
#[derive(Clone, PartialEq)]
pub struct Group {
    pub title: &'static str,
    pub facts: Vec<Fact>,
}

pub(super) struct Details {
    pub(super) root: gtk::ScrolledWindow,
    grid: gtk::Grid,
    /// What the grid is showing, so an answer that changes nothing rebuilds nothing.
    shown: RefCell<Vec<Group>>,
}

impl Details {
    pub(super) fn new() -> Details {
        let grid = gtk::Grid::builder()
            .column_spacing(12)
            .row_spacing(6)
            .margin_start(12)
            .margin_end(12)
            .margin_bottom(12)
            .build();
        Details {
            root: scroller(&grid),
            grid,
            shown: RefCell::default(),
        }
    }

    pub(super) fn set(&self, groups: &[Group]) {
        if *self.shown.borrow() == groups {
            return;
        }
        *self.shown.borrow_mut() = groups.to_vec();
        while let Some(child) = self.grid.first_child() {
            self.grid.remove(&child);
        }
        let mut row = 0;
        for (i, group) in groups.iter().enumerate() {
            let heading = gtk::Label::builder()
                .label(group.title)
                .xalign(0.0)
                .margin_top(if i == 0 { 0 } else { 12 })
                .build();
            heading.add_css_class("caption-heading");
            heading.add_css_class("dim-label");
            self.grid.attach(&heading, 0, row, 2, 1);
            row += 1;
            for fact in &group.facts {
                let name = gtk::Label::builder()
                    .label(fact.name)
                    .xalign(0.0)
                    .valign(gtk::Align::Start)
                    .build();
                name.add_css_class("dim-label");
                // Selectable, so a size or a title can be copied out.
                let value = gtk::Label::builder()
                    .label(&fact.value)
                    .xalign(0.0)
                    .hexpand(true)
                    .wrap(true)
                    .wrap_mode(pango::WrapMode::WordChar)
                    .selectable(true)
                    .build();
                self.grid.attach(&name, 0, row, 1, 1);
                self.grid.attach(&value, 1, row, 1, 1);
                row += 1;
            }
        }
    }

    /// The rows as `Group.Name=value`, what `ACCENT_BENCH_INFO` prints.
    #[cfg(feature = "bench")]
    pub(super) fn lines(&self) -> Vec<String> {
        self.shown
            .borrow()
            .iter()
            .flat_map(|group| {
                group
                    .facts
                    .iter()
                    .map(|fact| format!("{}.{}={}", group.title, fact.name, fact.value))
            })
            .collect()
    }
}
