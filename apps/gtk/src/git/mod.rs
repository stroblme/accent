//! The Git pane: what changed, what is staged, and where the history went.
//!
//! Everything here comes from `accent_api::git`, which drives the user's own `git` binary. A
//! `git status` on a cold cache takes long enough to drop frames, so every call runs on
//! `gio::spawn_blocking` and only its answer reaches the main thread. Refreshes are debounced and
//! coalesced: a save, a watcher event and a `.git` write in the same moment cost one `git status`.
//!
//! One [`Panel`], spread over this directory: the struct, the refresh that feeds it and the
//! window-facing queries live here, and each sibling adds the `impl Panel` block for one part of
//! the pane — [`changes`] the changed-files list, [`log`] the history, [`actions`] the commands
//! that write, [`compare`] the diffs a row opens, [`fetch`] the worker's half.

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

use crate::highlight;

mod actions;
mod changes;
mod compare;
mod fetch;
mod log;

use compare::Watch;
use log::same_head;

/// How much of git one refresh asks for.
///
/// A save moved the working tree and nothing else, so it costs a `git status` per repository. A
/// write inside `.git` — a commit, a checkout, a fetch — moved the repository, so the history,
/// the branches and the submodules are read again with it. Only a walk that found or lost
/// directories is a reason to go looking for repositories, which is a `git rev-parse` per indexed
/// directory carrying a `.git`. The order is what a coalesced burst takes the maximum of.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Depth {
    /// `git status`, and nothing else.
    Status,
    /// The selected repository's history, branches, submodules and incoming commits as well.
    Everything,
    /// Which repositories the vault touches, first of all.
    Discover,
}

/// The changes list gets the top half of the pane, the log the bottom.
pub const GIT_SHARE: (i32, i32) = (1, 2);

/// One page of history: what a refresh reads, and what Load More adds.
const PAGE: usize = 200;

/// How long the pane waits after being poked before asking git again. Long enough that a burst of
/// watcher events is one query, short enough that a save shows up while the hand is still there.
const DEBOUNCE: Duration = Duration::from_millis(500);

/// How often the vault is searched for repositories again while it is being indexed. Discovery
/// goes by the indexed directories, so a nested repository shows up this long after the walk has
/// reached it rather than when the walk ends. Not every progress tick: each look is a whole
/// refresh, with a `git rev-parse` per directory carrying a `.git` on top.
const REDISCOVER: Duration = Duration::from_secs(3);

/// Arrows going out and coming back, which is what a sync is. The same name the sidebar gives the
/// pane's own tab, and for the same reason: `network-transmit-receive-symbolic` is a pair of
/// arrows in Adwaita but a network device in WhiteSur, so a Sync button drew as a port.
const SYNC_ICON: &str = "mail-send-receive-symbolic";

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
    /// Move vault files to the trash, with one toast for the lot. Vault keys only, which is what
    /// leaves an untracked file outside the vault without a Discard button.
    pub trash: Box<dyn Fn(&[String])>,
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

/// Everything the last refresh learned. One struct behind one `RefCell`, because every field of
/// it is replaced at the same moment and a reader wants a consistent set.
#[derive(Default)]
struct State {
    repos: Vec<Repo>,
    /// Index into `repos`; the pane talks about one repository at a time.
    selected: usize,
    /// One per repository, index-aligned with `repos`.
    statuses: Vec<Status>,
    /// Git did not answer the last refresh's status of the selected repository, so what the pane
    /// shows for it is the status before (see [`merge_statuses`]). The Sync tooltip says so.
    status_kept: bool,
    /// The selected repository's history, as far as it has been paged in.
    commits: Vec<Commit>,
    /// The selected repository's branches, which is what the branch chooser lists.
    branches: git::Branches,
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
    /// Over the pane while the selected repository is part way through a merge.
    banner: adw::Banner,
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
    branch_shown: RefCell<(git::Branches, Option<usize>)>,
    counts: gtk::Label,
    sync: gtk::Button,
    /// The Sync button, or the spinner standing in for it while a sync runs.
    sync_slot: gtk::Stack,
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
    /// Something asked for a refresh while one was in flight; run once more when it lands, for
    /// the deepest of whatever asked.
    again: Cell<Option<Depth>>,
    /// What the debounced refresh has been asked for so far, taken when its timer fires.
    pending_depth: Cell<Option<Depth>>,
    /// When [`Panel::rediscover`] last went looking, in `glib::monotonic_time` microseconds. It
    /// starts at the pane's creation, which the window follows with a discovery of its own, so a
    /// walk that is over within [`REDISCOVER`] asks for nothing more.
    discovered: Cell<i64>,
    /// The comparisons open right now, re-read whenever a refresh lands: a diff tab is not a
    /// snapshot. Weak, so a closed one falls out on the next pass.
    watches: RefCell<Vec<Watch>>,
    /// Set while the repository chooser's list is being filled, so the selection notify that
    /// follows is not read as the user picking a repository.
    syncing: Cell<bool>,
    /// A sync is in flight. Not the same thing as `syncing` above, which is the chooser being
    /// filled: this is the transfer [`Panel::sync_slot`] spins for.
    sync_busy: Cell<bool>,
    /// A background fetch is in flight, so a timer tick landing on a slow one is dropped rather
    /// than stacked.
    fetch_busy: Cell<bool>,
    /// Whether the last background fetch did not go through. Not a toast: a fetch nobody asked
    /// for that fails every five minutes because the laptop is on a train would be a notification
    /// every five minutes. The Sync button's tooltip carries it instead, which is where a reader
    /// goes to ask why the counts beside it have not moved.
    fetch_failed: Cell<bool>,
    /// Whether the vault's repositories have been fetched since the window opened them. The first
    /// fetch waits for the first refresh, because until then there is no repository to fetch.
    fetched_once: Cell<bool>,
    /// A timer tick was skipped because the window did not have the focus. Run as soon as it
    /// does, so coming back to a window that has been aside for an hour is not another wait.
    missed_fetch: Cell<bool>,
    /// The commit whose file list is open, if any. One at a time: a second expansion closes the
    /// first, and a refresh re-opens whichever it was.
    ///
    /// [`Panel::expanded_at`] is where its file rows are — first row and count — so closing them
    /// again is a splice rather than a scan of the whole store.
    expanded: RefCell<Option<String>>,
    expanded_at: Cell<Option<(u32, u32)>>,
    /// Whether the changes list is grouped by folder. A copy of the `git_tree` preference, which
    /// the Changes header's toggle and the Preferences switch both write.
    tree: Cell<bool>,
    /// The [`folder_key`]s whose contents are folded away. Kept across a refresh, because a save
    /// schedules one and folding a folder must survive it.
    collapsed: RefCell<HashSet<String>>,
    /// A press is down over the changes list, so its rows are held where they are until the
    /// release (see [`Panel::rebuild_changes`]).
    pressed: Cell<bool>,
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
        // branches, each row switching to that branch and carrying a trash button, then the
        // remote-only ones under a Remote heading, with Create Branch… underneath. A
        // `GtkDropDown` can only ever pick one of the rows it already has.
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
        // `go-down-symbolic` rather than `pan-down-symbolic`: WhiteSur writes the latter with
        // single-quoted attributes, which GTK 4's symbolic recolouring does not parse, and the
        // chevron drew nothing (DESIGN.md, Iconography).
        face.append(&gtk::Image::from_icon_name("go-down-symbolic"));

        let branch_list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .build();
        branch_list.add_css_class("navigation-sidebar");
        let create = gtk::Button::builder().label("Create Branch…").build();
        create.add_css_class("flat");
        let merge = gtk::Button::builder().label("Merge Branch…").build();
        merge.add_css_class("flat");
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
        branch_box.append(&merge);
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
        // While a sync runs, a spinner stands where the button was. A stack is as big as its
        // biggest child, so the spinner keeps the button's footprint and the row does not move.
        let sync_slot = gtk::Stack::new();
        sync_slot.add_named(&sync, Some("button"));
        sync_slot.add_named(
            &adw::Spinner::builder()
                .halign(gtk::Align::Center)
                .valign(gtk::Align::Center)
                .build(),
            Some("spinner"),
        );
        // The "No Repository" page's own button: `git init` in a vault with no repository writes
        // nowhere the pane is watching, so this is the one refresh a user still has to ask for.
        let check = gtk::Button::builder()
            .label("Check Again")
            .halign(gtk::Align::Center)
            .build();
        check.add_css_class("pill");

        let branch_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        branch_row.append(&branch);
        branch_row.append(&sync_slot);

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

        // A banner and not a toast (DESIGN.md, States): a merge that stopped is a state that
        // lasts until it is committed or aborted, and Abort is the one decision it can offer.
        let banner = adw::Banner::builder()
            .title("A merge is in progress")
            .button_label("Abort")
            .build();
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.append(&banner);
        root.append(&stack);

        let panel = Rc::new(Panel {
            tree: Cell::new(hooks.tree),
            collapsed: RefCell::new(HashSet::new()),
            pressed: Cell::new(false),
            hooks,
            root: root.upcast(),
            stack,
            banner,
            column,
            names,
            chooser,
            branch_label,
            branch_menu,
            branch_list,
            branch_shown: RefCell::new((git::Branches::default(), None)),
            counts,
            sync,
            sync_slot,
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
            again: Cell::new(None),
            pending_depth: Cell::new(None),
            discovered: Cell::new(glib::monotonic_time()),
            watches: RefCell::new(Vec::new()),
            syncing: Cell::new(false),
            sync_busy: Cell::new(false),
            fetch_busy: Cell::new(false),
            fetch_failed: Cell::new(false),
            fetched_once: Cell::new(false),
            missed_fetch: Cell::new(false),
            expanded: RefCell::new(None),
            expanded_at: Cell::new(None),
        });
        // Wiring comes after the `Rc` exists, so every closure can hold the panel weakly: they
        // all live in its own widget tree, and a strong capture there is a cycle.
        panel.wire_header(&check, &create, &merge);
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
    /// has two surfaces — this one is the Preferences switch, and the toggle in another window's
    /// pane, both through `App::apply_config` — so nothing here writes the config back; only a
    /// move really redraws.
    pub fn tree(&self) -> bool {
        self.tree.get()
    }

    /// A move redraws every row: the view is the one thing a row's binding reads that the row
    /// does not carry — a file at the root is the same row either way, indented only in the tree
    /// — so [`Panel::rebuild_changes`] would keep it as it was.
    pub fn set_tree(&self, on: bool) {
        if self.tree.replace(on) != on {
            self.changes.remove_all();
            self.rebuild_changes();
        }
    }

    /// Ask git again, once, in [`DEBOUNCE`], for at least `depth`. Calling this ten times in a
    /// row is one query, and the deepest of the ten is what it asks for.
    pub fn schedule_refresh(self: &Rc<Self>, depth: Depth) {
        self.pending_depth
            .set(self.pending_depth.get().max(Some(depth)));
        if let Some(id) = self.pending.borrow_mut().take() {
            id.remove();
        }
        let panel = self.clone();
        let id = glib::timeout_add_local_once(DEBOUNCE, move || {
            panel.pending.replace(None);
            let depth = panel.pending_depth.take().unwrap_or(Depth::Status);
            panel.refresh(depth);
        });
        self.pending.replace(Some(id));
    }

    /// Look for repositories again, at most once per [`REDISCOVER`]. For the indexing progress,
    /// which arrives many times a second: [`Panel::schedule_refresh`] restarts its timer on every
    /// call, so passing each tick on would put the refresh off until the walk was over.
    pub fn rediscover(self: &Rc<Self>) {
        let now = glib::monotonic_time();
        if now - self.discovered.get() < REDISCOVER.as_micros() as i64 {
            return;
        }
        self.discovered.set(now);
        self.schedule_refresh(Depth::Discover);
    }

    fn wire_header(
        self: &Rc<Self>,
        check: &gtk::Button,
        create: &gtk::Button,
        merge: &gtk::Button,
    ) {
        on_click(self, &self.sync, |panel| panel.sync(None));
        on_click(self, check, |panel| panel.refresh(Depth::Discover));
        on_click(self, create, |panel| panel.create_branch());
        on_click(self, merge, |panel| panel.merge_branch());
        let weak = Rc::downgrade(self);
        self.banner.connect_button_clicked(move |_| {
            if let Some(panel) = weak.upgrade() {
                panel.abort_merge();
            }
        });

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
            panel.refresh(Depth::Everything);
            // And ask its remote what it has, rather than leaving the first look at a second
            // repository up to five minutes stale. One round trip per pick, which is what makes
            // this the user's choice rather than a timer's: nobody cycles a chooser for fun.
            panel.autofetch();
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

    /// Ask git what `depth` says the pane needs, off the main thread, and put the answers on
    /// screen. Whatever it did not ask for, the pane keeps.
    pub(super) fn refresh(self: &Rc<Self>, depth: Depth) {
        if self.busy.get() {
            self.again.set(self.again.get().max(Some(depth)));
            return;
        }
        self.busy.set(true);
        let vault = self.hooks.vault.clone();
        let (selected, known) = {
            let state = self.state.borrow();
            (state.selected, state.repos.clone())
        };
        let panel = self.clone();
        glib::spawn_future_local(async move {
            let fetched =
                gio::spawn_blocking(move || fetch::fetch(&vault, selected, depth, known)).await;
            panel.busy.set(false);
            match fetched {
                Ok(fetched) => panel.apply(fetched),
                Err(_) => tracing::warn!("the git worker panicked"),
            }
            if let Some(depth) = panel.again.take() {
                panel.refresh(depth);
            }
        });
    }

    fn apply(self: &Rc<Self>, fetched: fetch::Fetched) {
        // The chooser moved while git was answering, so this is the old repository's answer.
        // Dropping it is safe: changing the selection scheduled a refresh of its own.
        if self.state.borrow().selected != fetched.selected {
            return;
        }
        // A refusal is not an answer: where git could not be asked, the pane keeps what it had
        // rather than emptying itself. Only the fields the last refresh really learned move.
        let repos = match fetched.repos {
            Some(repos) => repos,
            None => self.state.borrow().repos.clone(),
        };
        if self.state.borrow().repos != repos {
            self.syncing.set(true);
            let names: Vec<&str> = repos.iter().map(|r| r.name.as_str()).collect();
            self.names.splice(0, self.names.n_items(), &names);
            let selected = clamp(self.state.borrow().selected, repos.len());
            self.state.borrow_mut().selected = selected;
            self.chooser.set_selected(selected as u32);
            self.syncing.set(false);
        }
        self.chooser.set_visible(repos.len() > 1);
        self.stack.set_visible_child_name(match repos.is_empty() {
            true => "empty",
            false => "repo",
        });

        let selected = clamp(self.state.borrow().selected, repos.len());
        let status_kept = fetched.statuses.get(selected).is_some_and(Option::is_none);
        let statuses = {
            let state = self.state.borrow();
            merge_statuses(&repos, fetched.statuses, &state.repos, &state.statuses)
        };
        let heads: HashMap<PathBuf, String> = repos
            .iter()
            .zip(&statuses)
            .filter_map(|(repo, status)| Some((repo.git_dir.clone(), status.branch.oid.clone()?)))
            .collect();
        let ignored = repos
            .iter()
            .zip(&statuses)
            .flat_map(|(repo, status)| {
                status
                    .ignored
                    .iter()
                    .map(|path| ignored_key(&self.hooks.vault.root(), repo, path))
            })
            .collect();

        let head = statuses.get(selected).and_then(|s| branch_parts(&s.branch));
        self.counts
            .set_text(head.as_ref().map_or("", |(_, counts)| counts.as_str()));
        let branches = match fetched.branches {
            Some(branches) => branches,
            None => self.state.borrow().branches.clone(),
        };
        let name = statuses
            .get(selected)
            .and_then(|s| head_name(s, Some(&branches)));
        let (rows, at) = branch_model(name, &branches);
        self.set_branches(&rows, at);
        tracing::debug!(
            repos = repos.len(),
            status_kept,
            branch = %self.branch_label.text(),
            "git refresh landed"
        );
        // Most refreshes read back the history that is already on screen — a save, a watcher
        // event and a `.git` write each schedule one — and splicing then costs an expanded commit
        // its file list and flashes every row, so only a real difference is drawn. A page that
        // has not moved also leaves whatever Load More added below it alone.
        // The incoming set is part of what a row draws, and pulling a fast-forward leaves the
        // commit list from `--all` exactly as it was — same oids, same order — so without this
        // the marks would survive the pull that cleared them.
        // A refresh that did not read the history has nothing to say about it.
        let moved = match &fetched.commits {
            Some(commits) => {
                let state = self.state.borrow();
                !same_head(&state.commits, commits)
                    || fetched
                        .incoming
                        .as_ref()
                        .is_some_and(|i| state.incoming != *i)
            }
            None => false,
        };
        let page = moved.then(|| fetched.commits.clone().unwrap_or_default());

        {
            let mut state = self.state.borrow_mut();
            state.head_moved = state.heads != heads;
            state.heads = heads;
            state.ignored = ignored;
            state.repos = repos;
            state.statuses = statuses;
            state.status_kept = status_kept;
            if let (true, Some(commits)) = (moved, fetched.commits) {
                state.commits = commits;
            }
            state.branches = branches;
            if let Some(submodules) = fetched.submodules {
                state.submodules = submodules;
            }
            if let Some(incoming) = fetched.incoming {
                state.incoming = incoming;
            }
            state.selected = selected;
        }
        self.sync_state();
        self.banner.set_revealed(self.merging());
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

    /// What the Sync button says it will do, and whether it can. Both read from the last refresh,
    /// so a background fetch that failed can put its own line on the tooltip without one.
    ///
    /// A branch with no upstream is not a dead end: syncing it publishes it (`git::sync`), so the
    /// button stays live and the tooltip says which of the two it will be.
    pub(super) fn sync_state(&self) {
        let state = self.state.borrow();
        let branch = state.statuses.get(state.selected).map(|s| &s.branch);
        self.sync.set_sensitive(branch.is_some());
        // A branch git has said nothing about has no upstream anyone knows of, so it is not
        // "Publish" either.
        let mut tip = branch
            .filter(|b| branch_parts(b).is_some())
            .map(sync_hint)
            .unwrap_or_else(|| "Sync".to_string());
        // Quiet, and only here: the button still works and a sync is still what it does. What a
        // failed background fetch costs is the counts beside it, and what a status that did not
        // come back costs is everything the pane shows beside it; this is the one surface that
        // can say so without interrupting anyone.
        if state.status_kept {
            tip.push_str("\n\nThe status could not be refreshed, so the branch and the changes shown may be out of date.");
        }
        if self.fetch_failed.get() {
            tip.push_str("\n\nThe last background fetch did not go through, so the counts may be out of date.");
        }
        self.sync.set_tooltip_text(Some(&tip));
    }

    /// Put the branch popover on `rows` (see [`branch_model`]), `at` being the local row HEAD is
    /// on.
    fn set_branches(self: &Rc<Self>, rows: &git::Branches, at: Option<usize>) {
        self.branch_label.set_text(
            at.and_then(|i| rows.local.get(i))
                .map_or("", String::as_str),
        );
        if *self.branch_shown.borrow() == (rows.clone(), at) {
            return;
        }
        self.branch_shown.replace((rows.clone(), at));
        while let Some(row) = self.branch_list.first_child() {
            self.branch_list.remove(&row);
        }
        for (i, name) in rows.local.iter().enumerate() {
            let row = self.branch_row(name, Some(i) != at);
            self.branch_list.append(&row);
        }
        if rows.remote.is_empty() {
            return;
        }
        self.branch_list.append(&remote_heading());
        // No trash button here: deleting a remote branch is a push, and stays a terminal job.
        for name in &rows.remote {
            self.branch_list
                .append(&self.pick_button(name, Panel::track));
        }
    }

    /// One row of the branch popover: the name, which switches to it, and — where git would let
    /// it go — a trash button. The branch HEAD is on has none: git refuses to delete it, and a
    /// control that cannot work is dead chrome (DESIGN.md, Principle 1).
    fn branch_row(self: &Rc<Self>, name: &str, deletable: bool) -> gtk::Box {
        let switch = self.pick_button(name, Panel::checkout);
        switch.set_hexpand(true);

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

    /// A branch's name as a flat button that puts the popover away and hands the name to `pick`.
    fn pick_button(self: &Rc<Self>, name: &str, pick: fn(&Rc<Panel>, String)) -> gtk::Button {
        let button = gtk::Button::builder()
            .child(&gtk::Label::builder().label(name).xalign(0.0).build())
            .build();
        button.add_css_class("flat");
        let (weak, asked) = (Rc::downgrade(self), name.to_string());
        button.connect_clicked(move |_| {
            if let Some(panel) = weak.upgrade() {
                panel.branch_menu.popdown();
                pick(&panel, asked.clone());
            }
        });
        button
    }

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

    /// Whether the selected repository is part way through a merge.
    fn merging(&self) -> bool {
        let state = self.state.borrow();
        state
            .statuses
            .get(state.selected)
            .is_some_and(|s| s.merging)
    }

    /// Whether the selected repository still has a conflict that is not staged as resolved. git
    /// refuses a commit over one, so Commit is not offered until there is none.
    pub(super) fn unresolved(&self) -> bool {
        let state = self.state.borrow();
        state
            .statuses
            .get(state.selected)
            .is_some_and(|s| s.conflicts().next().is_some())
    }

    /// The placeholder, the Commit button and whether the box is there at all.
    ///
    /// A merge under way always has its commit to make — resolved to "ours", it may stage nothing
    /// at all — and a message of its own already, so neither an index nor a message is needed.
    fn sync_commit(&self) {
        let message = self.message_text();
        let merging = self.merging();
        self.placeholder.set_visible(message.is_empty());
        self.placeholder.set_label(match merging {
            true => "Merge message (optional)",
            false => "Commit message",
        });
        let anything = merging || self.to_commit().1;
        let unresolved = self.unresolved();
        self.commit
            .set_sensitive(anything && !unresolved && (merging || !message.trim().is_empty()));
        self.commit
            .set_tooltip_text(unresolved.then_some("Stage the resolved conflicts first"));
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
    /// `None`. What the status bar shows: while git has given the repository no status, the bare
    /// name the chooser falls back on too ([`head_name`]).
    pub fn branch_label(&self, key: Option<&str>) -> Option<String> {
        let state = self.state.borrow();
        let index = key
            .and_then(|key| index_of(&state, &self.hooks.vault.root(), key))
            .unwrap_or(state.selected);
        let status = state.statuses.get(index)?;
        let listed = (index == state.selected).then_some(&state.branches);
        branch_line(status).or_else(|| head_name(status, listed))
    }

    /// What the Sync button says it will do. `ACCENT_BENCH_GIT` and nothing else: the button is
    /// otherwise a pair of arrows and a count, and a tooltip cannot be read from a screenshot.
    pub fn sync_hint(&self) -> Option<String> {
        self.sync.tooltip_text().map(|t| t.to_string())
    }

    /// How many local and remote-tracking branches the last refresh listed. `ACCENT_BENCH_GIT`
    /// and nothing else.
    pub fn branch_counts(&self) -> (usize, usize) {
        let branches = &self.state.borrow().branches;
        (branches.local.len(), branches.remote.len())
    }

    /// Whether Commit can be pressed, and its tooltip. `ACCENT_BENCH_GIT` and nothing else.
    pub fn commit_hint(&self) -> (bool, Option<String>) {
        let tip = self.commit.tooltip_text().map(|t| t.to_string());
        (self.commit.is_sensitive(), tip)
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

/// An object name as git abbreviates it in the log.
fn short(oid: &str) -> String {
    oid.chars().take(7).collect()
}

/// The line one changed file is shown on — icon, status letter, name, directory — in the changes
/// list and under an expanded history row alike. Whatever comes after the directory, the changes
/// list's action buttons, is appended by the caller, and the binders find all five by sibling
/// order. The icon leads so that under a folder row it lands in the column a sibling folder
/// draws its own icon in (`changes::FILE_INSET`).
fn file_line() -> gtk::Box {
    let icon = gtk::Image::new();
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
        icon.upcast_ref::<gtk::Widget>(),
        letter.upcast_ref(),
        name.upcast_ref(),
        dir.upcast_ref(),
    ] {
        row.append(child);
    }
    row
}

/// Put one changed file on a [`file_line`]: its icon, its status letter, its name, and the
/// directory it sits in. Both lists that show a file — the changes list and an expanded commit —
/// bind their row through here, so the two read the same way.
///
/// `dir` is what the directory label shows rather than where the file is: a row under a folder
/// leaves it empty, the path being on screen above it already.
fn bind_file_line(row: &gtk::Box, icon: &str, letter: char, path: &str, dir: &str) {
    let Some(image) = row.first_child().and_downcast::<gtk::Image>() else {
        return;
    };
    let Some(mark) = image.next_sibling().and_downcast::<gtk::Label>() else {
        return;
    };
    image.set_icon_name(Some(icon));
    let (Some(name), Some(directory)) = (
        mark.next_sibling().and_downcast::<gtk::Label>(),
        mark.next_sibling()
            .and_then(|n| n.next_sibling())
            .and_downcast::<gtk::Label>(),
    ) else {
        return;
    };
    mark.set_text(&letter.to_string());
    name.set_text(split_name(path).1);
    directory.set_text(dir);
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

/// The heading over the branch popover's remote rows. A row that is not a branch, so nothing
/// activates it and the keyboard passes it by. Inset by the 17 px Adwaita gives a text button
/// either side of its label, so it sits over the names below it rather than out at the row's
/// edge; small and dim, because every name under it is already a bold button label.
fn remote_heading() -> gtk::ListBoxRow {
    let label = gtk::Label::builder()
        .label("Remote")
        .xalign(0.0)
        .margin_start(17)
        .margin_top(6)
        .build();
    for class in ["caption-heading", "dim-label"] {
        label.add_css_class(class);
    }
    gtk::ListBoxRow::builder()
        .child(&label)
        .activatable(false)
        .selectable(false)
        .focusable(false)
        .build()
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

/// Read a list row where the answer is smaller than the row.
///
/// [`boxed`] is the one to reach for, but a scan over the whole store — which commit is at which
/// row — would clone a `LogItem`'s several `String`s per row to answer a `bool`.
fn peek<T: 'static, R>(object: Option<glib::Object>, read: impl FnOnce(&T) -> R) -> Option<R> {
    Some(read(
        &object?.downcast::<glib::BoxedAnyObject>().ok()?.borrow(),
    ))
}

/// What a list row carries. Every store in this pane holds [`glib::BoxedAnyObject`]s, and every
/// reader of one wants a clone: the data under a recycled row is replaced without the widgets
/// being rebuilt, so nothing here may hold a borrow past the call that took it.
fn boxed<T: Clone + 'static>(object: Option<glib::Object>) -> Option<T> {
    Some(
        object?
            .downcast::<glib::BoxedAnyObject>()
            .ok()?
            .borrow::<T>()
            .clone(),
    )
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

/// A repository-relative path as the rest of the app names it: vault-relative where the file is
/// in the vault, absolute where the repository reaches outside it.
fn vault_key(vault_root: &Path, repo: &Repo, repo_rel: &str) -> String {
    let full = repo.root.join(repo_rel);
    match full.strip_prefix(vault_root) {
        Ok(rel) => rel.to_string_lossy().into_owned(),
        Err(_) => full.to_string_lossy().into_owned(),
    }
}

/// The branch chooser's rows and which of the local ones HEAD is on.
///
/// `local` is the local branches, led by whatever HEAD is on when that is not one of them — a
/// detached HEAD, or a branch with no commit yet, so that the chooser always says where the
/// repository actually is. `remote` is the remote-tracking branches no local branch shares a name
/// with, which are the ones there is anything to check out: the others are a local row already.
/// `None` is a repository git told us nothing about, which shows an empty chooser as it used to
/// show an empty label.
fn branch_model(head: Option<String>, branches: &git::Branches) -> (git::Branches, Option<usize>) {
    let Some(head) = head else {
        return (git::Branches::default(), None);
    };
    let remote = branches
        .remote
        .iter()
        .filter(|r| !branches.local.iter().any(|b| b == local_name(r)))
        .cloned()
        .collect();
    let (local, at) = match branches.local.iter().position(|b| *b == head) {
        Some(at) => (branches.local.clone(), at),
        None => (
            std::iter::once(head)
                .chain(branches.local.iter().cloned())
                .collect(),
            0,
        ),
    };
    let rows = git::Branches {
        local,
        remote,
        ..git::Branches::default()
    };
    (rows, Some(at))
}

/// Each repository's status, index-aligned with `repos`: this refresh's answer, or where git gave
/// none, the one the pane already had for that repository. A status that failed — a host too busy
/// with its first index to answer in time, say — would otherwise take the branch, its counts and
/// the changes off the pane. Matched by git dir rather than position, because a rediscovery can
/// find a repository the last refresh did not; one seen for the first time has nothing to keep.
fn merge_statuses(
    repos: &[Repo],
    fetched: Vec<Option<Status>>,
    known: &[Repo],
    kept: &[Status],
) -> Vec<Status> {
    repos
        .iter()
        .zip(fetched)
        .map(|(repo, status)| {
            status.unwrap_or_else(|| {
                known
                    .iter()
                    .zip(kept)
                    .find(|(known, _)| known.git_dir == repo.git_dir)
                    .map(|(_, status)| status.clone())
                    .unwrap_or_default()
            })
        })
        .collect()
}

/// The local branch a remote-tracking one checks out as: `origin/topic` is `topic`. Cut at the
/// first slash, which is the remote's name wherever that name has no slash of its own.
fn local_name(remote: &str) -> &str {
    remote.split_once('/').map_or(remote, |(_, name)| name)
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

/// The branch HEAD is on, as the chooser and the status bar both name it: what the status says,
/// or while git has given the repository no status, the branch its branch list marks. Only the
/// selected repository's branches are read, so `listed` is `None` for any other; a detached HEAD
/// marks none, and stays unnamed until a status says where it is.
fn head_name(status: &Status, listed: Option<&git::Branches>) -> Option<String> {
    branch_parts(&status.branch)
        .map(|(name, _)| name)
        .or_else(|| listed?.head.clone())
}

/// The branch row on one line, for anywhere with room for one string.
fn branch_text(b: &Branch) -> Option<String> {
    branch_parts(b).map(|(name, counts)| match counts.is_empty() {
        true => name,
        false => format!("{name} {counts}"),
    })
}

/// The whole branch readout the status bar shows: the branch, its ahead and behind counts, and
/// the dot in front when the repository has work that is not committed.
///
/// The dot leads and is the very character a dirty tab wears, so one symbol means "there is
/// something here that is not written down" wherever it appears. What it counts is every record
/// `git status` produced, untracked files included ([`Status::dirty`]), so it and the Git pane's
/// changes list are the same answer.
fn branch_line(status: &Status) -> Option<String> {
    let text = branch_text(&status.branch)?;
    Some(match status.dirty() {
        true => format!("• {text}"),
        false => text,
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
    fn the_branch_readout_wears_the_dot_when_anything_is_uncommitted() {
        let clean = Status {
            branch: on_main(Some("origin/main"), 1, 2),
            entries: Vec::new(),
            ignored: vec!["build/".to_string()],
            merging: false,
        };
        assert_eq!(branch_line(&clean).as_deref(), Some("main ↑1 ↓2"));

        // Untracked on its own is enough: the dot counts what the changes list shows.
        let dirty = Status {
            entries: vec![entry("new.md", '?', '?')],
            ..clean.clone()
        };
        assert_eq!(branch_line(&dirty).as_deref(), Some("• main ↑1 ↓2"));
        assert_eq!(branch_line(&Status::default()), None, "git said nothing");
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

    fn listed(local: &[&str], remote: &[&str]) -> git::Branches {
        git::Branches {
            local: local.iter().map(|b| b.to_string()).collect(),
            remote: remote.iter().map(|b| b.to_string()).collect(),
            head: None,
        }
    }

    #[test]
    fn a_status_git_did_not_give_keeps_the_last_one_for_that_repository() {
        let on = |name: &str| Status {
            branch: Branch {
                head: Some(name.to_string()),
                ..Branch::default()
            },
            ..Status::default()
        };
        let (a, b, c) = (repo("/v"), repo("/v/b"), repo("/v/c"));
        // The rediscovery found `c`, which moved `b` along: matched by git dir, not position.
        let merged = merge_statuses(
            &[a.clone(), c, b.clone()],
            vec![None, None, Some(on("fresh"))],
            &[a, b],
            &[on("kept"), on("old")],
        );
        assert_eq!(
            merged,
            [on("kept"), Status::default(), on("fresh")],
            "kept, nothing to keep for a new one, and an answer beats what was kept"
        );
    }

    #[test]
    fn head_name_falls_back_on_the_branch_list_only_while_there_is_no_status() {
        let branches = git::Branches {
            head: Some("side".to_string()),
            ..listed(&["main", "side"], &[])
        };
        let status = Status {
            branch: on_main(None, 0, 0),
            ..Status::default()
        };
        assert_eq!(head_name(&status, Some(&branches)).as_deref(), Some("main"));
        assert_eq!(
            head_name(&Status::default(), Some(&branches)).as_deref(),
            Some("side")
        );
        assert_eq!(
            head_name(&Status::default(), None),
            None,
            "another repository's branches are not read"
        );
    }

    #[test]
    fn branch_model_always_shows_what_head_is_actually_on() {
        let locals = listed(&["main", "side"], &[]);
        assert_eq!(
            branch_model(Some("side".into()), &locals),
            (locals.clone(), Some(1))
        );
        assert_eq!(
            branch_model(Some("HEAD".into()), &locals),
            (listed(&["HEAD", "main", "side"], &[]), Some(0)),
            "a detached HEAD leads the list it is not in"
        );
        assert_eq!(
            branch_model(Some("main".into()), &listed(&[], &[])),
            (listed(&["main"], &[]), Some(0)),
            "a repository with no commits has a head and no branches"
        );
        assert_eq!(
            branch_model(None, &locals),
            (git::Branches::default(), None)
        );
    }

    #[test]
    fn branch_model_lists_a_remote_branch_only_where_no_local_one_has_its_name() {
        let both = listed(
            &["main", "side"],
            &["origin/main", "origin/topic", "upstream/side"],
        );
        assert_eq!(
            branch_model(Some("main".into()), &both),
            (listed(&["main", "side"], &["origin/topic"]), Some(0)),
            "after the local ones, and without the ones checked out already"
        );
    }
}
