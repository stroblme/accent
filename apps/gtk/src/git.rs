//! The Git pane: what changed, what is staged, and where the history went.
//!
//! Everything here comes from `accent_api::git`, which drives the user's own `git` binary. A
//! `git status` on a cold cache takes long enough to drop frames, so every call runs on
//! `gio::spawn_blocking` and only its answer reaches the main thread. Refreshes are debounced and
//! coalesced: a save, a watcher event and a `.git` write in the same moment cost one `git status`.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::Duration;

use accent_api::Vault;
use accent_api::git::{self, Blob, Branch, Commit, Entry, LogRow, Repo, Status, Submodule};
use adw::prelude::*;
use gtk::{gdk, gio, glib, pango};

use crate::diff::{Compare, DiffTab, Side};

use crate::fileops;
use crate::highlight;

/// The changes list gets the top half of the pane, the log the bottom.
pub const GIT_SHARE: (i32, i32) = (1, 2);

/// One page of history: what a refresh reads, and what Load More adds.
const PAGE: usize = 200;

/// The width of one graph lane, in px.
const LANE: i32 = 12;

/// The action group the history's context menu resolves its items against.
const MENU_GROUP: &str = "gitlog";

/// How long the pane waits after being poked before asking git again. Long enough that a burst of
/// watcher events is one query, short enough that a save shows up while the hand is still there.
const DEBOUNCE: Duration = Duration::from_millis(500);

/// How often the selected repository's remote is fetched while the window has the focus. VS
/// Code's `git.autofetchPeriod` default, and for its reason: it is often enough that a colleague's
/// push shows up in the history within a coffee break, and rare enough that a laptop on a phone
/// tether is not woken by us. Only while the window is focused, so a window left open behind
/// others stops talking to the network at all.
const AUTOFETCH: Duration = Duration::from_secs(300);

/// How far a commit the remote has and HEAD does not is faded. Enough to read as "this is not
/// here yet" beside a commit that is, and not so far that the summary stops being legible.
const NOT_PULLED_DIM: f64 = 0.55;

/// Arrows going out and coming back, which is what a sync is. The same name the sidebar gives the
/// pane's own tab, and for the same reason: `network-transmit-receive-symbolic` is a pair of
/// arrows in Adwaita but a network device in WhiteSur, so a Sync button drew as a port.
const SYNC_ICON: &str = "mail-send-receive-symbolic";

/// How far one level of the changes tree is indented, in px.
const INDENT: i32 = 12;

/// How tall the commit message box may grow before it scrolls, in px: about eight lines at the
/// default interface font. A commit message is a subject, a blank line and a body, so one line is
/// the wrong resting size and unbounded growth would push the changes and the history off the pane.
const MESSAGE_MAX_HEIGHT: i32 = 160;

/// What the pane needs from the window, as closures rather than a handle: this module knows
/// nothing about tabs or the vault tree. Every one of them holds the window weakly, or the pane
/// would keep a closed window alive for the life of the process.
// Boxed closures are the whole point of this struct; a type alias per field would only hide the
// signature the caller has to write.
#[allow(clippy::type_complexity)]
pub struct Hooks {
    pub vault: Arc<Vault>,
    /// Where the dialogs are presented.
    pub window: adw::ApplicationWindow,
    pub toast: Box<dyn Fn(&str)>,
    /// Open a file in a tab, by vault key.
    pub open: Box<dyn Fn(&str)>,
    /// Open a comparison of two texts that are not files: key, the file name behind it, the tab
    /// title, then (title, text) for each side. Hands the tab back so a refresh can reach it.
    pub open_diff: Box<dyn Fn(&str, &str, &str, (&str, &str), (&str, &str)) -> Option<Rc<DiffTab>>>,
    /// Compare a file's working tree with the index, inside the file's own tab: key, the index
    /// side's title and text, and what to call once the comparison exists so a refresh can reach
    /// it.
    pub compare_file: Box<dyn Fn(&str, &str, &str, Box<dyn FnOnce(Weak<Compare>)>)>,
    /// Move a vault file to the trash. Vault keys only, which is what leaves an untracked file
    /// outside the vault without a Discard button.
    pub trash: Box<dyn Fn(&str)>,
    /// A refresh landed and the pane's answers changed.
    pub changed: Box<dyn Fn()>,
    /// A sync started (`true`) or ended (`false`). Separate from `changed`, which is a refresh
    /// landing and costs an index write: this fires twice per sync and must stay cheap.
    pub syncing: Box<dyn Fn(bool)>,
    /// Whether the changes list starts grouped by folder — `git_tree` in the config.
    pub tree: bool,
    /// The in-pane toggle moved: write the preference the Preferences dialog also edits.
    pub set_tree: Box<dyn Fn(bool)>,
}

/// Which list a row belongs to, which is what decides the letter it shows, the buttons it offers
/// and what activating it compares.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Section {
    Conflicts,
    Staged,
    Changes,
}

/// One line of the changes list. Headers are rows of their own rather than list sections, so the
/// whole thing is one flat `ListStore` and an empty section is simply two rows that are not there.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Row {
    Header {
        title: &'static str,
        /// The section a "Stage All" / "Unstage All" button acts on, or `None` for a header with
        /// no bulk action: conflicts are resolved one file at a time, and submodules are a
        /// read-only list.
        all: Option<Section>,
    },
    /// A folder in the tree view, standing for everything under it in one section.
    Folder {
        /// The whole path from the repository root, which is what identifies the row while it is
        /// collapsed.
        path: String,
        /// What the row shows: the segments this row adds to the one above it. A chain of folders
        /// with a single child each lands on one row, so this is a path rather than a name.
        label: String,
        section: Section,
        depth: usize,
    },
    Entry {
        entry: Entry,
        section: Section,
        /// The path as the rest of the app names it: vault-relative, or absolute outside it.
        key: String,
        /// How far the row is indented. Always 0 in the flat view.
        depth: usize,
    },
    Submodule(Submodule),
}

/// One line of the history list. A flat store with two kinds rather than a `GtkTreeListModel`:
/// the log is spliced wholesale on every refresh anyway, so a tree model would only add a
/// create-child-model closure and a placeholder state to keep in step with it.
#[derive(Clone)]
enum LogItem {
    Commit(LogRow),
    /// A file the commit above it changed, shown while that commit is expanded.
    File {
        /// The commit the file belongs to, and its first parent — `None` on a root commit, whose
        /// files have nothing on the left to compare against.
        oid: String,
        parent: Option<String>,
        letter: char,
        path: String,
    },
    /// The last row while git has history the store does not: activating it pages the next
    /// [`PAGE`] in. A row rather than a button under the list, so it is reached by scrolling to
    /// the end of the history it continues.
    More,
}

/// Everything the last refresh learned. One struct behind one `RefCell`, because every field of
/// it is replaced at the same moment and a reader wants a consistent set.
#[derive(Default)]
struct State {
    repos: Vec<Repo>,
    /// Index into `repos`; the pane talks about one repository at a time.
    selected: usize,
    /// One per repository, index-aligned with `repos`.
    statuses: Vec<Status>,
    /// The selected repository's history, as far as it has been paged in.
    commits: Vec<Commit>,
    /// The selected repository's local branches, which is what the branch chooser lists.
    branches: Vec<String>,
    /// The oids the selected repository's upstream has and HEAD does not: the rows the history
    /// draws as not pulled yet. Empty unless a fetch has found something.
    incoming: HashSet<String>,
    submodules: Vec<Submodule>,
    /// Ignored paths across every repository, vault-relative, directories keeping their slash.
    ignored: HashSet<String>,
    /// git dir → the branch oid the last refresh saw.
    heads: HashMap<PathBuf, String>,
    head_moved: bool,
}

pub struct Panel {
    hooks: Hooks,
    root: gtk::Widget,
    /// "empty" (no repository) or "repo".
    stack: gtk::Stack,
    /// The "repo" page's box, and the only widget here a popover may hang off: GTK re-presents a
    /// popover from its parent's `allocate_native_children`, which a `GtkListView` never reaches
    /// (`fileops::context_menu` documents the symptom).
    column: gtk::Box,
    names: gtk::StringList,
    chooser: gtk::DropDown,
    /// What the branch button says, which is the branch HEAD is on or where it is detached.
    branch_label: gtk::Label,
    branch_menu: gtk::Popover,
    branch_list: gtk::ListBox,
    /// The names the popover is showing and which of them HEAD is on. A refresh that says the
    /// same thing leaves the rows alone: one fires on every save, and rebuilding them would take
    /// a row out from under the pointer already on it.
    branch_shown: RefCell<(Vec<String>, Option<usize>)>,
    counts: gtk::Label,
    sync: gtk::Button,
    message: gtk::TextView,
    placeholder: gtk::Label,
    commit: gtk::Button,
    /// The message box and its button, hidden together when there is nothing to commit.
    commit_box: gtk::Box,
    divider: gtk::Paned,
    changes: gio::ListStore,
    log: gio::ListStore,
    /// The history list, so the bench can activate a row without a pointer.
    log_view: gtk::ListView,
    /// Whether git has history the store does not hold, which is what puts the Load More row at
    /// the end of the log. Also the re-entrancy guard: it is cleared while a page is in flight.
    has_more: Cell<bool>,
    state: RefCell<State>,
    /// The debounce timer, replaced rather than stacked.
    pending: RefCell<Option<glib::SourceId>>,
    busy: Cell<bool>,
    /// Something asked for a refresh while one was in flight; run once more when it lands.
    again: Cell<bool>,
    /// The comparisons open right now, re-read whenever a refresh lands: a diff tab is not a
    /// snapshot. Weak, so a closed one falls out on the next pass.
    watches: RefCell<Vec<Watch>>,
    /// Set while the repository chooser's list is being filled, so the selection notify that
    /// follows is not read as the user picking a repository.
    syncing: Cell<bool>,
    /// A sync is in flight. Not the same thing as `syncing` above, which is the chooser being
    /// filled: this is the transfer the status bar spins for.
    sync_busy: Cell<bool>,
    /// A background fetch is in flight, so a timer tick landing on a slow one is dropped rather
    /// than stacked.
    fetch_busy: Cell<bool>,
    /// Whether the vault's repositories have been fetched since the window opened them. The first
    /// fetch waits for the first refresh, because until then there is no repository to fetch.
    fetched_once: Cell<bool>,
    /// A timer tick was skipped because the window did not have the focus. Run as soon as it
    /// does, so coming back to a window that has been aside for an hour is not another wait.
    missed_fetch: Cell<bool>,
    /// The commit whose file list is open, if any. One at a time: a second expansion closes the
    /// first, and a refresh closes them all.
    expanded: RefCell<Option<String>>,
    /// Whether the changes list is grouped by folder. A copy of the `git_tree` preference, which
    /// the Changes header's toggle and the Preferences switch both write.
    tree: Cell<bool>,
    /// The [`folder_key`]s whose contents are folded away. Kept across a refresh, because a save
    /// schedules one and folding a folder must survive it.
    collapsed: RefCell<HashSet<String>>,
}

impl Panel {
    pub fn new(hooks: Hooks) -> Rc<Panel> {
        let names = gtk::StringList::new(&[]);
        let chooser = gtk::DropDown::builder()
            .model(&names)
            .visible(false)
            // A repository is named after its directory, and the button's default label asks for
            // the whole name however long it is: measured at 345 px for a 43-character one, which
            // is the sidebar's real floor whenever a vault has more than one repository. The
            // button ellipsizes; the popup list keeps the names whole, having room for them.
            .factory(&name_factory(true))
            .list_factory(&name_factory(false))
            .build();

        // The branch is a menu button rather than a chooser: its popover is a list of the local
        // branches, each row switching to that branch and carrying a trash button, with Create
        // Branch… underneath. A `GtkDropDown` can only ever pick one of the rows it already has.
        // Flat and `heading`, because it stands where the branch label stood and reads as the
        // branch first and as a control second; the label ellipsizes so that a long branch name
        // is not what decides how narrow the sidebar can be dragged. It claims the row's spare
        // width without taking it, so Sync and Commit stay at the trailing edge.
        let branch_label = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(pango::EllipsizeMode::End)
            .build();
        let face = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        face.append(&branch_label);
        face.append(&gtk::Image::from_icon_name("pan-down-symbolic"));

        let branch_list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .build();
        branch_list.add_css_class("navigation-sidebar");
        let create = gtk::Button::builder().label("Create Branch…").build();
        create.add_css_class("flat");
        let branch_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        // A repository with fifty branches is a list that scrolls, not a popover taller than the
        // screen — the shape `start.rs::host_field` settled on for the ssh hosts.
        branch_box.append(
            &gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .propagate_natural_height(true)
                .max_content_height(280)
                .child(&branch_list)
                .build(),
        );
        branch_box.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        branch_box.append(&create);
        let branch_menu = gtk::Popover::builder().child(&branch_box).build();
        let branch = gtk::MenuButton::builder()
            .hexpand(true)
            .halign(gtk::Align::Start)
            .child(&face)
            .popover(&branch_menu)
            .build();
        for class in ["flat", "heading"] {
            branch.add_css_class(class);
        }
        let counts = gtk::Label::new(None);
        counts.add_css_class("dim-label");
        counts.add_css_class("numeric");
        // One button, both halves, and the counts inside it, which is the pane's whole answer to
        // "is there anything to pull": a background fetch keeps them current, so `↓2` inside the
        // Sync button is what says a pull would bring something. Three buttons was also what
        // stopped the sidebar shrinking — the branch row measured 186 px of minimum width with
        // them and 105 with one.
        let arrows = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        arrows.append(&counts);
        arrows.append(&gtk::Image::from_icon_name(SYNC_ICON));
        let sync = gtk::Button::builder()
            .child(&arrows)
            .valign(gtk::Align::Center)
            .build();
        sync.add_css_class("flat");
        // The "No Repository" page's own button: `git init` in a vault with no repository writes
        // nowhere the pane is watching, so this is the one refresh a user still has to ask for.
        let check = gtk::Button::builder()
            .label("Check Again")
            .halign(gtk::Align::Center)
            .build();
        check.add_css_class("pill");

        let branch_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        branch_row.append(&branch);
        branch_row.append(&sync);

        // The message box is a card so it reads as somewhere to type rather than as a label, and
        // it grows with what is in it: one line while the message is a subject, taller as a body
        // is written, and scrolling once it reaches [`MESSAGE_MAX_HEIGHT`], because past that it
        // would push the changes and the history off the pane. The scroller does the growing on
        // its own — `propagate-natural-height` asks the view how tall it wants to be and
        // `max-content-height` is the cap — so nothing here measures text.
        //
        // The margins are the 9 px Adwaita gives `entry` either side of its text, so the box has
        // the same inset as the search field rather than a tighter one of its own.
        let message = gtk::TextView::builder()
            .wrap_mode(gtk::WrapMode::WordChar)
            .accepts_tab(false)
            .left_margin(9)
            .right_margin(9)
            .top_margin(9)
            .bottom_margin(9)
            .build();
        message.add_css_class("card");
        let message_scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .max_content_height(MESSAGE_MAX_HEIGHT)
            .child(&message)
            .build();
        // GtkTextView has no placeholder of its own, so this is one laid over it. It cannot be
        // clicked through to, which would otherwise put the caret nowhere.
        let placeholder = gtk::Label::builder()
            .label("Commit message")
            .halign(gtk::Align::Start)
            .valign(gtk::Align::Start)
            .margin_start(9)
            .margin_top(9)
            .can_target(false)
            .build();
        placeholder.add_css_class("dim-label");
        let overlay = gtk::Overlay::builder().child(&message_scroller).build();
        overlay.add_overlay(&placeholder);

        let commit = gtk::Button::builder()
            .label("Commit")
            .halign(gtk::Align::End)
            .sensitive(false)
            .build();
        commit.add_css_class("suggested-action");
        // Commit sits in the branch row beside Sync rather than on a line of its own: the two are
        // the pane's actions, a row of its own cost 40 px of a column that also has to hold the
        // changes and the history, and a message box that now grows needs that room. Trailing
        // edge, which is where GNOME puts the affirmative action. It is hidden and shown with the
        // box below it, so a clean tree still shows neither.
        branch_row.append(&commit);
        // Nothing but the message box now, kept as its own container so the whole thing is hidden
        // and shown in one call.
        let commit_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
        commit_box.append(&overlay);

        let changes = gio::ListStore::new::<glib::BoxedAnyObject>();
        let changes_view = gtk::ListView::new(
            Some(gtk::NoSelection::new(Some(changes.clone()))),
            None::<gtk::SignalListItemFactory>,
        );
        changes_view.add_css_class("navigation-sidebar");
        // One click opens the diff, which is the rule the tree already follows: see
        // `set_single_click_activate` in `tree.rs`.
        changes_view.set_single_click_activate(true);

        let log = gio::ListStore::new::<glib::BoxedAnyObject>();
        let log_view = gtk::ListView::new(
            Some(gtk::NoSelection::new(Some(log.clone()))),
            None::<gtk::SignalListItemFactory>,
        );
        log_view.add_css_class("navigation-sidebar");
        // The lane a commit sits in is drawn per row, so the row's own vertical margin leaves a
        // gap between its line and the next one's and the graph comes out dashed. The rule in
        // `install_chrome_css` takes the margin off; the breathing room moves onto the text.
        log_view.add_css_class("git-log");

        let divider = gtk::Paned::builder()
            .orientation(gtk::Orientation::Vertical)
            .start_child(&scroller(&changes_view))
            .end_child(&scroller(&log_view))
            .resize_start_child(true)
            .resize_end_child(true)
            .shrink_start_child(false)
            .shrink_end_child(false)
            .vexpand(true)
            .build();
        place_once(&divider);

        let column = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_start(6)
            .margin_end(6)
            .margin_top(6)
            .margin_bottom(6)
            .build();
        column.append(&chooser);
        column.append(&branch_row);
        column.append(&commit_box);
        column.append(&divider);

        let stack = gtk::Stack::builder().vexpand(true).build();
        stack.add_named(&empty_page(&check), Some("empty"));
        stack.add_named(&column, Some("repo"));
        stack.set_visible_child_name("empty");

        let panel = Rc::new(Panel {
            tree: Cell::new(hooks.tree),
            collapsed: RefCell::new(HashSet::new()),
            hooks,
            root: stack.clone().upcast(),
            stack,
            column,
            names,
            chooser,
            branch_label,
            branch_menu,
            branch_list,
            branch_shown: RefCell::new((Vec::new(), None)),
            counts,
            sync,
            message,
            placeholder,
            commit,
            commit_box,
            divider,
            changes,
            log,
            log_view: log_view.clone(),
            has_more: Cell::new(false),
            state: RefCell::new(State::default()),
            pending: RefCell::new(None),
            busy: Cell::new(false),
            again: Cell::new(false),
            watches: RefCell::new(Vec::new()),
            syncing: Cell::new(false),
            sync_busy: Cell::new(false),
            fetch_busy: Cell::new(false),
            fetched_once: Cell::new(false),
            missed_fetch: Cell::new(false),
            expanded: RefCell::new(None),
        });
        // Wiring comes after the `Rc` exists, so every closure can hold the panel weakly: they
        // all live in its own widget tree, and a strong capture there is a cycle.
        panel.wire_header(&check, &create);
        panel.wire_autofetch();
        panel.wire_commit();
        panel.wire_changes(&changes_view);
        panel.wire_log(&log_view);
        panel
    }

    /// The pane itself, for the sidebar's stack.
    pub fn widget(&self) -> &gtk::Widget {
        &self.root
    }

    /// The divider between the changes and the log, so a double-click on it can be reset.
    pub fn divider(&self) -> &gtk::Paned {
        &self.divider
    }

    /// Whether the vault touches any repository at all. The pane hides itself when it does not.
    pub fn has_repos(&self) -> bool {
        !self.state.borrow().repos.is_empty()
    }

    /// How many rows the history list holds, and activating its last one. Only `ACCENT_BENCH_GIT`
    /// calls these: the headless image has no pointer, and the Load More row is only worth
    /// anything if activating the end of the list really pages the next chunk in.
    pub fn log_rows(&self) -> u32 {
        self.log.n_items()
    }

    pub fn activate_last_log_row(&self) {
        if let Some(last) = self.log.n_items().checked_sub(1) {
            self.log_view.emit_by_name::<()>("activate", &[&last]);
        }
    }

    /// How many rows the changes list holds. `ACCENT_BENCH_GIT` prints it either side of a
    /// [`Panel::set_tree`], which is how the grouping is proven without a pointer.
    pub fn changes_rows(&self) -> u32 {
        self.changes.n_items()
    }

    /// Whether the changes list is grouped by folder, and putting it either way. The preference
    /// has two surfaces — this one is the Preferences switch, through `App::apply_config` — so
    /// nothing here writes the config back; only a move really redraws.
    pub fn tree(&self) -> bool {
        self.tree.get()
    }

    pub fn set_tree(&self, on: bool) {
        if self.tree.replace(on) != on {
            self.rebuild_changes();
        }
    }

    /// Ask git again, once, in [`DEBOUNCE`]. Calling this ten times in a row is one query.
    pub fn schedule_refresh(self: &Rc<Self>) {
        if let Some(id) = self.pending.borrow_mut().take() {
            id.remove();
        }
        let panel = self.clone();
        let id = glib::timeout_add_local_once(DEBOUNCE, move || {
            panel.pending.replace(None);
            panel.refresh();
        });
        self.pending.replace(Some(id));
    }

    // --- the background fetch -----------------------------------------------------------------

    /// Fetch the selected repository's remote, off the main thread, and refresh once it lands.
    ///
    /// The selected repository only. `git::discover` finds every repository a vault touches and
    /// fetching all of them would put one network round trip per repository on a timer, for rows
    /// nobody is looking at; this is the one the pane and the status bar are speaking for.
    ///
    /// A failure is logged and nothing else — no toast, no dialog, no badge. A fetch nobody asked
    /// for that fails every five minutes because the laptop is on a train would otherwise be a
    /// notification every five minutes, and the honest consequence of a failed fetch is already
    /// on screen: the counts stay as stale as they were.
    fn autofetch(self: &Rc<Self>) {
        if self.fetch_busy.get() {
            return;
        }
        let Some(repo) = ({
            let state = self.state.borrow();
            state.repos.get(state.selected).cloned()
        }) else {
            return;
        };
        self.fetch_busy.set(true);
        let vault = self.hooks.vault.clone();
        let panel = self.clone();
        glib::spawn_future_local(async move {
            let fetched = gio::spawn_blocking(move || vault.git_fetch(&repo)).await;
            panel.fetch_busy.set(false);
            match fetched {
                // A fetch that brought nothing prints nothing, so this is quiet in the common case.
                Ok(Ok(transcript)) => {
                    if !transcript.is_empty() {
                        tracing::debug!("git fetch: {transcript}");
                    }
                }
                Ok(Err(e)) => return tracing::debug!("git fetch: {e:#}"),
                Err(_) => return tracing::warn!("the git worker panicked"),
            }
            // `.git/refs/remotes` is not among the paths the vault watches, so what a fetch moved
            // is only seen because we ask.
            panel.schedule_refresh();
        });
    }

    /// Start the timer that keeps the remote-tracking refs current.
    ///
    /// The tick is gated on the window having the focus rather than started and stopped, because
    /// a `GSource` removed and re-added would also restart its five minutes; what a skipped tick
    /// leaves behind is a flag the next focus-in reads.
    fn wire_autofetch(self: &Rc<Self>) {
        let (weak, window) = (Rc::downgrade(self), self.hooks.window.downgrade());
        glib::timeout_add_local(AUTOFETCH, move || {
            let (Some(panel), Some(window)) = (weak.upgrade(), window.upgrade()) else {
                return glib::ControlFlow::Break;
            };
            match window.is_active() {
                true => panel.autofetch(),
                false => panel.missed_fetch.set(true),
            }
            glib::ControlFlow::Continue
        });
        // Weak both ways: the window owns the sidebar that owns this pane, and the pane holds the
        // window through its hooks, so a strong capture here is a cycle neither side can break.
        let weak = Rc::downgrade(self);
        self.hooks
            .window
            .connect_notify_local(Some("is-active"), move |window, _| {
                if let Some(panel) = weak.upgrade()
                    && window.is_active()
                    && panel.missed_fetch.replace(false)
                {
                    panel.autofetch();
                }
            });
    }

    // --- wiring -------------------------------------------------------------------------------

    fn wire_header(self: &Rc<Self>, check: &gtk::Button, create: &gtk::Button) {
        on_click(self, &self.sync, |panel| panel.sync(None));
        on_click(self, check, |panel| panel.refresh());
        on_click(self, create, |panel| panel.create_branch());

        let weak = Rc::downgrade(self);
        self.chooser.connect_selected_notify(move |chooser| {
            let Some(panel) = weak.upgrade() else {
                return;
            };
            // Splicing the name list moves the selection, and that notify is not the user.
            if panel.syncing.get() {
                return;
            }
            panel.state.borrow_mut().selected = chooser.selected() as usize;
            panel.refresh();
        });
    }

    fn wire_commit(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.message.buffer().connect_changed(move |_| {
            if let Some(panel) = weak.upgrade() {
                panel.sync_commit();
            }
        });
        on_click(self, &self.commit, |panel| panel.do_commit());

        // Ctrl+Return commits from inside the box, which is where the hands already are. This
        // controller only gets the chord while no window accelerator claims it; while one does,
        // `commit_if_focused` below is how the window hands it over.
        let keys = gtk::EventControllerKey::new();
        let weak = Rc::downgrade(self);
        keys.connect_key_pressed(move |_, key, _, state| {
            let commit = matches!(key, gdk::Key::Return | gdk::Key::KP_Enter)
                && state.contains(gdk::ModifierType::CONTROL_MASK);
            match (commit, weak.upgrade()) {
                (true, Some(panel)) => {
                    panel.do_commit();
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        });
        self.message.add_controller(keys);
    }

    /// Commit, if the message box is where the keyboard is. GTK dispatches a window accelerator
    /// ahead of every controller on the focused widget, so the box cannot take `Ctrl+Return` back
    /// from `win.newline-below` by itself: the window's handler offers it here first.
    pub fn commit_if_focused(self: &Rc<Self>) -> bool {
        let mine = self.message.has_focus();
        if mine {
            self.do_commit();
        }
        mine
    }

    /// Put the keyboard in the commit box, which is what `Ctrl+Shift+G` is for once the pane is
    /// up. From an idle: the chord shows the pane in the same frame, and a widget that is not on
    /// screen yet cannot take focus.
    pub fn focus_commit(&self) {
        if self.stack.visible_child_name().as_deref() != Some("repo") {
            return;
        }
        let message = self.message.clone();
        glib::idle_add_local_once(move || {
            // Mapped, not merely visible: a box hidden because there is nothing to commit leaves
            // its children visible in their own right, and focus would go nowhere.
            if message.is_mapped() {
                message.grab_focus();
            }
        });
    }

    fn wire_changes(self: &Rc<Self>, view: &gtk::ListView) {
        let factory = gtk::SignalListItemFactory::new();
        let weak = Rc::downgrade(self);
        factory.connect_setup(move |_, item| {
            if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
                item.set_child(Some(&change_row(item, &weak)));
            }
        });
        let weak = Rc::downgrade(self);
        factory.connect_bind(move |_, item| {
            if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
                bind_change(item, &weak);
            }
        });
        view.set_factory(Some(&factory));

        let weak = Rc::downgrade(self);
        view.connect_activate(move |view, position| {
            let Some(panel) = weak.upgrade() else {
                return;
            };
            if let Some(row) = row_at(view.model().as_ref(), position) {
                panel.activate(&row);
            }
        });
    }

    fn wire_log(self: &Rc<Self>, view: &gtk::ListView) {
        let factory = gtk::SignalListItemFactory::new();
        let weak = Rc::downgrade(self);
        factory.connect_setup(move |_, item| {
            if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
                item.set_child(Some(&log_row(item, &weak)));
            }
        });
        let weak = Rc::downgrade(self);
        factory.connect_bind(move |_, item| {
            if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
                bind_log(item, &weak);
            }
        });
        view.set_factory(Some(&factory));

        // The same one-click rule as the changes list and the tree: a commit opens its file list,
        // a file in it opens its diff.
        view.set_single_click_activate(true);
        let weak = Rc::downgrade(self);
        view.connect_activate(move |view, position| {
            let (Some(panel), Some(item)) =
                (weak.upgrade(), log_at(view.model().as_ref(), position))
            else {
                return;
            };
            match item {
                LogItem::Commit(row) => panel.toggle(&row.commit),
                LogItem::File {
                    oid, parent, path, ..
                } => panel.compare(&path, &path, Sides::Commit { oid, parent }),
                LogItem::More => panel.load_more(),
            }
        });
    }

    // --- refresh ------------------------------------------------------------------------------

    /// Ask git everything the pane shows, off the main thread, and put the answers on screen.
    fn refresh(self: &Rc<Self>) {
        if self.busy.get() {
            self.again.set(true);
            return;
        }
        self.busy.set(true);
        let vault = self.hooks.vault.clone();
        let selected = self.state.borrow().selected;
        let panel = self.clone();
        glib::spawn_future_local(async move {
            let fetched = gio::spawn_blocking(move || fetch(&vault, selected)).await;
            panel.busy.set(false);
            match fetched {
                Ok(fetched) => panel.apply(fetched),
                Err(_) => tracing::warn!("the git worker panicked"),
            }
            if panel.again.replace(false) {
                panel.refresh();
            }
        });
    }

    fn apply(self: &Rc<Self>, fetched: Fetched) {
        if self.state.borrow().repos != fetched.repos {
            self.syncing.set(true);
            let names: Vec<&str> = fetched.repos.iter().map(|r| r.name.as_str()).collect();
            self.names.splice(0, self.names.n_items(), &names);
            let selected = clamp(self.state.borrow().selected, fetched.repos.len());
            self.state.borrow_mut().selected = selected;
            self.chooser.set_selected(selected as u32);
            self.syncing.set(false);
        }
        self.chooser.set_visible(fetched.repos.len() > 1);
        self.stack
            .set_visible_child_name(match fetched.repos.is_empty() {
                true => "empty",
                false => "repo",
            });

        let selected = clamp(self.state.borrow().selected, fetched.repos.len());
        let heads: HashMap<PathBuf, String> = fetched
            .repos
            .iter()
            .zip(&fetched.statuses)
            .filter_map(|(repo, status)| Some((repo.git_dir.clone(), status.branch.oid.clone()?)))
            .collect();
        let ignored = fetched
            .repos
            .iter()
            .zip(&fetched.statuses)
            .flat_map(|(repo, status)| {
                status
                    .ignored
                    .iter()
                    .map(|path| ignored_key(&self.hooks.vault.root(), repo, path))
            })
            .collect();

        let head = fetched
            .statuses
            .get(selected)
            .and_then(|s| branch_parts(&s.branch));
        self.counts
            .set_text(head.as_ref().map_or("", |(_, counts)| counts.as_str()));
        let (names, at) = branch_model(head.map(|(name, _)| name), &fetched.branches);
        self.set_branches(&names, at);
        // What a Sync would do, in words, beside the counts it already shows. A branch with no
        // upstream is not a dead end any more: syncing it publishes it (`git::sync`), so the
        // button stays live and says which of the two it will be.
        let branch = fetched.statuses.get(selected).map(|s| &s.branch);
        self.sync.set_sensitive(branch.is_some());
        self.sync
            .set_tooltip_text(branch.map(sync_hint).as_deref().or(Some("Sync")));
        // Most refreshes read back the history that is already on screen — a save, a watcher
        // event and a `.git` write each schedule one — and splicing then costs an expanded commit
        // its file list and flashes every row, so only a real difference is drawn. A page that
        // has not moved also leaves whatever Load More added below it alone.
        // The incoming set is part of what a row draws, and pulling a fast-forward leaves the
        // commit list from `--all` exactly as it was — same oids, same order — so without this
        // the marks would survive the pull that cleared them.
        let moved = {
            let state = self.state.borrow();
            !same_head(&state.commits, &fetched.commits) || state.incoming != fetched.incoming
        };
        let page = moved.then(|| fetched.commits.clone());

        {
            let mut state = self.state.borrow_mut();
            state.head_moved = state.heads != heads;
            state.heads = heads;
            state.ignored = ignored;
            state.repos = fetched.repos;
            state.statuses = fetched.statuses;
            if moved {
                state.commits = fetched.commits;
            }
            state.branches = fetched.branches;
            state.submodules = fetched.submodules;
            state.incoming = fetched.incoming;
            state.selected = selected;
        }
        // git has answered for the first time since the window opened this vault, so there is a
        // repository to fetch at last. Everything after this is the timer's.
        if !self.state.borrow().repos.is_empty() && !self.fetched_once.replace(true) {
            self.autofetch();
        }
        // After the state is written: the changes list is drawn from it, so that the tree toggle
        // and a folder's chevron redraw the same rows without a `git status` of their own.
        self.rebuild_changes();
        if let Some(page) = page {
            self.has_more.set(page.len() >= PAGE);
            self.fill_log(page, 0);
        }
        self.sync_commit();
        (self.hooks.changed)();
        self.reload_diffs();
    }

    /// Draw the changes list from what the last refresh learned, and nothing else: what the tree
    /// toggle and a folder row both need, neither of them being a reason to ask git again.
    fn rebuild_changes(&self) {
        let rows = {
            let state = self.state.borrow();
            match (
                state.statuses.get(state.selected),
                state.repos.get(state.selected),
            ) {
                (Some(status), Some(repo)) => rows_of(
                    status,
                    &state.submodules,
                    &|path| vault_key(&self.hooks.vault.root(), repo, path),
                    self.tree.get(),
                    &self.collapsed.borrow(),
                ),
                _ => Vec::new(),
            }
        };
        let items: Vec<glib::BoxedAnyObject> =
            rows.into_iter().map(glib::BoxedAnyObject::new).collect();
        self.changes.splice(0, self.changes.n_items(), &items);
    }

    /// Put the branch popover on `names`, `at` being the row HEAD is on.
    fn set_branches(self: &Rc<Self>, names: &[String], at: Option<usize>) {
        self.branch_label
            .set_text(at.and_then(|i| names.get(i)).map_or("", String::as_str));
        if *self.branch_shown.borrow() == (names.to_vec(), at) {
            return;
        }
        self.branch_shown.replace((names.to_vec(), at));
        while let Some(row) = self.branch_list.first_child() {
            self.branch_list.remove(&row);
        }
        for (i, name) in names.iter().enumerate() {
            let row = self.branch_row(name, Some(i) != at);
            self.branch_list.append(&row);
        }
    }

    /// One row of the branch popover: the name, which switches to it, and — where git would let
    /// it go — a trash button. The branch HEAD is on has none: git refuses to delete it, and a
    /// control that cannot work is dead chrome (DESIGN.md, Principle 1).
    fn branch_row(self: &Rc<Self>, name: &str, deletable: bool) -> gtk::Box {
        let switch = gtk::Button::builder()
            .child(&gtk::Label::builder().label(name).xalign(0.0).build())
            .hexpand(true)
            .build();
        switch.add_css_class("flat");
        let (weak, asked) = (Rc::downgrade(self), name.to_string());
        switch.connect_clicked(move |_| {
            if let Some(panel) = weak.upgrade() {
                panel.branch_menu.popdown();
                panel.checkout(asked.clone());
            }
        });

        let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        row.append(&switch);
        if deletable {
            let trash = icon_button("user-trash-symbolic", "Delete Branch");
            // The hover rule the changed files' actions already follow, and the same class: a
            // `GtkListBoxRow`'s node is `row`, which is what that CSS selects on.
            trash.add_css_class("git-actions");
            let (weak, asked) = (Rc::downgrade(self), name.to_string());
            trash.connect_clicked(move |_| {
                if let Some(panel) = weak.upgrade() {
                    panel.branch_menu.popdown();
                    panel.delete_branch(asked.clone(), false);
                }
            });
            row.append(&trash);
        }
        row
    }

    /// Put `commits` on the graph. `keep` is how many leading rows the store already holds
    /// unchanged: [`git::lanes`] is one forward pass, so a Load More can only append, and
    /// appending leaves the reader where they were instead of scrolling back to the top.
    fn fill_log(&self, commits: Vec<Commit>, keep: usize) {
        self.collapse();
        let rows = git::lanes(commits);
        let keep = keep.min(rows.len()) as u32;
        let mut items: Vec<glib::BoxedAnyObject> = rows[keep as usize..]
            .iter()
            .cloned()
            .map(|row| glib::BoxedAnyObject::new(LogItem::Commit(row)))
            .collect();
        // The splice reaches the end of the store, so this is also what takes the row away again
        // once the last page has come in.
        if self.has_more.get() {
            items.push(glib::BoxedAnyObject::new(LogItem::More));
        }
        self.log
            .splice(keep, self.log.n_items().saturating_sub(keep), &items);
    }

    /// Take away whatever file list is open. The file rows of one commit are contiguous, and only
    /// one commit is ever expanded, so this is a single splice.
    fn collapse(&self) {
        self.expanded.replace(None);
        let mut start = None;
        let mut n = 0;
        for i in 0..self.log.n_items() {
            if matches!(log_at_index(&self.log, i), Some(LogItem::File { .. })) {
                start.get_or_insert(i);
                n += 1;
            }
        }
        if let Some(start) = start {
            self.log.splice(start, n, &[] as &[glib::BoxedAnyObject]);
        }
    }

    /// Show, or hide again, the files one commit changed.
    fn toggle(self: &Rc<Self>, commit: &Commit) {
        let was = self.expanded.borrow().clone();
        self.collapse();
        if was.as_deref() == Some(commit.id.as_str()) {
            return;
        }
        let repo = {
            let state = self.state.borrow();
            match state.repos.get(state.selected) {
                Some(repo) => repo.clone(),
                None => return,
            }
        };
        self.expanded.replace(Some(commit.id.clone()));
        let (oid, parent) = (commit.id.clone(), commit.parents.first().cloned());
        let panel = self.clone();
        glib::spawn_future_local(async move {
            let query = oid.clone();
            let vault = panel.hooks.vault.clone();
            let files = gio::spawn_blocking(move || vault.git_changed_files(&repo, &query)).await;
            let files = match files {
                Ok(Ok(files)) => files,
                Ok(Err(e)) => return tracing::debug!("git show --name-status: {e}"),
                Err(_) => return tracing::warn!("the git worker panicked"),
            };
            // A refresh, or another commit, may have landed while git was answering.
            if panel.expanded.borrow().as_deref() != Some(oid.as_str()) {
                return;
            }
            let Some(at) = panel.row_of_commit(&oid) else {
                return;
            };
            let rows: Vec<glib::BoxedAnyObject> = files
                .into_iter()
                .map(|(letter, path)| {
                    glib::BoxedAnyObject::new(LogItem::File {
                        oid: oid.clone(),
                        parent: parent.clone(),
                        letter,
                        path,
                    })
                })
                .collect();
            panel.log.splice(at + 1, 0, &rows);
        });
    }

    /// Where a commit sits in the log store, or `None` if it has since been spliced away.
    fn row_of_commit(&self, oid: &str) -> Option<u32> {
        (0..self.log.n_items()).find(|i| {
            matches!(log_at_index(&self.log, *i), Some(LogItem::Commit(row)) if row.commit.id == oid)
        })
    }

    fn load_more(self: &Rc<Self>) {
        let (repo, skip) = {
            let state = self.state.borrow();
            match state.repos.get(state.selected) {
                Some(repo) => (repo.clone(), state.commits.len()),
                None => return,
            }
        };
        // Cleared for the whole hop: the row stays where it is, and activating it again while
        // the page is in flight finds nothing left to ask for.
        if !self.has_more.replace(false) {
            return;
        }
        let panel = self.clone();
        glib::spawn_future_local(async move {
            let vault = panel.hooks.vault.clone();
            let page = gio::spawn_blocking(move || vault.git_log(&repo, skip, PAGE)).await;
            let page = match page {
                Ok(Ok(page)) => page,
                // Whatever went wrong, the history behind the row is still there, so it stays.
                Ok(Err(e)) => {
                    tracing::debug!("git log: {e}");
                    return panel.has_more.set(true);
                }
                Err(_) => {
                    tracing::warn!("the git worker panicked");
                    return panel.has_more.set(true);
                }
            };
            panel.has_more.set(page.len() >= PAGE);
            let commits = {
                let mut state = panel.state.borrow_mut();
                state.commits.extend(page);
                state.commits.clone()
            };
            // `skip` is how many commit rows the store already had, which after the collapse
            // inside `fill_log` is exactly how many of them stay.
            panel.fill_log(commits, skip);
        });
    }

    // --- commands -----------------------------------------------------------------------------

    /// Run one git command on the selected repository off the main thread, say what happened, and
    /// refresh. `hold` goes insensitive while the job runs, which is what a transfer needs; it is
    /// also what marks the job as one the user is waiting on, so [`Hooks::syncing`] runs with it
    /// and the status bar can spin for the same span.
    ///
    /// A failure gets a dialog rather than a toast: what git puts on stderr is the whole answer to
    /// "why did the push not go", and it is too long and too important to let scroll past.
    fn command(
        self: &Rc<Self>,
        verb: &'static str,
        hold: Option<gtk::Button>,
        job: impl FnOnce(&Vault, &Repo) -> anyhow::Result<String> + Send + 'static,
    ) {
        let repo = {
            let state = self.state.borrow();
            match state.repos.get(state.selected) {
                Some(repo) => repo.clone(),
                None => return,
            }
        };
        if let Some(button) = &hold {
            button.set_sensitive(false);
            self.sync_busy.set(true);
            (self.hooks.syncing)(true);
        }
        let panel = self.clone();
        let vault = self.hooks.vault.clone();
        glib::spawn_future_local(async move {
            let done = gio::spawn_blocking(move || job(&vault, &repo)).await;
            if let Some(button) = &hold {
                button.set_sensitive(true);
                panel.sync_busy.set(false);
                (panel.hooks.syncing)(false);
            }
            match done {
                Ok(Ok(message)) => (panel.hooks.toast)(&message),
                Ok(Err(e)) => panel.failed(verb, &format!("{e:#}")),
                Err(_) => tracing::warn!("the git worker panicked"),
            }
            // Straight away, not through the debounce: the user asked for this and is watching
            // the row it moves. The debounce is there to fold a burst of watcher events into one
            // query, and the `.git` write this just made will schedule one of those anyway.
            panel.refresh();
        });
    }

    fn failed(&self, verb: &str, message: &str) {
        let dialog = adw::AlertDialog::new(Some(&format!("{verb} Failed")), Some(message));
        dialog.add_response("close", "Close");
        dialog.set_default_response(Some("close"));
        dialog.set_close_response("close");
        dialog.present(Some(&self.hooks.window));
    }

    fn do_commit(self: &Rc<Self>) {
        let message = self.message_text();
        if message.trim().is_empty() {
            return;
        }
        // Nothing staged means "commit what changed", which is `git commit -a`: every tracked
        // file goes in and an untracked one stays untracked, as VS Code's smart commit does.
        let all = !self.to_commit().0;
        self.message.buffer().set_text("");
        self.command("Commit", None, move |vault, repo| {
            vault
                .git_commit(repo, &message, all)
                .map(|id| format!("Committed {id}"))
        });
    }

    /// Pull and then push the repository `key` sits in, or the selected one where `key` names no
    /// repository. The pane's selection follows, so the status bar's branch and the pane never
    /// end up talking about two different repositories.
    pub fn sync(self: &Rc<Self>, key: Option<&str>) {
        // One at a time. The pane's own button is insensitive for the duration, but the status
        // bar's branch is a second surface on the same action and stays clickable.
        if self.sync_busy.get() {
            return;
        }
        let index = {
            let state = self.state.borrow();
            key.and_then(|key| index_of(&state, &self.hooks.vault.root(), key))
                .unwrap_or(state.selected)
        };
        if self.state.borrow().selected != index {
            self.state.borrow_mut().selected = index;
            // The notify this fires is the same one a user's pick fires, refresh included.
            self.chooser.set_selected(index as u32);
        }
        let hold = self.sync.clone();
        self.command("Sync", Some(hold), |vault, repo| {
            vault.git_sync(repo).map(|transcript| {
                tracing::debug!("git sync: {transcript}");
                "Synced".to_string()
            })
        });
    }

    /// Switch the selected repository to a local branch.
    ///
    /// Whether that is safe is git's call: it refuses where a checkout would overwrite work that
    /// is not committed, and its refusal is a toast rather than a dialog because nothing was lost
    /// and there is nothing to decide. The refresh that follows puts the chooser back on whatever
    /// HEAD actually is, so a refused switch does not leave it naming a branch we are not on.
    fn checkout(self: &Rc<Self>, branch: String) {
        // The row a detached HEAD adds to the list is a readout, not a branch to switch to.
        let repo = {
            let state = self.state.borrow();
            match state.branches.contains(&branch) {
                true => state.repos.get(state.selected).cloned(),
                false => None,
            }
        };
        let Some(repo) = repo else {
            return;
        };
        let panel = self.clone();
        let vault = self.hooks.vault.clone();
        glib::spawn_future_local(async move {
            let asked = branch.clone();
            let done = gio::spawn_blocking(move || vault.git_checkout(&repo, &asked)).await;
            match done {
                Ok(Ok(())) => (panel.hooks.toast)(&format!("Switched to {branch}")),
                Ok(Err(e)) => (panel.hooks.toast)(&format!(
                    "Could not switch to {branch}: {}",
                    reason(&format!("{e:#}"))
                )),
                Err(_) => tracing::warn!("the git worker panicked"),
            }
            panel.refresh();
        });
    }

    /// Branch from HEAD and switch to it in one step, which is `git switch -c`: no base picker,
    /// because the base a reader means is the state they are looking at.
    ///
    /// The name is git's to validate — a bad ref name, one already taken and a worktree the
    /// switch would clobber are all its refusals, and they come back through [`Panel::command`]'s
    /// dialog, which also brings the refresh.
    fn create_branch(self: &Rc<Self>) {
        self.branch_menu.popdown();
        let entry = fileops::name_entry("Branch name", "");
        let form = gtk::Box::new(gtk::Orientation::Vertical, 12);
        form.append(&entry);
        let dialog = fileops::name_dialog("Create Branch", "Create", &form);

        let (panel, field) = (self.clone(), entry.clone());
        dialog.choose(
            Some(&self.hooks.window),
            gio::Cancellable::NONE,
            move |response| {
                let name = field.text().trim().to_string();
                if response != fileops::CONFIRM || name.is_empty() {
                    return;
                }
                panel.command("Create Branch", None, move |vault, repo| {
                    vault
                        .git_create_branch(repo, &name, true)
                        .map(|()| format!("Switched to {name}"))
                });
            },
        );
        // After `choose` has presented the dialog: the entry is mapped only by then.
        entry.grab_focus();
    }

    /// Delete a local branch.
    ///
    /// `git branch -d` first, so that whether the work would be lost is git's answer and not a
    /// guess of ours at a default branch. Its one refusal worth escalating is "not fully merged",
    /// which asks before running `-D`; every other refusal is reported as it comes.
    fn delete_branch(self: &Rc<Self>, name: String, force: bool) {
        let repo = {
            let state = self.state.borrow();
            match state.repos.get(state.selected) {
                Some(repo) => repo.clone(),
                None => return,
            }
        };
        let panel = self.clone();
        let vault = self.hooks.vault.clone();
        glib::spawn_future_local(async move {
            let asked = name.clone();
            let done =
                gio::spawn_blocking(move || vault.git_delete_branch(&repo, &asked, force)).await;
            match done {
                Ok(Ok(())) => (panel.hooks.toast)(&format!("Deleted {name}")),
                Ok(Err(e)) => {
                    let message = format!("{e:#}");
                    match !force && git::unmerged(&message) {
                        true => panel.confirm_delete(name),
                        false => panel.failed("Delete Branch", &message),
                    }
                }
                Err(_) => tracing::warn!("the git worker panicked"),
            }
            panel.refresh();
        });
    }

    /// The one delete that loses commits, so it asks first (DESIGN.md, States).
    fn confirm_delete(self: &Rc<Self>, name: String) {
        let dialog = adw::AlertDialog::new(
            Some(&format!("Delete {name}?")),
            Some("Its commits are not merged into any other branch and will be lost."),
        );
        dialog.add_responses(&[("cancel", "Cancel"), ("delete", "Delete")]);
        dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let panel = self.clone();
        dialog.choose(
            Some(&self.hooks.window),
            gio::Cancellable::NONE,
            move |response| {
                if response == "delete" {
                    panel.delete_branch(name, true);
                }
            },
        );
    }

    /// Put HEAD on one commit, detached, so the repository can be read at that point.
    ///
    /// No confirmation, for the reason [`Panel::checkout`] gives: `git switch --detach` refuses
    /// where it would clobber uncommitted work, and that refusal is the whole answer. The refresh
    /// that follows puts `Detached at …` in the branch button and the status bar.
    fn detach(self: &Rc<Self>, oid: String) {
        let repo = {
            let state = self.state.borrow();
            match state.repos.get(state.selected) {
                Some(repo) => repo.clone(),
                None => return,
            }
        };
        let panel = self.clone();
        let vault = self.hooks.vault.clone();
        glib::spawn_future_local(async move {
            let asked = oid.clone();
            let done = gio::spawn_blocking(move || vault.git_checkout_commit(&repo, &asked)).await;
            match done {
                Ok(Ok(())) => (panel.hooks.toast)(&format!("Checked out {}", short(&oid))),
                Ok(Err(e)) => (panel.hooks.toast)(&format!(
                    "Could not check out {}: {}",
                    short(&oid),
                    reason(&format!("{e:#}"))
                )),
                Err(_) => tracing::warn!("the git worker panicked"),
            }
            panel.refresh();
        });
    }

    fn stage(self: &Rc<Self>, paths: Vec<String>) {
        let n = paths.len();
        self.write("Stage", paths, move |vault, repo, paths| {
            vault
                .git_stage(repo, paths)
                .map(|()| format!("Staged {}", files(n)))
        });
    }

    fn unstage(self: &Rc<Self>, paths: Vec<String>) {
        let n = paths.len();
        self.write("Unstage", paths, move |vault, repo, paths| {
            vault
                .git_unstage(repo, paths)
                .map(|()| format!("Unstaged {}", files(n)))
        });
    }

    /// [`Panel::command`] for the three that take paths, which have to outlive the borrow.
    fn write(
        self: &Rc<Self>,
        verb: &'static str,
        paths: Vec<String>,
        job: impl FnOnce(&Vault, &Repo, &[String]) -> anyhow::Result<String> + Send + 'static,
    ) {
        if paths.is_empty() {
            return;
        }
        self.command(verb, None, move |vault, repo| job(vault, repo, &paths));
    }

    /// The paths of one whole section, for its header's bulk button.
    fn section_paths(&self, section: Section) -> Vec<String> {
        let state = self.state.borrow();
        let Some(status) = state.statuses.get(state.selected) else {
            return Vec::new();
        };
        let entries: Box<dyn Iterator<Item = &Entry>> = match section {
            Section::Conflicts => Box::new(status.conflicts()),
            Section::Staged => Box::new(status.staged()),
            Section::Changes => Box::new(status.changes()),
        };
        entries.map(|e| e.path.clone()).collect()
    }

    /// Discarding is the one thing here that loses work, so it asks first (DESIGN.md, States).
    fn discard(self: &Rc<Self>, entry: &Entry, key: &str) {
        let name = split_name(&entry.path).1.to_string();
        // An untracked file has nothing in the index to go back to, so what "discard" means for
        // it is that the file itself goes — to the trash, which is at least recoverable.
        let untracked = entry.x == '?';
        let body = match untracked {
            true => format!("{name} is not tracked, so it moves to the trash."),
            false => format!("{name} goes back to what the index holds. This cannot be undone."),
        };
        let dialog = adw::AlertDialog::new(Some("Discard Changes?"), Some(&body));
        dialog.add_responses(&[("cancel", "Cancel"), ("discard", "Discard")]);
        dialog.set_response_appearance("discard", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");

        let (panel, path, key) = (self.clone(), entry.path.clone(), key.to_string());
        dialog.choose(
            Some(&self.hooks.window),
            gio::Cancellable::NONE,
            move |response| {
                if response != "discard" {
                    return;
                }
                match untracked {
                    true => {
                        (panel.hooks.trash)(&key);
                        panel.schedule_refresh();
                    }
                    false => panel.write("Discard", vec![path], move |vault, repo, paths| {
                        vault
                            .git_discard(repo, paths)
                            .map(|()| format!("Discarded {name}"))
                    }),
                }
            },
        );
    }

    // --- the history's context menu -------------------------------------------------------------

    /// What can be done with the commit under the pointer.
    ///
    /// The shape `fileops::context_menu` uses, and for the reasons documented there: the popover
    /// hangs off a layout-managed box rather than off the list, the actions live on that same box
    /// so an item can resolve them, and the unparent waits for an idle because `closed` is emitted
    /// from inside the item's own click and an unparented popover has no path to the action group.
    fn commit_menu(self: &Rc<Self>, oid: &str, anchor: gdk::Rectangle) {
        self.column
            .insert_action_group(MENU_GROUP, Some(&self.commit_actions()));

        let menu = gio::Menu::new();
        menu.append_item(&menu_item("Check Out Commit", "checkout-commit", oid));
        // Its own section: reading an id out is not a thing that moves HEAD.
        let copy = gio::Menu::new();
        copy.append_item(&menu_item("Copy Commit ID", "copy-id", oid));
        menu.append_section(None, &copy);

        let popover = gtk::PopoverMenu::from_model(Some(&menu));
        // The sidebar behind it is a list, so the menu needs a background of its own.
        popover.add_css_class("git-menu");
        popover.set_parent(&self.column);
        popover.set_has_arrow(false);
        popover.set_pointing_to(Some(&anchor));
        popover.connect_closed(|p| {
            let p = p.clone();
            glib::idle_add_local_once(move || p.unparent());
        });
        popover.popup();
    }

    /// The two actions the menu items name, each taking the commit's id as its parameter.
    fn commit_actions(self: &Rc<Self>) -> gio::SimpleActionGroup {
        let group = gio::SimpleActionGroup::new();
        for (name, detach) in [("checkout-commit", true), ("copy-id", false)] {
            let action = gio::SimpleAction::new(name, Some(glib::VariantTy::STRING));
            let weak = Rc::downgrade(self);
            action.connect_activate(move |_, target| {
                let (Some(panel), Some(oid)) = (weak.upgrade(), target.and_then(|t| t.str()))
                else {
                    return;
                };
                match detach {
                    true => panel.detach(oid.to_string()),
                    // No toast for the clipboard alone would be truer to DESIGN.md, but nothing
                    // else on screen says the id was taken: the row looks the same either way.
                    false => {
                        panel.hooks.window.clipboard().set_text(oid);
                        (panel.hooks.toast)(&format!("Copied {}", short(oid)));
                    }
                }
            });
            group.add_action(&action);
        }
        group
    }

    // --- rows ---------------------------------------------------------------------------------

    fn activate(self: &Rc<Self>, row: &Row) {
        if let Row::Folder { path, section, .. } = row {
            let key = folder_key(*section, path);
            {
                let mut collapsed = self.collapsed.borrow_mut();
                if !collapsed.remove(&key) {
                    collapsed.insert(key);
                }
            }
            return self.rebuild_changes();
        }
        let Row::Entry {
            entry,
            section,
            key,
            ..
        } = row
        else {
            return;
        };
        match section {
            // A conflict is resolved in the file, not in a diff of two sides that both lost.
            Section::Conflicts => (self.hooks.open)(key),
            Section::Staged => self.compare(&entry.path, key, Sides::Staged),
            Section::Changes => self.compare(&entry.path, key, Sides::Worktree),
        }
    }

    /// Open the comparison a row stands for. Both sides are read in one worker hop, because two
    /// would show the file mid-write if it changed between them.
    fn compare(self: &Rc<Self>, rel: &str, key: &str, sides: Sides) {
        let repo = {
            let state = self.state.borrow();
            match state.repos.get(state.selected) {
                Some(repo) => repo.clone(),
                None => return,
            }
        };
        let what = What {
            repo,
            rel: rel.to_string(),
            key: key.to_string(),
            sides,
        };
        let panel = self.clone();
        let vault = self.hooks.vault.clone();
        glib::spawn_future_local(async move {
            let read = {
                let what = what.clone();
                gio::spawn_blocking(move || what.read(&vault)).await
            };
            match read {
                Ok(read) => panel.show(what, read),
                Err(_) => tracing::warn!("the git worker panicked"),
            }
        });
    }

    /// Put a freshly read comparison on screen, and remember it for the refreshes to come.
    fn show(self: &Rc<Self>, what: What, read: (Blob, Blob)) {
        let name = split_name(&what.rel).1.to_string();
        // The same test the tab opener uses, and the same answer: a diff of two binaries is
        // noise, so the pane says why instead of showing it.
        let (Blob::Text(left), Blob::Text(right)) = read else {
            return (self.hooks.toast)(&format!("{name} is binary"));
        };
        let left_title = format!("{name} ({})", what.sides.left_title());
        let right_title = format!("{name} ({})", what.sides.right_title());
        match what.sides.clone() {
            // The working tree is the file itself, so the comparison lives in its tab and the
            // refresh only ever has the index side to re-read.
            Sides::Worktree => {
                let (panel, key) = (Rc::downgrade(self), what.key.clone());
                let register = move |compare: Weak<Compare>| {
                    if let Some(panel) = panel.upgrade() {
                        panel.watch(what, Target::Tab(compare));
                    }
                };
                (self.hooks.compare_file)(&key, &left_title, &left, Box::new(register));
            }
            Sides::Staged | Sides::Commit { .. } => {
                let key = format!("diff:{}:{}", what.sides.tag(), what.key);
                let tab = (self.hooks.open_diff)(
                    &key,
                    &name,
                    &right_title,
                    (&left_title, &left),
                    (&right_title, &right),
                );
                // A commit never changes; the index does.
                if let (Some(tab), Sides::Staged) = (tab, &what.sides) {
                    self.watch(what, Target::Diff(Rc::downgrade(&tab)));
                }
            }
        }
    }

    /// The working-tree comparison of `key`, for the bench: the vault root is the repository,
    /// so the key is the path git knows.
    pub fn compare_worktree(self: &Rc<Self>, key: &str) {
        self.compare(key, key, Sides::Worktree);
    }

    /// One watch per comparison: asking for the same one again replaces the old entry.
    fn watch(&self, what: What, target: Target) {
        let mut watches = self.watches.borrow_mut();
        watches.retain(|w| !(w.what.key == what.key && w.what.sides.tag() == what.sides.tag()));
        watches.push(Watch { what, target });
    }

    /// Re-read every comparison still open, now that what git says has moved under it.
    fn reload_diffs(self: &Rc<Self>) {
        self.watches.borrow_mut().retain(|w| w.target.alive());
        let watches = self.watches.borrow().clone();
        if watches.is_empty() {
            return;
        }
        let vault = self.hooks.vault.clone();
        glib::spawn_future_local(async move {
            let reads = {
                let whats: Vec<What> = watches.iter().map(|w| w.what.clone()).collect();
                gio::spawn_blocking(move || {
                    whats.iter().map(|w| w.read(&vault)).collect::<Vec<_>>()
                })
                .await
            };
            let Ok(reads) = reads else {
                return tracing::warn!("the git worker panicked");
            };
            for (watch, (left, right)) in watches.into_iter().zip(reads) {
                let (Blob::Text(left), Blob::Text(right)) = (left, right) else {
                    continue;
                };
                match watch.target {
                    Target::Tab(compare) => {
                        if let Some(compare) = compare.upgrade() {
                            compare.set_side(Side::Old, &left);
                        }
                    }
                    Target::Diff(tab) => {
                        if let Some(tab) = tab.upgrade() {
                            tab.set_texts(&left, &right);
                        }
                    }
                }
            }
        });
    }

    // --- small state readers ------------------------------------------------------------------

    fn message_text(&self) -> String {
        let buffer = self.message.buffer();
        let (start, end) = buffer.bounds();
        buffer.text(&start, &end, false).to_string()
    }

    /// What the selected repository has to commit: whether anything is in the index, and whether
    /// there is anything at all — index or worktree — for a `git commit -a` to take.
    fn to_commit(&self) -> (bool, bool) {
        let state = self.state.borrow();
        let Some(status) = state.statuses.get(state.selected) else {
            return (false, false);
        };
        let staged = status.staged().next().is_some();
        (staged, staged || status.changes().next().is_some())
    }

    /// The placeholder, the Commit button and whether the box is there at all.
    fn sync_commit(&self) {
        let message = self.message_text();
        self.placeholder.set_visible(message.is_empty());
        let anything = self.to_commit().1;
        self.commit
            .set_sensitive(anything && !message.trim().is_empty());
        // A clean tree has nothing to say, so the box goes — but never out from under a message
        // being written: a refresh fires on every save, and one of those would take it away
        // mid-sentence. The button lives in the branch row now, so it is hidden by the same rule
        // rather than by being in the same container.
        let show = anything || !message.is_empty() || self.message.has_focus();
        self.commit_box.set_visible(show);
        self.commit.set_visible(show);
    }
}

/// What the rest of the window asks the pane, all of it answered from the last refresh: the
/// tree's dimmed rows, the status bar's branch and the editor's change marks. This pane is the
/// one place in the window that has already asked git.
impl Panel {
    /// Everything git ignores, across every repository the vault touches: vault-relative, with a
    /// wholly ignored directory keeping its trailing slash as git reports it.
    pub fn ignored(&self) -> HashSet<String> {
        self.state.borrow().ignored.clone()
    }

    /// The repository `key` lives in and its path inside it. The longest root wins, so a file in
    /// a nested repository is that repository's and not the vault's.
    pub fn repo_of(&self, key: &str) -> Option<(Repo, String)> {
        let state = self.state.borrow();
        let root = self.hooks.vault.root();
        let repo = state.repos.get(index_of(&state, &root, key)?)?;
        let rel = absolute(&root, key)
            .strip_prefix(&repo.root)
            .ok()?
            .to_string_lossy()
            .into_owned();
        Some((repo.clone(), rel))
    }

    /// The branch line for the repository `key` lives in, or for the selected one when `key` is
    /// `None`. What the status bar shows.
    pub fn branch_label(&self, key: Option<&str>) -> Option<String> {
        let state = self.state.borrow();
        let index = key
            .and_then(|key| index_of(&state, &self.hooks.vault.root(), key))
            .unwrap_or(state.selected);
        branch_text(&state.statuses.get(index)?.branch)
    }

    /// How many history rows are drawn as not pulled yet. `ACCENT_BENCH_GIT` and nothing else:
    /// the marking is otherwise only visible as a faded row.
    pub fn not_pulled_rows(&self) -> usize {
        let incoming = &self.state.borrow().incoming;
        (0..self.log.n_items())
            .filter(|i| {
                matches!(log_at_index(&self.log, *i),
                    Some(LogItem::Commit(row)) if incoming.contains(&row.commit.id))
            })
            .count()
    }

    /// What the Sync button says it will do. `ACCENT_BENCH_GIT` and nothing else: the button is
    /// otherwise a pair of arrows and a count, and a tooltip cannot be read from a screenshot.
    pub fn sync_hint(&self) -> Option<String> {
        self.sync.tooltip_text().map(|t| t.to_string())
    }

    /// Whether any repository's HEAD moved in the last refresh: a commit, a checkout or a pull.
    /// Anything drawn against HEAD is stale when this is true.
    pub fn head_changed(&self) -> bool {
        self.state.borrow().head_moved
    }

    /// The text of `key` as HEAD has it, or `None` when HEAD has no such file, git refused, or
    /// the file belongs to no repository.
    pub fn head_text(&self, key: &str, done: impl FnOnce(Option<String>) + 'static) {
        let Some((repo, rel)) = self.repo_of(key) else {
            return done(None);
        };
        let vault = self.hooks.vault.clone();
        glib::spawn_future_local(async move {
            let blob = gio::spawn_blocking(move || vault.git_show(&repo, "HEAD", &rel)).await;
            done(match blob {
                Ok(Ok(Some(Blob::Text(text)))) => Some(text),
                _ => None,
            });
        });
    }
}

/// Which two things a row's diff compares.
#[derive(Clone)]
enum Sides {
    /// HEAD against the index: what this commit would add.
    Staged,
    /// The index against the file on disk: what is not staged yet.
    Worktree,
    /// One commit against its first parent, which is what a file under an expanded history row
    /// shows. `parent` is `None` on a root commit, whose left side is simply empty.
    Commit { oid: String, parent: Option<String> },
}

impl Sides {
    /// The revision the left pane reads, `None` meaning there is nothing on that side at all.
    fn left_rev(&self) -> Option<&str> {
        match self {
            Sides::Staged => Some("HEAD"),
            Sides::Worktree => Some(""),
            Sides::Commit { parent, .. } => parent.as_deref(),
        }
    }

    fn left_title(&self) -> String {
        match self {
            Sides::Staged => "HEAD".to_string(),
            Sides::Worktree => "Index".to_string(),
            Sides::Commit { parent, .. } => match parent {
                Some(parent) => short(parent),
                None => "Nothing".to_string(),
            },
        }
    }

    fn right_title(&self) -> String {
        match self {
            Sides::Staged => "Index".to_string(),
            Sides::Worktree => "Working Tree".to_string(),
            Sides::Commit { oid, .. } => short(oid),
        }
    }

    /// What keys the tab, so the comparisons of one file are a tab each and asking twice reveals
    /// the one already open.
    fn tag(&self) -> String {
        match self {
            Sides::Staged => "index".to_string(),
            Sides::Worktree => "worktree".to_string(),
            Sides::Commit { oid, .. } => format!("commit:{}", short(oid)),
        }
    }
}

/// What one comparison compares: the half of a [`Watch`] the worker reads with.
#[derive(Clone)]
struct What {
    repo: Repo,
    /// Repository-relative, which is what git is asked with.
    rel: String,
    /// Vault key, which is what the working tree is read by and the tab is keyed by.
    key: String,
    sides: Sides,
}

/// A comparison that is open: what it compares, and where it is on screen.
#[derive(Clone)]
struct Watch {
    what: What,
    target: Target,
}

#[derive(Clone)]
enum Target {
    /// The file's own tab, comparing its buffer with the index.
    Tab(Weak<Compare>),
    /// A tab of its own over two blobs.
    Diff(Weak<DiffTab>),
}

impl Target {
    fn alive(&self) -> bool {
        match self {
            Target::Tab(w) => w.strong_count() > 0,
            Target::Diff(w) => w.strong_count() > 0,
        }
    }
}

impl What {
    /// Both sides, on the worker. A side git has no file for is a new or deleted file, and an
    /// empty string is exactly the right thing to diff against.
    fn read(&self, vault: &Vault) -> (Blob, Blob) {
        let left = match self.sides.left_rev() {
            Some(rev) => side(vault.git_show(&self.repo, rev, &self.rel)),
            None => Blob::Text(String::new()),
        };
        let right = match &self.sides {
            Sides::Staged => side(vault.git_show(&self.repo, "", &self.rel)),
            // The working tree side is the file itself, which on a remote vault is on the other
            // machine: reading it through the vault is what makes the diff work there as well
            // as here. It is read even though the tab shows its own buffer, so that the same
            // hop answers "is this binary" for both.
            Sides::Worktree => match vault.read_text(&self.key) {
                Ok(accent_api::fs::Read::Text(t)) => Blob::Text(t.text),
                Ok(_) => Blob::Binary,
                Err(e) => {
                    tracing::debug!("reading {}: {e}", self.key);
                    Blob::Text(String::new())
                }
            },
            Sides::Commit { oid, .. } => side(vault.git_show(&self.repo, oid, &self.rel)),
        };
        (left, right)
    }
}

/// An object name as git abbreviates it in the log.
fn short(oid: &str) -> String {
    oid.chars().take(7).collect()
}

// --- the worker's half --------------------------------------------------------------------------

/// What one refresh reads. Every repository's status, because the ignored set spans them all, and
/// the history and submodules of the selected one only.
struct Fetched {
    repos: Vec<Repo>,
    statuses: Vec<Status>,
    commits: Vec<Commit>,
    branches: Vec<String>,
    submodules: Vec<Submodule>,
    /// The commits a pull would bring in, which is what marks the history's rows. Asked for only
    /// where the branch says there are any, so an up-to-date repository pays nothing for it.
    incoming: HashSet<String>,
}

fn fetch(vault: &Vault, selected: usize) -> Fetched {
    let repos = vault.repos();
    let statuses: Vec<Status> = repos
        .iter()
        .map(|repo| match vault.git_status(repo) {
            Ok(status) => status,
            Err(e) => {
                // A repository git will not talk about costs an empty row, not a dialog: it may
                // be mid-rebase, on a network mount, or gone since discovery.
                tracing::debug!("git status in {}: {e}", repo.root.display());
                Status::default()
            }
        })
        .collect();
    let at = clamp(selected, repos.len());
    let (commits, branches, submodules) = match repos.get(at) {
        Some(repo) => (
            vault.git_log(repo, 0, PAGE).unwrap_or_else(|e| {
                tracing::debug!("git log: {e}");
                Vec::new()
            }),
            vault.git_branches(repo).unwrap_or_default(),
            vault.git_submodules(repo).unwrap_or_default(),
        ),
        None => (Vec::new(), Vec::new(), Vec::new()),
    };
    // `behind` is the count and this is the same set by oid, so one implies the other: nothing to
    // pull means no `rev-list` at all, which is what keeps a refresh on every save as cheap as it
    // was. A non-zero count also means there is an upstream, which the range needs.
    let behind = statuses.get(at).is_some_and(|s| s.branch.behind > 0);
    let incoming = match repos.get(at).filter(|_| behind) {
        Some(repo) => vault
            .git_incoming(repo)
            .unwrap_or_else(|e| {
                tracing::debug!("git rev-list HEAD..@{{u}}: {e}");
                Vec::new()
            })
            .into_iter()
            .collect(),
        None => HashSet::new(),
    };
    Fetched {
        repos,
        statuses,
        commits,
        branches,
        submodules,
        incoming,
    }
}

fn side(read: anyhow::Result<Option<Blob>>) -> Blob {
    match read {
        Ok(blob) => blob.unwrap_or_else(|| Blob::Text(String::new())),
        Err(e) => {
            tracing::debug!("git show: {e:#}");
            Blob::Text(String::new())
        }
    }
}

// --- widgets ------------------------------------------------------------------------------------

/// The line one changed file is shown on — status letter, name, directory — in the changes list
/// and under an expanded history row alike. Whatever comes after the directory, the changes list's
/// action buttons, is appended by the caller, and the binders find all four by sibling order.
fn file_line() -> gtk::Box {
    let letter = gtk::Label::builder().width_chars(1).build();
    for class in ["dim-label", "numeric", "monospace"] {
        letter.add_css_class(class);
    }
    let name = gtk::Label::builder()
        .xalign(0.0)
        .ellipsize(pango::EllipsizeMode::End)
        .build();
    // Ellipsized at the start: what tells two `notes/…/index.md` apart is the end of the path.
    let dir = gtk::Label::builder()
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(pango::EllipsizeMode::Start)
        .build();
    dir.add_css_class("dim-label");

    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    for child in [
        letter.upcast_ref::<gtk::Widget>(),
        name.upcast_ref(),
        dir.upcast_ref(),
    ] {
        row.append(child);
    }
    row
}

/// One changes row: a header layout and an entry layout in a stack, so a recycled row can be
/// either. The buttons hold the `GtkListItem` rather than the row's data, because the data is
/// replaced under them every time the row is reused.
fn change_row(item: &gtk::ListItem, panel: &Weak<Panel>) -> gtk::Stack {
    let title = gtk::Label::builder()
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(pango::EllipsizeMode::End)
        .build();
    title.add_css_class("heading");
    let all = gtk::Button::builder().build();
    all.add_css_class("flat");
    let weak = panel.clone();
    all.connect_clicked(glib::clone!(
        #[weak]
        item,
        move |_| {
            let (
                Some(panel),
                Some(Row::Header {
                    all: Some(section), ..
                }),
            ) = (weak.upgrade(), row_of(&item))
            else {
                return;
            };
            let paths = panel.section_paths(section);
            match section {
                Section::Staged => panel.unstage(paths),
                _ => panel.stage(paths),
            }
        }
    ));
    // One preference with two surfaces: this and the switch in Preferences write the same
    // `git_tree`. It rides the Changes header because that is where the list it reshapes begins,
    // and the binder puts it back on the preference every time the row is reused — which is why
    // the handler below has to recognise its own echo and do nothing.
    let view = gtk::ToggleButton::builder()
        .icon_name("view-list-symbolic")
        .tooltip_text("Group changed files by folder")
        .valign(gtk::Align::Center)
        .build();
    view.add_css_class("flat");
    let weak = panel.clone();
    view.connect_toggled(move |button| {
        let Some(panel) = weak.upgrade() else {
            return;
        };
        let on = button.is_active();
        if panel.tree.replace(on) == on {
            return;
        }
        (panel.hooks.set_tree)(on);
        panel.rebuild_changes();
    });

    let header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    header.append(&title);
    header.append(&all);
    header.append(&view);

    // A folder of the tree view: the chevron says whether it is open, the label carries whatever
    // segments this row adds to the one above it.
    let chevron = gtk::Image::new();
    let folder_name = gtk::Label::builder()
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(pango::EllipsizeMode::Start)
        .build();
    let folder = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    folder.append(&chevron);
    folder.append(&folder_name);

    let entry = file_line();

    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    actions.add_css_class("git-actions");
    for (icon, tooltip, act) in [
        ("list-add-symbolic", "Stage", Act::Stage),
        ("list-remove-symbolic", "Unstage", Act::Unstage),
        ("document-revert-symbolic", "Discard", Act::Discard),
    ] {
        let button = icon_button(icon, tooltip);
        let weak = panel.clone();
        button.connect_clicked(glib::clone!(
            #[weak]
            item,
            move |_| {
                let (Some(panel), Some(Row::Entry { entry, key, .. })) =
                    (weak.upgrade(), row_of(&item))
                else {
                    return;
                };
                match act {
                    Act::Stage => panel.stage(vec![entry.path]),
                    Act::Unstage => panel.unstage(vec![entry.path]),
                    Act::Discard => panel.discard(&entry, &key),
                }
            }
        ));
        actions.append(&button);
    }

    entry.append(&actions);

    // Not homogeneous: the header's button is taller than an entry row, and every row taking that
    // height would turn the list into a ladder.
    let stack = gtk::Stack::builder()
        .hhomogeneous(false)
        .vhomogeneous(false)
        .build();
    stack.add_named(&header, Some("header"));
    stack.add_named(&folder, Some("folder"));
    stack.add_named(&entry, Some("entry"));
    stack
}

/// Which of a row's three buttons was pressed.
#[derive(Clone, Copy)]
enum Act {
    Stage,
    Unstage,
    Discard,
}

fn bind_change(item: &gtk::ListItem, panel: &Weak<Panel>) {
    let (Some(stack), Some(row), Some(panel)) = (
        item.child().and_downcast::<gtk::Stack>(),
        row_of(item),
        panel.upgrade(),
    ) else {
        return;
    };
    let (Some(header), Some(folder), Some(entry)) = (
        stack.child_by_name("header").and_downcast::<gtk::Box>(),
        stack.child_by_name("folder").and_downcast::<gtk::Box>(),
        stack.child_by_name("entry").and_downcast::<gtk::Box>(),
    ) else {
        return;
    };
    let (Some(title), Some(view)) = (
        header.first_child().and_downcast::<gtk::Label>(),
        header.last_child().and_downcast::<gtk::ToggleButton>(),
    ) else {
        return;
    };
    let Some(all) = title.next_sibling().and_downcast::<gtk::Button>() else {
        return;
    };
    let (Some(letter), Some(actions)) = (
        entry.first_child().and_downcast::<gtk::Label>(),
        entry.last_child().and_downcast::<gtk::Box>(),
    ) else {
        return;
    };
    let (Some(name), Some(dir)) = (
        letter.next_sibling().and_downcast::<gtk::Label>(),
        actions.prev_sibling().and_downcast::<gtk::Label>(),
    ) else {
        return;
    };

    match row {
        Row::Header {
            title: text,
            all: section,
        } => {
            stack.set_visible_child_name("header");
            title.set_text(text);
            all.set_visible(section.is_some());
            all.set_label(match section {
                Some(Section::Staged) => "Unstage All",
                _ => "Stage All",
            });
            // One toggle for the whole list, on the section it is most about. Setting it here is
            // what the handler in `change_row` reads back as its own echo.
            view.set_visible(section == Some(Section::Changes));
            view.set_active(panel.tree.get());
        }
        Row::Folder {
            label,
            section,
            depth,
            path,
        } => {
            stack.set_visible_child_name("folder");
            let (Some(chevron), Some(text)) = (
                folder.first_child().and_downcast::<gtk::Image>(),
                folder.last_child().and_downcast::<gtk::Label>(),
            ) else {
                return;
            };
            let shut = panel
                .collapsed
                .borrow()
                .contains(&folder_key(section, &path));
            chevron.set_icon_name(Some(match shut {
                true => "pan-end-symbolic",
                false => "pan-down-symbolic",
            }));
            text.set_text(&label);
            folder.set_margin_start(depth as i32 * INDENT);
            stack.set_tooltip_text(Some(&path));
        }
        Row::Entry {
            entry: e,
            section,
            key,
            depth,
        } => {
            stack.set_visible_child_name("entry");
            entry.set_margin_start(depth as i32 * INDENT);
            letter.set_text(&status_letter(&e, section).to_string());
            let (directory, file) = split_name(&e.path);
            name.set_text(file);
            // Under a folder row the path is already on screen, and repeating it puts the
            // directory on the row twice.
            dir.set_text(match depth {
                0 => directory,
                _ => "",
            });
            stack.set_tooltip_text(Some(&e.path));
            actions.set_visible(true);
            let Some((stage, unstage, discard)) = triple(&actions) else {
                return;
            };
            // Staging a conflicted file is how git is told it is resolved, so the button is there
            // for it too; unstaging one is not a thing the pane offers.
            stage.set_visible(section != Section::Staged);
            unstage.set_visible(section == Section::Staged);
            // ponytail: an untracked file outside the vault has no Discard, because the only
            // thing to do with it is delete it and the trash hook takes vault keys. `git clean`
            // is the upgrade, and it wants a confirmation naming the file it removes for good.
            discard.set_visible(
                section == Section::Changes && (e.x != '?' || !Path::new(&key).is_absolute()),
            );
        }
        Row::Submodule(sub) => {
            stack.set_visible_child_name("entry");
            entry.set_margin_start(0);
            letter.set_text(&sub.state.to_string());
            let (directory, file) = split_name(&sub.path);
            name.set_text(file);
            dir.set_text(sub.describe.as_deref().unwrap_or(directory));
            stack.set_tooltip_text(Some(&sub.oid));
            actions.set_visible(false);
        }
    }
}

/// The Stage / Unstage / Discard buttons of a row, in the order [`change_row`] appended them.
fn triple(actions: &gtk::Box) -> Option<(gtk::Widget, gtk::Widget, gtk::Widget)> {
    let stage = actions.first_child()?;
    let unstage = stage.next_sibling()?;
    let discard = unstage.next_sibling()?;
    Some((stage, unstage, discard))
}

/// One log row: the graph on the left, the summary and its author on the right.
fn log_row(item: &gtk::ListItem, panel: &Weak<Panel>) -> gtk::Stack {
    let area = gtk::DrawingArea::new();
    // The draw reads the bound row straight off the list item, so a recycled row cannot draw the
    // graph of the commit that used to be in it.
    area.set_draw_func(glib::clone!(
        #[weak]
        item,
        move |_, cr, _, height| {
            if let Some(LogItem::Commit(row)) = log_of(&item) {
                draw_lanes(cr, &row, height as f64);
            }
        }
    ));

    // Ellipsized like every other name in the pane: a decoration is as long as the branch it
    // names, and without this a long branch is the sidebar's floor.
    let refs = gtk::Label::builder()
        .ellipsize(pango::EllipsizeMode::End)
        .build();
    for class in ["caption", "dim-label"] {
        refs.add_css_class(class);
    }
    let summary = gtk::Label::builder()
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(pango::EllipsizeMode::End)
        .build();
    // The same arrow the branch readout's `↓2` uses, so one symbol means "the remote has this and
    // we do not" in both places. Leading, where a dirty tab and the status bar put their dot.
    let not_pulled = gtk::Label::new(Some("↓"));
    for class in ["caption", "dim-label", "numeric"] {
        not_pulled.add_css_class(class);
    }
    let line = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    line.append(&not_pulled);
    line.append(&refs);
    line.append(&summary);

    let meta = gtk::Label::builder()
        .xalign(0.0)
        .ellipsize(pango::EllipsizeMode::End)
        .build();
    for class in ["caption", "dim-label"] {
        meta.add_css_class(class);
    }

    // The row's own vertical margin is off, so the breathing room lives here (see the `.git-log`
    // rule): the drawing area has to reach the row's edges for the lanes to join.
    let text = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .margin_top(4)
        .margin_bottom(4)
        .build();
    text.append(&line);
    text.append(&meta);

    let commit = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    commit.append(&area);
    commit.append(&text);

    // A file of the expanded commit, indented past the graph so it reads as belonging above it.
    let file = file_line();
    file.set_margin_start(LANE * 2);
    file.set_margin_top(2);
    file.set_margin_bottom(2);

    // Centred and quiet: it continues the history above it rather than competing with it.
    let more = gtk::Label::builder()
        .label("Load More")
        .margin_top(6)
        .margin_bottom(6)
        .build();
    for class in ["caption", "dim-label"] {
        more.add_css_class(class);
    }

    // Not homogeneous, for the reason `change_row` gives: a commit row is two lines tall and a
    // file row one, and every row taking the taller of the two would be a ladder.
    let stack = gtk::Stack::builder()
        .hhomogeneous(false)
        .vhomogeneous(false)
        .build();
    stack.add_named(&commit, Some("commit"));
    stack.add_named(&file, Some("file"));
    stack.add_named(&more, Some("more"));

    // A secondary click on a commit opens its menu. The gesture holds the `GtkListItem` rather
    // than the row's data, for the reason `change_row`'s buttons do: the data under a recycled
    // row is replaced without the widgets being rebuilt.
    let click = gtk::GestureClick::builder()
        .button(gdk::BUTTON_SECONDARY)
        .build();
    let weak = panel.clone();
    click.connect_pressed(glib::clone!(
        #[weak]
        item,
        move |gesture, _, x, y| {
            let (Some(panel), Some(LogItem::Commit(row))) = (weak.upgrade(), log_of(&item)) else {
                return;
            };
            gesture.set_state(gtk::EventSequenceState::Claimed);
            // Out of the row's coordinates and into the host box's, or the menu would point at
            // wherever that row happened to be when the list was last scrolled.
            let point = gtk::graphene::Point::new(x as f32, y as f32);
            let Some(at) = item
                .child()
                .and_then(|child| child.compute_point(&panel.column, &point))
            else {
                return;
            };
            let anchor = gdk::Rectangle::new(at.x() as i32, at.y() as i32, 1, 1);
            panel.commit_menu(&row.commit.id, anchor);
        }
    ));
    stack.add_controller(click);
    stack
}

fn bind_log(item: &gtk::ListItem, panel: &Weak<Panel>) {
    let (Some(stack), Some(item_row)) = (item.child().and_downcast::<gtk::Stack>(), log_of(item))
    else {
        return;
    };
    let (Some(commit), Some(file)) = (
        stack.child_by_name("commit").and_downcast::<gtk::Box>(),
        stack.child_by_name("file").and_downcast::<gtk::Box>(),
    ) else {
        return;
    };

    let row = match item_row {
        LogItem::Commit(row) => row,
        LogItem::File { letter, path, .. } => {
            stack.set_visible_child_name("file");
            let (Some(mark), Some(dir)) = (
                file.first_child().and_downcast::<gtk::Label>(),
                file.last_child().and_downcast::<gtk::Label>(),
            ) else {
                return;
            };
            let Some(name) = mark.next_sibling().and_downcast::<gtk::Label>() else {
                return;
            };
            mark.set_text(&letter.to_string());
            let (directory, base) = split_name(&path);
            name.set_text(base);
            dir.set_text(directory);
            stack.set_tooltip_text(Some(&path));
            return;
        }
        LogItem::More => {
            stack.set_visible_child_name("more");
            stack.set_tooltip_text(None);
            return;
        }
    };

    stack.set_visible_child_name("commit");
    let (Some(area), Some(text)) = (
        commit.first_child().and_downcast::<gtk::DrawingArea>(),
        commit.last_child().and_downcast::<gtk::Box>(),
    ) else {
        return;
    };
    let (Some(line), Some(meta)) = (
        text.first_child().and_downcast::<gtk::Box>(),
        text.last_child().and_downcast::<gtk::Label>(),
    ) else {
        return;
    };
    let (Some(not_pulled), Some(summary)) = (
        line.first_child().and_downcast::<gtk::Label>(),
        line.last_child().and_downcast::<gtk::Label>(),
    ) else {
        return;
    };
    let Some(refs) = not_pulled.next_sibling().and_downcast::<gtk::Label>() else {
        return;
    };

    area.set_content_width(lane_width(&row));
    area.queue_draw();
    refs.set_visible(!row.commit.refs.is_empty());
    refs.set_text(&row.commit.refs.join(", "));
    summary.set_text(&row.commit.summary);
    meta.set_text(&format!(
        "{} · {}",
        row.commit.author,
        ago(now(), row.commit.time)
    ));
    // Read off the last refresh's answer rather than stored on the row: `git rev-list HEAD..@{u}`
    // is what decides this, and a row that has since been pulled is marked by the refresh that
    // noticed, not by whatever was true when it was spliced in.
    let waiting = panel
        .upgrade()
        .is_some_and(|panel| panel.state.borrow().incoming.contains(&row.commit.id));
    not_pulled.set_visible(waiting);
    // The text alone, so the graph the drawing area beside it paints stays at full strength and a
    // lane still joins the rows above and below.
    text.set_opacity(match waiting {
        true => NOT_PULLED_DIM,
        false => 1.0,
    });
    stack.set_tooltip_text(Some(&match waiting {
        true => format!("Not pulled yet\n\n{}", commit_tooltip(&row.commit)),
        false => commit_tooltip(&row.commit),
    }));
}

/// The graph: the lanes passing this row, the edges into and out of this commit, and the node.
///
/// `above` and `below` are lane indices and `below[0]` is the commit's own column, so an edge is
/// always drawn between a lane's x and the node's, meeting at the row's middle.
fn draw_lanes(cr: &gtk::cairo::Context, row: &LogRow, height: f64) {
    let x = |lane: usize| (LANE / 2 + lane as i32 * LANE) as f64;
    let (middle, node) = (height / 2.0, x(row.column));
    cr.set_line_width(1.5);
    for &lane in &row.through {
        lane_source(cr, lane);
        cr.move_to(x(lane), 0.0);
        cr.line_to(x(lane), height);
        let _ = cr.stroke();
    }
    for &lane in &row.above {
        lane_source(cr, lane);
        cr.move_to(x(lane), 0.0);
        cr.line_to(node, middle);
        let _ = cr.stroke();
    }
    for &lane in &row.below {
        lane_source(cr, lane);
        cr.move_to(node, middle);
        cr.line_to(x(lane), height);
        let _ = cr.stroke();
    }
    lane_source(cr, row.column);
    cr.arc(node, middle, 3.5, 0.0, std::f64::consts::TAU);
    let _ = cr.fill();
}

fn lane_source(cr: &gtk::cairo::Context, lane: usize) {
    let colour = highlight::lane_colour(lane);
    cr.set_source_rgba(
        colour.red() as f64,
        colour.green() as f64,
        colour.blue() as f64,
        colour.alpha() as f64,
    );
}

/// Room for every lane this row touches, plus one lane of air before the text.
fn lane_width(row: &LogRow) -> i32 {
    let widest = row
        .above
        .iter()
        .chain(&row.below)
        .chain(&row.through)
        .chain(std::iter::once(&row.column))
        .copied()
        .max()
        .unwrap_or(0);
    (widest as i32 + 1) * LANE + LANE
}

/// The "No Repository" state, with the one refresh the pane cannot do for itself: a `git init`
/// in a vault that had no repository writes only inside `.git`, which the walk skips and which no
/// monitor is watching yet, so nothing would ever tell the pane to look again.
fn empty_page(check: &gtk::Button) -> adw::StatusPage {
    let page = adw::StatusPage::builder()
        .icon_name(SYNC_ICON)
        .title("No Repository")
        .description("Run git init in the terminal to start one.")
        .child(check)
        .vexpand(true)
        .build();
    // Without this the icon alone takes 128 px of a 200 px column (DESIGN.md, States).
    page.add_css_class("compact");
    page
}

/// A row of the repository chooser: one label, ellipsized where it has to fit the sidebar's width
/// and whole where it does not.
fn name_factory(ellipsize: bool) -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(move |_, item| {
        let label = gtk::Label::builder().xalign(0.0).build();
        if ellipsize {
            label.set_ellipsize(pango::EllipsizeMode::End);
        }
        if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
            item.set_child(Some(&label));
        }
    });
    factory.connect_bind(|_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        if let (Some(label), Some(name)) = (
            item.child().and_downcast::<gtk::Label>(),
            item.item().and_downcast::<gtk::StringObject>(),
        ) {
            label.set_text(&name.string());
        }
    });
    factory
}

/// One context-menu item carrying its commit id as a `String` target rather than in a
/// detailed-action string, which is the shape `fileops::item` settled on.
fn menu_item(label: &str, action: &str, oid: &str) -> gio::MenuItem {
    let item = gio::MenuItem::new(Some(label), None);
    item.set_action_and_target_value(
        Some(&format!("{MENU_GROUP}.{action}")),
        Some(&oid.to_variant()),
    );
    item
}

fn icon_button(icon: &str, tooltip: &str) -> gtk::Button {
    let button = gtk::Button::builder()
        .icon_name(icon)
        .tooltip_text(tooltip)
        .valign(gtk::Align::Center)
        .build();
    button.add_css_class("flat");
    button
}

fn scroller(child: &impl IsA<gtk::Widget>) -> gtk::ScrolledWindow {
    gtk::ScrolledWindow::builder()
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(child)
        .build()
}

/// The paned's default split, set once. `map` runs before the first allocation, so the height is
/// only known an idle later; after that the position is the user's.
fn place_once(divider: &gtk::Paned) {
    let placed = Cell::new(false);
    divider.connect_map(move |divider| {
        if placed.replace(true) {
            return;
        }
        let divider = divider.clone();
        glib::idle_add_local_once(move || {
            if divider.height() > 0 {
                divider.set_position(divider.height() * GIT_SHARE.0 / GIT_SHARE.1);
            }
        });
    });
}

/// Connect a button with only a weak hold on the panel: every one of these lives in the panel's
/// own widget tree, so a strong capture is a cycle that outlives the window.
fn on_click(panel: &Rc<Panel>, button: &gtk::Button, f: impl Fn(&Rc<Panel>) + 'static) {
    let weak = Rc::downgrade(panel);
    button.connect_clicked(move |_| {
        if let Some(panel) = weak.upgrade() {
            f(&panel);
        }
    });
}

fn row_of(item: &gtk::ListItem) -> Option<Row> {
    Some(
        item.item()
            .and_downcast::<glib::BoxedAnyObject>()?
            .borrow::<Row>()
            .clone(),
    )
}

fn log_of(item: &gtk::ListItem) -> Option<LogItem> {
    Some(
        item.item()
            .and_downcast::<glib::BoxedAnyObject>()?
            .borrow::<LogItem>()
            .clone(),
    )
}

fn log_at(model: Option<&gtk::SelectionModel>, position: u32) -> Option<LogItem> {
    Some(
        model?
            .item(position)
            .and_downcast::<glib::BoxedAnyObject>()?
            .borrow::<LogItem>()
            .clone(),
    )
}

fn log_at_index(store: &gio::ListStore, position: u32) -> Option<LogItem> {
    Some(
        store
            .item(position)
            .and_downcast::<glib::BoxedAnyObject>()?
            .borrow::<LogItem>()
            .clone(),
    )
}

fn row_at(model: Option<&gtk::SelectionModel>, position: u32) -> Option<Row> {
    Some(
        model?
            .item(position)
            .and_downcast::<glib::BoxedAnyObject>()?
            .borrow::<Row>()
            .clone(),
    )
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() as i64)
}

fn clamp(selected: usize, len: usize) -> usize {
    selected.min(len.saturating_sub(1))
}

fn absolute(vault_root: &Path, key: &str) -> PathBuf {
    match Path::new(key).is_absolute() {
        true => PathBuf::from(key),
        false => vault_root.join(key),
    }
}

/// The index of the repository `key` belongs to, the longest root winning so a file in a nested
/// repository is that repository's and not the vault's.
fn index_of(state: &State, vault_root: &Path, key: &str) -> Option<usize> {
    let full = absolute(vault_root, key);
    state
        .repos
        .iter()
        .enumerate()
        .filter(|(_, repo)| full.starts_with(&repo.root))
        .max_by_key(|(_, repo)| repo.root.as_os_str().len())
        .map(|(index, _)| index)
}

/// One ignored path as the tree names it. git reports a wholly ignored directory as one entry
/// with a trailing slash, and that slash is the whole difference between a folder and a file, so
/// it is put back after the join.
fn ignored_key(vault_root: &Path, repo: &Repo, path: &str) -> String {
    let key = vault_key(vault_root, repo, path.trim_end_matches('/'));
    match path.ends_with('/') {
        true => key + "/",
        false => key,
    }
}

// --- pure helpers, the part of this module the tests can reach ------------------------------------

/// How long ago, in one field. Never more precise than the reader can use: a commit from this
/// morning is "5 h", not "5 h 12 min".
fn ago(now: i64, then: i64) -> String {
    let seconds = (now - then).max(0);
    let (minute, hour, day) = (60, 3600, 86_400);
    // 30 and 365 days, which is what every relative timestamp means by a month and a year.
    let (month, year) = (30 * day, 365 * day);
    if seconds < minute {
        "just now".to_string()
    } else if seconds < hour {
        format!("{} min", seconds / minute)
    } else if seconds < day {
        format!("{} h", seconds / hour)
    } else if seconds < month {
        format!("{} d", seconds / day)
    } else if seconds < year {
        format!("{} mo", seconds / month)
    } else {
        format!("{} y", seconds / year)
    }
}

/// A path split into the directory and the file name, both borrowed. A file at the top level has
/// an empty directory rather than a `.`, because the row shows the string as it is.
///
/// git reports a wholly untracked directory as one entry ending in `/`. That slash belongs to the
/// name — it is what tells the row apart from a file — so the split ignores it and the name keeps
/// it; otherwise the name would come out empty and the whole row would read as its dimmed
/// directory label.
fn split_name(path: &str) -> (&str, &str) {
    let body = path.strip_suffix('/').unwrap_or(path);
    match body.rsplit_once('/') {
        Some((dir, _)) => (dir, &path[dir.len() + 1..]),
        None => ("", path),
    }
}

/// What hovering a commit says: where it sits, what it is called, and the whole message.
///
/// The decorations `git log` already fetched rather than a `git branch --contains` per hover, so
/// a commit that is no branch tip simply has no first line.
fn commit_tooltip(c: &Commit) -> String {
    let head = match c.refs.is_empty() {
        true => short(&c.id),
        false => format!("{}\n{}", c.refs.join(", "), short(&c.id)),
    };
    let message = match c.body.is_empty() {
        true => c.summary.clone(),
        false => format!("{}\n\n{}", c.summary, c.body),
    };
    format!("{head}\n\n{message}")
}

/// A repository-relative path as the rest of the app names it: vault-relative where the file is
/// in the vault, absolute where the repository reaches outside it.
fn vault_key(vault_root: &Path, repo: &Repo, repo_rel: &str) -> String {
    let full = repo.root.join(repo_rel);
    match full.strip_prefix(vault_root) {
        Ok(rel) => rel.to_string_lossy().into_owned(),
        Err(_) => full.to_string_lossy().into_owned(),
    }
}

/// Whether a freshly-read first page says the history has not moved: the same commits, whole and
/// in the same order, at the head of what the pane already holds. Whole commits and not their ids
/// alone, so that a branch moving onto a commit — a decoration, and nothing else — still redraws.
///
/// A page that is longer than what is held is a first refresh or a shorter history; either way it
/// has to be drawn. A page that is shorter is what a Load More leaves behind, and its own rows
/// stay where they are.
fn same_head(held: &[Commit], page: &[Commit]) -> bool {
    held.len() >= page.len() && held[..page.len()] == *page
}

/// The branch chooser's rows and which of them HEAD is on: the local branches, led by whatever
/// HEAD is on when that is not one of them — a detached HEAD, or a branch with no commit yet, so
/// that the chooser always says where the repository actually is. `None` is a repository git told
/// us nothing about, which shows an empty chooser as it used to show an empty label.
fn branch_model(head: Option<String>, branches: &[String]) -> (Vec<String>, Option<usize>) {
    let Some(head) = head else {
        return (Vec::new(), None);
    };
    match branches.iter().position(|b| *b == head) {
        Some(at) => (branches.to_vec(), Some(at)),
        None => (
            std::iter::once(head)
                .chain(branches.iter().cloned())
                .collect(),
            Some(0),
        ),
    }
}

/// The one line of a git refusal that fits in a toast: git's own first line, without the prefix
/// it puts on it and without the colon that introduces the file list underneath.
fn reason(message: &str) -> &str {
    message
        .lines()
        .next()
        .unwrap_or(message)
        .trim_start_matches("error: ")
        .trim_end_matches(':')
}

/// The branch name and its ahead/behind counts, the two labels of the branch row. `None` when git
/// told us nothing at all, which is what a failed `status` leaves behind.
fn branch_parts(b: &Branch) -> Option<(String, String)> {
    if b.head.is_none() && b.oid.is_none() {
        return None;
    }
    // A detached HEAD has no name, so it says where it is instead: this string reaches the branch
    // button and the status bar alike, and both of them otherwise read as a branch called HEAD.
    let name = b.head.clone().unwrap_or_else(|| match &b.oid {
        Some(oid) => format!("Detached at {}", short(oid)),
        None => "Detached".to_string(),
    });
    let counts = [(b.ahead, '↑'), (b.behind, '↓')]
        .iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, arrow)| format!("{arrow}{count}"))
        .collect::<Vec<_>>()
        .join(" ");
    Some((name, counts))
}

/// The branch row on one line, for anywhere with room for one string.
fn branch_text(b: &Branch) -> Option<String> {
    branch_parts(b).map(|(name, counts)| match counts.is_empty() {
        true => name,
        false => format!("{name} {counts}"),
    })
}

/// What a Sync will do, for the Sync button's tooltip.
///
/// Words beside the arrows the button already shows, because `↓2 ↑1` is a readout and a tooltip
/// is where it is spelled out. A branch with no upstream reads as Publish: that is what syncing
/// one does now, and the tooltip is the only place that can say so before it happens.
fn sync_hint(b: &Branch) -> String {
    let Some(upstream) = &b.upstream else {
        return "Publish this branch to its remote and track it".to_string();
    };
    let moving: Vec<String> = [(b.behind, "to pull"), (b.ahead, "to push")]
        .iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, what)| format!("{count} {what}"))
        .collect();
    match moving.is_empty() {
        true => format!("Sync with {upstream}"),
        false => format!("Sync with {upstream}: {}", moving.join(", ")),
    }
}

/// The changes list: the four sections in order, each behind a header, empty ones dropped.
///
/// `key` turns a repository-relative path into the key the rest of the app uses; the tests pass
/// identity, and the pane passes [`vault_key`] bound to the selected repository. `tree` groups each
/// section's files by folder, and `collapsed` holds the [`folder_key`]s whose contents are hidden.
fn rows_of(
    status: &Status,
    subs: &[Submodule],
    key: &dyn Fn(&str) -> String,
    tree: bool,
    collapsed: &HashSet<String>,
) -> Vec<Row> {
    let mut rows = Vec::new();
    let sections = [
        ("Merge Conflicts", Section::Conflicts, None),
        ("Staged Changes", Section::Staged, Some(Section::Staged)),
        ("Changes", Section::Changes, Some(Section::Changes)),
    ];
    for (title, section, all) in sections {
        let entries: Vec<&Entry> = match section {
            Section::Conflicts => status.conflicts().collect(),
            Section::Staged => status.staged().collect(),
            Section::Changes => status.changes().collect(),
        };
        if entries.is_empty() {
            continue;
        }
        rows.push(Row::Header { title, all });
        match tree {
            true => rows.extend(grouped(&entries, section, collapsed, key)),
            false => rows.extend(entries.into_iter().map(|entry| Row::Entry {
                key: key(&entry.path),
                entry: entry.clone(),
                section,
                depth: 0,
            })),
        }
    }
    if !subs.is_empty() {
        rows.push(Row::Header {
            title: "Submodules",
            all: None,
        });
        rows.extend(subs.iter().cloned().map(Row::Submodule));
    }
    rows
}

/// What identifies a folder row while it is collapsed. The section is part of it because the same
/// folder can have a row under Staged and another under Changes, and folding one is not folding
/// the other.
fn folder_key(section: Section, dir: &str) -> String {
    format!("{section:?}/{dir}")
}

/// One section's entries grouped by folder.
///
/// Folders come before files at each level and both sets are sorted, which is the order the Files
/// pane's own listing has. A chain of folders with a single child each lands on one row —
/// `src/deep` — as VS Code does it, because a column of rows with one child says nothing. Nothing
/// under a collapsed folder is emitted at all: the list is rebuilt on every toggle.
fn grouped(
    entries: &[&Entry],
    section: Section,
    collapsed: &HashSet<String>,
    key: &dyn Fn(&str) -> String,
) -> Vec<Row> {
    let mut rows = Vec::new();
    group_level(&mut rows, entries, "", 0, section, collapsed, key);
    rows
}

fn group_level(
    rows: &mut Vec<Row>,
    entries: &[&Entry],
    prefix: &str,
    depth: usize,
    section: Section,
    collapsed: &HashSet<String>,
    key: &dyn Fn(&str) -> String,
) {
    let mut dirs: Vec<(String, Vec<&Entry>)> = Vec::new();
    let mut files: Vec<&Entry> = Vec::new();
    for &entry in entries {
        match segment(&entry.path, prefix) {
            Some(head) => match dirs.iter_mut().find(|(name, _)| name == head) {
                Some((_, group)) => group.push(entry),
                None => dirs.push((head.to_string(), vec![entry])),
            },
            None => files.push(entry),
        }
    }
    dirs.sort_by(|a, b| a.0.cmp(&b.0));
    files.sort_by(|a, b| a.path.cmp(&b.path));

    for (name, group) in dirs {
        let mut label = name;
        while let Some(only) = only_segment(&group, &format!("{prefix}{label}/")) {
            label = format!("{label}/{only}");
        }
        let path = format!("{prefix}{label}");
        rows.push(Row::Folder {
            label,
            section,
            depth,
            path: path.clone(),
        });
        if !collapsed.contains(&folder_key(section, &path)) {
            let under = format!("{path}/");
            group_level(rows, &group, &under, depth + 1, section, collapsed, key);
        }
    }
    rows.extend(files.into_iter().map(|entry| Row::Entry {
        key: key(&entry.path),
        entry: entry.clone(),
        section,
        depth,
    }));
}

/// The folder `path` lies in directly under `prefix`, or `None` where it names a file of that
/// folder. git reports a wholly untracked directory as one entry ending in `/`, and that is a row
/// in its own right rather than a folder with nothing inside it.
fn segment<'a>(path: &'a str, prefix: &str) -> Option<&'a str> {
    match path.get(prefix.len()..)?.split_once('/') {
        Some((head, rest)) if !rest.is_empty() => Some(head),
        _ => None,
    }
}

/// The one folder every entry of `group` lies under, or `None` where they part ways or any of them
/// is a file at this level. What decides whether a chain of folders is compressed onto one row.
fn only_segment(group: &[&Entry], prefix: &str) -> Option<String> {
    let mut heads = group.iter().map(|entry| segment(&entry.path, prefix));
    let first = heads.next()??;
    heads
        .all(|head| head == Some(first))
        .then(|| first.to_string())
}

/// The one letter a row shows: the side of porcelain's two that the section is about.
fn status_letter(e: &Entry, section: Section) -> char {
    match section {
        // Untracked is `?` in porcelain and `U` on screen, which is the letter every git UI uses
        // for it and the one a user reads as "untracked" rather than as a question.
        Section::Changes if e.x == '?' => 'U',
        Section::Changes => e.y,
        Section::Conflicts | Section::Staged => e.x,
    }
}

fn files(n: usize) -> String {
    match n {
        1 => "1 file".to_string(),
        n => format!("{n} files"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, x: char, y: char) -> Entry {
        Entry {
            path: path.to_string(),
            orig: None,
            x,
            y,
            unmerged: false,
            submodule: false,
        }
    }

    fn repo(root: &str) -> Repo {
        Repo {
            root: PathBuf::from(root),
            git_dir: PathBuf::from(root).join(".git"),
            name: "r".to_string(),
        }
    }

    fn identity(path: &str) -> String {
        path.to_string()
    }

    fn on_main(upstream: Option<&str>, ahead: u32, behind: u32) -> Branch {
        Branch {
            oid: Some("0123456789abcdef".to_string()),
            head: Some("main".to_string()),
            upstream: upstream.map(str::to_string),
            ahead,
            behind,
        }
    }

    #[test]
    fn the_sync_tooltip_says_which_way_the_work_would_move() {
        assert_eq!(
            sync_hint(&on_main(Some("origin/main"), 0, 0)),
            "Sync with origin/main"
        );
        assert_eq!(
            sync_hint(&on_main(Some("origin/main"), 1, 2)),
            "Sync with origin/main: 2 to pull, 1 to push"
        );
        assert!(sync_hint(&on_main(None, 0, 0)).starts_with("Publish"));
    }

    #[test]
    fn ago_says_one_thing_per_scale() {
        let day = 86_400;
        assert_eq!(ago(1000, 1000), "just now");
        assert_eq!(ago(1059, 1000), "just now");
        assert_eq!(ago(1060, 1000), "1 min");
        assert_eq!(ago(1000 + 5 * 60, 1000), "5 min");
        assert_eq!(ago(1000 + 3 * 3600, 1000), "3 h");
        assert_eq!(ago(1000 + 2 * day, 1000), "2 d");
        assert_eq!(ago(1000 + 120 * day, 1000), "4 mo");
        assert_eq!(ago(1000 + 400 * day, 1000), "1 y");
        assert_eq!(ago(1000, 2000), "just now", "a clock skew is not a future");
    }

    #[test]
    fn split_name_leaves_a_top_level_file_without_a_directory() {
        assert_eq!(split_name("note.md"), ("", "note.md"));
        assert_eq!(split_name("a/b/note.md"), ("a/b", "note.md"));
        // A wholly untracked directory is one entry with a trailing slash, and the slash is the
        // only thing on the row that says so, so it stays with the name.
        assert_eq!(split_name("newdir/"), ("", "newdir/"));
        assert_eq!(split_name("a/b/c/"), ("a/b", "c/"));
    }

    #[test]
    fn vault_key_is_relative_inside_the_vault_and_absolute_outside() {
        let vault = Path::new("/v");
        assert_eq!(vault_key(vault, &repo("/v"), "a.md"), "a.md");
        assert_eq!(vault_key(vault, &repo("/v/sub"), "a.md"), "sub/a.md");
        assert_eq!(vault_key(vault, &repo("/other"), "a.md"), "/other/a.md");
    }

    #[test]
    fn ignored_key_keeps_the_slash_that_makes_it_a_directory() {
        let vault = Path::new("/v");
        assert_eq!(ignored_key(vault, &repo("/v"), "build/"), "build/");
        assert_eq!(ignored_key(vault, &repo("/v"), "a.log"), "a.log");
    }

    #[test]
    fn branch_text_names_the_branch_and_only_the_counts_that_are_there() {
        let main = Branch {
            oid: Some("abc".into()),
            head: Some("main".into()),
            ..Branch::default()
        };
        assert_eq!(branch_text(&main).as_deref(), Some("main"));
        let ahead = Branch {
            ahead: 1,
            ..main.clone()
        };
        assert_eq!(branch_text(&ahead).as_deref(), Some("main ↑1"));
        let both = Branch {
            behind: 2,
            ..ahead.clone()
        };
        assert_eq!(branch_text(&both).as_deref(), Some("main ↑1 ↓2"));
        let detached = Branch { head: None, ..main };
        assert_eq!(branch_text(&detached).as_deref(), Some("Detached at abc"));
        assert_eq!(branch_text(&Branch::default()), None, "nothing to say");
    }

    #[test]
    fn rows_of_drops_the_sections_with_nothing_in_them() {
        let status = Status {
            entries: vec![entry("a.md", 'M', '.'), entry("new.md", '?', '?')],
            ..Status::default()
        };
        let rows = rows_of(&status, &[], &identity, false, &HashSet::new());
        let titles: Vec<&str> = rows
            .iter()
            .filter_map(|row| match row {
                Row::Header { title, .. } => Some(*title),
                _ => None,
            })
            .collect();
        assert_eq!(
            titles,
            ["Staged Changes", "Changes"],
            "no conflicts section"
        );
        assert_eq!(rows.len(), 4);
        assert!(matches!(
            &rows[1],
            Row::Entry { entry, section: Section::Staged, key, .. } if entry.path == "a.md" && key == "a.md"
        ));
    }

    #[test]
    fn rows_of_leads_with_conflicts_and_ends_with_submodules() {
        let mut conflict = entry("c.md", 'U', 'U');
        conflict.unmerged = true;
        let status = Status {
            entries: vec![conflict],
            ..Status::default()
        };
        let subs = [Submodule {
            path: "vendor/x".to_string(),
            oid: "abc".to_string(),
            state: ' ',
            describe: None,
        }];
        let rows = rows_of(&status, &subs, &identity, false, &HashSet::new());
        assert!(matches!(
            rows.first(),
            Some(Row::Header {
                title: "Merge Conflicts",
                all: None
            })
        ));
        assert!(matches!(rows.last(), Some(Row::Submodule(_))));
    }

    /// The tree rows as `(depth, what the row shows)`, which is what the shape of the list is.
    fn shape(rows: &[Row]) -> Vec<(usize, String)> {
        rows.iter()
            .map(|row| match row {
                Row::Folder { label, depth, .. } => (*depth, label.clone()),
                Row::Entry { entry, depth, .. } => (*depth, entry.path.clone()),
                _ => (0, String::new()),
            })
            .collect()
    }

    fn entries(paths: &[&str]) -> Vec<Entry> {
        paths.iter().map(|p| entry(p, '.', 'M')).collect()
    }

    #[test]
    fn grouped_puts_folders_before_files_and_indents_what_is_under_them() {
        let held = entries(&["a.md", "src/x.md", "src/deep/y.md", "b.md"]);
        let refs: Vec<&Entry> = held.iter().collect();
        let rows = grouped(&refs, Section::Changes, &HashSet::new(), &identity);
        assert_eq!(
            shape(&rows),
            [
                (0, "src".to_string()),
                (1, "deep".to_string()),
                (2, "src/deep/y.md".to_string()),
                (1, "src/x.md".to_string()),
                (0, "a.md".to_string()),
                (0, "b.md".to_string()),
            ]
        );
    }

    #[test]
    fn grouped_puts_a_chain_of_single_child_folders_on_one_row() {
        let held = entries(&["src/deep/y.md", "src/deep/z.md"]);
        let refs: Vec<&Entry> = held.iter().collect();
        let rows = grouped(&refs, Section::Changes, &HashSet::new(), &identity);
        assert_eq!(
            shape(&rows),
            [
                (0, "src/deep".to_string()),
                (1, "src/deep/y.md".to_string()),
                (1, "src/deep/z.md".to_string()),
            ]
        );

        // A wholly untracked directory is one entry ending in `/`, and it is a row of its own
        // rather than a folder with nothing inside it.
        let held = entries(&["newdir/"]);
        let refs: Vec<&Entry> = held.iter().collect();
        let rows = grouped(&refs, Section::Changes, &HashSet::new(), &identity);
        assert_eq!(shape(&rows), [(0, "newdir/".to_string())]);
    }

    #[test]
    fn a_collapsed_folder_drops_everything_under_it_and_only_in_its_own_section() {
        let held = entries(&["src/x.md", "a.md"]);
        let refs: Vec<&Entry> = held.iter().collect();
        let collapsed = HashSet::from([folder_key(Section::Changes, "src")]);
        assert_eq!(
            shape(&grouped(&refs, Section::Changes, &collapsed, &identity)),
            [(0, "src".to_string()), (0, "a.md".to_string())]
        );
        assert_eq!(
            shape(&grouped(&refs, Section::Staged, &collapsed, &identity)).len(),
            3,
            "the same folder under another section is its own row"
        );
    }

    #[test]
    fn the_flat_view_is_the_list_git_gave_us() {
        let status = Status {
            entries: vec![entry("src/x.md", '.', 'M'), entry("a.md", '.', 'M')],
            ..Status::default()
        };
        let rows = rows_of(&status, &[], &identity, false, &HashSet::new());
        assert_eq!(
            shape(&rows[1..]),
            [(0, "src/x.md".to_string()), (0, "a.md".to_string())]
        );
    }

    fn commit_at(id: &str) -> Commit {
        Commit {
            id: id.to_string(),
            parents: Vec::new(),
            refs: Vec::new(),
            author: "a".to_string(),
            time: 0,
            summary: "s".to_string(),
            body: String::new(),
        }
    }

    #[test]
    fn commit_tooltip_says_where_the_commit_is_and_what_it_says() {
        let mut c = commit_at("abcdef1234567");
        c.summary = "subject".to_string();
        assert_eq!(commit_tooltip(&c), "abcdef1\n\nsubject");

        c.body = "why it happened\nand a second line".to_string();
        assert_eq!(
            commit_tooltip(&c),
            "abcdef1\n\nsubject\n\nwhy it happened\nand a second line"
        );

        c.refs = vec!["HEAD -> main".to_string(), "origin/main".to_string()];
        assert_eq!(
            commit_tooltip(&c),
            "HEAD -> main, origin/main\nabcdef1\n\nsubject\n\nwhy it happened\nand a second line"
        );
    }

    #[test]
    fn same_head_skips_the_splice_only_where_the_page_really_is_unchanged() {
        let page: Vec<Commit> = ["c", "b", "a"].iter().map(|id| commit_at(id)).collect();
        assert!(same_head(&page, &page), "the ordinary refresh");
        assert!(same_head(&[], &[]));

        let mut loaded = page.clone();
        loaded.push(commit_at("older"));
        assert!(same_head(&loaded, &page), "a Load More survives a refresh");

        let mut newer = vec![commit_at("d")];
        newer.extend(page.clone());
        assert!(!same_head(&page, &newer), "a new commit");
        assert!(!same_head(&page, &page[1..]), "a commit taken away");
        assert!(
            !same_head(&[], &page),
            "the first refresh has nothing to keep"
        );

        let mut decorated = page.clone();
        decorated[0].refs = vec!["main".to_string()];
        assert!(!same_head(&page, &decorated), "a branch moved onto it");
    }

    #[test]
    fn branch_model_always_shows_what_head_is_actually_on() {
        let locals = ["main".to_string(), "side".to_string()];
        assert_eq!(
            branch_model(Some("side".into()), &locals),
            (locals.to_vec(), Some(1))
        );
        assert_eq!(
            branch_model(Some("HEAD".into()), &locals),
            (
                ["HEAD", "main", "side"].map(str::to_string).to_vec(),
                Some(0)
            ),
            "a detached HEAD leads the list it is not in"
        );
        assert_eq!(
            branch_model(Some("main".into()), &[]),
            (vec!["main".to_string()], Some(0)),
            "a repository with no commits has a head and no branches"
        );
        assert_eq!(branch_model(None, &locals), (Vec::new(), None));
    }

    #[test]
    fn reason_is_gits_own_first_line() {
        assert_eq!(
            reason("error: Your local changes would be overwritten:\n\tnote.md\nAborting"),
            "Your local changes would be overwritten"
        );
        assert_eq!(
            reason("fatal: invalid reference"),
            "fatal: invalid reference"
        );
    }

    #[test]
    fn status_letter_reads_the_side_its_section_is_about() {
        let renamed = entry("a.md", 'R', 'M');
        assert_eq!(status_letter(&renamed, Section::Staged), 'R');
        assert_eq!(status_letter(&renamed, Section::Changes), 'M');
        assert_eq!(
            status_letter(&entry("n.md", '?', '?'), Section::Changes),
            'U'
        );
    }
}
