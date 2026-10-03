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
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

mod drag;
mod listing;

pub use drag::dropped_paths;
use drag::{Import, Move, import_target, move_content, move_target};
use listing::{Asked, ShowHidden, Watches, children_model, fill, watch_unindexed};
pub use listing::{decode, dot_named};

/// The style class a row a Ctrl+click has marked carries, defined in `build::install_chrome_css`.
const MARKED: &str = "accent-marked";

/// The rows a Ctrl+click or a Shift+click has marked, by path, each with whether it is a folder.
///
/// A marked folder stands for everything under it, shut or open, so nothing under one is ever in
/// the set as well ([`fileops::topmost`](crate::fileops::topmost)): its rows are drawn marked when
/// it is opened, and a batch acts on the set exactly as it is. A folder shut with thousands of
/// files in it is one entry, not thousands.
type Marks = BTreeMap<String, bool>;

/// One row of the tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// `d` for a directory, `f` for a file — see [`encode`](listing::encode).
    pub kind: char,
    /// Vault-relative path.
    pub rel: String,
    /// Whether the index holds this row.
    ///
    /// False inside every tree the walk refuses, whichever kind: those are read off the disk and
    /// nothing else in the app knows they are there — they are not searched and not stored — so
    /// the row is dimmed the way a gitignored one is.
    pub indexed: bool,
    /// Whether the row is inside one of the **dependency** trees — a `node_modules`, a `.venv`, a
    /// cargo `target/` — rather than a folder the reader gitignored.
    ///
    /// Both are listed, because a file tree that leaves a folder out is one nobody can trust, but
    /// only this one is refused every edit: it is somebody else's tree, opened to look at. A
    /// gitignored folder is the reader's own — a build output, an `mlruns/` — and being out of
    /// the index stops it being *searched*, not edited. Implies `!indexed`.
    pub dependency: bool,
}

impl Row {
    pub fn is_dir(&self) -> bool {
        self.kind == 'd'
    }
}

/// The lazy tree, plus the per-directory child models it has already built.
///
/// The cache is not an optimisation of last resort, it is what makes binding a row free:
/// `GtkTreeExpander::set_list_row` asks `gtk_tree_list_row_is_expandable()`, which calls the
/// `TreeListModel` create-func and *throws the model away again*. Without the cache every row
/// scrolling into view ran a fresh `list_dir` query.
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
    /// What git ignores, shared with the row factory so binding a row is still two setters and a
    /// set lookup rather than a question for the index.
    ignored: Rc<RefCell<Ignored>>,
    /// The rows a Cut is waiting to move, shared with the factory the same way.
    cut: Rc<RefCell<HashSet<String>>>,
    /// The marked rows, which are the set the context menu acts on when the right-click lands on
    /// one of them. Sorted, so the menu and the toasts name them in path order. Shared with the
    /// factory like [`cut`](Self::cut): a marked row scrolled out of view and back has to come
    /// back marked.
    marked: Rc<RefCell<Marks>>,
    /// The row a Shift+click would range from, drawn marked while Shift is held over the list and
    /// nothing is marked yet. See [`build`].
    start: Start,
    /// The open file, which the selection follows. Shared with the pointer-leave handler: the
    /// list selects rows on hover (see `build`), so the selection has to be put back whenever
    /// the pointer goes away again.
    active: Rc<RefCell<Option<String>>>,
    /// The row a context menu is open over, which outranks [`active`](Self::active) for as long
    /// as it is. See [`pin`](Self::pin).
    pinned: Rc<RefCell<Option<String>>>,
}

impl Tree {
    /// Tell the tree what git ignores, and redraw the rows on screen.
    ///
    /// The factory is reset rather than the model spliced: a splice recreates every
    /// `GtkTreeListRow` and collapses the directories the reader had opened, while re-binding
    /// only touches the handful of rows actually visible.
    pub fn set_ignored(&self, ignored: HashSet<String>) {
        let ignored = Ignored::new(ignored);
        if *self.ignored.borrow() == ignored {
            return;
        }
        *self.ignored.borrow_mut() = ignored;
        rebind(&self.view);
    }

    /// Dim the rows a Cut is waiting on, and undim the rest. The same `dim-label` an ignored row
    /// takes: "this is on its way somewhere" and "search does not reach this" look alike, and one
    /// signal per row is enough to read (`connect_bind`).
    ///
    /// Reset the same way [`set_ignored`](Self::set_ignored) is, and for the same reason: a splice
    /// would collapse every folder the reader had opened.
    pub fn set_cut(&self, cut: HashSet<String>) {
        if *self.cut.borrow() == cut {
            return;
        }
        *self.cut.borrow_mut() = cut;
        rebind(&self.view);
    }

    /// The marked rows, each with whether it is a directory, in path order. None is inside
    /// another, a marked folder standing for what it holds.
    pub fn marked(&self) -> Vec<(String, bool)> {
        self.marked
            .borrow()
            .iter()
            .map(|(rel, is_dir)| (rel.clone(), *is_dir))
            .collect()
    }

    /// Whether `rel` is drawn marked: marked itself, or inside a marked folder.
    pub fn is_marked(&self, rel: &str) -> bool {
        is_marked(&self.marked.borrow(), rel)
    }

    /// What a Ctrl+click on the row `rel` does: mark it, or take the mark off it again.
    #[cfg(feature = "bench")]
    pub fn toggle_mark(&self, rel: &str) {
        let row = find_row(&self.model, rel).and_then(|row| row.item());
        let Some(row) = row.as_ref().and_then(decode) else {
            return;
        };
        let mut marked = self.marked.borrow_mut();
        toggle(&mut marked, &row, &self.cache);
        redraw_marks(&self.view, &marked, None);
    }

    /// What a Shift+click on the row `to` does with `from` as the last row clicked without Shift:
    /// mark every row between them, replacing the marks, or with Ctrl held too (`add`) adding
    /// to them.
    #[cfg(feature = "bench")]
    pub fn mark_range(&self, from: &str, to: &str, add: bool) {
        let mut marked = self.marked.borrow_mut();
        mark_range(&mut marked, &self.model, from, to, add);
        redraw_marks(&self.view, &marked, None);
    }

    /// Forget every mark, and say whether there was one to forget — which is what lets Escape
    /// fall through to the rest of the window when the tree has nothing marked.
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

    /// Show or hide the dot-named rows. Every level already listed is listed again, since a
    /// hidden row was never put in the store; a change of nothing costs nothing, because every
    /// preference edit arrives here.
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

    /// Re-read the root and every level that has already been expanded, after a full reconcile
    /// changed everything at once. The cached models are refilled rather than dropped: dropping
    /// them would leave the rows that are still expanded showing a listing nothing refreshes.
    pub fn refresh(&self) {
        let mut dirs: Vec<String> = vec![String::new()];
        dirs.extend(self.cache.borrow().keys().cloned());
        self.invalidate(&dirs);
    }

    /// Reload on a folder's menu: list `dir` and every folder open under it again, and say
    /// whether the index holds `dir`. Only a folder it holds is listed out of the index; one it
    /// does not walk is read off the disk, and so is all that is under it.
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

    /// Refill only these directories' cached child models ("" is the root). A background reindex
    /// touches a handful of directories, not the whole tree.
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

    /// The row under a pointer position, for the context menu.
    pub fn row_at(&self, x: f64, y: f64) -> Option<Row> {
        row_at(&self.view, x, y)
    }

    /// Move the selection onto the open file, or off every row when nothing is open.
    ///
    /// Only among the rows the tree already has: a path whose folders are still shut is not
    /// expanded to, and the list is not scrolled. Which tab is in front should not move the tree
    /// under the reader — Reveal in Sidebar is the gesture that does.
    pub fn set_active(&self, rel: Option<&str>) {
        *self.active.borrow_mut() = rel.map(str::to_string);
        // Remembered but not applied while a menu holds the highlight: a note opening behind the
        // popover must not take the lit row out from under it either.
        if self.pinned.borrow().is_none() {
            select(&self.view, rel);
        }
    }

    /// Hold the highlight on `rel`, the row a context menu has just been opened over, or let it
    /// go again with `None`, which puts it back on the open file.
    ///
    /// The list selects rows on hover (`single-click-activate`, see [`build`]), and the popover
    /// taking the pointer is a *leave* as far as the list is concerned — so without this the
    /// highlight slid back onto whatever file was open the moment the menu appeared, leaving the
    /// menu pointing at one row while another was lit.
    pub fn pin(&self, rel: Option<&str>) {
        *self.pinned.borrow_mut() = rel.map(str::to_string);
        let row = rel
            .map(str::to_string)
            .or_else(|| self.active.borrow().clone());
        select(&self.view, row.as_deref());
    }

    /// Expand everything above `rel`, then select it and scroll it into view. False when the path
    /// is not in the tree at all, so a caller can say so rather than silently doing nothing.
    pub fn reveal(&self, rel: &str) -> bool {
        for dir in ancestors(rel) {
            match find_row(&self.model, dir) {
                Some(row) => row.set_expanded(true),
                None => return false,
            }
        }
        let Some(row) = find_row(&self.model, rel) else {
            return false;
        };
        self.view.scroll_to(
            row.position(),
            gtk::ListScrollFlags::SELECT | gtk::ListScrollFlags::FOCUS,
            None,
        );
        true
    }
}

/// What search leaves out, in the two shapes git reports it: a wholly ignored directory as one
/// entry with a trailing slash, everything under it ignored with it, and a partly ignored
/// directory's files listed one by one.
///
/// Split into the two sets when it arrives rather than asked in git's own spelling on every bind:
/// the old shape built `format!("{rel}/")` for the row and one more for each of its ancestors,
/// which is a handful of allocations per row on a path the reader is only scrolling past.
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

/// Every directory above `rel`, outermost first: `a/b/c.md` yields `a` then `a/b`.
fn ancestors(rel: &str) -> impl Iterator<Item = &str> {
    rel.match_indices('/').map(|(at, _)| &rel[..at])
}

/// The row at a position in the list, or `None` over the blank area below the last one.
fn row_at(view: &gtk::ListView, x: f64, y: f64) -> Option<Row> {
    let mut widget = view.pick(x, y, gtk::PickFlags::DEFAULT)?;
    // `pick` lands on the label or the icon; the row identity hangs off the expander above it.
    let expander = loop {
        match widget.downcast::<gtk::TreeExpander>() {
            Ok(expander) => break expander,
            Err(w) => widget = w.parent()?,
        }
    };
    expander.list_row()?.item().as_ref().and_then(decode)
}

/// See [`Tree::start`].
type Start = Rc<RefCell<Option<String>>>;

/// The row a Shift+click ranges from: the last row clicked without Shift while it is in the list,
/// else the open file's row. Never one in a dependency tree, which is never marked.
fn start_of(
    model: &gtk::TreeListModel,
    anchor: Option<String>,
    open: Option<String>,
) -> Option<Row> {
    [anchor, open].into_iter().flatten().find_map(|rel| {
        let row = find_row(model, &rel)?.item().as_ref().and_then(decode)?;
        (!row.dependency).then_some(row)
    })
}

/// Whether `rel` is marked itself or inside a marked folder.
fn is_marked(marks: &Marks, rel: &str) -> bool {
    !marks.is_empty()
        && (marks.contains_key(rel) || ancestors(rel).any(|dir| marks.contains_key(dir)))
}

/// Add rows to the marks, keeping only the top-most: what a folder being marked already takes
/// along is not marked again.
fn add_marks(marks: &mut Marks, rows: impl IntoIterator<Item = (String, bool)>) {
    marks.extend(rows);
    let all: Vec<(String, bool)> = std::mem::take(marks).into_iter().collect();
    marks.extend(crate::fileops::topmost(&all));
}

/// A Ctrl+click on `row`: its mark on, or off again.
///
/// Off inside a marked folder, that folder gives way to what it holds, level by level down to
/// the row, which alone is left out: the rest of the folder stays marked. The levels are the
/// tree's own listings, which every folder above a row on screen has; what they leave out — a
/// dot-named file while Show Hidden Files is off — is then no longer marked.
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

/// The rows the tree lists in `dir`, as it has them, leaving out the dependency trees: those are
/// never marked.
fn listed(cache: &RefCell<HashMap<String, gio::ListStore>>, dir: &str) -> Vec<(String, bool)> {
    let Some(store) = cache.borrow().get(dir).cloned() else {
        return Vec::new();
    };
    (0..store.n_items())
        .filter_map(|i| store.item(i).as_ref().and_then(decode))
        .filter(|row| !row.dependency)
        .map(|row| (row.rel.clone(), row.is_dir()))
        .collect()
}

/// A Shift+click: every row from `from` to `to`, both included, in the order the tree lists them,
/// replacing the marks or added to them. A shut folder in between is marked whole.
fn mark_range(marks: &mut Marks, model: &gtk::TreeListModel, from: &str, to: &str, add: bool) {
    let at = |rel| find_row(model, rel).map(|row| row.position());
    let (Some(a), Some(b)) = (at(from), at(to)) else {
        return;
    };
    let rows: Vec<(String, bool)> = (a.min(b)..=a.max(b))
        .filter_map(|i| model.item(i).and_downcast::<gtk::TreeListRow>()?.item())
        .filter_map(|item| decode(&item))
        .filter(|row| !row.dependency)
        .map(|row| (row.rel.clone(), row.is_dir()))
        .collect();
    if !add {
        marks.clear();
    }
    add_marks(marks, rows);
}

/// Put the mark on the rows on screen that carry it and take it off the rest.
///
/// Written straight onto the row widgets rather than through [`rebind`]: a factory reset recreates
/// every row widget, and the press that clears the marks is one the list still has to answer —
/// with the widget pulled out from under it the note under a plain click stopped opening
/// (`ACCENT_BENCH_MENU=press:<rel>` and an XTEST click, 2026-09-14). The factory reads the set on
/// every bind all the same, which is what brings a mark back with a row scrolled out of view.
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

/// The row widgets the list has on screen, which is where anything a bind writes can be read back
/// off or written again. Used by the drills as well.
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

/// Redraw the rows on screen against the state the factory reads on every bind — what git
/// ignores, what a Cut is waiting on.
///
/// The factory is reset rather than the model spliced: a splice recreates every `GtkTreeListRow`
/// and collapses the directories the reader had opened, while re-binding only touches the handful
/// of rows actually visible.
fn rebind(view: &gtk::ListView) {
    let factory = view.factory();
    view.set_factory(None::<&gtk::ListItemFactory>);
    view.set_factory(factory.as_ref());
}

/// Put the selection on `rel`'s row, or on no row at all. Cheap when it is already there, which
/// is what keeps it out of the way of the pointer selecting rows as it crosses the list.
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

/// The row holding `rel`, or `None` while its parent is still collapsed.
///
/// ponytail: a linear scan of the rows the model currently has, which is every *visible* row and
/// not the whole vault. `reveal` runs it once per path segment, so revealing a note five levels
/// deep in a 2 400-row expansion is six scans of a few thousand items. Build a rel-path to row
/// index alongside the child-model cache if that ever shows up in a profile.
pub fn find_row(model: &gtk::TreeListModel, rel: &str) -> Option<gtk::TreeListRow> {
    (0..model.n_items()).find_map(|i| {
        let row = model.item(i).and_downcast::<gtk::TreeListRow>()?;
        let item = row.item().as_ref().and_then(decode)?;
        (item.rel == rel).then_some(row)
    })
}

/// The row above the tree naming the vault, and the drop zone for "put it in the vault root".
///
/// The blank area below the last row is the other one, and a tree scrolled deep in a large vault
/// has none, which is what this is for: it is always on screen. A label rather than a list row,
/// because it stands for what the whole listing is of — there is nothing to open or expand.
/// The row is as tall as the controls the other panes open with, so the three sit in one band:
/// 6 px of margin and then 34 px of row, which is the Search pane's entry (`search.rs`, margin 6
/// on a `GtkSearchEntry` whose Adwaita minimum is 34) and the Git pane's branch chooser (`git.rs`,
/// the same margin on a `GtkDropDown` of the same minimum). A label alone measured 20 px, which
/// put the vault name six pixels above both of them.
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

/// Build the tree. `show_hidden` is Show Hidden Files as the window opens. `on_activate` is called
/// with the rel_path of an activated non-directory row, `on_drag` with `true` while a row is being
/// dragged out of the tree and `false` when it is over, so the panes can put their drop zones up
/// for the duration, `on_move` with each path a drag carried and the path it was dropped onto, and
/// `on_import` with the files another application dropped in, the folder they go into and whether
/// the drag was a move.
pub fn build(
    vault: Arc<Vault>,
    root: &gio::ListStore,
    show_hidden: bool,
    on_activate: impl Fn(char, &str) + 'static,
    on_drag: impl Fn(bool) + 'static,
    on_move: impl Fn(Vec<(String, String)>) + 'static,
    on_import: impl Fn(Vec<PathBuf>, String, bool) + 'static,
) -> Tree {
    let cache: Rc<RefCell<HashMap<String, gio::ListStore>>> = Rc::new(RefCell::new(HashMap::new()));
    let asked = Asked::default();
    let show_hidden = ShowHidden::new(Cell::new(show_hidden));
    let ignored: Rc<RefCell<Ignored>> = Rc::new(RefCell::new(Ignored::default()));
    let cut: Rc<RefCell<HashSet<String>>> = Rc::new(RefCell::new(HashSet::new()));
    let marked: Rc<RefCell<Marks>> = Rc::new(RefCell::new(Marks::new()));
    let start = Start::default();
    let watches = Watches::default();
    let model = gtk::TreeListModel::new(root.clone(), false, false, {
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
    });

    // Abbreviated once rather than per row: neither the vault root nor `$HOME` moves while the
    // window is open, and the label is only ever a prefix of a tooltip.
    let root_label = crate::fileops::display_path(&vault.root(), "");
    // Shared, because `setup` runs once per recycled row widget and both ends of every drag
    // report through the same closure.
    let dragging: Rc<dyn Fn(bool)> = Rc::new(on_drag);
    let moves: Move = Rc::new(on_move);
    let imports: Import = Rc::new(on_import);
    let vault_row = root_row(&root_label);
    vault_row.add_controller(move_target(&moves, |_, _, _| Some(String::new())));
    vault_row.add_controller(import_target(&imports, |_, _, _| Some(String::new())));
    let bind_ignored = ignored.clone();
    let bind_cut = cut.clone();
    let (bind_marked, bind_start) = (marked.clone(), start.clone());
    let drag_marked = marked.clone();
    let row_moves = moves.clone();
    let row_imports = imports.clone();
    // A folder's listing is kept for as long as the window is open, which is what makes binding
    // its row free, and it moves only when the vault says the folder changed. So each opening of
    // a folder lists it again: a dependency tree is watched by nothing, and a folder whose news
    // was missed — changed before its watch was in place, or with no watch to be had — would open
    // onto what it held the first time, however often it was opened.
    let relist: Rc<dyn Fn(&str)> = Rc::new({
        let (vault, cache, asked, show_hidden) = (
            vault.clone(),
            cache.clone(),
            asked.clone(),
            show_hidden.clone(),
        );
        move |rel| {
            let store = cache.borrow().get(rel).cloned();
            if let Some(store) = store {
                fill(&store, &vault, &asked, &show_hidden, rel, None);
            }
        }
    });
    let factory = crate::widgets::factory(
        move |_| {
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
            let opened: RefCell<Option<(gtk::TreeListRow, glib::SignalHandlerId)>> =
                RefCell::default();
            let relist = relist.clone();
            expander.connect_list_row_notify(move |expander| {
                if let Some((row, handler)) = opened.borrow_mut().take() {
                    row.disconnect(handler);
                }
                let Some(row) = expander.list_row() else {
                    return;
                };
                let relist = relist.clone();
                let handler = row.connect_expanded_notify(move |row| {
                    let item = row.item();
                    if let Some(item) = item.as_ref().and_then(decode).filter(|_| row.is_expanded())
                    {
                        relist(&item.rel);
                    }
                });
                *opened.borrow_mut() = Some((row, handler));
            });
            let root_label = root_label.clone();
            expander.connect_query_tooltip(move |expander, _, _, _, tooltip| {
                let row = expander.list_row().and_then(|row| row.item());
                let Some(row) = row.as_ref().and_then(decode) else {
                    return false;
                };
                tooltip.set_text(Some(&format!("{root_label}/{}", row.rel)));
                true
            });
            // A row can be dragged into a pane, which opens the note there, onto a pane's edge, which
            // splits it, or back into the tree, which moves the file. The two travel as two content
            // types (see `move_content`) and the payload is the path either way, which is what every
            // drop handler wants and what survives the row being recycled under the drag.
            //
            // `MOVE` beside `COPY` so the pointer says which of the two is about to happen. The panes
            // ask for both and GTK's drop target settles a tie on `COPY`, so what they do is unchanged.
            let source = gtk::DragSource::builder()
                .actions(gdk::DragAction::COPY | gdk::DragAction::MOVE)
                .build();
            let marked = drag_marked.clone();
            source.connect_prepare(move |source, _, _| {
                let expander = source.widget()?.downcast::<gtk::TreeExpander>().ok()?;
                let row = expander.list_row()?.item().as_ref().and_then(decode)?;
                // Nothing is dragged out of somebody else's dependency tree: it is opened to look
                // at, never edited from here.
                if row.dependency {
                    return None;
                }
                // A marked row carries the whole set, unless the set is that row alone.
                let marks = marked.borrow();
                if is_marked(&marks, &row.rel)
                    && !(marks.len() == 1 && marks.contains_key(&row.rel))
                {
                    let set: Vec<&str> = marks.keys().map(String::as_str).collect();
                    let set = gtk::StringList::new(&set);
                    return Some(gdk::ContentProvider::for_value(&set.to_value()));
                }
                let moving = move_content(&row.rel);
                // A directory offers the move type alone: it has no single note to open, so a pane
                // must never be able to take it.
                Some(match row.is_dir() {
                    true => moving,
                    false => gdk::ContentProvider::new_union(&[
                        moving,
                        gdk::ContentProvider::for_value(&row.rel.to_value()),
                    ]),
                })
            });
            let begin = dragging.clone();
            source.connect_drag_begin(move |_, _| begin(true));
            let end = dragging.clone();
            source.connect_drag_end(move |_, _, _| end(false));
            expander.add_controller(source);
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
            expander.add_controller(move_target(&row_moves, row_dir));
            // The same row takes files from another application, into the same folder.
            expander.add_controller(import_target(&row_imports, row_dir));
            expander
        },
        // Widget lookups and two setters only: no database access on the bind path.
        move |expander: &gtk::TreeExpander, item| {
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
                || bind_ignored.borrow().has(&item.rel)
                || bind_cut.borrow().contains(&item.rel);
            for widget in [icon.upcast_ref::<gtk::Widget>(), label.upcast_ref()] {
                set_class(widget, "dim-label", dim);
            }
            // On the expander rather than on the label: a mark is about the row, not about its name,
            // and the expander is the one widget here that spans the whole of it (`file-tree` in
            // `build::install_chrome_css`). Never on the row widget above it, which is not yet in
            // the list the first time its row is bound, and which a reference taken to it then
            // would finalise.
            set_class(
                expander,
                MARKED,
                is_marked(&bind_marked.borrow(), &item.rel)
                    || bind_start.borrow().as_deref() == Some(item.rel.as_str()),
            );
        },
    );

    let selection = gtk::SingleSelection::new(Some(model.clone()));
    selection.set_autoselect(false);
    selection.set_can_unselect(true);
    let view = gtk::ListView::new(None::<gtk::SingleSelection>, Some(factory));
    crate::widgets::set_model(&view, &selection);
    view.add_css_class("navigation-sidebar");
    view.add_css_class("file-tree");
    // One click opens, as GNOME's own sidebars do. A folder still toggles rather than opening,
    // so a click never costs anything you did not ask for.
    view.set_single_click_activate(true);
    // Ctrl+click marks a row instead of opening it, and Shift+click marks every row from the last
    // one clicked without Shift to this one, replacing the marks — or with Ctrl held as well,
    // adding to them. That is the only multiple selection the tree has: the context menu acts on
    // the whole set when the right-click lands on one of them, Delete trashes it and a drag of one
    // of its rows carries it. The gesture runs in the capture phase and claims those presses, so
    // the list never sees them and neither a note opens nor a folder toggles. A click with
    // nothing held is the reader saying "this one", so it forgets the set again — and so does a
    // click on the blank area below the last row. Rows in a dependency tree are never marked:
    // nothing the menu offers may happen inside somebody else's tree.
    let marking = gtk::GestureClick::builder()
        .button(gdk::BUTTON_PRIMARY)
        .propagation_phase(gtk::PropagationPhase::Capture)
        .build();
    // Where a Shift+click's range starts: the last row clicked without Shift.
    let anchor = Rc::new(RefCell::new(None::<String>));
    let active = Rc::new(RefCell::new(None::<String>));
    marking.connect_pressed({
        let (marked, start, anchor, active, cache, model) = (
            marked.clone(),
            start.clone(),
            anchor.clone(),
            active.clone(),
            cache.clone(),
            model.clone(),
        );
        move |gesture, _, x, y| {
            let Some(view) = gesture.widget().and_downcast::<gtk::ListView>() else {
                return;
            };
            let held = gesture.current_event_state();
            let ctrl = held.contains(gdk::ModifierType::CONTROL_MASK);
            let shift = held.contains(gdk::ModifierType::SHIFT_MASK);
            let row = row_at(&view, x, y).filter(|row| !row.dependency);
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
                // stay until it turns out to be a click, which the list activates.
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
    view.add_controller(marking);
    let activate_marked = marked.clone();
    view.connect_activate(move |view, pos| {
        // A plain click on a marked row lets the marks go, as one anywhere else already has.
        {
            let mut marks = activate_marked.borrow_mut();
            if !marks.is_empty() {
                marks.clear();
                redraw_marks(view, &marks, None);
            }
        }
        let Some(row) = view
            .model()
            .and_then(|m| m.item(pos))
            .and_downcast::<gtk::TreeListRow>()
        else {
            return;
        };
        let Some(item) = row.item().as_ref().and_then(decode) else {
            return;
        };
        if item.is_dir() {
            row.set_expanded(!row.is_expanded());
        } else {
            on_activate(item.kind, &item.rel);
        }
    });
    // `single-click-activate` is GTK's "activated on single click **and selected on hover**", so
    // the selection is what the pointer leaves behind as it crosses the list — and it is also
    // the highlight that says which file is open. The two are the same thing, so the open file's
    // row is put back the moment the pointer goes away, instead of a row nobody chose staying lit.
    let pinned = Rc::new(RefCell::new(None::<String>));
    // Where the highlight belongs when nothing is pointing at a row: the row a context menu is
    // open over while there is one, and otherwise the open file.
    let highlight = {
        let (active, pinned) = (active.clone(), pinned.clone());
        move |view: &gtk::ListView| {
            let row = pinned.borrow().clone().or_else(|| active.borrow().clone());
            select(view, row.as_deref());
        }
    };
    // Holding Shift over the list draws the row a Shift+click would range from marked, while
    // nothing is: the selection that shows the open file moves with the pointer, so that row
    // would otherwise be lit by nothing while the pointer is on its way to the other end. Shown as
    // the pointer moves rather than on the key itself, so a capital typed with the pointer resting
    // on the list lights nothing; `Tree::hide_start` takes it down when the key is let go.
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
    motion.connect_leave({
        let highlight = highlight.clone();
        move |controller| {
            let Some(view) = controller.widget().and_downcast::<gtk::ListView>() else {
                return;
            };
            show_start(controller, false);
            highlight(&view);
        }
    });
    // The listing lands from a worker thread and expanding a folder inserts rows, so the open
    // file's row often is not there — or not there yet — at the moment the tab changed. Re-applied
    // whenever the model changes, but never while the pointer is in the list: the selection is
    // the hover highlight too, and a reindex must not pull it out from under the row being
    // pointed at. The controller weakly: its own handlers hold the model, through `show_start`,
    // and a strong one here would be a ring keeping the model, and the vault its listings ask,
    // alive after the window has closed.
    model.connect_items_changed({
        let motion = motion.downgrade();
        move |_, _, _, _| {
            let Some(motion) = motion.upgrade() else {
                return;
            };
            if motion.contains_pointer() {
                return;
            }
            let Some(view) = motion.widget().and_downcast::<gtk::ListView>() else {
                return;
            };
            highlight(&view);
        }
    });
    view.add_controller(motion);
    // The blank area below the last row is the vault root, the same place a right-click there
    // creates in. A drop that landed on a row is that row's own business — its target has already
    // accepted or refused it — so this one has to answer for the blank area alone, or a refusal
    // bubbling up out of a row would turn into a move to the root.
    let blank = |target: &gtk::DropTarget, x, y| {
        let view = target.widget()?.downcast::<gtk::ListView>().ok()?;
        row_at(&view, x, y).is_none().then(String::new)
    };
    view.add_controller(move_target(&moves, blank));
    view.add_controller(import_target(&imports, blank));

    let scroller = scroller(&view);
    // ponytail: a plain `GtkBox` around the scroller, purely so the context menu has a
    // layout-managed widget to hang off. GTK re-presents a popover from its parent's
    // `allocate_native_children`, which only runs for widgets that use a layout manager;
    // `GtkListView` has a custom `size_allocate` and never re-presents its popover children, so a
    // menu parented to the list is frozen at its first-frame size and `GtkPopoverMenu`'s inner
    // scrolled window turns everything that grows afterwards into a scrollbar. If a scrollbar
    // ever comes back, the next dial is setting that inner scrolled window's policies to Never.
    // A vault with nothing in it gets the sentence every other pane's emptiness gets, rather than
    // a blank column that reads as a tree that failed to load (DESIGN.md, States). Driven by the
    // root store, which is filled from a worker thread and so is empty for a frame either way.
    // A remote vault is not known to be empty until its root has been listed: until then the host
    // has not answered, or refused, and the tree says what it is waiting for, as the document
    // column does.
    let body = gtk::Stack::builder().vexpand(true).build();
    body.add_named(&scroller, Some("list"));
    body.add_named(
        &status_page(
            "folder-symbolic",
            "No Files Yet",
            "A blank slate—create a note to get started.",
        ),
        Some("empty"),
    );
    let remote_host = vault.remote().map(|r| r.url().host.clone());
    if let Some(host) = &remote_host {
        body.add_named(
            &status_page(
                "network-server-symbolic",
                &format!("Waiting for {host}"),
                "Files will appear here when the host responds.",
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
        cut,
        marked,
        start,
        active,
        pinned,
    }
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
}
