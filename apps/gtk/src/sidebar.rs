//! The left sidebar: Files / Search / Tags / References in a view switcher.
//!
//! The pane knows nothing about the vault. The file tree arrives as a finished widget and every
//! query goes through a closure in [`Data`], so this module never touches app state and the
//! integration step only has to hand it six closures.
//!
//! Search runs off the main loop. [`Data::search`] is called on a worker thread, so a full-vault
//! query never costs a keystroke; a bar pulsing above the results says one is running and the
//! previous results stay on screen until the new ones arrive. Exactly one query is in flight at a
//! time: when it lands and the box has moved on since, the current one is started instead of
//! painted.

use accent_core::index::{Match, SearchHit};
use accent_core::search::{self, Options, Regex};
use adw::prelude::*;
use gtk::{gio, glib, pango};
use std::cell::{Cell, Ref, RefCell};
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Same value as the palette and the switcher (DESIGN.md, Motion): long enough to swallow a burst
/// of keystrokes, short enough to feel immediate.
const DEBOUNCE: Duration = Duration::from_millis(50);
/// One step of the search progress bar. GTK4 has no indeterminate mode, so the bar is stepped by
/// a timer of ours; at the default pulse step this crosses the trough in about two seconds.
const PULSE: Duration = Duration::from_millis(80);
/// How many pulses a query has to outlive before its bar is drawn at all (DESIGN.md, Loading).
/// Nothing else in the window starts a search, so a query the user did not ask for — the requery
/// a changed file triggers under a finished search — is over inside this and never draws one.
const SHOW_AFTER: u32 = 2;
/// Notes pointing back at the open one, as an arrow returning to where it came from. Adwaita's one
/// link-named glyph, `insert-link-symbolic`, is a text-insertion mark (two rules over a caret): it
/// reads as "paste a link here" rather than "what links here", and it is the only icon of the four
/// whose artwork is off centre, sitting a pixel low in its 16 px box.
const BACKLINK_ICON: &str = "mail-reply-sender-symbolic";
const OUTLINE_ICON: &str = "view-list-bullet-symbolic";
/// Arrows leaving and arriving: the pane is about what has gone out and what is still to come in.
/// The reading is the one this pane had all along; the name is not. `network-transmit-receive`
/// draws as two arrows in Adwaita but as a boxed device in WhiteSur, where the Git tab read as a
/// network port — the artwork is the theme's, so a name whose glyph is arrows in both is the one
/// to hold (DESIGN.md, Iconography). Adwaita 50 has no git, branch or history glyph at all, so
/// this follows the precedent the References pane set: a mail name whose drawing says the
/// right thing.
const GIT_ICON: &str = "mail-send-receive-symbolic";
/// Two machines wired together, which is what a forward is: a port on one cabled to a port on the
/// other. The rest of Adwaita's network names are signal strengths, a server tower or a VPN
/// shield — none of them a port.
const PORTS_ICON: &str = "network-wired-symbolic";

/// How far each heading level is indented in the Outline pane, on the 6/12/18 spacing scale.
const OUTLINE_INDENT: i32 = 12;

/// Open a note, over the byte range of the match when the row that was activated names one.
type OnOpen = Rc<dyn Fn(&str, Option<Range<usize>>)>;

/// One query, already compiled. Built on the main thread from what the search box says, so an
/// invalid pattern is reported without a worker thread being spent on it.
pub enum Query {
    /// Ranked full text: what a plain query with no toggle means, and the fast path. The flag is
    /// the All toggle — search what git ignores and what the walk skipped, as well.
    Fts(String, bool),
    /// Exact matching over bodies, one result row per match. `all` means what it does above.
    ///
    /// The pattern travels as what was typed plus the toggles rather than as the compiled
    /// `Regex`: the vault may be on another machine, and case-insensitivity lives in the builder
    /// rather than in the pattern string, so the string alone would quietly change the search.
    /// It is still compiled here first, so an unusable pattern costs no worker thread.
    Grep {
        text: String,
        options: Options,
        all: bool,
    },
}

/// What a [`Query`] answered. The `usize` is how many of the matches a Replace All would rewrite,
/// which the capped list cannot give and which is smaller than the list: the rewrite is notes
/// only, while the rows reach every text file.
pub enum Answer {
    Fts(Vec<SearchHit>),
    Grep(Vec<Match>, usize),
}

/// Everything the sidebar needs from the index, as closures so it never sees a vault handle.
// Boxed closures returning a `Vec` are the whole point of this struct; a type alias per field would
// only hide the signature the caller has to write.
#[allow(clippy::type_complexity)]
pub struct Data {
    /// Runs on a worker thread, so it may touch nothing the main loop owns.
    pub search: Arc<dyn Fn(Query) -> Answer + Send + Sync>,
    pub tags: Box<dyn Fn() -> Vec<(String, i64)>>,
    pub files_with_tag: Box<dyn Fn(&str) -> Vec<String>>,
    /// Rewrite every match in the vault. `literal` says whether `$1` in the replacement is a
    /// capture group or two characters. It writes one note at a time, so it runs off the main
    /// loop and calls `done` there once it has: the pane stays busy until then.
    pub replace_all: Box<dyn Fn(String, Options, String, bool, Box<dyn FnOnce()>)>,
    /// Forward a remote port to a local one, or stop forwarding it. Answers an error message when
    /// ssh refuses, which is what the pane shows.
    pub add_forward: Box<dyn Fn(u16, u16) -> Result<(), String>>,
    pub remove_forward: Box<dyn Fn(u16, u16)>,
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
            let data = Rc::new(data);
            stack.add_titled_with_icon(&files, Some("files"), "Files", "folder-symbolic");

            let search = search_pane(&data, &on_open);
            stack.add_titled_with_icon(
                &search.widget,
                Some("search"),
                "Search",
                "system-search-symbolic",
            );

            let tags = tags_pane(&data, &on_open);
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

            let ports = ports_pane(&data);
            stack.add_titled_with_icon(&ports, Some("ports"), "Ports", PORTS_ICON);
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
        outline_bin.set_child(Some(&outline_empty()));
        stack.add_titled_with_icon(&outline_bin, Some("outline"), "Outline", OUTLINE_ICON);

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
            TAGS_SHARE
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
            None => self.outline_bin.set_child(Some(&outline_empty())),
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

// --- pure helpers, the only part of this module the tests can reach ------------------------------

/// Whether the search progress bar is drawn after `pulses` steps of a query still running.
fn shows_bar(pulses: u32) -> bool {
    pulses >= SHOW_AFTER
}

/// FTS5 wraps matched terms in `«` and `»` (see `Index::search`). Escape first, so a note holding a
/// literal `<` or `&` cannot corrupt the markup, and only then turn the markers into bold. A note
/// can contain those guillemets itself, so nesting is counted rather than substituted blindly and
/// an unclosed run is closed at the end; the result always parses.
fn snippet_markup(snippet: &str) -> String {
    let escaped = glib::markup_escape_text(snippet);
    let mut out = String::with_capacity(escaped.len() + 7);
    let mut depth = 0usize;
    for c in escaped.chars() {
        match c {
            '«' => {
                if depth == 0 {
                    out.push_str("<b>");
                }
                depth += 1;
            }
            '»' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    out.push_str("</b>");
                }
            }
            // Runs of whitespace collapse to one space, and that includes the newlines an FTS
            // snippet carries out of the note body. Pango counts `lines(2)` per paragraph, so a
            // snippet spanning a frontmatter block rendered a dozen lines and one hit filled the
            // pane; as a single paragraph the row ellipsizes after two lines as intended.
            c if c.is_whitespace() => {
                if !out.ends_with(' ') && !out.is_empty() {
                    out.push(' ');
                }
            }
            _ => out.push(c),
        }
    }
    if depth > 0 {
        out.push_str("</b>");
    }
    out.truncate(out.trim_end().len());
    out
}

/// A grep row's snippet: the matched line with the match in bold, or, once the replace row is
/// open, the match struck through beside what it would become. `accent` is the only colour this
/// module names and it comes from the style manager (DESIGN.md, Colour).
fn match_markup(line: &str, range: Range<usize>, replaced: Option<&str>, accent: &str) -> String {
    let esc = |s: &str| glib::markup_escape_text(s).to_string();
    let (before, matched, after) = (
        &line[..range.start],
        &line[range.clone()],
        &line[range.end..],
    );
    match replaced {
        None => format!("{}<b>{}</b>{}", esc(before), esc(matched), esc(after)),
        Some(new) => format!(
            "{}<s>{}</s> <span foreground=\"{accent}\">{}</span>{}",
            esc(before),
            esc(matched),
            esc(new),
            esc(after)
        ),
    }
}

/// A result row names the file — `design.md` and the folder holding it — rather than the note's
/// title: the title hides the extension, and two notes titled the same are then one row twice.
fn name_and_dir(rel_path: &str) -> (&str, &str) {
    match rel_path.rfind('/') {
        Some(i) => (&rel_path[i + 1..], &rel_path[..i + 1]),
        None => (rel_path, ""),
    }
}

/// Case-insensitive substring filtering for the Tags pane. An empty needle keeps everything, so
/// the filter costs nothing until it is typed in.
fn filtered(all: &[(String, i64)], needle: &str) -> Vec<(String, i64)> {
    let needle = needle.trim().to_lowercase();
    all.iter()
        .filter(|(name, _)| needle.is_empty() || name.to_lowercase().contains(&needle))
        .cloned()
        .collect()
}

/// What the two port boxes say, as a forward, or `None` while they are not one yet. `u16` does the
/// range check; port 0 is refused on top of it, because to the kernel it means "any free port" and
/// there is then nothing for the user to connect to.
fn ports(local: &str, remote: &str) -> Option<(u16, u16)> {
    let port = |text: &str| text.trim().parse::<u16>().ok().filter(|p| *p > 0);
    Some((port(local)?, port(remote)?))
}

/// The system accent as pango markup understands it. `Widget::color()` and the style manager are
/// the only colour sources in the app, and pango's parser takes `#rrggbb` and colour names only,
/// so the resolved accent is spelled out here rather than handed over as a CSS function.
fn accent_markup_colour() -> String {
    let c = adw::StyleManager::default().accent_color_rgba();
    let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!(
        "#{:02x}{:02x}{:02x}",
        byte(c.red()),
        byte(c.green()),
        byte(c.blue())
    )
}

// --- widgets ------------------------------------------------------------------------------------

/// A finished result row. Both query kinds meet here, already marked up, so binding a row costs
/// nothing and the factory does not have to know which kind produced it.
struct Row {
    rel_path: String,
    /// The match's byte range in the note; `None` opens the note at the top, which is all a hit
    /// on a note's title alone can name.
    at: Option<Range<usize>>,
    /// The file's name, and beside it in dim the folder it sits in — plus, on a grep row, the
    /// line. A row with no `snippet` is a tail row: the dim line alone, saying what the per-file
    /// cap left out.
    name: String,
    dir: String,
    snippet: String,
}

/// A `GtkListView` of plain strings — references and the files carrying a tag are the same row.
fn path_list(model: &gtk::StringList, on_activate: impl Fn(&str) + 'static) -> gtk::ListView {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let label = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(pango::EllipsizeMode::Middle)
            .build();
        item.downcast_ref::<gtk::ListItem>()
            .expect("list item")
            .set_child(Some(&label));
    });
    factory.connect_bind(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().expect("list item");
        if let (Some(label), Some(s)) = (
            item.child().and_downcast::<gtk::Label>(),
            item.item().and_downcast::<gtk::StringObject>(),
        ) {
            label.set_text(&s.string());
        }
    });

    let view = gtk::ListView::new(
        Some(gtk::SingleSelection::new(Some(model.clone()))),
        Some(factory),
    );
    view.add_css_class("navigation-sidebar");
    // Backlinks and the files under a tag are result lists too, and open on one click like the
    // rest of them.
    view.set_single_click_activate(true);
    view.connect_activate(move |view, pos| {
        if let Some(s) = view
            .model()
            .and_then(|m| m.item(pos))
            .and_downcast::<gtk::StringObject>()
        {
            on_activate(&s.string());
        }
    });
    view
}

/// The shared empty state of every pane. A full-size `AdwStatusPage` is drawn for a window, not
/// for a 200 px column: `.compact` takes the icon from 128 to 96 px, drops the title a step and
/// halves the margins from 36 to 24.
///
/// ponytail: libadwaita has no smaller variant than `.compact`, so if it still crowds a narrow
/// sidebar the next dial is an app CSS rule shrinking the icon inside `statuspage.compact`.
fn status_page(icon: &str, title: &str, description: &str) -> adw::StatusPage {
    let page = adw::StatusPage::builder()
        .icon_name(icon)
        .title(title)
        .description(description)
        .vexpand(true)
        .build();
    page.add_css_class("compact");
    page
}

fn scroller(child: &impl IsA<gtk::Widget>) -> gtk::ScrolledWindow {
    gtk::ScrolledWindow::builder()
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(child)
        .build()
}

// --- search pane --------------------------------------------------------------------------------

/// What the search box is asking for. Compared instead of the compiled [`Query`], because `Regex`
/// has no equality and two searches are the same search when the same text and toggles made them.
#[derive(Clone, PartialEq, Eq)]
struct Key {
    text: String,
    options: Options,
    /// Exact matching rather than ranked full text: any of the three query toggles on, or the
    /// replace row open.
    grep: bool,
    /// Search what is normally left out: what git ignores, and the trees the walk never entered.
    all: bool,
}

/// The search pane's query loop and the widgets it drives, in one `Rc` so the future that waits
/// on a worker thread can hold all of it without cloning a dozen handles.
struct Search {
    data: Rc<Data>,
    entry: gtk::SearchEntry,
    toggles: [gtk::ToggleButton; 4],
    replace_row: gtk::Revealer,
    replace_entry: gtk::Entry,
    apply: gtk::Button,
    progress: gtk::ProgressBar,
    /// The timer pulsing [`Search::progress`], shared with the timer's own closure so it can
    /// clear the slot when it stops itself.
    pulse: Rc<Cell<Option<glib::SourceId>>>,
    body: gtk::Stack,
    results: gio::ListStore,
    /// A query is on a worker thread. Only one runs at a time; the rest of the box is read again
    /// when it lands.
    busy: Cell<bool>,
}

impl Drop for Search {
    /// A pane that goes away takes its pulse timer with it, as `editor.rs` does with its
    /// debounces.
    fn drop(&mut self) {
        if let Some(id) = self.pulse.take() {
            id.remove();
        }
    }
}

impl Search {
    fn key(&self) -> Key {
        let options = Options {
            case: self.toggles[0].is_active(),
            word: self.toggles[1].is_active(),
            regex: self.toggles[2].is_active(),
        };
        Key {
            text: self.entry.text().to_string(),
            options,
            // Replacing is an exact operation, so opening the replace row switches modes too:
            // a ranked full-text hit is not a place in a file that can be rewritten.
            //
            // All still does not switch modes, and that is a decision rather than an oversight:
            // forcing exact would turn a two-word ranked query into a literal-substring one, so
            // asking for more files would quietly find fewer, and it would pay `grep_unindexed`'s
            // walk on every keystroke. What it costs is that the walked trees stay out of ranked
            // results, which is what All's tooltip now says out loud.
            grep: options.any() || self.replace_row.reveals_child(),
            all: self.toggles[3].is_active(),
        }
    }

    /// What Replace All would write, or `None` while the replace row is closed.
    fn replacement(&self) -> Option<String> {
        self.replace_row
            .reveals_child()
            .then(|| self.replace_entry.text().to_string())
    }

    /// Run the pulse timer while a query is on a worker thread, and show the bar once that query
    /// has run long enough to be worth reporting.
    ///
    /// Opacity rather than visibility: the bar keeps its height either way, so results do not jump
    /// down a few pixels the moment a query starts. And it is shown late rather than at once,
    /// because a bar that appears and goes in the same breath reads as a flash, not as progress.
    fn set_busy(&self, busy: bool) {
        self.progress.set_opacity(0.0);
        if let Some(id) = self.pulse.take() {
            id.remove();
        }
        if !busy {
            return;
        }
        let (bar, slot) = (self.progress.clone(), self.pulse.clone());
        let mut pulses = 0;
        self.pulse.set(Some(glib::timeout_add_local(PULSE, move || {
            // `Search` is kept alive by the handlers it connected to its own widgets, so `Drop`
            // is not guaranteed to run. An unrooted bar means the window closed under a query;
            // that is the timer's cue to stop on its own.
            if bar.root().is_none() {
                slot.set(None);
                return glib::ControlFlow::Break;
            }
            pulses += 1;
            if shows_bar(pulses) {
                bar.set_opacity(1.0);
                bar.pulse();
            }
            glib::ControlFlow::Continue
        })));
    }

    /// Run what the box currently asks for, or note that the running query has to be redone.
    fn start(self: &Rc<Self>) {
        let key = self.key();
        // The bar tracks `busy` in every branch: a query still on a worker thread keeps it
        // pulsing, and the one that lands after the box was cleared takes it down through here.
        if key.text.trim().is_empty() {
            self.entry.remove_css_class("error");
            self.results.remove_all();
            self.body.set_visible_child_name("prompt");
            self.set_busy(self.busy.get());
            self.set_total(0);
            return;
        }
        let query = match compile(&key) {
            Ok(query) => query,
            // Only regex mode can fail to compile, and the message is always "that is not a
            // pattern", so the entry says it in place rather than through a toast.
            Err(e) => {
                tracing::debug!("invalid search pattern {:?}: {e}", key.text);
                self.entry.add_css_class("error");
                self.body.set_visible_child_name("invalid");
                self.set_busy(self.busy.get());
                self.set_total(0);
                return;
            }
        };
        self.entry.remove_css_class("error");
        if self.busy.get() {
            return;
        }
        self.busy.set(true);
        self.set_busy(true);
        self.apply.set_sensitive(false);

        let search = self.clone();
        glib::spawn_future_local(async move {
            let run = search.data.search.clone();
            let t0 = Instant::now();
            let answer = gio::spawn_blocking(move || run(query)).await;
            search.busy.set(false);
            let Ok(answer) = answer else {
                search.set_busy(false);
                return tracing::warn!("the search worker panicked");
            };
            tracing::debug!(
                query = key.text,
                grep = key.grep,
                ms = t0.elapsed().as_secs_f64() * 1e3,
                "sidebar query"
            );
            // The box may have moved on while this ran; then its answer is stale and the current
            // question is asked instead. Old results stay on screen until one of them is current.
            if search.key() == key {
                search.show(&key, answer);
                search.set_busy(false);
            } else {
                search.start();
            }
        });
    }

    fn show(&self, key: &Key, answer: Answer) {
        let rows = match answer {
            Answer::Fts(hits) => {
                self.set_total(0);
                hits.into_iter()
                    .map(|hit| {
                        let (name, dir) = name_and_dir(&hit.rel_path);
                        Row {
                            name: name.to_string(),
                            dir: dir.to_string(),
                            snippet: snippet_markup(&hit.snippet),
                            at: hit.at,
                            rel_path: hit.rel_path,
                        }
                    })
                    .collect()
            }
            Answer::Grep(hits, total) => {
                self.set_total(total);
                let Ok(re) = compile_regex(key) else {
                    return;
                };
                let replacement = self.replacement();
                let accent = accent_markup_colour();
                grep_rows(
                    hits,
                    &re,
                    replacement.as_deref(),
                    !key.options.regex,
                    &accent,
                )
            }
        };
        self.body
            .set_visible_child_name(if rows.is_empty() { "empty" } else { "results" });
        let objects: Vec<glib::BoxedAnyObject> =
            rows.into_iter().map(glib::BoxedAnyObject::new).collect();
        self.results.splice(0, self.results.n_items(), &objects);
    }

    /// How many matches the button would rewrite — not how many the query found. The list is
    /// capped and spans every text file; the number is uncapped and counts notes, because that
    /// is what Replace All opens. A vault of source files would otherwise be promised edits that
    /// never happen.
    fn set_total(&self, total: usize) {
        self.apply.set_label(&format!("Replace All ({total})"));
        self.apply.set_sensitive(total > 0);
    }

    /// Rewrite the vault, then ask the same question again so the rows show what is there now.
    ///
    /// The rewrite is one fsync per note and runs on a worker thread; 245 notes took 1.9 s on the
    /// generated vault and 3.3k took 35 s, all of which the main loop used to spend frozen. The
    /// pane marks itself busy for the duration instead — the bar pulses, Replace All goes
    /// insensitive, and a query typed meanwhile waits for the writes rather than racing them.
    fn replace_all(self: &Rc<Self>) {
        let key = self.key();
        // Compiled and thrown away: the vault is asked in the same terms the box holds, and this
        // is only here to refuse a pattern that does not compile before anything is rewritten.
        let (Ok(_), Some(replacement)) = (compile_regex(&key), self.replacement()) else {
            return;
        };
        if self.busy.get() {
            return;
        }
        self.busy.set(true);
        self.set_busy(true);
        self.apply.set_sensitive(false);
        let search = self.clone();
        (self.data.replace_all)(
            key.text.clone(),
            key.options,
            replacement,
            !key.options.regex,
            Box::new(move || {
                search.busy.set(false);
                search.start();
            }),
        );
    }
}

/// A [`Key`] as the worker thread needs it.
fn compile(key: &Key) -> Result<Query, search::Error> {
    match key.grep {
        true => {
            compile_regex(key)?;
            Ok(Query::Grep {
                text: key.text.clone(),
                options: key.options,
                all: key.all,
            })
        }
        false => Ok(Query::Fts(key.text.clone(), key.all)),
    }
}

fn compile_regex(key: &Key) -> Result<Regex, search::Error> {
    search::pattern(&key.text, key.options)
}

/// One row per match, with the diff against the replacement when there is one, and a tail row
/// under a file whose matches the per-file cap cut short.
///
/// ponytail: the replacement is computed by running `re` over the matched text again, which is
/// what makes `$1` expand in the preview. A pattern whose groups depend on context outside the
/// match would preview wrongly; the regex crate has no lookaround, so today that cannot happen.
fn grep_rows(
    hits: Vec<Match>,
    re: &Regex,
    replacement: Option<&str>,
    literal: bool,
    accent: &str,
) -> Vec<Row> {
    let mut rows: Vec<Row> = Vec::with_capacity(hits.len());
    // A file's matches arrive together, so the row under a new path is that file's first match —
    // which is where its tail row opens it.
    let mut first: Option<(String, Range<usize>)> = None;
    for m in hits {
        let matched = &m.line_text[m.range.clone()];
        let at = m.offset..m.offset + m.range.len();
        let replaced = replacement.map(|r| match literal {
            true => re.replace(matched, search::NoExpand(r)).into_owned(),
            false => re.replace(matched, r).into_owned(),
        });
        if first.as_ref().is_none_or(|(rel, _)| *rel != m.rel_path) {
            first = Some((m.rel_path.clone(), at.clone()));
        }
        let (name, dir) = name_and_dir(&m.rel_path);
        rows.push(Row {
            name: name.to_string(),
            // "notes/deep/ — line 12"; a file at the vault root has no folder to name.
            dir: match dir.is_empty() {
                true => format!("line {}", m.line),
                false => format!("{dir} — line {}", m.line),
            },
            snippet: match_markup(&m.line_text, m.range, replaced.as_deref(), accent),
            at: Some(at),
            rel_path: m.rel_path.clone(),
        });
        if m.more > 0 {
            rows.push(Row {
                name: String::new(),
                dir: format!("+{} more in this file", m.more),
                snippet: String::new(),
                at: first.as_ref().map(|(_, at)| at.clone()),
                rel_path: m.rel_path,
            });
        }
    }
    rows
}

struct SearchPane {
    widget: gtk::Widget,
    entry: gtk::SearchEntry,
    replace_toggle: gtk::ToggleButton,
    all_toggle: gtk::ToggleButton,
    replace_entry: gtk::Entry,
    /// Ask the current question again. The window calls it when the ignore set changes under a
    /// query that is already on screen.
    restart: Rc<dyn Fn()>,
}

fn search_pane(data: &Rc<Data>, on_open: &OnOpen) -> SearchPane {
    // ponytail: rows are `glib::BoxedAnyObject`s wrapping a `Row` instead of a GObject with typed
    // properties, the same trade `tree.rs` documents. Define a real item type if the row ever
    // needs bindable state.
    let results = gio::ListStore::new::<glib::BoxedAnyObject>();

    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let name = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(pango::EllipsizeMode::Middle)
            .build();
        name.add_css_class("heading");
        // The folder shares the name's line and gives way first, cut at its front: the last
        // folders are what tell two `design.md`s apart, and a grep row's line number sits here
        // too, so both survive the cut.
        let dir = gtk::Label::builder()
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(pango::EllipsizeMode::Start)
            .build();
        dir.add_css_class("dim-label");
        let head = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        head.append(&name);
        head.append(&dir);
        let snippet = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(pango::WrapMode::WordChar)
            .lines(2)
            .ellipsize(pango::EllipsizeMode::End)
            .build();
        snippet.add_css_class("dim-label");
        // `.navigation-sidebar` gives its rows horizontal padding only, so a two-line row sits on
        // the top and bottom edges of its own selection pill. 6 is the scale's inside-a-group step
        // (DESIGN.md, Spacing).
        let row = gtk::Box::new(gtk::Orientation::Vertical, 0);
        row.set_margin_top(6);
        row.set_margin_bottom(6);
        row.append(&head);
        row.append(&snippet);
        item.downcast_ref::<gtk::ListItem>()
            .expect("list item")
            .set_child(Some(&row));
    });
    factory.connect_bind(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().expect("list item");
        let Some(row) = item.child().and_downcast::<gtk::Box>() else {
            return;
        };
        let (Some(head), Some(snippet)) = (
            row.first_child().and_downcast::<gtk::Box>(),
            row.last_child().and_downcast::<gtk::Label>(),
        ) else {
            return;
        };
        let (Some(name), Some(dir)) = (
            head.first_child().and_downcast::<gtk::Label>(),
            head.last_child().and_downcast::<gtk::Label>(),
        ) else {
            return;
        };
        let Some(boxed) = item.item().and_downcast::<glib::BoxedAnyObject>() else {
            return;
        };
        let hit: Ref<Row> = boxed.borrow();
        name.set_text(&hit.name);
        dir.set_text(&hit.dir);
        // A tail row is the dim line alone, so the empty second line is taken away rather than
        // left as a gap under it.
        snippet.set_markup(&hit.snippet);
        snippet.set_visible(!hit.snippet.is_empty());
    });

    let view = gtk::ListView::new(
        Some(gtk::SingleSelection::new(Some(results.clone()))),
        Some(factory),
    );
    view.add_css_class("navigation-sidebar");
    // One click opens, the rule `tree.rs` and the Git pane already follow: a result is a place to
    // go, and the tab it opens is this pane's preview, so walking the list replaces one tab
    // rather than leaving twenty behind.
    view.set_single_click_activate(true);
    view.connect_activate({
        let on_open = on_open.clone();
        move |view, pos| {
            if let Some(boxed) = view
                .model()
                .and_then(|m| m.item(pos))
                .and_downcast::<glib::BoxedAnyObject>()
            {
                let row = boxed.borrow::<Row>();
                on_open(&row.rel_path, row.at.clone());
            }
        }
    });

    let body = gtk::Stack::builder().vexpand(true).build();
    body.add_named(
        &status_page(
            "system-search-symbolic",
            "Search Notes",
            "Type to search this vault. Ignored files are left out; All puts them back.",
        ),
        Some("prompt"),
    );
    body.add_named(
        &status_page(
            "system-search-symbolic",
            "No Results",
            "Nothing in this vault matches this search.",
        ),
        Some("empty"),
    );
    body.add_named(
        &status_page(
            "dialog-warning-symbolic",
            "Invalid Pattern",
            "This is not a valid regular expression.",
        ),
        Some("invalid"),
    );
    body.add_named(&scroller(&view), Some("results"));
    body.set_visible_child_name("prompt");

    let entry = gtk::SearchEntry::builder()
        .placeholder_text("Search…")
        .hexpand(true)
        .build();
    // A bar spanning the width right above the results, not a spinner beside the entry: the wait
    // belongs to the list that is about to change, and the entry needs the whole sidebar width.
    // It is faded rather than hidden, so the results never shift when a query starts.
    let progress = gtk::ProgressBar::builder().opacity(0.0).build();

    // `edit-find-replace-symbolic`, not a chevron: it names what the button reveals rather than
    // which way a panel opens, and the chevron was invisible for the user who reported this. An
    // icon theme may replace any Adwaita name with artwork of its own, and WhiteSur's
    // `pan-down-symbolic` is written with single-quoted attributes, which GTK4's symbolic
    // recolouring does not parse: the button drew nothing at all (DESIGN.md, Iconography).
    let replace_toggle = gtk::ToggleButton::builder()
        .icon_name("edit-find-replace-symbolic")
        .tooltip_text("Toggle Replace")
        .valign(gtk::Align::Center)
        .build();
    replace_toggle.add_css_class("flat");

    // Text buttons, not icons: Adwaita has no glyph for any of the four, and VS Code's `Aa`,
    // `Word` and `.*` are what a user arriving from there already reads.
    //
    // All's tooltip says which half of "everywhere" it reaches, because the two halves are not
    // the same mechanism: dropping the git-ignored exclusion is a column the ranked search reads
    // too, while the skipped trees are a walk that only the exact-match path makes.
    let toggles = [
        ("Aa", "Match Case"),
        ("Word", "Match Whole Word"),
        (".*", "Use Regular Expression"),
        (
            "All",
            "Search Ignored Files, and Skipped Ones in an Exact Search",
        ),
    ]
    .map(|(label, tooltip)| {
        let button = gtk::ToggleButton::builder()
            .label(label)
            .tooltip_text(tooltip)
            .build();
        button.add_css_class("flat");
        button
    });
    let options = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    options.add_css_class("linked");
    options.set_halign(gtk::Align::Start);
    options.set_hexpand(true);
    for button in &toggles {
        options.append(button);
    }
    // The replace toggle shares the row but not the `.linked` group: the three toggles change what
    // the query means, this one reveals another control, and a fourth button welded to them would
    // read as a fourth query option.
    let toggle_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    toggle_row.append(&options);
    toggle_row.append(&replace_toggle);

    let replace_entry = gtk::Entry::builder()
        .placeholder_text("Replace…")
        .hexpand(true)
        .build();
    let apply = gtk::Button::builder()
        .label("Replace All (0)")
        .halign(gtk::Align::End)
        .sensitive(false)
        .build();
    apply.add_css_class("suggested-action");
    // Stacked rather than side by side: the sidebar's floor is 200 px, where an entry and a button
    // on one line leave neither of them readable.
    let replace_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
    replace_box.append(&replace_entry);
    replace_box.append(&apply);
    let replace_row = gtk::Revealer::builder().child(&replace_box).build();

    let search = Rc::new(Search {
        data: data.clone(),
        entry: entry.clone(),
        toggles: toggles.clone(),
        replace_row: replace_row.clone(),
        replace_entry: replace_entry.clone(),
        apply: apply.clone(),
        progress: progress.clone(),
        pulse: Rc::new(Cell::new(None)),
        body: body.clone(),
        results,
        busy: Cell::new(false),
    });

    // Debounce: one pending source at a time, replaced on every keystroke. A toggle is a click
    // rather than a burst, so it re-runs the query straight away.
    let pending: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
    let debounced: Rc<dyn Fn()> = Rc::new({
        let (search, pending) = (search.clone(), pending.clone());
        move || {
            if let Some(id) = pending.borrow_mut().take() {
                id.remove();
            }
            let id = glib::timeout_add_local_once(DEBOUNCE, {
                let (search, pending) = (search.clone(), pending.clone());
                move || {
                    *pending.borrow_mut() = None;
                    search.start();
                }
            });
            *pending.borrow_mut() = Some(id);
        }
    });

    entry.connect_search_changed({
        let (search, debounced, pending) = (search.clone(), debounced.clone(), pending.clone());
        move |entry| {
            // Clearing the entry is free, so it cancels the pending query and repaints at once.
            if entry.text().trim().is_empty() {
                if let Some(id) = pending.borrow_mut().take() {
                    id.remove();
                }
                return search.start();
            }
            debounced();
        }
    });
    // ponytail: a changed replacement re-runs the whole query, because the rows carry finished
    // markup rather than the matches they were built from. Off the main thread it costs nothing
    // the user can feel; cache the last answer if a huge vault ever makes it visible.
    replace_entry.connect_changed({
        let debounced = debounced.clone();
        move |_| debounced()
    });
    for button in &toggles {
        button.connect_toggled({
            let search = search.clone();
            move |_| search.start()
        });
    }
    replace_toggle.connect_toggled({
        let (search, replace_row) = (search.clone(), replace_row.clone());
        move |toggle| {
            replace_row.set_reveal_child(toggle.is_active());
            search.start();
        }
    });
    apply.connect_clicked({
        let search = search.clone();
        move |_| search.replace_all()
    });

    let controls = gtk::Box::new(gtk::Orientation::Vertical, 6);
    controls.set_margin_top(6);
    controls.set_margin_bottom(6);
    controls.set_margin_start(6);
    controls.set_margin_end(6);
    controls.append(&entry);
    controls.append(&toggle_row);
    controls.append(&replace_row);

    let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
    column.append(&controls);
    column.append(&progress);
    column.append(&body);
    SearchPane {
        widget: column.upcast(),
        entry,
        replace_toggle,
        all_toggle: toggles[3].clone(),
        replace_entry,
        restart: Rc::new({
            let search = search.clone();
            move || search.start()
        }),
    }
}

// --- tags pane ----------------------------------------------------------------------------------

/// The tag list gets two thirds of the pane, the files under the selected tag the lower third.
const TAGS_SHARE: (i32, i32) = (2, 3);

struct TagsPane {
    widget: gtk::Widget,
    /// Kept so a double-click on it can be reset to [`TAGS_SHARE`].
    divider: gtk::Paned,
    /// Set by `mark_tags_dirty`, cleared by the refill the next time the pane is shown.
    dirty: Rc<Cell<bool>>,
    select: Rc<dyn Fn(&str)>,
    refill: Rc<dyn Fn()>,
}

fn tags_pane(data: &Rc<Data>, on_open: &OnOpen) -> TagsPane {
    let tags = gio::ListStore::new::<glib::BoxedAnyObject>();

    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let name = gtk::Label::builder().xalign(0.0).hexpand(true).build();
        name.set_ellipsize(pango::EllipsizeMode::End);
        let count = gtk::Label::builder().xalign(1.0).build();
        count.add_css_class("dim-label");
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        row.append(&name);
        row.append(&count);
        item.downcast_ref::<gtk::ListItem>()
            .expect("list item")
            .set_child(Some(&row));
    });
    factory.connect_bind(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().expect("list item");
        let Some(row) = item.child().and_downcast::<gtk::Box>() else {
            return;
        };
        let (Some(name), Some(count)) = (
            row.first_child().and_downcast::<gtk::Label>(),
            row.last_child().and_downcast::<gtk::Label>(),
        ) else {
            return;
        };
        let Some(boxed) = item.item().and_downcast::<glib::BoxedAnyObject>() else {
            return;
        };
        let tag: Ref<(String, i64)> = boxed.borrow();
        name.set_text(&tag.0);
        count.set_text(&tag.1.to_string());
    });

    // No autoselect: the file list stays hidden until the user actually picks a tag.
    let selection = gtk::SingleSelection::new(Some(tags.clone()));
    selection.set_autoselect(false);
    selection.set_can_unselect(true);
    selection.set_selected(gtk::INVALID_LIST_POSITION);

    let view = gtk::ListView::new(Some(selection.clone()), Some(factory));
    view.add_css_class("navigation-sidebar");

    let files = gtk::StringList::new(&[]);
    let heading = gtk::Label::builder()
        .xalign(0.0)
        .ellipsize(pango::EllipsizeMode::End)
        .margin_start(12)
        .margin_end(12)
        .margin_top(6)
        .margin_bottom(6)
        .build();
    heading.add_css_class("heading");

    let files_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    files_box.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    files_box.append(&heading);
    files_box.append(&scroller(&path_list(&files, {
        let on_open = on_open.clone();
        move |rel: &str| on_open(rel, None)
    })));
    files_box.set_visible(false);

    // A paned rather than a fixed height: the two lists share the pane, and where the user puts
    // the divider survives the window being resized.
    let paned = gtk::Paned::builder()
        .orientation(gtk::Orientation::Vertical)
        .start_child(&scroller(&view))
        .end_child(&files_box)
        .resize_start_child(true)
        .resize_end_child(true)
        .shrink_start_child(false)
        .shrink_end_child(false)
        .vexpand(true)
        .build();
    // The default position is set the first time there is anything below the divider, when the
    // pane already knows how tall it is. Afterwards the position is the user's.
    let placed = Cell::new(false);
    files_box.connect_map({
        let paned = paned.clone();
        move |_| {
            if !placed.replace(true) {
                paned.set_position(paned.height() * TAGS_SHARE.0 / TAGS_SHARE.1);
            }
        }
    });

    // Selection drives the filter, so a single click picks a tag and the refill's "no selection"
    // hides the list through the same path.
    selection.connect_selected_item_notify({
        let (files, files_box, heading, data) = (
            files.clone(),
            files_box.clone(),
            heading.clone(),
            data.clone(),
        );
        move |selection| {
            let Some(boxed) = selection
                .selected_item()
                .and_downcast::<glib::BoxedAnyObject>()
            else {
                files.splice(0, files.n_items(), &[]);
                files_box.set_visible(false);
                return;
            };
            let name = boxed.borrow::<(String, i64)>().0.clone();
            let rows = (data.files_with_tag)(&name);
            let refs: Vec<&str> = rows.iter().map(String::as_str).collect();
            files.splice(0, files.n_items(), refs.as_slice());
            heading.set_text(&name);
            files_box.set_visible(true);
        }
    });

    let filter = gtk::SearchEntry::builder()
        .placeholder_text("Filter tags…")
        .margin_top(6)
        .margin_bottom(6)
        .margin_start(6)
        .margin_end(6)
        .build();

    // The whole list is kept, so filtering is a splice rather than a query: the tags come from one
    // GROUP BY over the index and re-running it per keystroke would buy nothing.
    let all: Rc<RefCell<Vec<(String, i64)>>> = Rc::new(RefCell::new(Vec::new()));
    let apply: Rc<dyn Fn()> = Rc::new({
        let (tags, selection, all, filter) =
            (tags.clone(), selection.clone(), all.clone(), filter.clone());
        move || {
            let rows: Vec<glib::BoxedAnyObject> = filtered(&all.borrow(), &filter.text())
                .into_iter()
                .map(glib::BoxedAnyObject::new)
                .collect();
            tags.splice(0, tags.n_items(), &rows);
            selection.set_selected(gtk::INVALID_LIST_POSITION);
        }
    });
    filter.connect_search_changed({
        let apply = apply.clone();
        move |_| apply()
    });

    let refill: Rc<dyn Fn()> = Rc::new({
        let (all, apply, data) = (all.clone(), apply.clone(), data.clone());
        move || {
            *all.borrow_mut() = (data.tags)();
            apply();
        }
    });

    let select: Rc<dyn Fn(&str)> = Rc::new({
        let (tags, selection, filter) = (tags.clone(), selection.clone(), filter.clone());
        move |wanted: &str| {
            // The tag the caller wants may be filtered out of the list; clearing the filter puts
            // every tag back before it is looked for.
            filter.set_text("");
            let found = (0..tags.n_items()).find(|i| {
                tags.item(*i)
                    .and_downcast::<glib::BoxedAnyObject>()
                    .is_some_and(|b| b.borrow::<(String, i64)>().0 == wanted)
            });
            selection.set_selected(found.unwrap_or(gtk::INVALID_LIST_POSITION));
        }
    });

    let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
    column.append(&filter);
    column.append(&paned);

    TagsPane {
        divider: paned.clone(),
        widget: column.upcast(),
        // The first time the pane is shown there is nothing in it yet.
        dirty: Rc::new(Cell::new(true)),
        select,
        refill,
    }
}

// --- ports pane ---------------------------------------------------------------------------------

/// Take one forward down: the two ports it carries, and the row it is drawn in.
type DropForward = Rc<dyn Fn(u16, u16, &gtk::ListBoxRow)>;

/// The forwards running over the window's ssh connection, and the row that starts another one.
///
/// The pane keeps the list itself. Nothing asks ssh what it has open, so what the user added is
/// what is drawn; the caller re-establishes them after a reconnect and the pane is only the list.
fn ports_pane(data: &Rc<Data>) -> gtk::Widget {
    let forwards: Rc<RefCell<Vec<(u16, u16)>>> = Rc::new(RefCell::new(Vec::new()));

    // A `GtkListBox` rebuilt row by row rather than a list view and a factory: there are a handful
    // of forwards at most, so a model to recycle rows into would cost more than it saves.
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .build();
    list.add_css_class("navigation-sidebar");

    let body = gtk::Stack::builder().vexpand(true).build();
    body.add_named(
        &status_page(
            PORTS_ICON,
            "No Forwarded Ports",
            "A forward makes a port on the remote machine reachable at the same address on this one.",
        ),
        Some("empty"),
    );
    body.add_named(&scroller(&list), Some("list"));
    body.set_visible_child_name("empty");

    // A banner and not a toast (DESIGN.md, States): ssh refusing a port is not something that
    // happened and is over, it is the state of the forward that is not up, and the way out of it
    // is to choose another port — a decision made in the row right below the message.
    let banner = adw::Banner::new("");

    let switch_body: Rc<dyn Fn()> = Rc::new({
        let (body, forwards) = (body.clone(), forwards.clone());
        move || {
            body.set_visible_child_name(match forwards.borrow().is_empty() {
                true => "empty",
                false => "list",
            });
        }
    });

    let drop_forward: DropForward = Rc::new({
        let (data, forwards, list, switch_body) = (
            data.clone(),
            forwards.clone(),
            list.clone(),
            switch_body.clone(),
        );
        move |local, remote, row: &gtk::ListBoxRow| {
            (data.remove_forward)(local, remote);
            forwards.borrow_mut().retain(|f| *f != (local, remote));
            list.remove(row);
            switch_body();
        }
    });

    let local = port_entry("Local");
    let remote = port_entry("Remote");
    let add = gtk::Button::builder()
        .label("Add")
        .halign(gtk::Align::End)
        .sensitive(false)
        .build();

    let submit: Rc<dyn Fn()> = Rc::new({
        let (data, forwards, list, banner, local, remote) = (
            data.clone(),
            forwards.clone(),
            list.clone(),
            banner.clone(),
            local.clone(),
            remote.clone(),
        );
        let (switch_body, drop_forward) = (switch_body.clone(), drop_forward.clone());
        move || {
            let Some((from, to)) = ports(&local.text(), &remote.text()) else {
                return;
            };
            match (data.add_forward)(from, to) {
                Ok(()) => {
                    banner.set_revealed(false);
                    forwards.borrow_mut().push((from, to));
                    list.append(&forward_row(from, to, drop_forward.clone()));
                    local.set_text("");
                    remote.set_text("");
                    switch_body();
                }
                Err(message) => {
                    banner.set_title(&message);
                    banner.set_revealed(true);
                }
            }
        }
    });

    for entry in [&local, &remote] {
        entry.connect_changed({
            let (add, local, remote) = (add.clone(), local.clone(), remote.clone());
            move |_| add.set_sensitive(ports(&local.text(), &remote.text()).is_some())
        });
        entry.connect_activate({
            let submit = submit.clone();
            move |_| submit()
        });
    }
    add.connect_clicked({
        let submit = submit.clone();
        move |_| submit()
    });

    let arrow = gtk::Label::new(Some("→"));
    arrow.add_css_class("dim-label");
    let entries = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    entries.append(&local);
    entries.append(&arrow);
    entries.append(&remote);

    // The button on a line of its own, as the Search pane's replace row has it: the sidebar's
    // floor is 200 px, and two entries and a button do not share one line there.
    let form = gtk::Box::new(gtk::Orientation::Vertical, 6);
    form.set_margin_top(6);
    form.set_margin_bottom(6);
    form.set_margin_start(6);
    form.set_margin_end(6);
    form.append(&entries);
    form.append(&add);

    let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
    column.append(&banner);
    column.append(&body);
    column.append(&form);
    column.upcast()
}

/// One live forward: `local → remote`, and the button that takes it down.
fn forward_row(local: u16, remote: u16, drop_forward: DropForward) -> gtk::ListBoxRow {
    let label = gtk::Label::builder()
        .label(format!("{local} → {remote}"))
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(pango::EllipsizeMode::End)
        .build();
    label.add_css_class("numeric");
    let close = gtk::Button::builder()
        .icon_name("window-close-symbolic")
        .tooltip_text("Stop Forwarding")
        .valign(gtk::Align::Center)
        .build();
    close.add_css_class("flat");

    let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    content.append(&label);
    content.append(&close);
    let row = gtk::ListBoxRow::builder()
        .child(&content)
        .activatable(false)
        .build();
    // Weak: the button is inside the row it removes, and a strong handle would be a cycle no
    // amount of removing frees.
    close.connect_clicked(glib::clone!(
        #[weak]
        row,
        move |_| drop_forward(local, remote, &row)
    ));
    row
}

/// A port box. Digits only, enforced on `insert-text` rather than through a `GtkEntryBuffer` of
/// our own: a buffer subclass is a GObject and a hundred lines for the same rule, while refusing
/// the insertion covers typing, pasting and a drop alike, all three arriving here as one.
fn port_entry(placeholder: &str) -> gtk::Entry {
    let entry = gtk::Entry::builder()
        .placeholder_text(placeholder)
        .input_purpose(gtk::InputPurpose::Digits)
        .max_length(5)
        .width_chars(5)
        .hexpand(true)
        .build();
    entry.connect_insert_text(|entry, text, _| {
        if !text.chars().all(|c| c.is_ascii_digit()) {
            entry.stop_signal_emission_by_name("insert-text");
        }
    });
    entry
}

/// What the Outline pane says with nothing to outline.
fn outline_empty() -> gtk::Widget {
    status_page(
        OUTLINE_ICON,
        "No Outline",
        "Open a note to see its headings.",
    )
    .upcast()
}

/// A sentence in the Outline pane's own shape, for a tab that has no outline to give.
pub fn outline_note(title: &str, body: &str) -> gtk::Widget {
    status_page(OUTLINE_ICON, title, body).upcast()
}

/// An outline as rows that jump: `(level, text, where a click goes)`.
///
/// Generic in what a row jumps to, because the two callers mean different things by it: a text
/// tab's symbols carry a position in the buffer and a PDF's bookmarks carry a page number.
///
/// Indented by level rather than nested in a tree: an outline is read top to bottom, and an
/// expander per row would hide exactly what the pane exists to show.
pub fn outline_list<T: Copy + 'static>(
    rows: &[(u8, String, T)],
    on_jump: impl Fn(T) + 'static,
) -> gtk::Widget {
    let texts: Vec<&str> = rows.iter().map(|(_, text, _)| text.as_str()).collect();
    let model = gtk::StringList::new(&texts);
    let levels: Vec<u8> = rows.iter().map(|(level, _, _)| *level).collect();
    let targets: Vec<T> = rows.iter().map(|(_, _, at)| *at).collect();

    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let label = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(pango::EllipsizeMode::End)
            .build();
        item.downcast_ref::<gtk::ListItem>()
            .expect("list item")
            .set_child(Some(&label));
    });
    factory.connect_bind(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().expect("list item");
        if let (Some(label), Some(s)) = (
            item.child().and_downcast::<gtk::Label>(),
            item.item().and_downcast::<gtk::StringObject>(),
        ) {
            label.set_text(&s.string());
            let level = levels.get(item.position() as usize).copied().unwrap_or(1);
            label.set_margin_start(OUTLINE_INDENT * i32::from(level.saturating_sub(1)));
        }
    });

    let view = gtk::ListView::new(Some(gtk::SingleSelection::new(Some(model))), Some(factory));
    view.add_css_class("navigation-sidebar");
    view.set_single_click_activate(true);
    view.connect_activate(move |_, row| {
        if let Some(at) = targets.get(row as usize) {
            on_jump(*at);
        }
    });
    scroller(&view).upcast()
}

/// The References pane: the rows, and the page shown instead when there are none.
fn references_body(
    model: &gtk::StringList,
    on_reference: impl Fn(&str) + 'static,
) -> (gtk::Stack, adw::StatusPage) {
    let stack = gtk::Stack::builder().vexpand(true).build();
    let empty = status_page(
        BACKLINK_ICON,
        "No Backlinks",
        "No note links to the open one.",
    );
    stack.add_named(&empty, Some("empty"));
    stack.add_named(&scroller(&path_list(model, on_reference)), Some("list"));
    stack.set_visible_child_name("empty");
    (stack, empty)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_query_shorter_than_the_grace_period_never_draws_a_bar() {
        assert!(!shows_bar(0), "the bar is not up when the query starts");
        assert!(!shows_bar(SHOW_AFTER - 1));
        assert!(shows_bar(SHOW_AFTER));
        // A requery nobody asked for costs a few milliseconds on a warm index; the wait has to be
        // long enough to cover one and short enough that a real query still reports itself.
        assert!((100..=300).contains(&(PULSE * SHOW_AFTER).as_millis()));
    }

    #[test]
    fn snippet_escapes_before_marking_up() {
        // `<` inside the note must survive as text, not as the start of a tag.
        let out = snippet_markup("a «b» < c & d");
        assert_eq!(out, "a <b>b</b> &lt; c &amp; d");
        assert!(pango::parse_markup(&out, '\u{0}').is_ok());
    }

    #[test]
    fn a_snippet_is_one_paragraph_however_the_note_was_wrapped() {
        // `lines(2)` on the row label is counted per paragraph, so a newline here is a row that
        // grows without limit.
        let out = snippet_markup("---\ntitle: x\n---\n\nthe «body»");
        assert!(!out.contains('\n'), "{out:?}");
        assert_eq!(out, "--- title: x --- the <b>body</b>");
        assert_eq!(snippet_markup("  padded  "), "padded");
    }

    #[test]
    fn snippet_markup_stays_valid_when_markers_are_unbalanced() {
        // A note may contain guillemets of its own, so every shape has to parse.
        for s in ["«open", "close»", "«a «b»", "»«", "«<»", ""] {
            let out = snippet_markup(s);
            assert!(
                pango::parse_markup(&out, '\u{0}').is_ok(),
                "{s:?} -> {out:?}"
            );
        }
    }

    #[test]
    fn a_row_is_named_by_its_file_and_its_folder() {
        assert_eq!(
            name_and_dir("notes/deep/thought.md"),
            ("thought.md", "notes/deep/")
        );
        assert_eq!(name_and_dir("top.md"), ("top.md", ""));
    }

    #[test]
    fn a_grep_row_marks_the_match_and_then_the_replacement() {
        // A colour name, not a hex literal: DESIGN.md's pre-flight grep allows neither outside
        // `theme.rs`, and pango parses both.
        let plain = match_markup("a <b> c", 2..5, None, "teal");
        assert_eq!(plain, "a <b>&lt;b&gt;</b> c");
        assert!(pango::parse_markup(&plain, '\u{0}').is_ok());

        let replaced = match_markup("a <b> c", 2..5, Some("&x"), "teal");
        assert!(replaced.contains("<s>&lt;b&gt;</s>"), "{replaced}");
        assert!(replaced.contains(">&amp;x</span>"), "{replaced}");
        assert!(pango::parse_markup(&replaced, '\u{0}').is_ok());
    }

    #[test]
    fn a_forward_needs_two_real_port_numbers() {
        assert_eq!(ports("8080", "3000"), Some((8080, 3000)));
        assert_eq!(ports(" 22 ", "22"), Some((22, 22)));
        for (local, remote) in [
            ("", "3000"),
            ("8080", ""),
            ("0", "3000"),
            ("8080", "0"),
            ("http", "3000"),
            ("80.80", "3000"),
            ("-1", "3000"),
            ("65536", "3000"),
        ] {
            assert_eq!(ports(local, remote), None, "{local:?} -> {remote:?}");
        }
    }

    #[test]
    fn tag_filter_is_a_case_insensitive_substring() {
        let all = [
            ("rust".to_string(), 3),
            ("Rustaceans".to_string(), 1),
            ("go".to_string(), 2),
        ];
        assert_eq!(filtered(&all, "").len(), 3);
        assert_eq!(filtered(&all, " RUST ").len(), 2);
        assert_eq!(filtered(&all, "ace")[0].0, "Rustaceans");
        assert!(filtered(&all, "zzz").is_empty());
    }
}
