//! The Git pane's changes, staging and history through `accent_api::git`.
//!
//! Git calls run on workers because a cold `git status` can block frames. Debounced,
//! coalesced refreshes turn simultaneous saves and watcher events into one status query.
//!
//! [`Panel`] and window-facing queries live here. Siblings implement refresh, branches,
//! changes, history, writing commands, comparisons and worker reads.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use accent_api::git::{self, Blob, Branch, Commit, Entry, LogRow, Repo, Status, Submodule};
use adw::prelude::*;
use gtk::{gdk, gio, glib, pango};

use crate::diff::{Compare, Merge, Side};
use crate::difftab::DiffTab;

use crate::highlight;
use crate::widgets::{icon_button, reveal_on_hover};

mod actions;
mod branch;
mod changes;
mod compare;
mod fetch;
mod log;
mod refresh;
mod source;

use branch::{branch_line, branch_model, branch_parts, head_name, local_name};
use compare::Watch;
use log::same_head;
pub(crate) use source::Source;

/// Refresh depth: status for worktree changes, repository data for `.git` changes, and discovery
/// for added or removed directories. Ordered so coalesced requests take the deepest required
/// refresh.
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

/// One page of history: what Load More adds, and what a refresh reads until it has.
const PAGE: usize = 200;

/// How many rows one turn of the main loop adds to the changes list or the history ([`Fill`]).
const FILL_CHUNK: usize = 10;

/// How long the pane waits after being poked before asking git again. Long enough that a burst of
/// watcher events is one query, short enough that a save shows up while the hand is still there.
const DEBOUNCE: Duration = Duration::from_millis(500);

/// How often the vault is searched for repositories again while it is being indexed. Discovery
/// goes by the indexed directories, so a nested repository shows up this long after the walk has
/// reached it rather than when the walk ends. Not every progress tick: each look is a whole
/// refresh, with a `git rev-parse` per directory carrying a `.git` on top.
const REDISCOVER: Duration = Duration::from_secs(3);

/// Sync arrows shared with the sidebar tab. Avoid `network-transmit-receive-symbolic`, which
/// WhiteSur renders as a device rather than arrows.
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
    pub source: Arc<Source>,
    /// Where the dialogs are presented.
    pub window: adw::ApplicationWindow,
    pub toast: Box<dyn Fn(&str)>,
    /// Open a file in a tab, by vault key.
    pub open: Box<dyn Fn(&str)>,
    /// Open a comparison of two texts that are not files: key, the file name behind it, the tab
    /// title, then (title, text) for each side, and whether a session restore is putting it back
    /// ([`Panel::restore`]). Hands the tab back so a refresh can reach it.
    pub open_diff:
        Box<dyn Fn(&str, &str, &str, (&str, &str), (&str, &str), bool) -> Option<Rc<DiffTab>>>,
    /// Compare a file's working tree with the index, inside the file's own tab: key, the index
    /// side's title and text, whether a session restore is putting it back, and what to call once
    /// the comparison exists so a refresh can reach it. That call answers whether the comparison
    /// is worth keeping; `false` takes it down again ([`Panel::show`]).
    pub compare_file: Box<dyn Fn(&str, &str, &str, bool, Box<dyn FnOnce(Weak<Compare>) -> bool>)>,
    /// Show a file git left unmerged as a merge, in its own tab: key, the conflict's base, current
    /// and incoming texts, whether it goes into the tab already showing the file rather than the
    /// pane's preview, and what to call once the merge exists so a refresh can reach it.
    pub merge_file: Box<dyn Fn(&str, [String; 3], bool, Box<dyn FnOnce(Weak<Merge>)>)>,
    /// Put the file's tab back to the file alone, from a comparison or a merge git has moved past.
    pub leave: Box<dyn Fn(&str)>,
    /// Move vault files to the trash, with one toast for the lot, or delete them on a remote
    /// vault, which has none, without asking again: Discard's question has said so. Vault keys
    /// only, which is what leaves an untracked file outside the vault without a Discard button.
    pub trash: Box<dyn Fn(&[String])>,
    /// A refresh landed and the pane's answers changed.
    pub changed: Box<dyn Fn()>,
    /// A sync started (`true`) or ended (`false`). Separate from `changed`, which is a refresh
    /// landing and costs an index write: this fires twice per sync and must stay cheap.
    pub syncing: Box<dyn Fn(bool)>,
    /// Whether the changes list starts grouped by folder — `git_tree` in the config.
    pub tree: bool,
}

/// What a close waiting for git does once the commands under way have ended, told whether they
/// all went through ([`Panel::when_done`]).
type Leave = Box<dyn FnOnce(bool)>;

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
    /// What the pane last held about every repository but the selected one, by git dir.
    seen: HashMap<PathBuf, Seen>,
}

/// The part of [`State`] only the selected repository has, set aside while another is picked:
/// a pick draws it again at once, and the refresh the pick asks for replaces it once it lands.
#[derive(Default)]
struct Seen {
    commits: Vec<Commit>,
    branches: git::Branches,
    incoming: HashSet<String>,
    submodules: Vec<Submodule>,
}

impl State {
    /// Make `at` the selected repository, setting aside what is held about the one it replaces
    /// and bringing back what was set aside for `at`: nothing, the first time. The history keeps
    /// its first page alone, so the refresh a pick asks for reads one page.
    fn pick(&mut self, at: usize) {
        let mut commits = std::mem::take(&mut self.commits);
        commits.truncate(PAGE);
        let held = Seen {
            commits,
            branches: std::mem::take(&mut self.branches),
            incoming: std::mem::take(&mut self.incoming),
            submodules: std::mem::take(&mut self.submodules),
        };
        if let Some(repo) = self.repos.get(self.selected) {
            self.seen.insert(repo.git_dir.clone(), held);
        }
        self.selected = at;
        let back = self
            .repos
            .get(at)
            .and_then(|repo| self.seen.remove(&repo.git_dir))
            .unwrap_or_default();
        self.commits = back.commits;
        self.branches = back.branches;
        self.incoming = back.incoming;
        self.submodules = back.submodules;
    }
}

pub struct Panel {
    hooks: Hooks,
    source: RefCell<Arc<Source>>,
    source_generation: Cell<u64>,
    root: gtk::Widget,
    /// Over the pane while the selected repository is part way through a merge or a rebase.
    banner: adw::Banner,
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
    /// Sync All beside the repository chooser, likewise.
    sync_all_slot: gtk::Stack,
    message: gtk::TextView,
    placeholder: gtk::Label,
    commit: gtk::Button,
    /// The message box and its button, hidden together when there is nothing to commit.
    commit_box: gtk::Box,
    divider: gtk::Paned,
    changes: gio::ListStore,
    log: gio::ListStore,
    /// The history list, which Commit hands the keyboard to ([`Panel::focus_head`]), and whose
    /// rows the bench activates without a pointer.
    log_view: gtk::ListView,
    /// Whether git has history the store does not hold, which is what puts the Load More row at
    /// the end of the log. Also the re-entrancy guard: it is cleared while a page is in flight.
    has_more: Cell<bool>,
    state: RefCell<State>,
    /// The debounce timer, replaced rather than stacked.
    pending: crate::widgets::Debounce,
    refresh_gate: RefCell<refresh::RefreshGate>,
    running_git: RefCell<actions::RunningGit>,
    /// What the debounced refresh has been asked for so far, taken when its timer fires.
    pending_depth: Cell<Option<Depth>>,
    /// When [`Panel::rediscover`] last went looking, in `glib::monotonic_time` microseconds. It
    /// starts at the pane's creation, which the window follows with a discovery of its own, so a
    /// walk that is over within [`REDISCOVER`] asks for nothing more.
    discovered: Cell<i64>,
    /// The comparisons open right now, re-read whenever a refresh lands, a commit's excepted: a
    /// diff tab is not a snapshot. And what the session keeps of them ([`Panel::kept`]). Weak, so
    /// a closed one falls out on the next pass.
    watches: RefCell<Vec<Watch>>,
    /// How many comparisons a row has asked for, so only the last one asked shows: see
    /// [`Panel::compare`].
    asked: Cell<u64>,
    /// Set while the repository chooser's list is being filled, so the selection notify that
    /// follows is not read as the user picking a repository.
    syncing: Cell<bool>,
    /// A sync is in flight. Not the same thing as `syncing` above, which is the chooser being
    /// filled: this is the transfer [`Panel::sync_slot`] or [`Panel::sync_all_slot`] spins for.
    sync_busy: Cell<bool>,
    /// The sync in flight has pulled and is pushing: raised by the worker between the two calls,
    /// so a closing window knows which half it has caught ([`Panel::busy`]), on a host as here.
    pushing: Arc<AtomicBool>,
    /// A background fetch is in flight, so a timer tick landing on a slow one is dropped rather
    /// than stacked.
    fetch_busy: Cell<bool>,
    /// Held by the background fetch for as long as it runs, and taken by a Sync before it pulls,
    /// so a Sync asked for mid-fetch waits for it — `git::FETCH_TIMEOUT` at most — rather than
    /// racing it for the remote-tracking refs, which git refuses with `cannot lock ref`.
    fetch_lock: Arc<std::sync::Mutex<()>>,
    /// Whether background fetch failed. Show it in the Sync tooltip rather than repeatedly
    /// notifying about unattended network failures.
    fetch_failed: Cell<bool>,
    /// Whether the vault's repositories have been fetched since the window opened them. The first
    /// fetch waits for the first refresh, because until then there is no repository to fetch.
    fetched_once: Cell<bool>,
    /// A timer tick was skipped because the window did not have the focus. Run as soon as it
    /// does, so coming back to a window that has been aside for an hour is not another wait.
    missed_fetch: Cell<bool>,
    /// How many commands [`Panel::command_then`] has running, the sync among them: what a
    /// closing window waits for ([`Panel::busy`]).
    jobs: Cell<usize>,
    /// A close waiting for those commands to end ([`Panel::when_done`]), told whether they all
    /// went through.
    leaving: RefCell<Option<Leave>>,
    /// The window has closed ([`Panel::stop`]): whatever is still running ends without a word.
    gone: Cell<bool>,
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
    /// The `folder_key`s whose contents are folded away. Kept across a refresh, because a save
    /// schedules one and folding a folder must survive it.
    collapsed: RefCell<HashSet<String>>,
    /// A press is down over the changes list, so its rows are held where they are until the
    /// release (see [`Panel::rebuild_changes`]).
    pressed: Rc<Cell<bool>>,
    /// A commit went in from the Commit button while it held the keyboard, which goes to the
    /// commit once the history holds it ([`Panel::focus_head`]).
    to_head: Cell<bool>,
    /// The two lists' fills under way, if any.
    changes_fill: Fill,
    log_fill: Fill,
}

impl Panel {
    fn source(&self) -> Arc<Source> {
        self.source.borrow().clone()
    }

    /// A vaultless window follows its focused shell. Empty clears the pane on focus or link loss.
    pub fn set_source(self: &Rc<Self>, source: Source) {
        if self.source.borrow().same_location(&source) {
            return;
        }
        let empty = matches!(source, Source::Empty);
        self.source.replace(Arc::new(source));
        self.source_generation.set(self.source_generation.get() + 1);
        self.refresh_gate.borrow_mut().invalidate();
        self.pending.cancel();
        self.pending_depth.take();
        self.asked.set(self.asked.get() + 1);
        self.state.replace(State {
            head_moved: true,
            ..State::default()
        });
        self.watches.borrow_mut().clear();
        self.collapsed.borrow_mut().clear();
        self.expanded.take();
        self.expanded_at.set(None);
        self.to_head.set(false);
        self.message.buffer().set_text("");
        self.syncing.set(true);
        self.names.splice(0, self.names.n_items(), &[]);
        self.syncing.set(false);
        self.changes.remove_all();
        self.log.remove_all();
        self.has_more.set(false);
        self.fetched_once.set(false);
        self.fetch_failed.set(false);
        self.draw();
        (self.hooks.changed)();
        if !empty {
            self.refresh(Depth::Discover);
        }
    }

    pub fn new(hooks: Hooks) -> Rc<Panel> {
        let names = gtk::StringList::new(&[]);
        let chooser = gtk::DropDown::builder()
            .model(&names)
            .visible(false)
            .hexpand(true)
            // A repository is named after its directory, and the button's default label asks for
            // the whole name however long it is: measured at 345 px for a 43-character one, which
            // is the sidebar's real floor whenever a vault has more than one repository. The
            // button ellipsizes; the popup list keeps the names whole, having room for them.
            .factory(&name_factory(true))
            .list_factory(&name_factory(false))
            .build();

        let BranchRow {
            row: branch_row,
            label: branch_label,
            menu: branch_menu,
            list: branch_list,
            create,
            merge,
            counts,
            sync,
            sync_slot,
            commit,
        } = build_branch_row();
        let MessageBox {
            commit_box,
            message,
            placeholder,
        } = build_message_box();

        let changes = gio::ListStore::new::<glib::BoxedAnyObject>();
        let changes_view =
            gtk::ListView::new(None::<gtk::NoSelection>, None::<gtk::SignalListItemFactory>);
        crate::widgets::set_model(&changes_view, &gtk::NoSelection::new(Some(changes.clone())));
        changes_view.add_css_class("navigation-sidebar");
        // One click opens the diff, which is the rule the tree already follows: see
        // `set_single_click_activate` in `tree.rs`.
        changes_view.set_single_click_activate(true);

        let log = gio::ListStore::new::<glib::BoxedAnyObject>();
        let log_view =
            gtk::ListView::new(None::<gtk::NoSelection>, None::<gtk::SignalListItemFactory>);
        crate::widgets::set_model(&log_view, &gtk::NoSelection::new(Some(log.clone())));
        log_view.add_css_class("navigation-sidebar");
        // The lane a commit sits in is drawn per row, so the row's own vertical margin leaves a
        // gap between its line and the next one's and the graph comes out dashed. The rule in
        // `install_chrome_css` takes the margin off; the breathing room moves onto the text.
        log_view.add_css_class("git-log");

        // Beside the chooser and only with it, being about the repositories it lists: every one of
        // them pulled and pushed in turn.
        let sync_all = gtk::Button::builder()
            .icon_name(SYNC_ICON)
            .tooltip_text("Sync All Repositories")
            .valign(gtk::Align::Center)
            .build();
        sync_all.add_css_class("flat");
        let sync_all_slot = spinner_slot(&sync_all);
        let repo_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        repo_row.append(&chooser);
        repo_row.append(&sync_all_slot);
        // The chooser hides itself where there is one repository (`Panel::apply`), and the row
        // goes with it.
        chooser
            .bind_property("visible", &repo_row, "visible")
            .sync_create()
            .build();

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
            .vexpand(true)
            .spacing(12)
            .margin_start(6)
            .margin_end(6)
            .margin_top(6)
            .margin_bottom(6)
            .build();
        column.append(&repo_row);
        column.append(&branch_row);
        column.append(&commit_box);
        column.append(&divider);

        // A banner and not a toast (DESIGN.md, States): a merge that stopped is a state that
        // lasts until it is committed or aborted, and Abort is the one decision it can offer. A
        // rebase is the same state with Continue where Commit is; the title is set per refresh.
        let banner = adw::Banner::builder().button_label("Abort").build();
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.append(&banner);
        root.append(&column);

        let source = RefCell::new(hooks.source.clone());
        let panel = Rc::new(Panel {
            tree: Cell::new(hooks.tree),
            collapsed: RefCell::new(HashSet::new()),
            pressed: Rc::default(),
            to_head: Cell::new(false),
            changes_fill: Fill::default(),
            log_fill: Fill::default(),
            hooks,
            source,
            source_generation: Cell::new(0),
            root: root.upcast(),
            banner,
            names,
            chooser,
            branch_label,
            branch_menu,
            branch_list,
            branch_shown: RefCell::new((git::Branches::default(), None)),
            counts,
            sync,
            sync_slot,
            sync_all_slot,
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
            pending: crate::widgets::Debounce::new(DEBOUNCE),
            refresh_gate: RefCell::new(refresh::RefreshGate::default()),
            running_git: RefCell::new(actions::RunningGit::default()),
            pending_depth: Cell::new(None),
            discovered: Cell::new(glib::monotonic_time()),
            watches: RefCell::new(Vec::new()),
            asked: Cell::new(0),
            syncing: Cell::new(false),
            sync_busy: Cell::new(false),
            pushing: Arc::default(),
            fetch_busy: Cell::new(false),
            fetch_lock: Arc::default(),
            fetch_failed: Cell::new(false),
            fetched_once: Cell::new(false),
            missed_fetch: Cell::new(false),
            jobs: Cell::new(0),
            leaving: RefCell::new(None),
            gone: Cell::new(false),
            expanded: RefCell::new(None),
            expanded_at: Cell::new(None),
        });
        // Wiring comes after the `Rc` exists, so every closure can hold the panel weakly: they
        // all live in its own widget tree, and a strong capture there is a cycle.
        panel.wire_header(&create, &merge, &sync_all);
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
    #[cfg(feature = "bench")]
    pub fn log_rows(&self) -> u32 {
        self.log.n_items()
    }

    #[cfg(feature = "bench")]
    pub fn activate_last_log_row(&self) {
        if let Some(last) = self.log.n_items().checked_sub(1) {
            self.activate_log_row(last);
        }
    }

    /// Activate the history's row `at`, and pick the repository `at` in the chooser, as a click
    /// does. `ACCENT_BENCH_GIT=switch` and nothing else.
    #[cfg(feature = "bench")]
    pub fn activate_log_row(&self, at: u32) {
        self.log_view.emit_by_name::<()>("activate", &[&at]);
    }

    #[cfg(feature = "bench")]
    pub fn select_repo(&self, at: u32) {
        self.chooser.set_selected(at);
    }

    /// What the branch button reads. `ACCENT_BENCH_GIT=switch` and nothing else.
    #[cfg(feature = "bench")]
    pub fn shown_branch(&self) -> String {
        self.branch_label.text().to_string()
    }

    /// The repositories the chooser lists, by name. `ACCENT_BENCH_COMPARE=pick:` and nothing else.
    #[cfg(feature = "bench")]
    pub fn repo_names(&self) -> Vec<String> {
        let state = self.state.borrow();
        state.repos.iter().map(|repo| repo.name.clone()).collect()
    }

    /// How many rows the changes list holds. `ACCENT_BENCH_GIT` prints it either side of a
    /// [`Panel::set_tree`], which is how the grouping is proven without a pointer.
    #[cfg(feature = "bench")]
    pub fn changes_rows(&self) -> u32 {
        self.changes.n_items()
    }

    /// Whether the changes list is grouped by folder, and putting it either way. The one surface
    /// is the Preferences switch, which reaches every window's pane through `App::apply_config`,
    /// so nothing here writes the config back; only a move really redraws.
    #[cfg(feature = "bench")]
    pub fn tree(&self) -> bool {
        self.tree.get()
    }

    /// A move redraws every row: the view is the one thing a row's binding reads that the row
    /// does not carry — a file at the root is the same row either way, indented only in the tree
    /// — so [`Panel::rebuild_changes`] would keep it as it was.
    pub fn set_tree(&self, on: bool) {
        if self.tree.replace(on) != on {
            self.changes_fill.cancel();
            self.changes.remove_all();
            self.rebuild_changes();
        }
    }

    fn wire_header(self: &Rc<Self>, create: &gtk::Button, merge: &gtk::Button, all: &gtk::Button) {
        on_click(self, &self.sync, |panel| panel.sync(None));
        on_click(self, all, |panel| panel.sync_all());
        on_click(self, create, |panel| panel.create_branch());
        on_click(self, merge, |panel| panel.merge_branch());
        // The popover gives its first row the keyboard, whose `:focus-within` then shows that
        // row's trash to a pointer nowhere near it. It opens on HEAD's row instead, which has no
        // trash, as a dropdown opens on its selection.
        let weak = Rc::downgrade(self);
        self.branch_menu.connect_show(move |_| {
            if let Some(panel) = weak.upgrade()
                && let Some(at) = panel.branch_shown.borrow().1
                && let Some(row) = panel.branch_list.row_at_index(at as i32)
            {
                row.grab_focus();
            }
        });
        let weak = Rc::downgrade(self);
        self.banner.connect_button_clicked(move |_| {
            if let Some(panel) = weak.upgrade() {
                match panel.rebasing() {
                    true => panel.abort_rebase(),
                    false => panel.abort_merge(),
                }
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
            // Everything on screen is the other repository's, and a row clicked before the
            // refresh lands would be asked of this one: a commit answers `fatal: bad object`, a
            // changed file compares its path here, an empty Index side against the whole file.
            // So the whole pane is drawn again at once from what it holds about this repository —
            // the status every refresh reads for every one, the rest from the last time it was
            // picked — and the refresh replaces that once it lands.
            panel.state.borrow_mut().pick(chooser.selected() as usize);
            panel.draw();
            panel.redraw_log();
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

        // Ctrl+Return commits from inside the box, which is where the hands already are. The
        // window binds the chord, Insert Line Below's, only while the keyboard is in a tab
        // (`actions::EDITING`), so it reaches this controller.
        let keys = gtk::EventControllerKey::new();
        let weak = Rc::downgrade(self);
        keys.connect_key_pressed(move |_, key, _, state| {
            let commit = matches!(key, gdk::Key::Return | gdk::Key::KP_Enter)
                && state.contains(crate::platform::PRIMARY_MODIFIER);
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

    /// Put the keyboard in the commit box, which is what `Ctrl+Shift+G` is for once the pane is
    /// up. From an idle: the chord shows the pane in the same frame, and a widget that is not on
    /// screen yet cannot take focus.
    pub fn focus_commit(&self) {
        if !self.has_repos() {
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

    /// Whether every change in the selected repository is an untracked file, which a
    /// `git commit -a` leaves out.
    fn only_untracked(&self) -> bool {
        let state = self.state.borrow();
        state
            .statuses
            .get(state.selected)
            .is_some_and(|s| !s.entries.is_empty() && s.entries.iter().all(|e| e.x == '?'))
    }

    /// Whether the selected repository is part way through a merge.
    fn merging(&self) -> bool {
        let state = self.state.borrow();
        state
            .statuses
            .get(state.selected)
            .is_some_and(|s| s.merging)
    }

    /// Whether the selected repository is part way through a rebase.
    pub(super) fn rebasing(&self) -> bool {
        let state = self.state.borrow();
        state
            .statuses
            .get(state.selected)
            .is_some_and(|s| s.rebasing)
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
    /// A rebase under way keeps each commit's own message, so the box goes and the button is
    /// Continue.
    fn sync_commit(&self) {
        let message = self.message_text();
        let (merging, rebasing) = (self.merging(), self.rebasing());
        self.placeholder.set_visible(message.is_empty());
        self.placeholder.set_label(match merging {
            true => "Merge message (optional)",
            false => "Commit message",
        });
        self.commit.set_label(match rebasing {
            true => "Continue",
            false => "Commit",
        });
        let under_way = merging || rebasing;
        let anything = under_way || self.to_commit().1;
        let unresolved = self.unresolved();
        self.commit
            .set_sensitive(anything && !unresolved && (under_way || !message.trim().is_empty()));
        self.commit
            .set_tooltip_text(unresolved.then_some("Stage the resolved conflicts first"));
        // A clean tree has nothing to say, so the box goes — but never out from under a message
        // being written: a refresh fires on every save, and one of those would take it away
        // mid-sentence. The button lives in the branch row now, so it is hidden by the same rule
        // rather than by being in the same container.
        let show = anything || !message.is_empty() || self.message.has_focus();
        self.commit_box.set_visible(show && !rebasing);
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

    /// Every changed file across every repository the vault touches, vault-relative, with what
    /// the Files tree tints it as. A nested repository, which its parent lists as one untracked
    /// `dir/`, is left to its own status, so each file takes its own repository's word.
    pub fn changed(&self) -> Vec<(String, crate::tree::Change)> {
        let state = self.state.borrow();
        let root = self.source().key_root();
        let mut changed = Vec::new();
        for (repo, status) in state.repos.iter().zip(&state.statuses) {
            for entry in status.entries.iter().filter(|e| !own_repository(&e.path)) {
                changed.push((vault_key(&root, repo, &entry.path), tint(entry)));
            }
        }
        changed
    }

    /// The repository `key` lives in and its path inside it. The longest root wins, so a file in
    /// a nested repository is that repository's and not the vault's.
    pub fn repo_of(&self, key: &str) -> Option<(Repo, String)> {
        let state = self.state.borrow();
        let root = self.source().key_root();
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
            .and_then(|key| index_of(&state, &self.source().key_root(), key))
            .unwrap_or(state.selected);
        let status = state.statuses.get(index)?;
        let listed = (index == state.selected).then_some(&state.branches);
        branch_line(status).or_else(|| head_name(status, listed))
    }

    /// What the Sync button says it will do. `ACCENT_BENCH_GIT` and nothing else: the button is
    /// otherwise a pair of arrows and a count, and a tooltip cannot be read from a screenshot.
    #[cfg(feature = "bench")]
    pub fn sync_hint(&self) -> Option<String> {
        self.sync.tooltip_text().map(|t| t.to_string())
    }

    /// How many local and remote-tracking branches the last refresh listed. `ACCENT_BENCH_GIT`
    /// and nothing else.
    #[cfg(feature = "bench")]
    pub fn branch_counts(&self) -> (usize, usize) {
        let branches = &self.state.borrow().branches;
        (branches.local.len(), branches.remote.len())
    }

    /// The branch popover. `ACCENT_BENCH_GIT` and nothing else.
    #[cfg(feature = "bench")]
    pub fn branch_popover(&self) -> gtk::Popover {
        self.branch_menu.clone()
    }

    /// Whether Commit can be pressed, and its tooltip. `ACCENT_BENCH_GIT` and nothing else.
    #[cfg(feature = "bench")]
    pub fn commit_hint(&self) -> (bool, Option<String>) {
        let tip = self.commit.tooltip_text().map(|t| t.to_string());
        (self.commit.is_sensitive(), tip)
    }

    /// The banner's title while it is up, and what the commit button reads. `ACCENT_BENCH_GIT`
    /// and nothing else.
    #[cfg(feature = "bench")]
    pub fn banner_hint(&self) -> (Option<String>, String) {
        let title = self
            .banner
            .is_revealed()
            .then(|| self.banner.title().to_string());
        let label = self
            .commit
            .label()
            .map(|l| l.to_string())
            .unwrap_or_default();
        (title, label)
    }

    /// Press the commit button, whatever it reads. `ACCENT_BENCH_GIT` and nothing else.
    #[cfg(feature = "bench")]
    pub fn press_commit(&self) {
        self.commit.emit_clicked();
    }

    /// Whether any repository's HEAD moved in the last refresh: a commit, a checkout or a pull.
    /// Anything drawn against HEAD is stale when this is true.
    pub fn head_changed(&self) -> bool {
        self.state.borrow().head_moved
    }

    pub fn is_shell_remote(&self) -> bool {
        matches!(self.source().as_ref(), Source::Remote { .. })
    }

    /// The text of `key` as HEAD has it, or `None` when HEAD has no such file, git refused, or
    /// the file belongs to no repository.
    pub fn head_text(self: &Rc<Self>, key: &str, done: impl FnOnce(Option<String>) + 'static) {
        let Some((repo, rel)) = self.repo_of(key) else {
            return done(None);
        };
        let vault = self.source();
        let (weak, generation) = (Rc::downgrade(self), self.source_generation.get());
        glib::spawn_future_local(async move {
            let blob =
                crate::work::off_thread("git", move || vault.git_show(&repo, "HEAD", &rel)).await;
            if weak
                .upgrade()
                .is_some_and(|panel| panel.source_generation.get() == generation)
            {
                done(match blob {
                    Some(Ok(Some(Blob::Text(text)))) => Some(text),
                    _ => None,
                });
            }
        });
    }
}

/// An object name as git abbreviates it in the log.
fn short(oid: &str) -> String {
    oid.chars().take(7).collect()
}

/// Shared changed-file row: icon, status, name and directory. Callers append actions; binders
/// use sibling order. Align the icon with tree-folder icons through `changes::FILE_INSET`, and
/// create [`dir_label`] lazily.
fn file_line() -> gtk::Box {
    let icon = gtk::Image::new();
    let letter = gtk::Label::builder().width_chars(1).build();
    for class in ["dimmed", "numeric", "monospace"] {
        letter.add_css_class(class);
    }
    let name = gtk::Label::builder()
        .xalign(0.0)
        .ellipsize(pango::EllipsizeMode::End)
        .build();

    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    for child in [
        icon.upcast_ref::<gtk::Widget>(),
        letter.upcast_ref(),
        name.upcast_ref(),
    ] {
        row.append(child);
    }
    row
}

/// A [`file_line`]'s directory, dimmed. Ellipsized at the start: what tells two `notes/…/index.md`
/// apart is the end of the path. Grouped by folder, the changes list shows none, so its rows are
/// a label lighter for it ([`part`]).
fn dir_label() -> gtk::Label {
    let dir = gtk::Label::builder()
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(pango::EllipsizeMode::Start)
        .build();
    dir.add_css_class("dimmed");
    dir
}

/// Bind a changed file's icon, status, name and directory to a [`file_line`] in either list.
///
/// `dir` is the visible label, empty beneath a folder row. Without it, the name takes the
/// directory label's width and spacing so truncation stays consistent.
fn bind_file_line(row: &gtk::Box, icon: &str, letter: char, path: &str, dir: &str) {
    let Some(image) = row.first_child().and_downcast::<gtk::Image>() else {
        return;
    };
    let Some(mark) = image.next_sibling().and_downcast::<gtk::Label>() else {
        return;
    };
    image.set_icon_name(Some(icon));
    let Some(name) = mark.next_sibling().and_downcast::<gtk::Label>() else {
        return;
    };
    mark.set_text(&letter.to_string());
    name.set_text(split_name(path).1);
    name.set_hexpand(dir.is_empty());
    name.set_margin_end(match dir.is_empty() {
        true => row.spacing(),
        false => 0,
    });
    let directory = name.next_sibling().and_downcast::<gtk::Label>();
    if let Some(directory) = part(directory, !dir.is_empty(), || {
        let directory = dir_label();
        directory.insert_after(row, Some(&name));
        directory
    }) {
        directory.set_text(dir);
    }
}

/// The branch row's widgets: the row the panel appends to its column, everything on it the panel
/// keeps a handle on, and the two buttons at the foot of the branch popover, which only the
/// wiring wants. Split out of [`Panel::new`] for reading; the row it builds is the same one.
struct BranchRow {
    row: gtk::Box,
    label: gtk::Label,
    menu: gtk::Popover,
    list: gtk::ListBox,
    create: gtk::Button,
    merge: gtk::Button,
    counts: gtk::Label,
    sync: gtk::Button,
    sync_slot: gtk::Stack,
    commit: gtk::Button,
}

/// The branch, the counts and the pane's two actions, on one line.
fn build_branch_row() -> BranchRow {
    // The branch is a menu button rather than a chooser: its popover is a list of the local
    // branches, each row switching to that branch and carrying a trash button, then the
    // remote-only ones under a Remote heading, with Create Branch… underneath. A
    // `GtkDropDown` can only ever pick one of the rows it already has.
    // Flat, with its face `heading`, because it stands where the branch label stood and reads as
    // the branch first and as a control second; on the button, the popover under it would take
    // the weight too. The label ellipsizes so that a long branch name
    // is not what decides how narrow the sidebar can be dragged. It claims the row's spare
    // width without taking it, so Sync and Commit stay at the trailing edge.
    let branch_label = gtk::Label::builder()
        .xalign(0.0)
        .ellipsize(pango::EllipsizeMode::End)
        .build();
    let face = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    face.add_css_class("heading");
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
    // Its rows are flat buttons, which libadwaita sets in bold: in regular weight they read as a
    // menu's items, and the Remote heading among them as a heading.
    branch_menu.add_css_class("git-branches");
    let branch = gtk::MenuButton::builder()
        .hexpand(true)
        .halign(gtk::Align::Start)
        .child(&face)
        .popover(&branch_menu)
        .build();
    branch.add_css_class("flat");
    let counts = gtk::Label::new(None);
    counts.add_css_class("dimmed");
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
    let sync_slot = spinner_slot(&sync);

    let branch_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    branch_row.append(&branch);
    branch_row.append(&sync_slot);

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

    BranchRow {
        row: branch_row,
        label: branch_label,
        menu: branch_menu,
        list: branch_list,
        create,
        merge,
        counts,
        sync,
        sync_slot,
        commit,
    }
}

/// `button` in a stack with the spinner that stands where it was while its sync runs. A stack is
/// as big as its biggest child, so the spinner keeps the button's footprint and the row does not
/// move.
fn spinner_slot(button: &gtk::Button) -> gtk::Stack {
    let slot = gtk::Stack::new();
    slot.add_named(button, Some("button"));
    slot.add_named(
        &adw::Spinner::builder()
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .build(),
        Some("spinner"),
    );
    slot
}

/// The commit message box: the container the panel hides and shows in one call, the view the
/// message is typed into, and the placeholder laid over it. Split out of [`Panel::new`] for
/// reading; the widgets are the same ones.
struct MessageBox {
    commit_box: gtk::Box,
    message: gtk::TextView,
    placeholder: gtk::Label,
}

fn build_message_box() -> MessageBox {
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
    placeholder.add_css_class("dimmed");
    let overlay = gtk::Overlay::builder().child(&message_scroller).build();
    overlay.add_overlay(&placeholder);
    // Nothing but the message box now, kept as its own container so the whole thing is hidden
    // and shown in one call.
    let commit_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
    commit_box.append(&overlay);

    MessageBox {
        commit_box,
        message,
        placeholder,
    }
}

/// A row of the repository chooser: one label, ellipsized where it has to fit the sidebar's width
/// and whole where it does not.
fn name_factory(ellipsize: bool) -> gtk::SignalListItemFactory {
    let ellipsize = match ellipsize {
        true => pango::EllipsizeMode::End,
        false => pango::EllipsizeMode::None,
    };
    crate::widgets::label_factory(ellipsize, |label, item| {
        if let Some(name) = item.item().and_downcast::<gtk::StringObject>() {
            label.set_text(&name.string());
        }
    })
}

/// Build row action buttons on first reveal and retain them. Avoid constructing unused buttons
/// for every list item; focus reveals them before Tab navigation through [`reveal_on_hover`].
fn revealed_actions(
    item: &gtk::ListItem,
    build: impl Fn(&gtk::ListItem) -> gtk::Box + 'static,
) -> gtk::Revealer {
    let revealer = crate::widgets::hover_revealer();
    revealer.connect_reveal_child_notify(glib::clone!(
        #[weak]
        item,
        move |revealer| {
            if revealer.child().is_none() && revealer.reveals_child() {
                revealer.set_child(Some(&build(&item)));
            }
        }
    ));
    revealer
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

/// Fill a store in [`FILL_CHUNK`] batches, first immediately and then on idle. Bounded widget
/// construction gives the main loop frames between large list updates.
#[derive(Default)]
struct Fill(Rc<RefCell<Option<glib::SourceId>>>);

/// A run of a store's rows to replace: where it starts and how many rows it replaces, both as the
/// store holds them before any run is spliced, and what replaces them.
type Run = (u32, u32, Vec<glib::BoxedAnyObject>);

impl Fill {
    /// Splice sorted, disjoint `runs` from bottom to top so original positions remain valid.
    /// Insert later chunks before their following rows, one chunk per turn. Pause while `hold`
    /// is true; `done` runs when all items land.
    ///
    /// Separate runs retain untouched focus and scroll anchors, unlike replacing one span across
    /// distant changes.
    fn splice(
        &self,
        store: &gio::ListStore,
        runs: Vec<Run>,
        hold: impl Fn() -> bool + 'static,
        done: impl FnOnce() + 'static,
    ) {
        self.cancel();
        let n = i64::from(store.n_items());
        // How many rows the runs below the one being spliced add.
        let mut grown = 0;
        let mut pending = Vec::new();
        for (at, removed, mut items) in runs.into_iter().rev() {
            let tail = n - i64::from(at + removed) + grown;
            grown += items.len() as i64 - i64::from(removed);
            let rest = items.split_off(items.len().min(FILL_CHUNK));
            store.splice(at, removed, &items);
            if !rest.is_empty() {
                pending.push((tail as u32, rest.into_iter()));
            }
        }
        if pending.is_empty() {
            return done();
        }
        let (store, slot, mut done) = (store.downgrade(), self.0.clone(), Some(done));
        let id = glib::idle_add_local(move || {
            let Some(store) = store.upgrade().filter(|_| !hold()) else {
                slot.take();
                return glib::ControlFlow::Break;
            };
            let (tail, rest) = &mut pending[0];
            let chunk: Vec<_> = rest.by_ref().take(FILL_CHUNK).collect();
            store.splice(store.n_items() - *tail, 0, &chunk);
            if rest.len() == 0 {
                pending.remove(0);
            }
            if !pending.is_empty() {
                return glib::ControlFlow::Continue;
            }
            slot.take();
            if let Some(done) = done.take() {
                done();
            }
            glib::ControlFlow::Break
        });
        self.0.replace(Some(id));
    }

    /// Stop a fill under way, saying whether there was one: what it had not spliced yet is not.
    fn cancel(&self) -> bool {
        self.0.take().map(glib::SourceId::remove).is_some()
    }
}

/// The runs of `rows` that differ from what `store` holds, as [`Fill::splice`] takes them. The rows
/// the two share are left out (Myers' diff, `git diff`'s), so their widgets stay where they are.
fn changed_runs<T: Clone + Eq + std::hash::Hash + 'static>(
    store: &gio::ListStore,
    rows: &[T],
) -> Vec<Run> {
    let held: Vec<T> = (0..store.n_items())
        .filter_map(|i| boxed(store.item(i)))
        .collect();
    similar::capture_diff_slices(similar::Algorithm::Myers, &held, rows)
        .iter()
        .map(similar::DiffOp::as_tag_tuple)
        .filter(|(tag, ..)| *tag != similar::DiffTag::Equal)
        .map(|(_, old, new)| {
            let items = rows[new].iter().cloned().map(glib::BoxedAnyObject::new);
            (old.start as u32, old.len() as u32, items.collect())
        })
        .collect()
}

/// A row of the changes list or the history: a stack of the layouts the items bound to it have
/// needed, so a recycled row can be any of them ([`layout`]). Not homogeneous: a header's button is
/// taller than an entry, a commit two lines to a file's one, and every row taking the tallest
/// height would turn the list into a ladder.
///
/// The stack carries the row's tooltip. Where the pointer comes straight from another row's,
/// GTK hides that one on the motion that brings it here and arms this one only on the next,
/// which a flick that stops at once never sends (ISSUES.md); asking again on the way in arms it.
fn row_stack() -> gtk::Stack {
    let stack = gtk::Stack::builder()
        .hhomogeneous(false)
        .vhomogeneous(false)
        .build();
    let enter = gtk::EventControllerMotion::new();
    enter.connect_enter(|enter, _, _| {
        if let Some(stack) = enter.widget() {
            stack.trigger_tooltip_query();
        }
    });
    stack.add_controller(enter);
    stack
}

/// Build a row layout on its first binding, then return it for filling. Avoid constructing every
/// possible layout when most rows use just one kind.
fn layout<W: IsA<gtk::Widget>>(stack: &gtk::Stack, name: &str, build: impl FnOnce() -> W) -> W {
    let child = match stack.child_by_name(name).and_downcast::<W>() {
        Some(child) => child,
        None => {
            let child = build();
            stack.add_named(&child, Some(name));
            child
        }
    };
    stack.set_visible_child(&child);
    child
}

/// Create an optional row part with `make` only when first shown, then hide and reuse it.
/// `found` retrieves an existing part. Lazy construction avoids paying for unused widgets across
/// large recycled lists.
fn part<W: IsA<gtk::Widget>>(found: Option<W>, shown: bool, make: impl FnOnce() -> W) -> Option<W> {
    if let Some(found) = &found {
        found.set_visible(shown);
    }
    shown.then(|| found.unwrap_or_else(make))
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

/// What the Files tree tints a changed file as: new to git, untracked or only added to the index,
/// or anything else, staged or not. A conflict is a change like any other: the Git pane gives it
/// no colour of its own.
fn tint(entry: &Entry) -> crate::tree::Change {
    match entry.x {
        '?' | 'A' if !entry.unmerged => crate::tree::Change::Added,
        _ => crate::tree::Change::Modified,
    }
}

/// Whether a changed path is a repository of its own inside the selected one — a nested one, or a
/// worktree: under `--untracked-files=all` git names a directory, `dir/`, only where it has a
/// `.git` of its own. Its row is listed and does nothing: staging it would record an embedded
/// gitlink, and discarding it would trash its history.
fn own_repository(path: &str) -> bool {
    path.ends_with('/')
}

/// Borrow directory and filename, with an empty directory at the root. Preserve a trailing slash
/// in nested-repository names so they remain distinct from files and do not produce an empty
/// label.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changed_runs_leave_the_rows_a_change_does_not_touch_alone() {
        let runs = |held: &[&'static str], rows: &[&'static str]| {
            let store = gio::ListStore::new::<glib::BoxedAnyObject>();
            for row in held {
                store.append(&glib::BoxedAnyObject::new(*row));
            }
            changed_runs(&store, rows)
                .into_iter()
                .map(|(at, removed, items)| {
                    let items: Vec<&str> = items.iter().map(|i| *i.borrow::<&str>()).collect();
                    (at, removed, items)
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(runs(&["a", "b"], &["a", "b"]), [], "nothing");
        // Staging `b` opens a Staged section above and takes `b` out of Changes: the rows between
        // the two keep their widgets, as do the rows after.
        assert_eq!(
            runs(&["C", "a", "b", "c"], &["S", "B", "C", "a", "c"]),
            [(0, 0, vec!["S", "B"]), (2, 1, vec![])]
        );
        assert_eq!(runs(&["a", "c"], &["a", "b", "c"]), [(1, 0, vec!["b"])]);
    }

    fn repo(root: &str) -> Repo {
        Repo {
            root: PathBuf::from(root),
            git_dir: PathBuf::from(root).join(".git"),
            name: "r".to_string(),
        }
    }

    fn commit(id: &str) -> Commit {
        Commit {
            id: id.to_string(),
            parents: Vec::new(),
            refs: Vec::new(),
            author: String::new(),
            email: String::new(),
            time: 0,
            summary: String::new(),
            body: String::new(),
        }
    }

    #[test]
    fn a_pick_brings_back_what_that_repository_last_showed_and_never_another_ones() {
        let mut state = State {
            repos: vec![repo("/v"), repo("/v/sub")],
            commits: (0..PAGE + 5).map(|i| commit(&format!("a{i}"))).collect(),
            submodules: vec![Submodule {
                path: "lib".to_string(),
                oid: "0".to_string(),
                state: ' ',
                describe: None,
            }],
            ..State::default()
        };
        state.pick(1);
        assert_eq!(state.selected, 1);
        assert!(
            state.commits.is_empty(),
            "never picked, so nothing to show yet"
        );
        assert!(state.submodules.is_empty());

        state.commits = vec![commit("b0")];
        state.pick(0);
        // Load More's pages are not kept for a repository set aside.
        assert_eq!(state.commits.len(), PAGE);
        assert_eq!(state.commits[0].id, "a0");
        assert_eq!(state.submodules.len(), 1);

        state.pick(1);
        assert_eq!(state.commits, [commit("b0")]);
        assert!(state.submodules.is_empty());
    }

    #[test]
    fn split_name_leaves_a_top_level_file_without_a_directory() {
        assert_eq!(split_name("note.md"), ("", "note.md"));
        assert_eq!(split_name("a/b/note.md"), ("a/b", "note.md"));
        // An untracked nested repository is one entry with a trailing slash, and the slash is the
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
}
