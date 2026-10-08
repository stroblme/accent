//! The Info pane: what links to the file in front and the vault's tags, as sections one above
//! the other. Each folds to its header; the open ones share the height across a divider the
//! reader drags, as VS Code's views do.
//!
//! The shape is fixed when the pane is built — a vertical `GtkPaned` per divider, never
//! reparented — and a section that does not apply to the tab in front is hidden rather than
//! taken out. A divider with a shut section beside it lies against that section, which then
//! shows its header alone.

use crate::widgets::status_page;
use accent_core::config::InfoPane;
use adw::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// The pane's icon, and the one its empty state is drawn with.
pub(super) const ICON: &str = "dialog-information-symbolic";

/// One section: a header that folds it, with its title and a count, over its body.
pub(super) struct Section {
    pub(super) root: gtk::Expander,
    title: gtk::Label,
    count: gtk::Label,
}

impl Section {
    fn new(title: &str, body: &impl IsA<gtk::Widget>) -> Section {
        let title = gtk::Label::builder().label(title).xalign(0.0).build();
        title.add_css_class("caption-heading");
        let count = gtk::Label::builder().visible(false).build();
        count.add_css_class("caption");
        count.add_css_class("dim-label");
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        header.append(&title);
        header.append(&count);
        let root = gtk::Expander::builder()
            .label_widget(&header)
            .child(body)
            .expanded(true)
            .build();
        root.add_css_class("accent-section");
        Section { root, title, count }
    }

    pub(super) fn set_title(&self, title: &str) {
        self.title.set_text(title);
    }

    /// How many rows the section holds. `None` and none at all show no number, so neither an
    /// answer still coming nor an empty one reads as "0".
    pub(super) fn set_count(&self, n: Option<usize>) {
        let n = n.filter(|n| *n > 0);
        self.count.set_visible(n.is_some());
        if let Some(n) = n {
            self.count.set_text(&n.to_string());
        }
    }

    /// Shown and unfolded.
    pub(super) fn is_open(&self) -> bool {
        self.root.is_visible() && self.root.is_expanded()
    }
}

/// A divider between two parts of the pane. Where it sits while both sides are open is a share
/// of its height, so it survives the side beside it being shut and opened again.
struct Divider {
    paned: gtk::Paned,
    /// The start side's share, from where the reader left the divider; `None` is an even split.
    share: Cell<Option<f64>>,
    /// Whether both sides were open the last time the pane was fitted.
    both: Cell<bool>,
    /// A share waiting for the paned to be given a height.
    pending: Rc<Cell<Option<f64>>>,
}

impl Divider {
    fn new(start: &impl IsA<gtk::Widget>, end: &impl IsA<gtk::Widget>) -> Divider {
        let paned = gtk::Paned::builder()
            .orientation(gtk::Orientation::Vertical)
            .start_child(start)
            .end_child(end)
            .shrink_start_child(false)
            .shrink_end_child(false)
            .vexpand(true)
            .build();
        // A paned learns its height in its first allocation, which is also the first time its
        // range moves: a share asked for before then is put in place there.
        let pending: Rc<Cell<Option<f64>>> = Rc::default();
        paned.connect_max_position_notify({
            let pending = pending.clone();
            move |paned| {
                if let Some(share) = pending.take() {
                    paned.set_position(at(paned, share));
                }
            }
        });
        Divider {
            paned,
            share: Cell::new(None),
            both: Cell::new(false),
            pending,
        }
    }

    /// Lay the divider out for which of its sides are open: at its share while both are, else
    /// against the shut side, `GtkPaned` sizing that side to its header when no position is set
    /// and only the other side takes the space.
    fn fit(&self, start: bool, end: bool, even: f64) {
        let both = start && end;
        if self.both.get() && !both {
            self.share.set(self.measured());
        }
        let was = self.both.replace(both);
        self.paned.set_resize_start_child(start);
        self.paned.set_resize_end_child(end || !start);
        match both {
            true if !was => self.place(self.share.get().unwrap_or(even)),
            true => {}
            false => {
                self.pending.set(None);
                self.paned.set_position(-1);
            }
        }
    }

    fn place(&self, share: f64) {
        match self.paned.height() {
            0 => self.pending.set(Some(share)),
            _ => {
                self.pending.set(None);
                self.paned.set_position(at(&self.paned, share));
            }
        }
    }

    /// The share to remember: where the divider is while both sides are open, else where it was.
    fn measured(&self) -> Option<f64> {
        match (self.both.get(), self.paned.height()) {
            (true, height) if height > 0 => {
                Some(f64::from(self.paned.position()) / f64::from(height))
            }
            _ => self.share.get(),
        }
    }

    /// Back to an even split: a double-click on the handle.
    fn reset(&self, even: f64) {
        self.share.set(None);
        if self.both.get() {
            self.place(even);
        }
    }
}

/// `share` of the paned's height, in pixels.
fn at(paned: &gtk::Paned, share: f64) -> i32 {
    (share * f64::from(paned.height())).round() as i32
}

pub(super) struct Info {
    /// "sections", or "nothing" while no file is in front.
    pub(super) root: gtk::Stack,
    pub(super) references: Section,
    pub(super) tags: Section,
    divider: Divider,
    /// The tab the pane was last fitted for, and whether a picked tag showed the Tags section
    /// over it anyway: it stays until another tab comes to the front.
    shown_for: RefCell<String>,
    tags_forced: Cell<bool>,
}

impl Info {
    pub(super) fn new(
        references: &impl IsA<gtk::Widget>,
        tags: &impl IsA<gtk::Widget>,
    ) -> Rc<Info> {
        let references = Section::new("References", references);
        let tags = Section::new("Tags", tags);
        let divider = Divider::new(&references.root, &tags.root);
        let root = gtk::Stack::builder().vexpand(true).build();
        root.add_named(&divider.paned, Some("sections"));
        root.add_named(
            &status_page(
                ICON,
                "No File",
                "Open a file from this vault to see what links to it.",
            ),
            Some("nothing"),
        );
        let info = Rc::new(Info {
            root,
            references,
            tags,
            divider,
            shown_for: RefCell::default(),
            tags_forced: Cell::new(false),
        });
        for section in [&info.references, &info.tags] {
            let weak = Rc::downgrade(&info);
            section.root.connect_expanded_notify(move |_| {
                if let Some(info) = weak.upgrade() {
                    info.fit();
                }
            });
        }
        info
    }

    pub(super) fn section(&self, name: &str) -> Option<&Section> {
        match name {
            "references" => Some(&self.references),
            "tags" => Some(&self.tags),
            _ => None,
        }
    }

    /// Lay the dividers out for the sections now open. Called whenever one opens, shuts, shows
    /// or hides.
    pub(super) fn fit(&self) {
        let (references, tags) = (self.references.is_open(), self.tags.is_open());
        self.divider.fit(references, tags, 0.5);
    }

    /// Show what applies to the tab `key` (empty for none): the sections for a file, the Tags
    /// section over a note, and a status page for no file at all.
    pub(super) fn sync(&self, key: &str, file: bool, note: bool) {
        if *self.shown_for.borrow() != key {
            self.shown_for.replace(key.to_string());
            self.tags_forced.set(false);
        }
        let forced = self.tags_forced.get();
        self.tags.root.set_visible(note || forced);
        self.root.set_visible_child_name(match file || forced {
            true => "sections",
            false => "nothing",
        });
        self.fit();
    }

    /// Open the section `name` without moving the keyboard. The Tags section shows even where
    /// the tab in front would hide it, until another tab comes to the front.
    pub(super) fn open(&self, name: &str) {
        let Some(section) = self.section(name) else {
            return;
        };
        if name == "tags" {
            self.tags_forced.set(true);
            section.root.set_visible(true);
            self.root.set_visible_child_name("sections");
        }
        section.root.set_expanded(true);
        self.fit();
    }

    /// Whether `paned` is one of the pane's dividers, putting it back to an even split if so.
    pub(super) fn reset_divider(&self, paned: &gtk::Paned) -> bool {
        let found = paned == &self.divider.paned;
        if found {
            self.divider.reset(0.5);
        }
        found
    }

    /// The sections as the session keeps them.
    pub(super) fn saved(&self) -> InfoPane {
        InfoPane {
            references: self.references.root.is_expanded(),
            tags: self.tags.root.is_expanded(),
            dividers: [self.divider.measured()],
        }
    }

    /// The page shown, each section as `Title:open|shut|hidden` and its count, and the divider
    /// as `position/height`: what `ACCENT_BENCH_INFO` prints.
    #[cfg(feature = "bench")]
    pub(super) fn state(&self) -> String {
        let sections: Vec<String> = [&self.references, &self.tags]
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
        let paned = &self.divider.paned;
        format!(
            "page={} {} divider={}/{}",
            self.root.visible_child_name().unwrap_or_default(),
            sections.join(" "),
            paned.position(),
            paned.height()
        )
    }

    pub(super) fn restore(&self, saved: &InfoPane) {
        self.divider.share.set(saved.dividers[0]);
        // Placed again even where both sides stay open, so the share read back is the one shown.
        self.divider.both.set(false);
        self.references.root.set_expanded(saved.references);
        self.tags.root.set_expanded(saved.tags);
        self.fit();
    }
}
