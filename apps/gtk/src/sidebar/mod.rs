//! The left sidebar: Files / Search / Tags / References / Git / Ports / Outline in a view
//! switcher.
//!
//! The pane knows nothing about the vault. The file tree arrives as a finished widget and every
//! query goes through a closure in [`Data`], so this module never touches app state and the
//! integration step only has to hand it the closures. Each pane gets its own half of [`Data`]:
//! the Ports pane is built with what forwards a port and nothing else, which is what keeps a
//! pane from being wired to machinery it never calls.

mod outline;
mod ports;
mod search;
mod tags;
mod widgets;

pub use outline::{outline_list, outline_note};
pub use ports::Data as PortsData;
pub use search::{Answer, Data as SearchData, Query};
pub use tags::Data as TagsData;

use adw::prelude::*;
use std::cell::Cell;
use std::ops::Range;
use std::rc::Rc;
use widgets::path_list;

use crate::widgets::{scroller, status_page};

/// Notes pointing back at the open one, as an arrow returning to where it came from. Adwaita's one
/// link-named glyph, `insert-link-symbolic`, is a text-insertion mark (two rules over a caret): it
/// reads as "paste a link here" rather than "what links here", and it is the only icon of the four
/// whose artwork is off centre, sitting a pixel low in its 16 px box.
const BACKLINK_ICON: &str = "mail-reply-sender-symbolic";
/// Arrows leaving and arriving: the pane is about what has gone out and what is still to come in.
/// The reading is the one this pane had all along; the name is not. `network-transmit-receive`
/// draws as two arrows in Adwaita but as a boxed device in WhiteSur, where the Git tab read as a
/// network port — the artwork is the theme's, so a name whose glyph is arrows in both is the one
/// to hold (DESIGN.md, Iconography). Adwaita 50 has no git, branch or history glyph at all, so
/// this follows the precedent the References pane set: a mail name whose drawing says the
/// right thing.
const GIT_ICON: &str = "mail-send-receive-symbolic";
/// Open a note, over the byte range of the match when the row that was activated names one.
type OnOpen = Rc<dyn Fn(&str, Option<Range<usize>>)>;

/// Everything the sidebar needs from the index, as closures so it never sees a vault handle.
/// One field per pane that reads one: a pane is built with its own half and cannot reach the
/// rest.
pub struct Data {
    pub search: search::Data,
    pub tags: tags::Data,
    pub ports: ports::Data,
}

pub struct Sidebar {
    root: gtk::Widget,
    switcher: gtk::Widget,
    stack: adw::ViewStack,
    /// Everything that needs an index behind it. `None` in a window with no vault, where the
    /// Outline pane is the only one there is.
    panes: Option<VaultPanes>,
    /// Whatever the Outline pane is showing. A `Bin` rather than a list of its own, because what
    /// belongs in it depends entirely on the open tab: a note's headings, a PDF's bookmarks and
    /// thumbnails, or a sentence saying why there is nothing.
    outline_bin: adw::Bin,
}

/// The panes that read the vault's index.
struct VaultPanes {
    search_entry: gtk::SearchEntry,
    replace_toggle: gtk::ToggleButton,
    all_toggle: gtk::ToggleButton,
    replace_entry: gtk::Entry,
    restart_search: Rc<dyn Fn()>,
    references: gtk::StringList,
    references_stack: gtk::Stack,
    /// The empty page of the References pane. Its words change with what the tab holds — a note
    /// has backlinks, a source file has references — so they are set rather than built in.
    references_empty: adw::StatusPage,
    tags_dirty: Rc<Cell<bool>>,
    tags_divider: gtk::Paned,
    /// The Git pane's page, so it can be hidden: a vault under no version control has nothing to
    /// put in it, and one more icon in the switcher is one more thing to explain.
    git_page: adw::ViewStackPage,
    git_divider: gtk::Paned,
    /// The Ports pane's page, hidden for the same reason the Git one is: a vault on this machine
    /// has no ssh connection to forward anything over.
    ports_page: adw::ViewStackPage,
    select_tag: Rc<dyn Fn(&str)>,
}

impl Sidebar {
    /// `files` is the existing vault tree widget, dropped into the Files pane unchanged.
    /// `vault` carries the tree widget and the index closures behind Files, Search, Tags and
    /// References; `None` builds a sidebar with only the Outline pane, which is what a window
    /// opened on a single file has to show. `on_open` is called with a vault-relative path when
    /// the user activates a result, a tagged file or a reference, plus the byte range the match
    /// covers when the row is one.
    /// `on_reference` is called with a References row, which carries a line number of its own.
    pub fn new(
        vault: Option<(gtk::Widget, Data, gtk::Widget, gtk::Paned)>,
        on_open: impl Fn(&str, Option<Range<usize>>) + 'static,
        on_reference: impl Fn(&str) + 'static,
    ) -> Sidebar {
        let on_open: OnOpen = Rc::new(on_open);
        let stack = adw::ViewStack::builder().vexpand(true).build();

        // Every pane but the outline is a view of an index, so without one there is nothing for
        // them to show and they are not built at all.
        let panes = vault.map(|(files, data, git, git_divider)| {
            stack.add_titled_with_icon(&files, Some("files"), "Files", "folder-symbolic");

            let search = search::pane(&Rc::new(data.search), &on_open);
            stack.add_titled_with_icon(
                &search.widget,
                Some("search"),
                "Search",
                "system-search-symbolic",
            );

            let tags = tags::pane(&Rc::new(data.tags), &on_open);
            stack.add_titled_with_icon(
                &tags.widget,
                Some("tags"),
                "Tags",
                "user-bookmarks-symbolic",
            );

            let references = gtk::StringList::new(&[]);
            let (references_stack, references_empty) = references_body(&references, on_reference);
            stack.add_titled_with_icon(
                &references_stack,
                Some("references"),
                "References",
                BACKLINK_ICON,
            );

            stack.add_titled_with_icon(&git, Some("git"), "Git", GIT_ICON);
            let git_page = stack.page(&git);
            // Hidden until the pane says there is a repository, which is one refresh away.
            git_page.set_visible(false);

            let ports = ports::pane(&Rc::new(data.ports));
            stack.add_titled_with_icon(&ports, Some("ports"), "Ports", ports::ICON);
            let ports_page = stack.page(&ports);
            // Hidden until the window says its vault is on another machine.
            ports_page.set_visible(false);

            // Lazy fill: a background reindex only flips the flag, so it costs no query while
            // the user is looking at Files or Search.
            stack.connect_visible_child_notify({
                let (dirty, refill) = (tags.dirty.clone(), tags.refill.clone());
                move |stack| {
                    if stack.visible_child_name().as_deref() == Some("tags") && dirty.replace(false)
                    {
                        refill();
                    }
                }
            });
            (
                search,
                tags,
                (references, references_stack, references_empty),
                git_page,
                git_divider,
                ports_page,
            )
        });

        let outline_bin = adw::Bin::builder().vexpand(true).build();
        outline_bin.set_child(Some(&outline::empty()));
        stack.add_titled_with_icon(&outline_bin, Some("outline"), "Outline", outline::ICON);

        // Icons, because four labels do not fit a 200 px sidebar without truncating. The switcher
        // gives every toggle the page title as its tooltip, so icon-only stays discoverable. No
        // vertical alignment of its own: as a header title widget it takes the header's full
        // content height, which is what puts its toggles on the same line as the buttons opposite.
        // Centred rather than filling: a toggle that stretches to the header's height comes out
        // taller than it is wide, and these are square icon buttons.
        let switcher = adw::InlineViewSwitcher::builder()
            .stack(&stack)
            .display_mode(adw::InlineViewSwitcherDisplayMode::Icons)
            .margin_start(6)
            .margin_end(6)
            .valign(gtk::Align::Center)
            .build();
        switcher.add_css_class("flat");

        Sidebar {
            root: stack.clone().upcast(),
            switcher: switcher.upcast(),
            stack,
            panes: panes.map(
                |(search, tags, references, git_page, git_divider, ports_page)| {
                    let (references, references_stack, references_empty) = references;
                    VaultPanes {
                        search_entry: search.entry,
                        replace_toggle: search.replace_toggle,
                        all_toggle: search.all_toggle,
                        replace_entry: search.replace_entry,
                        restart_search: search.restart,
                        references,
                        references_stack,
                        references_empty,
                        tags_dirty: tags.dirty,
                        tags_divider: tags.divider,
                        git_page,
                        git_divider,
                        ports_page,
                        select_tag: tags.select,
                    }
                },
            ),
            outline_bin,
        }
    }

    /// Put `divider` back to its default if it is one of the sidebar's own, reporting whether it
    /// was. The window's double-click handler asks every divider owner in turn, so the rule for a
    /// pane lives next to the pane rather than in the shell.
    pub fn reset_divider(&self, divider: &gtk::Paned) -> bool {
        let Some(panes) = self.panes.as_ref() else {
            return false;
        };
        let share = if divider == &panes.tags_divider {
            tags::SHARE
        } else if divider == &panes.git_divider {
            crate::git::GIT_SHARE
        } else {
            return false;
        };
        divider.set_position(divider.height() * share.0 / share.1);
        true
    }

    /// The pane switcher, the title widget of the sidebar header so that it shares the header band
    /// with the main header instead of taking a band of its own. `AdwHeaderBar` centres a title
    /// widget, so this hands out the switcher itself with no wrapper to do the centring.
    pub fn switcher(&self) -> &gtk::Widget {
        &self.switcher
    }

    /// The panes themselves, for the sidebar column's content.
    pub fn widget(&self) -> &gtk::Widget {
        &self.root
    }

    /// Replace what the References pane lists, and say what its emptiness would mean: a note's
    /// backlinks and a source file's references are the same pane asking different questions.
    pub fn set_references(&self, rows: &[String], empty: (&str, &str)) {
        let Some(panes) = self.panes.as_ref() else {
            return;
        };
        let rows: Vec<&str> = rows.iter().map(String::as_str).collect();
        panes
            .references
            .splice(0, panes.references.n_items(), rows.as_slice());
        panes.references_empty.set_title(empty.0);
        panes.references_empty.set_description(Some(empty.1));
        panes
            .references_stack
            .set_visible_child_name(if rows.is_empty() { "empty" } else { "list" });
    }

    /// Which pane is on screen, for the callers that only refresh what is being looked at.
    pub fn is_showing(&self, name: &str) -> bool {
        self.stack.visible_child_name().as_deref() == Some(name)
    }

    /// Whether this sidebar has the named pane at all, and is showing it. A window with no vault
    /// has only the outline, and a vault with no repository has no Git pane, so the chords for
    /// the others must not open a column that cannot answer them.
    pub fn has_pane(&self, name: &str) -> bool {
        self.stack
            .child_by_name(name)
            .is_some_and(|child| self.stack.page(&child).is_visible())
    }

    /// Show or hide the Git pane. It starts hidden and the pane's first refresh decides.
    pub fn set_git_visible(&self, on: bool) {
        if let Some(panes) = self.panes.as_ref() {
            panes.git_page.set_visible(on);
        }
    }

    /// Show or hide the Ports pane. It starts hidden; the window turns it on once its vault
    /// turns out to be on another machine.
    pub fn set_ports_visible(&self, on: bool) {
        if let Some(panes) = self.panes.as_ref() {
            panes.ports_page.set_visible(on);
        }
    }

    /// Replace what the Outline pane shows; `None` puts the empty state back.
    pub fn set_outline(&self, content: Option<&gtk::Widget>) {
        match content {
            Some(widget) => self.outline_bin.set_child(Some(widget)),
            None => self.outline_bin.set_child(Some(&outline::empty())),
        }
    }

    /// What the Outline pane is showing, which is what `ACCENT_BENCH_TABS` reads to say whether a
    /// closed document left its outline behind.
    pub fn outline_child(&self) -> Option<gtk::Widget> {
        self.outline_bin.child()
    }

    /// The tag list is out of date; refill it the next time the Tags pane is shown.
    pub fn mark_tags_dirty(&self) {
        if let Some(panes) = self.panes.as_ref() {
            panes.tags_dirty.set(true);
        }
    }

    /// Show a pane by name: "files", "search", "tags", "references", "git", "ports" or "outline",
    /// focusing its entry where there is one.
    pub fn show_pane(&self, name: &str) {
        // A pane this sidebar does not have leaves it where it was, which for a window with no
        // vault means the outline stays up whatever chord was pressed.
        if !self.has_pane(name) {
            return;
        }
        self.stack.set_visible_child_name(name);
        if let (Some(panes), "search") = (self.panes.as_ref(), name) {
            panes.search_entry.grab_focus();
        }
    }

    /// Ctrl+Shift+H: the Search pane with its replace row open, focused where there is still
    /// something to type.
    pub fn show_replace(&self) {
        let Some(panes) = self.panes.as_ref() else {
            return;
        };
        self.show_pane("search");
        panes.replace_toggle.set_active(true);
        if !panes.search_entry.text().is_empty() {
            panes.replace_entry.grab_focus();
        }
    }

    /// Put `text` in the Search pane's box, which runs it: Ctrl+Shift+F or Ctrl+Shift+H over a
    /// selection searches for what is selected. Nothing selected never reaches here, so the box
    /// then keeps whatever it already holds — VS Code's behaviour.
    pub fn set_search_text(&self, text: &str) {
        if let Some(panes) = self.panes.as_ref() {
            panes.search_entry.set_text(text);
        }
    }

    /// Flip the Search pane's All toggle, showing the pane first: the same thing clicking the
    /// button does, for the palette and for anyone who binds a chord to it.
    pub fn toggle_search_all(&self) {
        let Some(panes) = self.panes.as_ref() else {
            return;
        };
        self.show_pane("search");
        panes.all_toggle.set_active(!panes.all_toggle.is_active());
    }

    /// Ask the search question again, if one is on screen. What the window calls when the answer
    /// would have changed without the box being touched — a git refresh moving the ignore set.
    pub fn requery_search(&self) {
        let Some(panes) = self.panes.as_ref() else {
            return;
        };
        if self.stack.visible_child_name().as_deref() == Some("search") {
            (panes.restart_search)();
        }
    }

    /// Show the Tags pane with `tag` already selected.
    pub fn show_tag(&self, tag: &str) {
        // Ordering matters: this refills the tag list if it is dirty, and the refill clears the
        // selection, so the tag has to be picked afterwards.
        let Some(panes) = self.panes.as_ref() else {
            return;
        };
        self.show_pane("tags");
        (panes.select_tag)(tag);
    }
}

/// The References pane: the rows, and the page shown instead when there are none.
fn references_body(
    model: &gtk::StringList,
    on_reference: impl Fn(&str) + 'static,
) -> (gtk::Stack, adw::StatusPage) {
    let stack = gtk::Stack::builder().vexpand(true).build();
    // What the pane says before any tab has been opened; from then on the window sets the words
    // to suit what the tab holds (`references::references_empty`).
    let empty = status_page(
        BACKLINK_ICON,
        "No References",
        "Open a note to see what links to it.",
    );
    stack.add_named(&empty, Some("empty"));
    stack.add_named(&scroller(&path_list(model, on_reference)), Some("list"));
    stack.set_visible_child_name("empty");
    (stack, empty)
}
