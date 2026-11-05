//! Lazy vault file tree: `gtk::ListView` over a `gtk::TreeListModel` whose children come from
//! `Vault::list_dir(prefix)`, one directory level per expansion.

use crate::doc::{FOLDER_ICON, icon_for};
use crate::widgets::{scroller, set_class, status_page};
use accent_api::Vault;
use accent_core::fs::is_sync_conflict;
use accent_core::path::basename;
use accent_core::walk::FileKind;
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

mod drag;
mod listing;

pub use drag::dropped_paths;
use drag::{Append, Carry, Import, Move, files_content, import_target, move_content, move_target};
use listing::{Asked, ShowHidden, Watches, children_model, fill, watch_unindexed};
pub use listing::{changed_span, decode, dot_named};

/// CSS class for Ctrl-click marked rows (`build::install_chrome_css`).
const MARKED: &str = "accent-marked";
/// CSS class outlining the row Reveal in Sidebar went to ([`Tree::flash`]).
const REVEALED: &str = "accent-revealed";
/// How long [`REVEALED`]'s outline holds before it fades.
const FLASH_HOLD: Duration = Duration::from_secs(1);
/// How long the outline takes to fade, the stylesheet's transition (`build::install_chrome_css`).
pub const FLASH_FADE_MS: u64 = 2000;

/// The folder the reader opened, whose children fade in as they land under it until its listing
/// is back: the rows the list binds as they land, so rows scrolled to later, a refill (Show
/// Hidden, a change on disk) and Reveal in Sidebar's folders do not fade.
#[derive(Default)]
struct Opened {
    folder: Option<String>,
    /// Reveal in Sidebar is opening folders, which is not the reader opening them.
    revealing: bool,
}

type Fresh = Rc<RefCell<Opened>>;

/// Marked paths and folder flags from Ctrl+click or Shift+click.
///
/// Folders represent their descendants, so [`fileops::topmost`](crate::fileops::topmost)
/// removes nested entries. This keeps large collapsed-folder selections compact.
type Marks = BTreeMap<String, bool>;

/// One row of the tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// `d` for a directory, `f` for a file — see `listing::encode`.
    pub kind: char,
    /// Vault-relative path.
    pub rel: String,
    /// Whether the index holds the row. Disk-only skipped-tree rows remain
    /// unsearchable and dim like gitignored files.
    pub indexed: bool,
    /// Whether a row belongs to a dependency tree such as node_modules, .venv or cargo target.
    ///
    /// Dependency files are visible but read-only because they are externally managed. Ordinary
    /// gitignored folders remain editable; being unindexed affects search only. Implies `!indexed`.
    pub dependency: bool,
}

impl Row {
    pub fn is_dir(&self) -> bool {
        self.kind == 'd'
    }
}

/// Lazy tree with cached child models for listed directories.
///
/// GtkTreeExpander probes expandability by creating and discarding child models. Caching
/// prevents each row bind or scroll from issuing another listing.
pub struct Tree {
    host: gtk::Box,
    view: gtk::ListView,
    model: gtk::TreeListModel,
    vault: Arc<Vault>,
    root: gio::ListStore,
    cache: Rc<RefCell<HashMap<String, gio::ListStore>>>,
    /// What keeps the listings the index does not walk fresh. See [`watch_unindexed`].
    watches: Watches,
    asked: Asked,
    show_hidden: ShowHidden,
    /// The root's [`Landed`](listing::Landed), which tells the empty page the host has answered.
    landed: Rc<dyn Fn()>,
    /// Shared ignored-path set so row binding uses setters and lookup,
    /// without querying the index.
    ignored: Rc<RefCell<Ignored>>,
    /// Changed paths rolled up to ancestors ([`roll_up`]), shared with the factory.
    changed: Rc<RefCell<HashMap<String, Change>>>,
    /// The rows a Cut is waiting to move, shared with the factory the same way.
    cut: Rc<RefCell<HashSet<String>>>,
    /// Sorted marked rows used by context menus and toasts. Share with the
    /// factory so recycled rows restore marks, as with [`cut`](Self::cut).
    marked: Rc<RefCell<Marks>>,
    /// Shift range anchor, temporarily marked while Shift hovers an otherwise
    /// unmarked list ([`track_shift`]).
    start: Start,
    /// The open file's row, which the selection follows (`set_active`).
    active: Rc<RefCell<Option<String>>>,
    /// Context-menu target, holding the selection while its menu is open ([`pin`](Self::pin)).
    pinned: Rc<RefCell<Option<String>>>,
    /// The row selected again once the menu lets go: the one selected before it, or the file
    /// opened while it was up.
    unpinned: RefCell<Option<String>>,
    /// The row [`flash`](Self::flash) outlines, shared with the factory as [`cut`](Self::cut) is.
    revealed: Rc<RefCell<Option<String>>>,
    /// Counts flashes, so only the last one's timer takes the outline off.
    flashes: Rc<Cell<u32>>,
    opened: Fresh,
}

impl Tree {
    /// Refresh ignored-row styles by resetting the factory, not splicing
    /// the model. Rebind only visible rows without recreating expanded
    /// `GtkTreeListRow`s and collapsing folders.
    pub fn set_ignored(&self, ignored: HashSet<String>) {
        let ignored = Ignored::new(ignored);
        if *self.ignored.borrow() == ignored {
            return;
        }
        *self.ignored.borrow_mut() = ignored;
        rebind(&self.view);
    }

    /// Refresh git tints as [`set_ignored`](Self::set_ignored) refreshes styles.
    /// Empty changes clear all tints.
    pub fn set_changed(&self, files: Vec<(String, Change)>) {
        let changed = roll_up(files);
        if *self.changed.borrow() == changed {
            return;
        }
        *self.changed.borrow_mut() = changed;
        rebind(&self.view);
    }

    /// Dim pending Cut rows and restore the rest using the ignored-row `dimmed` style.
    ///
    /// Rebind as [`set_ignored`](Self::set_ignored) does; splicing would collapse expanded folders.
    pub fn set_cut(&self, cut: HashSet<String>) {
        if *self.cut.borrow() == cut {
            return;
        }
        *self.cut.borrow_mut() = cut;
        rebind(&self.view);
    }

    /// Return sorted topmost marked rows with directory flags; marked
    /// folders represent their descendants.
    pub fn marked(&self) -> Vec<(String, bool)> {
        self.marked
            .borrow()
            .iter()
            .map(|(rel, is_dir)| (rel.clone(), *is_dir))
            .collect()
    }

    /// Whether the marked set holds a dependency tree, marked whole through its top folder: read
    /// off the folder's listing, which stays cached while the folder above it is shut.
    pub fn marks_a_dependency(&self) -> bool {
        let cache = self.cache.borrow();
        let dependency = |rel: &str| {
            let store = cache.get(accent_core::path::parent_dir(rel))?;
            (0..store.n_items())
                .filter_map(|i| store.item(i).as_ref().and_then(decode))
                .find(|row| row.rel == rel)
                .map(|row| row.dependency)
        };
        self.marked
            .borrow()
            .iter()
            .any(|(rel, is_dir)| *is_dir && dependency(rel) == Some(true))
    }

    /// Whether `rel` is left out of search, git's ignores and `[search] exclude` together: what
    /// the index marks ignored and the corpus leaves out.
    pub fn is_ignored(&self, rel: &str) -> bool {
        self.ignored.borrow().has(rel)
    }

    /// Whether `rel` is drawn marked: marked itself, or inside a marked folder.
    pub fn is_marked(&self, rel: &str) -> bool {
        is_marked(&self.marked.borrow(), rel)
    }

    /// What a Ctrl+click on the row `rel` does: mark it, or take the mark off it again.
    #[cfg(feature = "bench")]
    pub fn toggle_mark(&self, rel: &str) {
        let Some(row) = find_row(&self.model, rel).and_then(|row| markable(&row)) else {
            return;
        };
        let mut marked = self.marked.borrow_mut();
        toggle(&mut marked, &row, &self.cache);
        redraw_marks(&self.view, &marked, None);
    }

    /// Mark the inclusive range from `from` to `to`, replacing marks or
    /// adding with Ctrl (`add`). `from` is the last non-Shift click.
    #[cfg(feature = "bench")]
    pub fn mark_range(&self, from: &str, to: &str, add: bool) {
        let mut marked = self.marked.borrow_mut();
        mark_range(&mut marked, &self.model, from, to, add);
        redraw_marks(&self.view, &marked, None);
    }

    /// Clear marks and report whether any existed, allowing Escape to
    /// fall through when the tree has no selection to dismiss.
    pub fn clear_marks(&self) -> bool {
        let mut marked = self.marked.borrow_mut();
        if marked.is_empty() {
            return false;
        }
        marked.clear();
        redraw_marks(&self.view, &marked, None);
        true
    }

    /// Take down the start of a range drawn while Shift was held over the list, the key having
    /// been let go or the window left.
    pub fn hide_start(&self) {
        if self.start.borrow_mut().take().is_some() {
            redraw_marks(&self.view, &self.marked.borrow(), None);
        }
    }

    /// Toggle hidden rows and relist cached levels because excluded names
    /// are absent from stores. Ignore unchanged preferences.
    pub fn set_show_hidden(&self, on: bool) {
        if self.show_hidden.replace(on) != on {
            self.refresh();
        }
    }

    pub fn view(&self) -> &gtk::ListView {
        &self.view
    }

    /// What the sidebar puts in its Files pane, and what the context menu parents itself to.
    pub fn widget(&self) -> &gtk::Widget {
        self.host.upcast_ref()
    }

    /// The row model, for tests and the `ACCENT_BENCH_*` hooks in `main`.
    pub fn model(&self) -> &gtk::TreeListModel {
        &self.model
    }

    /// Refill root and previously expanded cached models after reconciliation.
    /// Retain model identities so expanded rows keep receiving updates.
    pub fn refresh(&self) {
        let mut dirs: Vec<String> = vec![String::new()];
        dirs.extend(self.cache.borrow().keys().cloned());
        self.invalidate(&dirs);
    }

    /// Reload `dir` and expanded descendants, returning whether `dir` is
    /// indexed. Indexed folders read the index; skipped folders and their
    /// children read disk.
    pub fn reload(&self, dir: &str) -> bool {
        let dirs: Vec<String> = self
            .cache
            .borrow()
            .keys()
            .filter(|open| crate::fileops::trashed_with(dir, open))
            .cloned()
            .collect();
        self.invalidate(&dirs);
        find_row(&self.model, dir)
            .and_then(|row| row.item())
            .and_then(|item| decode(&item))
            .is_some_and(|row| row.indexed)
    }

    /// Refill only changed cached directories (`""` for root), avoiding
    /// whole-tree refresh during incremental indexing.
    pub fn invalidate(&self, dirs: &[String]) {
        // Looked up first, so no cache borrow is live while `fill` reaches into the index. A
        // directory that was never expanded has no model to refill: it is filled on first expand.
        let stores: Vec<(&String, gio::ListStore)> = {
            let cache = self.cache.borrow();
            dirs.iter()
                .filter_map(|dir| match dir.is_empty() {
                    true => Some((dir, self.root.clone())),
                    false => cache.get(dir).map(|s| (dir, s.clone())),
                })
                .collect()
        };
        for (dir, store) in stores {
            if dir.is_empty() {
                let landed = Some(self.landed.clone());
                fill(
                    &store,
                    &self.vault,
                    &self.asked,
                    &self.show_hidden,
                    dir,
                    landed,
                );
                continue;
            }
            // A directory that is gone keeps no model: a same-named one created later must be
            // listed afresh instead of re-expanding into the files this one used to hold. Asked
            // of the vault rather than of this disk, which on a remote vault is not where the
            // files are — and asked on a worker thread, because on that vault it is one round
            // trip per invalidated directory and a reindex invalidates a handful at a time.
            let (vault, cache, watches, asked, show_hidden) = (
                self.vault.clone(),
                self.cache.clone(),
                self.watches.clone(),
                self.asked.clone(),
                self.show_hidden.clone(),
            );
            let dir = dir.clone();
            glib::spawn_future_local(async move {
                let there = crate::work::off_thread("tree", {
                    let (vault, dir) = (vault.clone(), dir.clone());
                    move || vault.stat(&dir)
                })
                .await;
                match there {
                    Some(Ok(Some(_))) => fill(&store, &vault, &asked, &show_hidden, &dir, None),
                    Some(Ok(None)) => {
                        cache.borrow_mut().remove(&dir);
                        if watches.borrow_mut().remove(&dir) {
                            let dirs = vec![dir];
                            gio::spawn_blocking(move || vault.unwatch_unindexed(&dirs));
                        }
                    }
                    // No answer is not an answer that it is gone. A reconnect's reindex arrives
                    // while the link is still being made, and taking that as gone left each
                    // expanded folder showing a model nothing refilled any more: the files made
                    // in it afterwards never appeared until it was collapsed.
                    Some(Err(e)) => tracing::warn!("asking after {dir} failed: {e}"),
                    None => {}
                }
            });
        }
    }

    /// The selected row.
    pub fn selected(&self) -> Option<Row> {
        self.view
            .model()
            .and_downcast::<gtk::SingleSelection>()?
            .selected_item()
            .and_downcast::<gtk::TreeListRow>()?
            .item()
            .as_ref()
            .and_then(decode)
    }

    /// Whether `row` is the top folder of a dependency tree, the one folder of it that may be
    /// renamed, moved or trashed: the folder it is listed in is no dependency tree's.
    pub fn dependency_top(&self, row: &Row) -> bool {
        let parent = accent_core::path::parent_dir(&row.rel);
        let inside = find_row(&self.model, parent)
            .and_then(|row| row.item())
            .as_ref()
            .and_then(decode)
            .is_some_and(|parent| parent.dependency);
        row.dependency && row.is_dir() && !inside
    }

    /// The row under a pointer position, for the context menu.
    pub fn row_at(&self, x: f64, y: f64) -> Option<Row> {
        row_at(&self.view, x, y)
    }

    /// Highlight the open file among existing rows without expansion or
    /// scrolling. Tab changes preserve tree position; Reveal in Sidebar
    /// explicitly moves it.
    pub fn set_active(&self, rel: Option<&str>) {
        *self.active.borrow_mut() = rel.map(str::to_string);
        // Selected once the menu lets go, not while it is up: a note opening behind the popover
        // must not take the lit row out from under it.
        match self.pinned.borrow().is_some() {
            true => *self.unpinned.borrow_mut() = rel.map(str::to_string),
            false => select(&self.view, rel),
        }
    }

    /// Select context-menu row `rel` while its menu is open, a right-click selecting nothing by
    /// itself; `None` selects again the row selected before, so the keys act where they did.
    pub fn pin(&self, rel: Option<&str>) {
        let was = self.pinned.replace(rel.map(str::to_string));
        match rel {
            Some(rel) => {
                if was.is_none() {
                    *self.unpinned.borrow_mut() = self.selected().map(|row| row.rel);
                }
                select(&self.view, Some(rel));
            }
            None if was.is_some() => select(&self.view, self.unpinned.take().as_deref()),
            None => {}
        }
    }

    /// Outline `rel`'s row in the accent for [`FLASH_HOLD`], then let the stylesheet fade it out
    /// over [`FLASH_FADE_MS`]; with animations off it holds for both and goes.
    pub fn flash(&self, rel: &str) {
        *self.revealed.borrow_mut() = Some(rel.to_string());
        redraw_revealed(&self.view, Some(rel));
        let flash = self.flashes.get().wrapping_add(1);
        self.flashes.set(flash);
        let hold = match self.view.settings().is_gtk_enable_animations() {
            true => FLASH_HOLD,
            false => FLASH_HOLD + Duration::from_millis(FLASH_FADE_MS),
        };
        let (view, revealed, flashes) = (
            self.view.downgrade(),
            self.revealed.clone(),
            self.flashes.clone(),
        );
        glib::timeout_add_local_once(hold, move || {
            if flashes.get() != flash {
                return;
            }
            revealed.take();
            if let Some(view) = view.upgrade() {
                redraw_revealed(&view, None);
            }
        });
    }

    /// Expand ancestors, select `rel`, scroll to it and give it the keyboard. Return false for
    /// paths not listed yet so callers can report failure.
    #[cfg(feature = "bench")]
    pub fn reveal(&self, rel: &str) -> bool {
        show_row(&self.model, &self.view, &self.opened, rel, true)
    }

    /// [`reveal`](Self::reveal) once the folders above `rel` are listed, which happens off the
    /// main thread as each opens: tried again as rows land, for at most [`LISTING_WAIT`], then
    /// `done` says whether the row was reached. `focus` gives the row the keyboard.
    pub fn reveal_when_listed(&self, rel: &str, focus: bool, done: impl FnOnce(bool) + 'static) {
        if show_row(&self.model, &self.view, &self.opened, rel, focus) {
            return done(true);
        }
        let done: Waiting = Rc::new(RefCell::new(Some(Box::new(done))));
        let (view, opened, waiting, rel) = (
            self.view.downgrade(),
            self.opened.clone(),
            done.clone(),
            rel.to_string(),
        );
        let landed = self.model.connect_items_changed(move |model, _, _, _| {
            // Not from inside the signal, since expanding a folder changes the model again.
            let (model, view, opened, waiting, rel) = (
                model.downgrade(),
                view.clone(),
                opened.clone(),
                waiting.clone(),
                rel.clone(),
            );
            glib::idle_add_local_once(move || {
                let (Some(model), Some(view)) = (model.upgrade(), view.upgrade()) else {
                    return;
                };
                if waiting.borrow().is_some() && show_row(&model, &view, &opened, &rel, focus) {
                    let done = waiting.take();
                    done.into_iter().for_each(|done| done(true));
                }
            });
        });
        let model = self.model.clone();
        glib::timeout_add_local_once(LISTING_WAIT, move || {
            model.disconnect(landed);
            let failed = done.take();
            failed.into_iter().for_each(|done| done(false));
        });
    }
}

/// How long [`Tree::reveal_when_listed`] waits for the folders above a row to be listed.
const LISTING_WAIT: Duration = Duration::from_secs(3);

/// What [`Tree::reveal_when_listed`] calls once, when the row is reached or the wait is over.
type Waiting = Rc<RefCell<Option<Box<dyn FnOnce(bool)>>>>;

/// Expand the folders above `rel` as far as they are listed, then select its row and scroll to
/// it, with `focus` giving it the keyboard. False while the row is not listed.
fn show_row(
    model: &gtk::TreeListModel,
    view: &gtk::ListView,
    opened: &Fresh,
    rel: &str,
    focus: bool,
) -> bool {
    for dir in ancestors(rel) {
        let Some(row) = find_row(model, dir) else {
            return false;
        };
        opened.borrow_mut().revealing = true;
        row.set_expanded(true);
        opened.borrow_mut().revealing = false;
    }
    let Some(row) = find_row(model, rel) else {
        return false;
    };
    let flags = match focus {
        true => gtk::ListScrollFlags::SELECT | gtk::ListScrollFlags::FOCUS,
        false => gtk::ListScrollFlags::SELECT,
    };
    view.scroll_to(row.position(), flags, None);
    true
}

/// Separate ignored directories and individual files when Git status arrives.
///
/// Directory entries end in `/` and exclude descendants. Splitting once avoids formatting
/// paths and ancestors for every visible row bind.
#[derive(Default, PartialEq, Eq)]
pub struct Ignored {
    files: HashSet<String>,
    /// Without the trailing slash, so an ancestor is a lookup and not a string to build.
    dirs: HashSet<String>,
}

impl Ignored {
    pub fn new(entries: HashSet<String>) -> Ignored {
        let (mut files, mut dirs) = (HashSet::new(), HashSet::new());
        for entry in entries {
            match entry.strip_suffix('/') {
                Some(dir) => {
                    dirs.insert(dir.to_string());
                }
                None => {
                    files.insert(entry);
                }
            }
        }
        Ignored { files, dirs }
    }

    /// Whether `rel` is left out: named itself, or inside a directory that is.
    pub fn has(&self, rel: &str) -> bool {
        if self.files.is_empty() && self.dirs.is_empty() {
            return false;
        }
        self.files.contains(rel)
            || self.dirs.contains(rel)
            || ancestors(rel).any(|dir| self.dirs.contains(dir))
    }
}

/// Git change tint ordered by strength, matching editor change bars.
/// Folders inherit the strongest descendant: modified outranks added.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Change {
    /// Untracked or newly staged file, using added-line green.
    Added,
    /// Other worktree/index changes, including conflicts, using modified-line accent.
    Modified,
}

impl Change {
    const ALL: [Change; 2] = [Change::Added, Change::Modified];

    fn class(self) -> &'static str {
        match self {
            Change::Added => "git-added",
            Change::Modified => "git-modified",
        }
    }
}

/// Roll each changed path's strongest tint onto ancestors so collapsed
/// folders indicate changes. Stop at already-equivalent ancestors;
/// work scales with changed paths, not a full-tree scan.
fn roll_up(files: Vec<(String, Change)>) -> HashMap<String, Change> {
    let mut map: HashMap<String, Change> = HashMap::new();
    for (rel, change) in files {
        let mut at = rel.as_str();
        while let Some((dir, _)) = at.rsplit_once('/') {
            if map.get(dir).is_some_and(|held| *held >= change) {
                break;
            }
            map.insert(dir.to_string(), change);
            at = dir;
        }
        let held = map.entry(rel).or_insert(change);
        *held = (*held).max(change);
    }
    map
}

/// Every directory above `rel`, outermost first: `a/b/c.md` yields `a` then `a/b`.
fn ancestors(rel: &str) -> impl Iterator<Item = &str> {
    rel.match_indices('/').map(|(at, _)| &rel[..at])
}

/// The row at a position in the list, or `None` over the blank area below the last one.
fn row_at(view: &gtk::ListView, x: f64, y: f64) -> Option<Row> {
    list_row_at(view, x, y)?.item().as_ref().and_then(decode)
}

/// The list's row at a position, which knows the rows above it.
fn list_row_at(view: &gtk::ListView, x: f64, y: f64) -> Option<gtk::TreeListRow> {
    let mut widget = view.pick(x, y, gtk::PickFlags::DEFAULT)?;
    // `pick` lands on the label or the icon; the row identity hangs off the expander above it.
    let expander = loop {
        match widget.downcast::<gtk::TreeExpander>() {
            Ok(expander) => break expander,
            Err(w) => widget = w.parent()?,
        }
    };
    expander.list_row()
}

/// What `row` stands for in marking: a dependency tree is marked only whole, through its top
/// folder, so a row inside one stands for that folder, and any other row for itself.
fn markable(row: &gtk::TreeListRow) -> Option<Row> {
    let mut row = row.clone();
    let mut found = decode(&row.item()?)?;
    while found.dependency {
        let Some(parent) = row.parent() else { break };
        match parent.item().as_ref().and_then(decode) {
            Some(above) if above.dependency => (row, found) = (parent, above),
            _ => break,
        }
    }
    Some(found)
}

/// See [`Tree::start`].
type Start = Rc<RefCell<Option<String>>>;

/// Shift anchor: last non-Shift clicked row still listed, otherwise the
/// open-file row, either standing for its dependency tree's top folder ([`markable`]).
fn start_of(
    model: &gtk::TreeListModel,
    anchor: Option<String>,
    open: Option<String>,
) -> Option<Row> {
    [anchor, open]
        .into_iter()
        .flatten()
        .find_map(|rel| markable(&find_row(model, &rel)?))
}

/// Whether `rel` is marked itself or inside a marked folder.
fn is_marked(marks: &Marks, rel: &str) -> bool {
    !marks.is_empty()
        && (marks.contains_key(rel) || ancestors(rel).any(|dir| marks.contains_key(dir)))
}

/// Add topmost marks only, omitting descendants represented by marked folders.
fn add_marks(marks: &mut Marks, rows: impl IntoIterator<Item = (String, bool)>) {
    marks.extend(rows);
    let all: Vec<(String, bool)> = std::mem::take(marks).into_iter().collect();
    marks.extend(crate::fileops::topmost(&all));
}

/// Toggle `row`'s mark on Ctrl+click.
///
/// Unmarking inside a marked folder expands its selection into sibling entries down to the
/// excluded row. Use visible tree listings, so currently hidden names also leave the set.
fn toggle(marks: &mut Marks, row: &Row, cache: &RefCell<HashMap<String, gio::ListStore>>) {
    if !is_marked(marks, &row.rel) {
        return add_marks(marks, [(row.rel.clone(), row.is_dir())]);
    }
    if marks.remove(&row.rel).is_some() {
        return;
    }
    let Some(top) = ancestors(&row.rel).find(|dir| marks.contains_key(*dir)) else {
        return;
    };
    marks.remove(top);
    let mut dir = top.to_string();
    loop {
        let mut next = None;
        for (child, is_dir) in listed(cache, &dir) {
            if ancestors(&row.rel).any(|above| above == child) {
                next = Some(child);
            } else if child != row.rel {
                marks.insert(child, is_dir);
            }
        }
        match next {
            Some(child) => dir = child,
            None => break,
        }
    }
}

/// Current listed children of `dir`. [`toggle`] never lists inside a dependency tree, so a
/// dependency row here is a tree's top folder, which marks whole.
fn listed(cache: &RefCell<HashMap<String, gio::ListStore>>, dir: &str) -> Vec<(String, bool)> {
    let Some(store) = cache.borrow().get(dir).cloned() else {
        return Vec::new();
    };
    (0..store.n_items())
        .filter_map(|i| store.item(i).as_ref().and_then(decode))
        .map(|row| (row.rel.clone(), row.is_dir()))
        .collect()
}

/// Mark inclusive tree-order range `from..to`, replacing or adding.
/// Collapsed folders in the range represent their whole contents, and a dependency tree it
/// reaches into is marked whole ([`markable`]).
fn mark_range(marks: &mut Marks, model: &gtk::TreeListModel, from: &str, to: &str, add: bool) {
    let at = |rel| find_row(model, rel).map(|row| row.position());
    let (Some(a), Some(b)) = (at(from), at(to)) else {
        return;
    };
    let rows: Vec<(String, bool)> = (a.min(b)..=a.max(b))
        .filter_map(|i| markable(&model.item(i).and_downcast::<gtk::TreeListRow>()?))
        .map(|row| (row.rel.clone(), row.is_dir()))
        .collect();
    if !add {
        marks.clear();
    }
    add_marks(marks, rows);
}

/// Update visible row marks directly without recreating widgets.
///
/// A factory reset during a click removes its target before activation. The factory still
/// reads marks on bind so recycled rows regain their state.
fn redraw_marks(view: &gtk::ListView, marked: &Marks, start: Option<&str>) {
    for expander in expanders(view) {
        let rel = expander
            .list_row()
            .and_then(|row| row.item())
            .as_ref()
            .and_then(decode)
            .map(|row| row.rel);
        set_class(
            &expander,
            MARKED,
            rel.is_some_and(|rel| is_marked(marked, &rel) || start == Some(rel.as_str())),
        );
    }
}

/// Put [`REVEALED`] on the visible row of `rel` alone; the factory puts it on a recycled one.
fn redraw_revealed(view: &gtk::ListView, rel: Option<&str>) {
    for expander in expanders(view) {
        let row = expander.list_row().and_then(|row| row.item());
        let on = row.as_ref().and_then(decode).map(|row| row.rel);
        set_class(&expander, REVEALED, rel.is_some() && on.as_deref() == rel);
    }
}

/// Visible row widgets for reading or updating bound state, including drills.
pub fn expanders(view: &gtk::ListView) -> Vec<gtk::TreeExpander> {
    let mut found = Vec::new();
    let mut todo = vec![view.clone().upcast::<gtk::Widget>()];
    while let Some(widget) = todo.pop() {
        let widget = match widget.downcast::<gtk::TreeExpander>() {
            Ok(expander) => {
                found.push(expander);
                continue;
            }
            Err(widget) => widget,
        };
        let mut child = widget.first_child();
        while let Some(c) = child {
            child = c.next_sibling();
            todo.push(c);
        }
    }
    found
}

/// Rebind visible rows after ignored or pending-Cut state changes.
///
/// Reset the factory rather than splice models, preserving expanded GtkTreeListRows while
/// updating only visible widgets.
fn rebind(view: &gtk::ListView) {
    let factory = view.factory();
    view.set_factory(None::<&gtk::ListItemFactory>);
    view.set_factory(factory.as_ref());
}

/// Select `rel` or clear selection, skipping an unchanged one.
fn select(view: &gtk::ListView, rel: Option<&str>) {
    let Some(selection) = view.model().and_downcast::<gtk::SingleSelection>() else {
        return;
    };
    let Some(model) = selection.model().and_downcast::<gtk::TreeListModel>() else {
        return;
    };
    let at = rel
        .and_then(|rel| find_row(&model, rel))
        .map_or(gtk::INVALID_LIST_POSITION, |row| row.position());
    if selection.selected() != at {
        selection.set_selected(at);
    }
}

/// Find the visible model row for `rel`, or `None` while its parent is collapsed.
///
/// A linear scan covers expanded rows rather than the whole vault; revealing a nested path
/// repeats it per segment.
pub fn find_row(model: &gtk::TreeListModel, rel: &str) -> Option<gtk::TreeListRow> {
    (0..model.n_items()).find_map(|i| {
        let row = model.item(i).and_downcast::<gtk::TreeListRow>()?;
        let item = row.item().as_ref().and_then(decode)?;
        (item.rel == rel).then_some(row)
    })
}

/// Build the always-visible vault label and root drop zone above the tree.
///
/// Scrolled trees may have no blank area for root drops. A label has nothing to expand;
/// 6 px margin and a 34 px row align it with Search and Git controls.
fn root_row(label: &str) -> gtk::Box {
    let row = gtk::Box::builder()
        .spacing(6)
        .margin_start(12)
        .margin_end(12)
        .margin_top(6)
        .margin_bottom(6)
        .height_request(34)
        .tooltip_text(label)
        .build();
    row.append(
        &gtk::Image::builder()
            .icon_name(FOLDER_ICON)
            .valign(gtk::Align::Center)
            .build(),
    );
    row.append(
        &gtk::Label::builder()
            .label(basename(label))
            .xalign(0.0)
            .valign(gtk::Align::Center)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .css_classes(["heading"])
            .build(),
    );
    row
}

/// Build the tree with `show_hidden` and activation, drag, move, import and PDF-append hooks.
///
/// `on_activate` receives the file key and double-click flag. `on_drag` tracks outgoing drags;
/// `on_move` receives paths and destination; `on_import` receives local files, folder and
/// move flag; `on_append` receives the dropped PDF and target PDF key. `foreign` says whether the
/// drag in flight came out of another window's tree, whose drop `on_carry` receives: its paths,
/// the folder and the move flag.
#[allow(clippy::too_many_arguments)]
pub fn build(
    vault: Arc<Vault>,
    root: &gio::ListStore,
    show_hidden: bool,
    on_activate: impl Fn(char, &str, bool) + 'static,
    on_drag: impl Fn(bool) + 'static,
    on_move: impl Fn(Vec<(String, String)>) + 'static,
    on_import: impl Fn(Vec<PathBuf>, String, bool) + 'static,
    on_append: impl Fn(String, String) + 'static,
    foreign: impl Fn() -> bool + 'static,
    on_carry: impl Fn(Vec<String>, String, bool) + 'static,
) -> Tree {
    let cache: Rc<RefCell<HashMap<String, gio::ListStore>>> = Rc::new(RefCell::new(HashMap::new()));
    let asked = Asked::default();
    let show_hidden = ShowHidden::new(Cell::new(show_hidden));
    let ignored: Rc<RefCell<Ignored>> = Rc::new(RefCell::new(Ignored::default()));
    let changed: Rc<RefCell<HashMap<String, Change>>> = Rc::default();
    let cut: Rc<RefCell<HashSet<String>>> = Rc::new(RefCell::new(HashSet::new()));
    let marked: Rc<RefCell<Marks>> = Rc::new(RefCell::new(Marks::new()));
    let start = Start::default();
    let watches = Watches::default();
    let model = tree_model(root, &vault, &cache, &asked, &show_hidden, &watches);

    // Abbreviated once rather than per row: neither the vault root nor `$HOME` moves while the
    // window is open, and the label is only ever a prefix of a tooltip.
    let root_label = crate::fileops::display_path(&vault.root(), "");
    // Shared, because `setup` runs once per recycled row widget and both ends of every drag
    // report through the same closure.
    let dragging: Rc<dyn Fn(bool)> = Rc::new(on_drag);
    let moves: Move = Rc::new(on_move);
    let imports: Import = Rc::new(on_import);
    let appends: Append = Rc::new(on_append);
    let carries = Carry {
        foreign: Rc::new(foreign),
        drop: Rc::new(on_carry),
    };
    let vault_row = root_row(&root_label);
    vault_row.add_controller(move_target(&moves, &appends, &carries, |_, _, _| {
        Some(String::new())
    }));
    vault_row.add_controller(import_target(&imports, &appends, |_, _, _| {
        Some(String::new())
    }));
    let bind_ignored = ignored.clone();
    let bind_changed = changed.clone();
    let bind_cut = cut.clone();
    let (bind_marked, bind_start) = (marked.clone(), start.clone());
    let revealed: Rc<RefCell<Option<String>>> = Rc::default();
    let bind_revealed = revealed.clone();
    let drag_marked = marked.clone();
    let row_moves = moves.clone();
    let row_imports = imports.clone();
    let row_appends = appends.clone();
    let row_carries = carries.clone();
    let local: Option<Rc<Path>> = (!vault.is_remote()).then(|| vault.root().into());
    let opened: Fresh = Rc::default();
    let row_opened = opened.clone();
    // A folder's listing is kept for as long as the window is open, which is what makes binding
    // its row free, and it moves only when the vault says the folder changed. So each opening of
    // a folder lists it again: a dependency tree is watched by nothing, and a folder whose news
    // was missed — changed before its watch was in place, or with no watch to be had — would open
    // onto what it held the first time, however often it was opened.
    let relist: Rc<dyn Fn(&str)> = Rc::new({
        let (vault, cache, asked, show_hidden, opened) = (
            vault.clone(),
            cache.clone(),
            asked.clone(),
            show_hidden.clone(),
            opened.clone(),
        );
        move |rel| {
            let store = cache.borrow().get(rel).cloned();
            if let Some(store) = store {
                // What lands under the folder fades in until this listing is back (`Opened`).
                let (opened, dir) = (opened.clone(), rel.to_string());
                let landed: Rc<dyn Fn()> = Rc::new(move || {
                    let mut opened = opened.borrow_mut();
                    if opened.folder.as_deref() == Some(dir.as_str()) {
                        opened.folder = None;
                    }
                });
                fill(&store, &vault, &asked, &show_hidden, rel, Some(landed));
            }
        }
    });
    let factory = crate::widgets::factory(
        move |_| {
            setup_row(
                &relist,
                &row_opened,
                &root_label,
                &drag_marked,
                &dragging,
                local.as_ref(),
                &row_moves,
                &row_imports,
                &row_appends,
                &row_carries,
            )
        },
        move |expander: &gtk::TreeExpander, item| {
            bind_row(
                expander,
                item,
                &bind_ignored,
                &bind_changed,
                &bind_cut,
                &bind_marked,
                &bind_start,
                &bind_revealed,
            )
        },
    );

    let selection = gtk::SingleSelection::new(Some(model.clone()));
    selection.set_autoselect(false);
    selection.set_can_unselect(true);
    let view = gtk::ListView::new(None::<gtk::SingleSelection>, Some(factory));
    crate::widgets::set_model(&view, &selection);
    view.add_css_class("navigation-sidebar");
    view.add_css_class("file-tree");
    // Where a Shift+click's range starts: the last row clicked without Shift.
    let anchor = Rc::new(RefCell::new(None::<String>));
    let active = Rc::new(RefCell::new(None::<String>));
    let on_activate = Rc::new(on_activate);
    view.add_controller(marking(
        &model,
        &cache,
        &marked,
        &start,
        &anchor,
        &active,
        &on_activate,
    ));
    click_opens(&view, &marked, on_activate);
    let pinned = Rc::new(RefCell::new(None::<String>));
    track_shift(&view, &model, &marked, &start, &anchor, &active);
    keep_selected(&view, &model, &active, &pinned);
    // The blank area below the last row is the vault root, the same place a right-click there
    // creates in. A drop that landed on a row is that row's own business — its target has already
    // accepted or refused it — so this one has to answer for the blank area alone, or a refusal
    // bubbling up out of a row would turn into a move to the root.
    let blank = |target: &gtk::DropTarget, x, y| {
        let view = target.widget()?.downcast::<gtk::ListView>().ok()?;
        row_at(&view, x, y).is_none().then(String::new)
    };
    view.add_controller(move_target(&moves, &appends, &carries, blank));
    view.add_controller(import_target(&imports, &appends, blank));

    // A listing that lands later than the folder opened, a remote one or one never listed.
    model.connect_items_changed({
        let (opened, view) = (opened.clone(), view.downgrade());
        move |model, at, _, added| {
            let folder = opened.borrow().folder.clone();
            let (Some(folder), Some(view)) = (folder, view.upgrade()) else {
                return;
            };
            let parent = model
                .item(at)
                .and_downcast::<gtk::TreeListRow>()
                .and_then(|row| row.parent()?.item());
            if added > 0
                && parent
                    .as_ref()
                    .and_then(decode)
                    .is_some_and(|p| p.rel == folder)
            {
                fade_rows(&view, at..at + added);
            }
        }
    });

    let (body, landed) = body_stack(&view, &vault, root);
    // Populated straight from the index: the window must be up before the reconcile finishes.
    fill(root, &vault, &asked, &show_hidden, "", Some(landed.clone()));

    let host = gtk::Box::new(gtk::Orientation::Vertical, 0);
    host.append(&vault_row);
    host.append(&body);
    Tree {
        host,
        view,
        model,
        vault,
        root: root.clone(),
        cache,
        watches,
        asked,
        show_hidden,
        landed,
        ignored,
        changed,
        cut,
        marked,
        start,
        active,
        pinned,
        unpinned: RefCell::default(),
        revealed,
        flashes: Rc::default(),
        opened,
    }
}

/// Fade in the rows at `rows`, which just landed under the folder the reader opened and which
/// `view` has bound already, as it binds rows when they land, before it maps them: hidden at once,
/// each fades in once it is mapped, so a frame does not show it whole first.
fn fade_rows(view: &gtk::ListView, rows: std::ops::Range<u32>) {
    if !view.settings().is_gtk_enable_animations() {
        return;
    }
    let landed: Vec<gtk::TreeExpander> = expanders(view)
        .into_iter()
        .filter(|e| {
            e.list_row()
                .is_some_and(|row| rows.contains(&row.position()))
        })
        .collect();
    for expander in &landed {
        expander.set_opacity(0.0);
    }
    let (landed, frames) = (RefCell::new(landed), Cell::new(0));
    view.add_tick_callback(move |_, _| {
        frames.set(frames.get() + 1);
        let mut landed = landed.borrow_mut();
        landed.retain(|expander| {
            let mapped = expander.is_mapped();
            if mapped {
                crate::widgets::fade_in(expander);
            }
            !mapped
        });
        // One the list has not shown by then is shown whole.
        if landed.is_empty() || frames.get() > 3 {
            landed.iter().for_each(|expander| expander.set_opacity(1.0));
            return glib::ControlFlow::Break;
        }
        glib::ControlFlow::Continue
    });
}

/// The reader opened folder `rel`, `row` of the tree, whose row widget is `expander`: what is
/// already listed of it has landed under it, the rest will as its listing comes back.
fn opening(opened: &Fresh, row: &gtk::TreeListRow, rel: &str, expander: Option<gtk::TreeExpander>) {
    if opened.borrow().revealing {
        return;
    }
    opened.borrow_mut().folder = Some(rel.to_string());
    let view = expander.and_then(|e| e.ancestor(gtk::ListView::static_type()));
    let listed = row.children().map_or(0, |children| children.n_items());
    if let Some(view) = view.and_downcast::<gtk::ListView>().filter(|_| listed > 0) {
        let at = row.position() + 1;
        fade_rows(&view, at..at + listed);
    }
}

/// Lazy child model from `cache`, created and populated when first requested.
fn tree_model(
    root: &gio::ListStore,
    vault: &Arc<Vault>,
    cache: &Rc<RefCell<HashMap<String, gio::ListStore>>>,
    asked: &Asked,
    show_hidden: &ShowHidden,
    watches: &Watches,
) -> gtk::TreeListModel {
    gtk::TreeListModel::new(root.clone(), false, false, {
        let (vault, cache, asked, show_hidden, watches) = (
            vault.clone(),
            cache.clone(),
            asked.clone(),
            show_hidden.clone(),
            watches.clone(),
        );
        move |obj| {
            let row = decode(obj)?;
            if !row.is_dir() {
                return None;
            }
            let store = children_model(&vault, &cache, &asked, &show_hidden, &row.rel);
            // A gitignored folder is one the reader opened on purpose and one the index does not
            // walk, so its listing has nothing keeping it fresh but this.
            if !row.indexed && !row.dependency {
                watch_unindexed(&watches, &vault, &row.rel);
            }
            Some(store.upcast())
        }
    })
}

/// Reusable icon/name expander row: relist folders on opening, show
/// full-path tooltips and support outgoing drags and folder drops.
#[allow(clippy::too_many_arguments)]
fn setup_row(
    relist: &Rc<dyn Fn(&str)>,
    fresh: &Fresh,
    root_label: &str,
    marked: &Rc<RefCell<Marks>>,
    dragging: &Rc<dyn Fn(bool)>,
    local: Option<&Rc<Path>>,
    moves: &Move,
    imports: &Import,
    appends: &Append,
    carries: &Carry,
) -> gtk::TreeExpander {
    let icon = gtk::Image::new();
    let label = gtk::Label::builder()
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::Middle)
        .build();
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    row.append(&icon);
    row.append(&label);
    let expander = gtk::TreeExpander::new();
    expander.set_child(Some(&row));
    // The name is ellipsized in the middle inside a 200 px sidebar, so the only way to read
    // where a row really lives is to hover it. Answered on hover rather than written on bind:
    // `set_tooltip_text` triggers a tooltip query on the whole window, and paying that per
    // bound row tripled the cost of expanding a 2 400-child directory (12 ms to 40 ms).
    expander.set_has_tooltip(true);
    // The row this widget is bound to, and its handler: rows are recycled, so the handler
    // moves with the binding rather than piling up on every row the widget has shown.
    let opened: RefCell<Option<(gtk::TreeListRow, glib::SignalHandlerId)>> = RefCell::default();
    let (relist, fresh) = (relist.clone(), fresh.clone());
    expander.connect_list_row_notify(move |expander| {
        if let Some((row, handler)) = opened.borrow_mut().take() {
            row.disconnect(handler);
        }
        let Some(row) = expander.list_row() else {
            return;
        };
        let (relist, fresh, expander) = (relist.clone(), fresh.clone(), expander.downgrade());
        let handler = row.connect_expanded_notify(move |row| {
            let Some(item) = row.item().as_ref().and_then(decode) else {
                return;
            };
            if row.is_expanded() {
                opening(&fresh, row, &item.rel, expander.upgrade());
                relist(&item.rel);
            } else if fresh.borrow().folder.as_deref() == Some(item.rel.as_str()) {
                fresh.borrow_mut().folder = None;
            }
        });
        *opened.borrow_mut() = Some((row, handler));
    });
    crate::widgets::turn_arrows(&expander);
    let root_label = root_label.to_string();
    expander.connect_query_tooltip(move |expander, _, _, _, tooltip| {
        let row = expander.list_row().and_then(|row| row.item());
        let Some(row) = row.as_ref().and_then(decode) else {
            return false;
        };
        tooltip.set_text(Some(&format!("{root_label}/{}", row.rel)));
        true
    });
    expander.add_controller(drag_source(marked, dragging, local));
    // Dropped on a row: into the folder, or into the folder holding the file, which is where
    // that row's New File would have put one too.
    let row_dir = |target: &gtk::DropTarget, _: f64, _: f64| {
        let expander = target.widget()?.downcast::<gtk::TreeExpander>().ok()?;
        let row = expander.list_row()?.item().as_ref().and_then(decode)?;
        // And nothing is dropped into one either, for the same reason. The row simply never
        // lights up.
        (!row.dependency)
            .then(|| crate::fileops::row_dir(Some((&row.rel, row.is_dir()))).to_string())
    };
    expander.add_controller(move_target(moves, appends, carries, row_dir));
    // The same row takes files from another application, into the same folder.
    expander.add_controller(import_target(imports, appends, row_dir));
    expander
}

/// Drag row paths as pane-opening strings and separate tree-move payloads, and on a local vault
/// the files for other applications ([`files_content`]).
///
/// Paths survive widget recycling. Panes open or split; tree targets move files. Offer COPY
/// and MOVE so pointers reflect targets, while pane ties still choose COPY.
fn drag_source(
    marked: &Rc<RefCell<Marks>>,
    dragging: &Rc<dyn Fn(bool)>,
    local: Option<&Rc<Path>>,
) -> gtk::DragSource {
    let source = gtk::DragSource::builder()
        .actions(gdk::DragAction::COPY | gdk::DragAction::MOVE)
        .build();
    let marked = marked.clone();
    let local = local.cloned();
    source.connect_prepare(move |source, _, _| {
        let expander = source.widget()?.downcast::<gtk::TreeExpander>().ok()?;
        let row = expander.list_row()?.item().as_ref().and_then(decode)?;
        // Nothing is dragged out of somebody else's dependency tree, which is opened to look
        // at, never edited from here, but the tree whole, marked through its top folder.
        let marks = marked.borrow();
        if row.dependency && !is_marked(&marks, &row.rel) {
            return None;
        }
        // A marked row carries the whole set, unless the set is that row alone.
        if is_marked(&marks, &row.rel) && !(marks.len() == 1 && marks.contains_key(&row.rel)) {
            let set: Vec<&str> = marks.keys().map(String::as_str).collect();
            let set = gtk::StringList::new(&set);
            let marked: Vec<(String, bool)> = marks.iter().map(|(r, d)| (r.clone(), *d)).collect();
            let top: Vec<String> = crate::fileops::topmost(&marked)
                .into_iter()
                .map(|(rel, _)| rel)
                .collect();
            let mut offered = vec![gdk::ContentProvider::for_value(&set.to_value())];
            offered.extend(files_content(local.as_deref(), &top));
            return Some(gdk::ContentProvider::new_union(&offered));
        }
        let mut offered = vec![move_content(&row.rel)];
        // A directory offers no pane type: it has no single note to open, so a pane must never
        // be able to take it.
        if !row.is_dir() {
            offered.push(gdk::ContentProvider::for_value(&row.rel.to_value()));
        }
        offered.extend(files_content(
            local.as_deref(),
            std::slice::from_ref(&row.rel),
        ));
        Some(gdk::ContentProvider::new_union(&offered))
    });
    let begin = dragging.clone();
    source.connect_drag_begin(move |_, _| begin(true));
    let end = dragging.clone();
    source.connect_drag_end(move |_, _, _| end(false));
    source
}

/// Bind `item` through widget lookups and setters only, keeping database
/// access off the scroll/bind path.
#[allow(clippy::too_many_arguments)]
fn bind_row(
    expander: &gtk::TreeExpander,
    item: &gtk::ListItem,
    ignored: &RefCell<Ignored>,
    changed: &RefCell<HashMap<String, Change>>,
    cut: &RefCell<HashSet<String>>,
    marked: &RefCell<Marks>,
    start: &RefCell<Option<String>>,
    revealed: &RefCell<Option<String>>,
) {
    let Some(row) = item.item().and_downcast::<gtk::TreeListRow>() else {
        return;
    };
    let Some(item) = row.item().as_ref().and_then(decode) else {
        return;
    };
    expander.set_list_row(Some(&row));
    let hbox = expander
        .child()
        .and_downcast::<gtk::Box>()
        .expect("row box");
    let icon = hbox
        .first_child()
        .and_downcast::<gtk::Image>()
        .expect("icon");
    icon.set_icon_name(Some(match item.is_dir() {
        true => FOLDER_ICON,
        false => icon_for(&item.rel),
    }));
    let label = icon
        .next_sibling()
        .and_downcast::<gtk::Label>()
        .expect("label");
    label.set_text(basename(&item.rel));
    // Both branches, always: row widgets are recycled, so a row that stops being ignored has
    // to have the class taken off it again. A row the index does not hold is dimmed by the
    // same rule and for the same reason the ignored ones are: search does not reach it. A
    // dot-named row is dimmed so that it still reads as hidden while it is shown, and a cut
    // one so that it reads as already on its way out.
    let dim = !item.indexed
        || dot_named(&item.rel)
        || ignored.borrow().has(&item.rel)
        || cut.borrow().contains(&item.rel);
    for widget in [icon.upcast_ref::<gtk::Widget>(), label.upcast_ref()] {
        set_class(widget, "dimmed", dim);
    }
    // The name alone, as VS Code tints it: the icon is the kind of file, not its state.
    let change = changed.borrow().get(&item.rel).copied();
    for each in Change::ALL {
        set_class(&label, each.class(), change == Some(each));
    }
    // On the expander rather than on the label: a mark is about the row, not about its name,
    // and the expander is the one widget here that spans the whole of it (`file-tree` in
    // `build::install_chrome_css`). Never on the row widget above it, which is not yet in
    // the list the first time its row is bound, and which a reference taken to it then
    // would finalise.
    set_class(
        expander,
        MARKED,
        is_marked(&marked.borrow(), &item.rel)
            || start.borrow().as_deref() == Some(item.rel.as_str()),
    );
    let on = revealed.borrow().as_deref() == Some(item.rel.as_str());
    set_class(expander, REVEALED, on);
}

/// Open a row on a plain click and handle marked-row selection, ahead of the list's own click.
///
/// A click opens a file or toggles a folder on its release, once it is not a drag, and selects
/// the row; the list's own click is claimed from it, the tree doing all a click does. Ctrl
/// toggles marks; Shift replaces with the anchor range, or adds with Ctrl. Context
/// menus, Delete and drags act on the set. Capture claiming prevents opening or toggling
/// rows; plain clicks and blank-area clicks clear marks. A row inside a dependency tree marks
/// the tree's top folder ([`markable`]), as batch operations must not edit inside it.
fn marking(
    model: &gtk::TreeListModel,
    cache: &Rc<RefCell<HashMap<String, gio::ListStore>>>,
    marked: &Rc<RefCell<Marks>>,
    start: &Start,
    anchor: &Rc<RefCell<Option<String>>>,
    active: &Rc<RefCell<Option<String>>>,
    on_activate: &Rc<impl Fn(char, &str, bool) + 'static>,
) -> gtk::GestureClick {
    let marking = gtk::GestureClick::builder()
        .button(gdk::BUTTON_PRIMARY)
        .propagation_phase(gtk::PropagationPhase::Capture)
        .build();
    // The row a plain press landed on, which its release opens.
    let pressed: Rc<RefCell<Option<String>>> = Rc::default();
    marking.connect_pressed({
        let (marked, start, anchor, active, cache, model, on_activate, pressed) = (
            marked.clone(),
            start.clone(),
            anchor.clone(),
            active.clone(),
            cache.clone(),
            model.clone(),
            on_activate.clone(),
            pressed.clone(),
        );
        move |gesture, presses, x, y| {
            let Some(view) = gesture.widget().and_downcast::<gtk::ListView>() else {
                return;
            };
            let held = gesture.current_event_state();
            let ctrl = held.contains(crate::platform::PRIMARY_MODIFIER);
            let shift = held.contains(gdk::ModifierType::SHIFT_MASK);
            let plain = (!ctrl && !shift).then(|| row_at(&view, x, y)).flatten();
            *pressed.borrow_mut() = plain
                .as_ref()
                .filter(|_| presses == 1)
                .map(|r| r.rel.clone());
            // The second press of a double click, which the list would activate again: the first
            // one opened a file as a preview, and this keeps it; a folder it toggled stays so.
            if presses == 2
                && let Some(row) = plain
            {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                if !row.is_dir() {
                    on_activate(row.kind, &row.rel, true);
                }
                return;
            }
            let row = list_row_at(&view, x, y).and_then(|row| markable(&row));
            let mut marks = marked.borrow_mut();
            match row {
                Some(row) if shift => {
                    // From where a range starts, else the row alone; Shift leaves the anchor
                    // where it is.
                    let from = start_of(&model, anchor.borrow().clone(), active.borrow().clone())
                        .map_or_else(|| row.rel.clone(), |from| from.rel);
                    mark_range(&mut marks, &model, &from, &row.rel, ctrl);
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                }
                Some(row) if ctrl => {
                    toggle(&mut marks, &row, &cache);
                    *anchor.borrow_mut() = Some(row.rel);
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                }
                // A press on a marked row may start a drag that carries the set, so the marks
                // stay until it turns out to be a click, which opens the row on its release.
                Some(row) if is_marked(&marks, &row.rel) => {
                    *anchor.borrow_mut() = Some(row.rel);
                    return;
                }
                row => {
                    *anchor.borrow_mut() = row.map(|row| row.rel);
                    if marks.is_empty() {
                        return;
                    }
                    marks.clear();
                }
            }
            start.replace(None);
            redraw_marks(&view, &marks, None);
        }
    });
    marking.connect_released({
        let (marked, model, on_activate) = (marked.clone(), model.clone(), on_activate.clone());
        move |gesture, presses, x, y| {
            let Some(rel) = pressed.take().filter(|_| presses == 1) else {
                return;
            };
            let Some(view) = gesture.widget().and_downcast::<gtk::ListView>() else {
                return;
            };
            if row_at(&view, x, y).is_some_and(|row| row.rel == rel)
                && let Some(row) = find_row(&model, &rel)
            {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                open_row(&view, &marked, &*on_activate, &row);
                // The row clicked is what the keys act on next, a folder's too as in VS Code
                // (user decision 2026-10-29), until the tab in front changes; a file already in
                // front opens no tab that would select it.
                select(&view, Some(&rel));
            }
        }
    });
    marking
}

/// A row activated from the keyboard opens as a clicked one does ([`open_row`]).
fn click_opens(
    view: &gtk::ListView,
    marked: &Rc<RefCell<Marks>>,
    on_activate: Rc<impl Fn(char, &str, bool) + 'static>,
) {
    let marked = marked.clone();
    view.connect_activate(move |view, pos| {
        let row = view.model().and_then(|m| m.item(pos));
        if let Some(row) = row.and_downcast::<gtk::TreeListRow>() {
            open_row(view, &marked, &*on_activate, &row);
        }
    });
}

/// A row opened: a folder toggles, a file is handed to `on_activate`. A plain click on a marked
/// row lets the marks go, as one anywhere else already has.
fn open_row(
    view: &gtk::ListView,
    marked: &RefCell<Marks>,
    on_activate: &impl Fn(char, &str, bool),
    row: &gtk::TreeListRow,
) {
    {
        let mut marks = marked.borrow_mut();
        if !marks.is_empty() {
            marks.clear();
            redraw_marks(view, &marks, None);
        }
    }
    let Some(item) = row.item().as_ref().and_then(decode) else {
        return;
    };
    if item.is_dir() {
        row.set_expanded(!row.is_expanded());
    } else {
        on_activate(item.kind, &item.rel, false);
    }
}

/// Show the Shift range anchor while Shift is held over the list.
fn track_shift(
    view: &gtk::ListView,
    model: &gtk::TreeListModel,
    marked: &Rc<RefCell<Marks>>,
    start: &Start,
    anchor: &Rc<RefCell<Option<String>>>,
    active: &Rc<RefCell<Option<String>>>,
) {
    // Holding Shift over the list draws the row a Shift+click would range from marked, while
    // nothing is: it need not be the selected row, the last click being where a range starts.
    // Shown as the pointer moves rather than on the key itself, so a capital typed with the
    // pointer resting on the list lights nothing; `Tree::hide_start` takes it down when the key
    // is let go.
    let show_start = {
        let (marked, start, anchor, active, model) = (
            marked.clone(),
            start.clone(),
            anchor.clone(),
            active.clone(),
            model.clone(),
        );
        move |motion: &gtk::EventControllerMotion, held: bool| {
            let Some(view) = motion.widget().and_downcast::<gtk::ListView>() else {
                return;
            };
            let marks = marked.borrow();
            let want = held && marks.is_empty();
            if want == start.borrow().is_some() {
                return;
            }
            let row = want
                .then(|| start_of(&model, anchor.borrow().clone(), active.borrow().clone()))
                .flatten();
            *start.borrow_mut() = row.map(|row| row.rel);
            redraw_marks(&view, &marks, start.borrow().as_deref());
        }
    };
    let held = |motion: &gtk::EventControllerMotion| {
        motion
            .current_event_state()
            .contains(gdk::ModifierType::SHIFT_MASK)
    };
    let motion = gtk::EventControllerMotion::new();
    motion.connect_enter({
        let show_start = show_start.clone();
        move |motion, _, _| show_start(motion, held(motion))
    });
    motion.connect_motion({
        let show_start = show_start.clone();
        move |motion, _, _| show_start(motion, held(motion))
    });
    motion.connect_leave(move |motion| show_start(motion, false));
    view.add_controller(motion);
}

/// Select the open file's row, or a menu's while one is up, whenever the model changes with
/// nothing selected: the listing lands from a worker thread and expanding a folder inserts rows,
/// so the row often is not there — or not there yet — at the moment the tab changed. A row the
/// keys moved the selection to keeps it. The view weakly: the model holds this handler, and a
/// strong one would be a ring keeping the model, and the vault its listings ask, alive after the
/// window has closed.
fn keep_selected(
    view: &gtk::ListView,
    model: &gtk::TreeListModel,
    active: &Rc<RefCell<Option<String>>>,
    pinned: &Rc<RefCell<Option<String>>>,
) {
    let (view, active, pinned) = (view.downgrade(), active.clone(), pinned.clone());
    model.connect_items_changed(move |_, _, _, _| {
        let Some(view) = view.upgrade() else {
            return;
        };
        let selection = view.model().and_downcast::<gtk::SingleSelection>();
        if selection.is_some_and(|s| s.selected() == gtk::INVALID_LIST_POSITION) {
            let row = pinned.borrow().clone().or_else(|| active.borrow().clone());
            select(&view, row.as_deref());
        }
    });
}

/// Show the tree or its empty/loading state according to the root listing.
///
/// An empty result needs an explicit message, while a pending remote listing is not yet
/// known to be empty. Worker completion distinguishes these states.
fn body_stack(
    view: &gtk::ListView,
    vault: &Vault,
    root: &gio::ListStore,
) -> (gtk::Stack, Rc<dyn Fn()>) {
    let scroller = scroller(view);
    // ponytail: a plain `GtkBox` around the scroller, purely so the context menu has a
    // layout-managed widget to hang off. GTK re-presents a popover from its parent's
    // `allocate_native_children`, which only runs for widgets that use a layout manager;
    // `GtkListView` has a custom `size_allocate` and never re-presents its popover children, so a
    // menu parented to the list is frozen at its first-frame size and `GtkPopoverMenu`'s inner
    // scrolled window turns everything that grows afterwards into a scrollbar. If a scrollbar
    // ever comes back, the next dial is setting that inner scrolled window's policies to Never.
    let body = gtk::Stack::builder().vexpand(true).build();
    body.add_named(&scroller, Some("list"));
    body.add_named(
        &status_page(
            "folder-symbolic",
            "No Files Yet",
            "Create a note to get started.",
        ),
        Some("empty"),
    );
    let remote_host = vault.remote().map(|r| r.url().host.clone());
    if let Some(host) = &remote_host {
        body.add_named(
            &status_page(
                "remote-server-symbolic",
                &format!("Waiting for {host}"),
                "Files will appear when the host responds.",
            ),
            Some("waiting"),
        );
    }
    let listed = Rc::new(Cell::new(remote_host.is_none()));
    // Weak: `body` holds the list, the list the model, and the model this store, so a strong
    // handle is a cycle that keeps the tree, and the vault in the model's create-func, alive after
    // the window has closed.
    let show_rows = glib::clone!(
        #[weak]
        body,
        #[strong]
        listed,
        move |rows: u32| {
            body.set_visible_child_name(match (rows, listed.get()) {
                (0, true) => "empty",
                (0, false) => "waiting",
                _ => "list",
            });
        }
    );
    show_rows(root.n_items());
    // An empty vault's listing changes nothing in the store, so the store alone never says it
    // came.
    let landed: Rc<dyn Fn()> = Rc::new({
        let (root, show_rows) = (root.clone(), show_rows.clone());
        move || {
            listed.set(true);
            show_rows(root.n_items());
        }
    });
    root.connect_items_changed(move |store, _, _, _| show_rows(store.n_items()));
    (body, landed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ancestors_lists_the_directories_reveal_has_to_expand() {
        let dirs = |rel| ancestors(rel).collect::<Vec<_>>();
        assert_eq!(dirs("a/b/c.md"), ["a", "a/b"]);
        // A note at the vault root has nothing above it to expand.
        assert_eq!(dirs("c.md"), [] as [&str; 0]);
        assert_eq!(dirs(""), [] as [&str; 0]);
    }

    #[test]
    fn is_ignored_covers_a_file_a_directory_and_what_is_under_it() {
        let set = Ignored::new(
            ["build/".to_string(), "notes/a.log".to_string()]
                .into_iter()
                .collect(),
        );
        assert!(set.has("notes/a.log"));
        assert!(set.has("build"));
        assert!(set.has("build/deep/thing.o"));
        assert!(!set.has("notes/b.log"));
        // A directory whose name merely starts the same is a different directory.
        assert!(!set.has("builder/x"));
        assert!(!Ignored::default().has("build/x"));
    }

    #[test]
    fn a_folder_takes_the_strongest_change_under_it() {
        use Change::{Added, Modified};
        let map = roll_up(vec![
            ("a/b/new.md".into(), Added),
            ("a/b/c/edited.md".into(), Modified),
            ("a/d/new.md".into(), Added),
            ("top.md".into(), Modified),
        ]);
        let at = |rel: &str| map.get(rel).copied();
        assert_eq!(at("a/b/new.md"), Some(Added));
        assert_eq!(at("a/d"), Some(Added));
        // Modified outweighs new, whichever came first.
        assert_eq!(at("a/b"), Some(Modified));
        assert_eq!(at("a"), Some(Modified));
        assert_eq!(at("top.md"), Some(Modified));
        assert_eq!(at("a/e"), None);
        assert_eq!(map.len(), 8);
    }
}
