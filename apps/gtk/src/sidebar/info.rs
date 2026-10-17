//! Collapsible backlinks, tags and file details in the Info pane.
//!
//! A fixed tree of vertical `GtkPaned`s lets readers divide space between open sections.
//! Inapplicable sections are hidden without reparenting; collapsed sections retain headers.

use super::sections::Sections;
use super::widgets::Section;
use crate::widgets::status_page;
use accent_core::config::InfoPane;
use adw::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// The pane's icon, and the one its empty state is drawn with.
pub(super) const ICON: &str = "dialog-information-symbolic";

pub(super) struct Info {
    /// "sections", or "nothing" while no file is in front.
    pub(super) root: gtk::Stack,
    pub(super) references: Section,
    pub(super) tags: Section,
    pub(super) details: Section,
    /// References over the two sections below it, and Tags over Details.
    sections: Rc<Sections>,
    /// Whether the window has a vault: without an index, References and Tags have nothing to
    /// say, and stay hidden.
    vault: bool,
    /// The tab the pane was last fitted for, and whether a picked tag showed the Tags section
    /// over it anyway: it stays until another tab comes to the front.
    shown_for: RefCell<String>,
    tags_forced: Cell<bool>,
    /// Whether Details was open before a shell came to the front, which unfolds it there, being
    /// the only section shown: the fold the session keeps, and the one a file gets back. `None`
    /// while no shell is in front.
    details_kept: Cell<Option<bool>>,
}

impl Info {
    /// `vault` is the References and Tags sections' bodies, in a window with a vault.
    pub(super) fn new(
        vault: Option<(&gtk::Widget, &gtk::Widget)>,
        details: &impl IsA<gtk::Widget>,
    ) -> Rc<Info> {
        let empty = || gtk::Box::new(gtk::Orientation::Vertical, 0).upcast::<gtk::Widget>();
        let (references, tags) = vault.map_or_else(
            || (empty(), empty()),
            |(references, tags)| (references.clone(), tags.clone()),
        );
        let references = Section::new("References", &references);
        let tags = Section::new("Tags", &tags);
        let details = Section::new("Details", details);
        details.root.set_expanded(false);
        let sections = Sections::new(&[&references.root, &tags.root, &details.root]);
        let root = gtk::Stack::builder().vexpand(true).build();
        root.add_named(&sections.root, Some("sections"));
        let open_one = match vault {
            Some(_) => "Open a file to see its references, tags and details.",
            None => "Open a file to see its details.",
        };
        root.add_named(&status_page(ICON, "No File", open_one), Some("nothing"));
        let info = Rc::new(Info {
            root,
            references,
            tags,
            details,
            sections,
            vault: vault.is_some(),
            shown_for: RefCell::default(),
            tags_forced: Cell::new(false),
            details_kept: Cell::new(None),
        });
        info.sync("", false, false, false);
        info
    }

    pub(super) fn section(&self, name: &str) -> Option<&Section> {
        match name {
            "references" => Some(&self.references),
            "tags" => Some(&self.tags),
            "details" => Some(&self.details),
            _ => None,
        }
    }

    /// Lay the dividers out for the sections now open.
    pub(super) fn fit(&self) {
        self.sections.fit();
    }

    /// Show what applies to the tab `key` (empty for none): the sections for a file, the Tags
    /// section over a note, Details alone over a shell, and a status page for neither.
    pub(super) fn sync(&self, key: &str, file: bool, shell: bool, note: bool) {
        let fronted = *self.shown_for.borrow() != key;
        if fronted {
            self.shown_for.replace(key.to_string());
            self.tags_forced.set(false);
        }
        let forced = self.tags_forced.get();
        self.references.root.set_visible(self.vault && !shell);
        self.tags.root.set_visible(self.vault && (note || forced));
        self.details.root.set_visible(file || shell);
        self.root
            .set_visible_child_name(match file || shell || forced {
                true => "sections",
                false => "nothing",
            });
        // Details is all a shell shows, so a shell coming to the front opens it, and whatever
        // comes next gets back the fold it had. Kept first: the unfold saves the session.
        match (shell, self.details_kept.get()) {
            (true, kept) if fronted => {
                let open = self.details.root.is_expanded();
                self.details_kept.set(kept.or(Some(open)));
                self.details.root.set_expanded(true);
            }
            (false, Some(kept)) => {
                self.details_kept.set(None);
                self.details.root.set_expanded(kept);
            }
            _ => {}
        }
        self.fit();
    }

    /// Open the section `name` without moving the keyboard. The Tags section shows even where
    /// the tab in front would hide it, until another tab comes to the front.
    pub(super) fn open(&self, name: &str) {
        let Some(section) = self.section(name) else {
            return;
        };
        if name == "tags" && self.vault {
            self.tags_forced.set(true);
            section.root.set_visible(true);
            self.root.set_visible_child_name("sections");
        }
        section.root.set_expanded(true);
        self.fit();
    }

    /// Whether `paned` is one of the pane's dividers, putting it back to an even split if so.
    pub(super) fn reset_divider(&self, paned: &gtk::Paned) -> bool {
        self.sections.reset(paned)
    }

    /// The sections as the session keeps them.
    pub(super) fn saved(&self) -> InfoPane {
        let shares = self.sections.shares();
        InfoPane {
            references: self.references.root.is_expanded(),
            tags: self.tags.root.is_expanded(),
            details: self
                .details_kept
                .get()
                .unwrap_or_else(|| self.details.root.is_expanded()),
            dividers: [shares[0], shares[1]],
        }
    }

    /// The page shown, each section as `Title:open|shut|hidden` and its count, and the dividers
    /// as `position/height`: what `ACCENT_BENCH_INFO` prints.
    #[cfg(feature = "bench")]
    pub(super) fn state(&self) -> String {
        let sections: Vec<String> = [&self.references, &self.tags, &self.details]
            .iter()
            .map(|section| {
                let root = &section.root;
                let how = match (root.is_visible(), root.is_expanded()) {
                    (false, _) => "hidden",
                    (true, true) => "open",
                    (true, false) => "shut",
                };
                let count = match section.count.is_visible() {
                    true => format!("({})", section.count.text()),
                    false => String::new(),
                };
                format!("{}:{how}{count}", section.title.text())
            })
            .collect();
        format!(
            "page={} {} dividers={}",
            self.root.visible_child_name().unwrap_or_default(),
            sections.join(" "),
            self.sections.positions().join(",")
        )
    }

    pub(super) fn restore(&self, saved: &InfoPane) {
        self.sections.restore(&saved.dividers);
        self.references.root.set_expanded(saved.references);
        self.tags.root.set_expanded(saved.tags);
        // A shell restored in front keeps Details open; the stored fold waits for a file.
        match self.details_kept.get() {
            Some(_) => self.details_kept.set(Some(saved.details)),
            None => self.details.root.set_expanded(saved.details),
        }
        self.fit();
    }
}
