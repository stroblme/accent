//! The Info pane: what links to the file in front, the vault's tags and what the file is, as
//! sections one above the other. Each folds to its header; the open ones share the height across
//! dividers the reader drags, as VS Code's views do.
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

/// A share of a divider's height, where the paned's own handlers can reach it.
type Share = Rc<Cell<Option<f64>>>;

/// A divider between two parts of the pane. While both sides are open it is held at an even
/// split of what is open, or at the share of its height the reader dragged it to, which it keeps
/// through the side beside it being shut and opened again.
struct Divider {
    paned: gtk::Paned,
    /// The start side's share where the reader last dragged the divider; `None` until they do,
    /// which keeps it at an even split however many sections come and go.
    share: Share,
    /// The share the divider is held at while both sides are open; `None` while one is shut.
    held: Share,
    /// Whether a placing is waiting for the frame's layout.
    placing: Rc<Cell<bool>>,
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
        let (share, held): (Share, Share) = Default::default();
        // A drag is told from every other move by the class `paned::watch` puts on a handle
        // while it is held: the pane placing the divider, and GTK handing a resize out, are not
        // the reader's.
        paned.connect_position_notify({
            let (share, held) = (share.clone(), held.clone());
            move |paned| {
                let height = paned.height();
                if paned.has_css_class(crate::paned::DRAGGING) && held.get().is_some() && height > 0
                {
                    let dragged = f64::from(paned.position()) / f64::from(height);
                    share.set(Some(dragged));
                    held.set(Some(dragged));
                }
            }
        });
        Divider {
            paned,
            share,
            held,
            placing: Rc::default(),
        }
    }

    /// Lay the divider out for which of its sides are open: while both are, at the reader's
    /// share or else at `even`, which moves with the sections open; else against the shut side,
    /// `GtkPaned` sizing that side to its header when no position is set and only the other side
    /// takes the space.
    fn fit(&self, start: bool, end: bool, even: f64) {
        self.paned.set_resize_start_child(start);
        self.paned.set_resize_end_child(end || !start);
        let held = (start && end).then(|| self.share.get().unwrap_or(even));
        self.held.set(held);
        match held {
            Some(_) => self.place(),
            None => self.paned.set_position(-1),
        }
    }

    /// Put the divider at its held share of the height it is about to have.
    ///
    /// In the frame's layout, after the window's own handler has allocated: a section shown or
    /// shut above this divider changes its height in the same turn, so the height it has now is
    /// the one it is leaving, and `GtkPaned` hands the change out by the old position — or not at
    /// all, where it last laid out one side alone. What is moved there is laid out again within
    /// the frame (`App::restyle_all` leans on the same order). A paned not realized yet is placed
    /// when the pane is mapped.
    fn place(&self) {
        let Some(clock) = self.paned.frame_clock() else {
            return;
        };
        if self.placing.replace(true) {
            return;
        }
        let handler = Rc::new(RefCell::new(None));
        let id = clock.connect_layout({
            let (paned, held, placing) = (
                self.paned.downgrade(),
                self.held.clone(),
                self.placing.clone(),
            );
            let handler = handler.clone();
            move |clock| {
                if let Some(id) = handler.take() {
                    clock.disconnect(id);
                }
                placing.set(false);
                if let (Some(paned), Some(share)) = (paned.upgrade(), held.get())
                    && paned.height() > 0
                {
                    paned.set_position(at(&paned, share));
                }
            }
        });
        *handler.borrow_mut() = Some(id);
        clock.request_phase(gtk::gdk::FrameClockPhase::LAYOUT);
    }

    /// Back to an even split: a double-click on the handle.
    fn reset(&self, even: f64) {
        self.share.set(None);
        if self.held.get().is_some() {
            self.held.set(Some(even));
            self.place();
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
    pub(super) details: Section,
    /// References over the two sections below it, and Tags over Details.
    dividers: [Divider; 2],
    /// Whether the window has a vault: without an index, References and Tags have nothing to
    /// say, and stay hidden.
    vault: bool,
    /// The tab the pane was last fitted for, and whether a picked tag showed the Tags section
    /// over it anyway: it stays until another tab comes to the front.
    shown_for: RefCell<String>,
    tags_forced: Cell<bool>,
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
        let lower = Divider::new(&tags.root, &details.root);
        let upper = Divider::new(&references.root, &lower.paned);
        let root = gtk::Stack::builder().vexpand(true).build();
        root.add_named(&upper.paned, Some("sections"));
        let open_one = match vault {
            Some(_) => "Open a file to see what links to it and what it is.",
            None => "Open a file to see what it is.",
        };
        root.add_named(&status_page(ICON, "No File", open_one), Some("nothing"));
        let info = Rc::new(Info {
            root,
            references,
            tags,
            details,
            dividers: [upper, lower],
            vault: vault.is_some(),
            shown_for: RefCell::default(),
            tags_forced: Cell::new(false),
        });
        // A paned has its height once the pane is on screen, which is where the dividers placed
        // before then go.
        info.root.connect_map({
            let weak = Rc::downgrade(&info);
            move |_| {
                if let Some(info) = weak.upgrade() {
                    info.fit();
                }
            }
        });
        for section in [&info.references, &info.tags, &info.details] {
            let weak = Rc::downgrade(&info);
            section.root.connect_expanded_notify(move |_| {
                if let Some(info) = weak.upgrade() {
                    info.fit();
                }
            });
        }
        info.sync("", false, false);
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

    /// Each divider's even split: a third for References over two open sections, else half.
    fn evens(&self) -> [f64; 2] {
        match self.tags.is_open() && self.details.is_open() {
            true => [1.0 / 3.0, 0.5],
            false => [0.5, 0.5],
        }
    }

    /// Lay the dividers out for the sections now open. Called whenever one opens, shuts, shows
    /// or hides.
    pub(super) fn fit(&self) {
        let (references, tags, details) = (
            self.references.is_open(),
            self.tags.is_open(),
            self.details.is_open(),
        );
        let [upper, lower] = self.evens();
        self.dividers[1].fit(tags, details, lower);
        self.dividers[0].fit(references, tags || details, upper);
    }

    /// Show what applies to the tab `key` (empty for none): the sections for a file, the Tags
    /// section over a note, and a status page for no file at all.
    pub(super) fn sync(&self, key: &str, file: bool, note: bool) {
        if *self.shown_for.borrow() != key {
            self.shown_for.replace(key.to_string());
            self.tags_forced.set(false);
        }
        let forced = self.tags_forced.get();
        self.references.root.set_visible(self.vault);
        self.tags.root.set_visible(self.vault && (note || forced));
        self.details.root.set_visible(file);
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
        let evens = self.evens();
        let found = self.dividers.iter().position(|d| &d.paned == paned);
        if let Some(i) = found {
            self.dividers[i].reset(evens[i]);
        }
        found.is_some()
    }

    /// The sections as the session keeps them.
    pub(super) fn saved(&self) -> InfoPane {
        InfoPane {
            references: self.references.root.is_expanded(),
            tags: self.tags.root.is_expanded(),
            details: self.details.root.is_expanded(),
            dividers: self.dividers.each_ref().map(|divider| divider.share.get()),
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
        let dividers: Vec<String> = self
            .dividers
            .iter()
            .map(|d| format!("{}/{}", d.paned.position(), d.paned.height()))
            .collect();
        format!(
            "page={} {} dividers={}",
            self.root.visible_child_name().unwrap_or_default(),
            sections.join(" "),
            dividers.join(",")
        )
    }

    pub(super) fn restore(&self, saved: &InfoPane) {
        for (divider, share) in self.dividers.iter().zip(saved.dividers) {
            divider.share.set(share);
        }
        self.references.root.set_expanded(saved.references);
        self.tags.root.set_expanded(saved.tags);
        self.details.root.set_expanded(saved.details);
        self.fit();
    }
}
