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

use crate::highlight;

/// The changes list gets the top half of the pane, the log the bottom.
pub const GIT_SHARE: (i32, i32) = (1, 2);

/// One page of history: what a refresh reads, and what Load More adds.
const PAGE: usize = 200;

/// The width of one graph lane, in px.
const LANE: i32 = 12;

/// How long the pane waits after being poked before asking git again. Long enough that a burst of
/// watcher events is one query, short enough that a save shows up while the hand is still there.
const DEBOUNCE: Duration = Duration::from_millis(500);

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
    /// Open a comparison tab: key, title, the built widget.
    pub open_diff: Box<dyn Fn(&str, &str, &gtk::Widget)>,
    /// Move a vault file to the trash. Vault keys only, which is what leaves an untracked file
    /// outside the vault without a Discard button.
    pub trash: Box<dyn Fn(&str)>,
    /// A refresh landed and the pane's answers changed.
    pub changed: Box<dyn Fn()>,
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
    Entry {
        entry: Entry,
        section: Section,
        /// The path as the rest of the app names it: vault-relative, or absolute outside it.
        key: String,
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
    names: gtk::StringList,
    chooser: gtk::DropDown,
    branch: gtk::Label,
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
    more: gtk::Button,
    state: RefCell<State>,
    /// The debounce timer, replaced rather than stacked.
    pending: RefCell<Option<glib::SourceId>>,
    busy: Cell<bool>,
    /// Something asked for a refresh while one was in flight; run once more when it lands.
    again: Cell<bool>,
    /// Set while the repository list is being replaced, so the chooser's own notify does not read
    /// the splice as the user picking a repository.
    syncing: Cell<bool>,
    /// The commit whose file list is open, if any. One at a time: a second expansion closes the
    /// first, and a refresh closes them all.
    expanded: RefCell<Option<String>>,
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

        let branch = gtk::Label::builder()
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(pango::EllipsizeMode::End)
            .build();
        branch.add_css_class("heading");
        let counts = gtk::Label::new(None);
        counts.add_css_class("dim-label");
        counts.add_css_class("numeric");
        // One button, both halves, and the counts inside it: nothing in the app fetches, so a
        // behind count is only as fresh as the last sync and cannot decide whether to pull.
        // Three buttons was also what stopped the sidebar shrinking — the branch row measured
        // 186 px of minimum width with them and 105 with one.
        let arrows = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        arrows.append(&counts);
        arrows.append(&gtk::Image::from_icon_name(
            "network-transmit-receive-symbolic",
        ));
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
        // it scrolls rather than growing: a long commit message must not push the lists away.
        let message = gtk::TextView::builder()
            .wrap_mode(gtk::WrapMode::WordChar)
            .accepts_tab(false)
            .left_margin(6)
            .right_margin(6)
            .top_margin(6)
            .bottom_margin(6)
            .build();
        message.add_css_class("card");
        let message_scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .height_request(72)
            .child(&message)
            .build();
        // GtkTextView has no placeholder of its own, so this is one laid over it. It cannot be
        // clicked through to, which would otherwise put the caret nowhere.
        let placeholder = gtk::Label::builder()
            .label("Commit message")
            .halign(gtk::Align::Start)
            .valign(gtk::Align::Start)
            .margin_start(6)
            .margin_top(6)
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
        // The button leads: it belongs with the branch row above it, where the pane's actions
        // are, rather than below a box that grows as it is typed into.
        let commit_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
        commit_box.append(&commit);
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
        let more = gtk::Button::builder()
            .label("Load More")
            .visible(false)
            .build();
        more.add_css_class("flat");
        let log_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        log_box.append(&scroller(&log_view));
        log_box.append(&more);

        let divider = gtk::Paned::builder()
            .orientation(gtk::Orientation::Vertical)
            .start_child(&scroller(&changes_view))
            .end_child(&log_box)
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
            hooks,
            root: stack.clone().upcast(),
            stack,
            names,
            chooser,
            branch,
            counts,
            sync,
            message,
            placeholder,
            commit,
            commit_box,
            divider,
            changes,
            log,
            more,
            state: RefCell::new(State::default()),
            pending: RefCell::new(None),
            busy: Cell::new(false),
            again: Cell::new(false),
            syncing: Cell::new(false),
            expanded: RefCell::new(None),
        });
        // Wiring comes after the `Rc` exists, so every closure can hold the panel weakly: they
        // all live in its own widget tree, and a strong capture there is a cycle.
        panel.wire_header(&check);
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

    // --- wiring -------------------------------------------------------------------------------

    fn wire_header(self: &Rc<Self>, check: &gtk::Button) {
        on_click(self, &self.sync, |panel| panel.sync(None));
        on_click(self, check, |panel| panel.refresh());
        on_click(self, &self.more, |panel| panel.load_more());

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
        factory.connect_bind(|_, item| {
            if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
                bind_change(item);
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
        factory.connect_setup(|_, item| {
            if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
                item.set_child(Some(&log_row(item)));
            }
        });
        factory.connect_bind(|_, item| {
            if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
                bind_log(item);
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

        let rows = match fetched.statuses.get(selected) {
            Some(status) => rows_of(status, &fetched.submodules, &|path| {
                vault_key(&self.hooks.vault.root(), &fetched.repos[selected], path)
            }),
            None => Vec::new(),
        };
        let items: Vec<glib::BoxedAnyObject> =
            rows.into_iter().map(glib::BoxedAnyObject::new).collect();
        self.changes.splice(0, self.changes.n_items(), &items);

        match fetched
            .statuses
            .get(selected)
            .map(|s| branch_parts(&s.branch))
        {
            Some(Some((name, counts))) => {
                self.branch.set_text(&name);
                self.counts.set_text(&counts);
            }
            _ => {
                self.branch.set_text("");
                self.counts.set_text("");
            }
        }
        // Without an upstream every click answers "There is no tracking information", so the
        // button says so up front instead.
        let upstream = fetched
            .statuses
            .get(selected)
            .and_then(|s| s.branch.upstream.clone());
        self.sync.set_sensitive(upstream.is_some());
        self.sync.set_tooltip_text(Some(&match &upstream {
            Some(name) => format!("Sync with {name}"),
            None => "This branch has no upstream to sync with".to_string(),
        }));
        self.more.set_visible(fetched.commits.len() >= PAGE);
        self.fill_log(fetched.commits.clone(), 0);

        {
            let mut state = self.state.borrow_mut();
            state.head_moved = state.heads != heads;
            state.heads = heads;
            state.ignored = ignored;
            state.repos = fetched.repos;
            state.statuses = fetched.statuses;
            state.commits = fetched.commits;
            state.submodules = fetched.submodules;
            state.selected = selected;
        }
        self.sync_commit();
        (self.hooks.changed)();
    }

    /// Put `commits` on the graph. `keep` is how many leading rows the store already holds
    /// unchanged: [`git::lanes`] is one forward pass, so a Load More can only append, and
    /// appending leaves the reader where they were instead of scrolling back to the top.
    fn fill_log(&self, commits: Vec<Commit>, keep: usize) {
        self.collapse();
        let rows = git::lanes(commits);
        let keep = keep.min(rows.len()) as u32;
        let items: Vec<glib::BoxedAnyObject> = rows[keep as usize..]
            .iter()
            .cloned()
            .map(|row| glib::BoxedAnyObject::new(LogItem::Commit(row)))
            .collect();
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
        self.more.set_sensitive(false);
        let panel = self.clone();
        glib::spawn_future_local(async move {
            let vault = panel.hooks.vault.clone();
            let page = gio::spawn_blocking(move || vault.git_log(&repo, skip, PAGE)).await;
            panel.more.set_sensitive(true);
            let page = match page {
                Ok(Ok(page)) => page,
                Ok(Err(e)) => return tracing::debug!("git log: {e}"),
                Err(_) => return tracing::warn!("the git worker panicked"),
            };
            panel.more.set_visible(page.len() >= PAGE);
            if page.is_empty() {
                return;
            }
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
    /// refresh. `hold` goes insensitive while the job runs, which is what a transfer needs.
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
        }
        let panel = self.clone();
        let vault = self.hooks.vault.clone();
        glib::spawn_future_local(async move {
            let done = gio::spawn_blocking(move || job(&vault, &repo)).await;
            if let Some(button) = &hold {
                button.set_sensitive(true);
            }
            match done {
                Ok(Ok(message)) => (panel.hooks.toast)(&message),
                Ok(Err(e)) => panel.failed(verb, &format!("{e:#}")),
                Err(_) => tracing::warn!("the git worker panicked"),
            }
            panel.schedule_refresh();
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

    // --- rows ---------------------------------------------------------------------------------

    fn activate(self: &Rc<Self>, row: &Row) {
        let Row::Entry {
            entry,
            section,
            key,
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
        let (rel, key) = (rel.to_string(), key.to_string());
        let name = split_name(&rel).1.to_string();
        let (left_title, right_title, tag) = (sides.left_title(), sides.right_title(), sides.tag());
        let panel = self.clone();
        let vault = self.hooks.vault.clone();
        let worktree_key = key.clone();
        let tab_key = key.clone();
        glib::spawn_future_local(async move {
            let read = gio::spawn_blocking(move || {
                // A side git has no file for is a new or deleted file, and an empty string is
                // exactly the right thing to diff against.
                let left = match sides.left_rev() {
                    Some(rev) => side(vault.git_show(&repo, rev, &rel)),
                    None => Blob::Text(String::new()),
                };
                let right = match &sides {
                    Sides::Staged => side(vault.git_show(&repo, "", &rel)),
                    // The worktree side is the file itself, which on a remote vault is on the
                    // other machine: reading it through the vault is what makes the diff work
                    // there as well as here.
                    Sides::Worktree => match vault.read_text(&worktree_key) {
                        Ok(accent_api::fs::Read::Text(t)) => Blob::Text(t.text),
                        Ok(_) => Blob::Binary,
                        Err(e) => {
                            tracing::debug!("reading {key}: {e}");
                            Blob::Text(String::new())
                        }
                    },
                    Sides::Commit { oid, .. } => side(vault.git_show(&repo, oid, &rel)),
                };
                (left, right)
            })
            .await;
            let Ok((left, right)) = read else {
                return tracing::warn!("the git worker panicked");
            };
            // The same test the tab opener uses, and the same answer: a diff of two binaries is
            // noise, so the pane says why instead of showing it.
            let (Blob::Text(left), Blob::Text(right)) = (left, right) else {
                return (panel.hooks.toast)(&format!("{name} is binary"));
            };
            let title = format!("{name} ({right_title})");
            let (body, _) = crate::diff::view(
                (&format!("{name} ({left_title})"), &left),
                (&title, &right),
                &accent_core::diff::lines(&left, &right),
                false,
            );
            (panel.hooks.open_diff)(&format!("diff:{tag}:{tab_key}"), &title, &body);
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
        // mid-sentence.
        self.commit_box
            .set_visible(anything || !message.is_empty() || self.message.has_focus());
    }
}

/// What the rest of the window asks the pane, all of it answered from the last refresh.
///
/// Nothing calls these yet: the tree's dimmed rows, the status bar's branch and the editor's
/// change marks are the work packages that follow, and this pane is the one place in the window
/// that has already asked git.
#[allow(dead_code)]
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
    submodules: Vec<Submodule>,
}

fn fetch(vault: &Vault, selected: usize) -> Fetched {
    let repos = vault.repos();
    let statuses = repos
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
    let (commits, submodules) = match repos.get(clamp(selected, repos.len())) {
        Some(repo) => (
            vault.git_log(repo, 0, PAGE).unwrap_or_else(|e| {
                tracing::debug!("git log: {e}");
                Vec::new()
            }),
            vault.git_submodules(repo).unwrap_or_default(),
        ),
        None => (Vec::new(), Vec::new()),
    };
    Fetched {
        repos,
        statuses,
        commits,
        submodules,
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
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    header.append(&title);
    header.append(&all);

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

fn bind_change(item: &gtk::ListItem) {
    let (Some(stack), Some(row)) = (item.child().and_downcast::<gtk::Stack>(), row_of(item)) else {
        return;
    };
    let (Some(header), Some(entry)) = (
        stack.child_by_name("header").and_downcast::<gtk::Box>(),
        stack.child_by_name("entry").and_downcast::<gtk::Box>(),
    ) else {
        return;
    };
    let (Some(title), Some(all)) = (
        header.first_child().and_downcast::<gtk::Label>(),
        header.last_child().and_downcast::<gtk::Button>(),
    ) else {
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
        }
        Row::Entry {
            entry: e,
            section,
            key,
        } => {
            stack.set_visible_child_name("entry");
            letter.set_text(&status_letter(&e, section).to_string());
            let (directory, file) = split_name(&e.path);
            name.set_text(file);
            dir.set_text(directory);
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
fn log_row(item: &gtk::ListItem) -> gtk::Stack {
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
    let line = gtk::Box::new(gtk::Orientation::Horizontal, 6);
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

    // Not homogeneous, for the reason `change_row` gives: a commit row is two lines tall and a
    // file row one, and every row taking the taller of the two would be a ladder.
    let stack = gtk::Stack::builder()
        .hhomogeneous(false)
        .vhomogeneous(false)
        .build();
    stack.add_named(&commit, Some("commit"));
    stack.add_named(&file, Some("file"));
    stack
}

fn bind_log(item: &gtk::ListItem) {
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
    let (Some(refs), Some(summary)) = (
        line.first_child().and_downcast::<gtk::Label>(),
        line.last_child().and_downcast::<gtk::Label>(),
    ) else {
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
    stack.set_tooltip_text(Some(&row.commit.id));
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
        .icon_name("network-transmit-receive-symbolic")
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
fn split_name(path: &str) -> (&str, &str) {
    match path.rsplit_once('/') {
        Some((dir, name)) => (dir, name),
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

/// The branch name and its ahead/behind counts, the two labels of the branch row. `None` when git
/// told us nothing at all, which is what a failed `status` leaves behind.
fn branch_parts(b: &Branch) -> Option<(String, String)> {
    if b.head.is_none() && b.oid.is_none() {
        return None;
    }
    // A detached HEAD has no name, and "HEAD" is what git itself calls that state.
    let name = b.head.clone().unwrap_or_else(|| "HEAD".to_string());
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

/// The changes list: the four sections in order, each behind a header, empty ones dropped.
///
/// `key` turns a repository-relative path into the key the rest of the app uses; the tests pass
/// identity, and the pane passes [`vault_key`] bound to the selected repository.
fn rows_of(status: &Status, subs: &[Submodule], key: &dyn Fn(&str) -> String) -> Vec<Row> {
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
        rows.extend(entries.into_iter().map(|entry| Row::Entry {
            key: key(&entry.path),
            entry: entry.clone(),
            section,
        }));
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
        assert_eq!(branch_text(&detached).as_deref(), Some("HEAD"));
        assert_eq!(branch_text(&Branch::default()), None, "nothing to say");
    }

    #[test]
    fn rows_of_drops_the_sections_with_nothing_in_them() {
        let status = Status {
            entries: vec![entry("a.md", 'M', '.'), entry("new.md", '?', '?')],
            ..Status::default()
        };
        let rows = rows_of(&status, &[], &identity);
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
            Row::Entry { entry, section: Section::Staged, key } if entry.path == "a.md" && key == "a.md"
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
        let rows = rows_of(&status, &subs, &identity);
        assert!(matches!(
            rows.first(),
            Some(Row::Header {
                title: "Merge Conflicts",
                all: None
            })
        ));
        assert!(matches!(rows.last(), Some(Row::Submodule(_))));
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
