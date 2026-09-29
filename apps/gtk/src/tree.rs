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
    /// `d` for a directory, `f` for a file — see [`encode`].
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

/// ponytail: rows are `gtk::StringObject`s holding `"<kind><state><rel_path>"` instead of a custom
/// GObject with typed properties. Saves ~40 lines of subclass boilerplate; if the tree ever needs
/// more per-row state (git status, unsaved marker) define a real `FileItem` GObject then.
///
/// `kind` is `d` or `f`; `state` is `i` for a row the index holds, `g` for one left out because
/// git ignores its folder, and `x` for one inside a dependency tree ([`Row::dependency`]).
fn encode(kind: FileKind, rel: &str, indexed: bool, dependency: bool) -> String {
    // What a file is — its icon, what it opens as — is read off its name, so a directory is the
    // one thing the row has to carry.
    let c = match kind {
        FileKind::Dir => 'd',
        _ => 'f',
    };
    let state = match (indexed, dependency) {
        (true, _) => 'i',
        (false, false) => 'g',
        (false, true) => 'x',
    };
    format!("{c}{state}{rel}")
}

pub fn decode(item: &glib::Object) -> Option<Row> {
    decode_str(&item.downcast_ref::<gtk::StringObject>()?.string())
}

/// The pure half of [`decode`], so the encoding is a test rather than a running window.
fn decode_str(s: &str) -> Option<Row> {
    let mut cs = s.chars();
    let kind = cs.next()?;
    let state = cs.next()?;
    Some(Row {
        kind,
        rel: cs.as_str().to_string(),
        indexed: state == 'i',
        dependency: state == 'x',
    })
}

/// Whether the tree leaves a listed row out: a Syncthing conflict always, and a dot-named one
/// while Show Hidden Files (`show_hidden`) is off.
///
/// A row the index does not hold is never left out: it is one of the skipped trees, which the walk
/// has already filtered, and hiding the dot-named ones would hide `.venv` and four of the six names
/// in `SKIP_DIRS`. `.git` and `.trash` never get this far whatever the toggle says, because
/// neither the index nor that listing ever holds them (`walk::ALWAYS_SKIP_DIRS`).
pub fn hidden(row_kind: FileKind, rel: &str, indexed: bool, show_hidden: bool) -> bool {
    indexed
        && (row_kind == FileKind::Conflict
            || (!show_hidden && dot_named(rel))
            || rel.rsplit('/').next().is_some_and(is_sync_conflict))
}

/// A dot-named path, or one inside a dot-named folder: what a file manager calls hidden.
pub fn dot_named(rel: &str) -> bool {
    rel.split('/').any(|c| c.starts_with('.'))
}

/// How many listings of each directory ("" is the root) one tree has asked for.
///
/// Two listings of one directory can be on their way at once — the refresh after a reconnect and
/// the file that was just made, or two reindexes in a row — and over a link they need not land in
/// the order they were asked. Only the newest one is spliced in: an older one landing after it
/// would put back the rows it no longer has.
type Asked = Rc<RefCell<HashMap<String, u64>>>;

/// Show Hidden Files, shared by every listing the tree asks for. Read when a listing lands rather
/// than when it is asked for, so one still on its way after a toggle is filtered by the new value.
type ShowHidden = Rc<Cell<bool>>;

/// Run when a listing of the root lands, whether or not it changed the store.
type Landed = Option<Rc<dyn Fn()>>;

/// Bring `store` in step with the direct children of `prefix`.
///
/// The listing is asked for on a worker thread and spliced in when it lands, so the store this
/// returns to is empty for a frame or two. That is what lets a vault on another machine expand a
/// directory without the click waiting for a round trip; on a local vault the index answers in
/// well under a frame and nobody sees the gap. `list_dir` already returns directories first, then
/// names case-insensitively. `landed` runs once the listing is in.
fn fill(
    store: &gio::ListStore,
    vault: &Arc<Vault>,
    asked: &Asked,
    show_hidden: &ShowHidden,
    prefix: &str,
    landed: Landed,
) {
    let ticket = {
        let mut asked = asked.borrow_mut();
        let n = asked.entry(prefix.to_string()).or_default();
        *n += 1;
        *n
    };
    let (store, vault, asked, show_hidden) = (
        store.clone(),
        vault.clone(),
        asked.clone(),
        show_hidden.clone(),
    );
    let dir = prefix.to_string();
    glib::spawn_future_local(async move {
        let listed = crate::work::off_thread("tree", {
            let dir = dir.clone();
            move || vault.list_dir(&dir)
        })
        .await;
        // A newer listing of this directory was asked for while this one was on its way.
        if asked.borrow().get(&dir) != Some(&ticket) {
            return;
        }
        match listed {
            Some(Ok(rows)) => {
                splice(&store, rows, show_hidden.get());
                if let Some(landed) = landed {
                    landed();
                }
            }
            // Leaving the rows alone beats blanking a directory the index simply could not answer
            // for — or, on a remote vault, one the connection could not reach.
            Some(Err(e)) => tracing::warn!("listing a directory failed: {e:#}"),
            None => {}
        }
    });
}

/// The rows the listing produced, against the ones the store already holds.
fn splice(store: &gio::ListStore, rows: Vec<accent_api::FileRow>, show_hidden: bool) {
    let items: Vec<String> = rows
        .into_iter()
        // `id == 0` is `Vault::list_dir` saying this row came off the disk rather than out of
        // the index.
        .filter(|r| !hidden(r.kind, &r.rel_path, r.id != 0, show_hidden))
        .map(|r| encode(r.kind, &r.rel_path, r.id != 0, r.dependency))
        .collect();
    let Some((at, removed, added)) = changed_span(&current(store), &items) else {
        return;
    };
    let new: Vec<gtk::StringObject> = items[at..at + added]
        .iter()
        .map(|s| gtk::StringObject::new(s))
        .collect();
    // One splice, one `items-changed`. Appending row by row made a 2 400-child directory emit
    // 2 400 signals out through TreeListModel -> SingleSelection -> ListView.
    store.splice(at as u32, removed as u32, &new);
}

/// The encoded value of every row currently in `store`.
fn current(store: &gio::ListStore) -> Vec<String> {
    (0..store.n_items())
        .filter_map(|i| store.item(i).and_downcast::<gtk::StringObject>())
        .map(|s| s.string().to_string())
        .collect()
}

/// The one span `old` and `new` differ in, as (start, rows to remove, rows to insert), or `None`
/// when they are already the same.
///
/// A row's expanded children hang off the *object* in the store, so a blanket splice collapses
/// every expanded directory and jumps the scroll position. Trimming the equal head and tail means
/// a reindex that changed nothing splices nothing, and one added or removed file touches one row.
fn changed_span(old: &[String], new: &[String]) -> Option<(usize, usize, usize)> {
    let head = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    let tail = old[head..]
        .iter()
        .rev()
        .zip(new[head..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    match (old.len() - head - tail, new.len() - head - tail) {
        (0, 0) => None,
        (removed, added) => Some((head, removed, added)),
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
    /// The root's [`Landed`], which tells the empty page the host has answered.
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
        redraw_marks(&self.view, &marked);
    }

    /// What a Shift+click on the row `to` does with `from` as the last row clicked without Shift:
    /// mark every row between them, replacing the marks, or with Ctrl held too (`add`) adding
    /// to them.
    #[cfg(feature = "bench")]
    pub fn mark_range(&self, from: &str, to: &str, add: bool) {
        let mut marked = self.marked.borrow_mut();
        mark_range(&mut marked, &self.model, from, to, add);
        redraw_marks(&self.view, &marked);
    }

    /// Forget every mark, and say whether there was one to forget — which is what lets Escape
    /// fall through to the rest of the window when the tree has nothing marked.
    pub fn clear_marks(&self) -> bool {
        let mut marked = self.marked.borrow_mut();
        if marked.is_empty() {
            return false;
        }
        marked.clear();
        redraw_marks(&self.view, &marked);
        true
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
fn redraw_marks(view: &gtk::ListView, marked: &Marks) {
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
            rel.is_some_and(|rel| is_marked(marked, &rel)),
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

/// The folders the index does not walk whose listings the tree keeps, which the vault watches for
/// it.
type Watches = Rc<RefCell<HashSet<String>>>;

/// Keep `dir`'s listing in step with the disk for as long as the tree holds it.
///
/// The index never walks a gitignored folder, so nothing in the vault's own watch set reports a
/// file written into one: a build filling the folder whose row is open, or a training run writing
/// into an `mlruns/`, showed nothing new until the row was collapsed and opened again. One watch
/// per such folder answers for exactly the listings the tree keeps — a child model is built and
/// cached the first time its row is bound and lives as long as the window, so the watch has the
/// same lifetime as the rows it keeps honest, and collapsing one throws neither away. The listing
/// it stands beside is already paid for, which is what makes this the cheap answer rather than a
/// budget of its own.
///
/// The vault's own watcher keeps it, one level deep, where the files are: on the host for a remote
/// vault, which is the one place they can be watched from. Its news is
/// [`Event::UnindexedChanged`](accent_api::Event::UnindexedChanged), which lists the folder again,
/// once per debounced burst rather than per file.
///
/// The dependency trees get none: a `node_modules` is opened to look at, and 40 000 files is the
/// one tree this must not start watching.
fn watch_unindexed(watches: &Watches, vault: &Arc<Vault>, dir: &str) {
    if !watches.borrow_mut().insert(dir.to_string()) {
        return;
    }
    // A round trip on a remote vault, from a row being bound: sent from a worker and not waited
    // for. One that cannot be sent yet is kept, and asked of the host once it answers.
    let (vault, dirs) = (vault.clone(), vec![dir.to_string()]);
    gio::spawn_blocking(move || {
        if let Err(e) = vault.watch_unindexed(&dirs) {
            tracing::debug!("watching an unindexed folder: {e:#}");
        }
    });
}

fn children_model(
    vault: &Arc<Vault>,
    cache: &Rc<RefCell<HashMap<String, gio::ListStore>>>,
    asked: &Asked,
    show_hidden: &ShowHidden,
    rel: &str,
) -> gio::ListStore {
    // Cloned out so the cache borrow cannot still be live during `fill`.
    let hit = cache.borrow().get(rel).cloned();
    if let Some(store) = hit {
        return store;
    }
    let t0 = Instant::now();
    let store = gio::ListStore::new::<gtk::StringObject>();
    fill(&store, vault, asked, show_hidden, rel, None);
    cache.borrow_mut().insert(rel.to_string(), store.clone());
    tracing::debug!(
        dir = rel,
        rows = store.n_items(),
        ms = t0.elapsed().as_secs_f64() * 1e3,
        "expanded directory"
    );
    store
}

/// What a tree-to-tree move travels as, beside the plain string a pane opens.
///
/// ponytail: a `GtkStringObject` rather than the `application/x-accent-path` mime the design note
/// named, because `GtkDropTarget` matches on GType and never on a mime type — a mime would mean
/// `GtkDropTargetAsync` and reading the drop's stream by hand. What the decision asks for is a
/// type the panes do not take, and their target takes `AdwTabPage` and `String` only, so a folder
/// offering this and nothing else cannot be dropped into a pane at all.
fn move_content(rel: &str) -> gdk::ContentProvider {
    gdk::ContentProvider::for_value(&gtk::StringObject::new(rel).to_value())
}

/// What a drop is handed to: each path a drag carried, and the path it goes to.
type Move = Rc<dyn Fn(Vec<(String, String)>)>;

/// The paths a tree drag is carrying: one row, or the marked set a marked row carries along
/// (a `GtkStringList`, which no pane takes either — there is no one note in it to open).
fn dragged(value: &glib::Value) -> Vec<String> {
    if let Ok(one) = value.get::<gtk::StringObject>() {
        return vec![one.string().to_string()];
    }
    value.get::<gtk::StringList>().map_or_else(
        |_| Vec::new(),
        |list| {
            (0..list.n_items())
                .filter_map(|i| list.string(i))
                .map(|rel| rel.to_string())
                .collect()
        },
    )
}

/// What dropping `paths` into `dir` moves: each of them that has somewhere to go there.
fn moves_into(paths: &[String], dir: &str) -> Vec<(String, String)> {
    paths
        .iter()
        .filter_map(|from| Some((from.clone(), crate::fileops::move_dest(from, dir)?)))
        .collect()
}

/// How long a drag rests over a shut folder before it opens. Long enough that crossing one on the
/// way somewhere else never opens it, short enough to read as part of the drag — the second
/// GTK's own file chooser and Nautilus both wait.
const SPRING_OPEN: Duration = Duration::from_millis(800);

/// A timer waiting to open the folder a drag is resting on.
type Spring = Rc<crate::widgets::Debounce>;

/// Open the shut folder a drag has come to rest on, so a file can be dropped into something that
/// was not on screen when the drag began. `row` is the row under the pointer, `None` when the drag
/// has left the target or has been dropped, which disarms the timer.
///
/// Per drop target, which is per row: a drag crossing three folders arms and disarms three timers,
/// one at a time. Nothing closes the folder again — a drag that opened one and went elsewhere
/// leaves the tree as the reader would have left it by clicking the chevron.
fn spring_open(timer: &Spring, row: Option<gtk::TreeListRow>) {
    timer.cancel();
    let Some(row) = row.filter(|row| row.is_expandable() && !row.is_expanded()) else {
        return;
    };
    timer.call(move || row.set_expanded(true));
}

/// The `GtkTreeListRow` a drop target on a row expander is over, and `None` for a target that is
/// not on one — the vault row above the tree, and the list's own blank area.
fn target_row(target: &gtk::DropTarget) -> Option<gtk::TreeListRow> {
    target
        .widget()?
        .downcast::<gtk::TreeExpander>()
        .ok()?
        .list_row()
}

/// A drop target that moves the dragged files into the directory `dir` answers with for the
/// pointer position — `Some("")` being the vault root — and refuses the drop where it answers
/// `None`, or where none of them has anywhere to go there.
///
/// The refusal happens while the pointer is still moving rather than after the drop, so a row
/// that cannot take what is over it never lights up: a folder onto itself, into what is under it,
/// or into the folder it is already in are simply not targets. GTK's own `:drop(active)` outline
/// on the row is then the whole of the feedback, and there is nothing else to draw.
fn move_target(
    on_move: &Move,
    dir: impl Fn(&gtk::DropTarget, f64, f64) -> Option<String> + 'static,
) -> gtk::DropTarget {
    let target = gtk::DropTarget::new(glib::Type::INVALID, gdk::DragAction::MOVE);
    target.set_types(&[
        gtk::StringObject::static_type(),
        gtk::StringList::static_type(),
    ]);
    // The dragged paths have to be readable while the drag is still in flight, or the decision
    // could only be taken once the drop had already happened.
    target.set_preload(true);
    let dir = Rc::new(dir);
    let planned = {
        let dir = dir.clone();
        move |target: &gtk::DropTarget, x, y| {
            let moves = moves_into(&dragged(&target.value()?), &dir(target, x, y)?);
            (!moves.is_empty()).then_some(moves)
        }
    };
    let planned = Rc::new(planned);
    let spring = Spring::new(crate::widgets::Debounce::new(SPRING_OPEN));
    // Both, because `enter` is what decides whether the row highlights at all and `motion` is
    // what corrects it once the preloaded value has arrived.
    let answer = {
        let (planned, spring) = (planned.clone(), spring.clone());
        move |target: &gtk::DropTarget, x, y| match planned(target, x, y) {
            Some(_) => {
                spring_open(&spring, target_row(target));
                gdk::DragAction::MOVE
            }
            // A folder that cannot take what is over it has no reason to open either.
            None => {
                spring_open(&spring, None);
                gdk::DragAction::empty()
            }
        }
    };
    target.connect_enter({
        let answer = answer.clone();
        move |target, x, y| answer(target, x, y)
    });
    target.connect_motion(answer);
    target.connect_leave({
        let spring = spring.clone();
        move |_| spring_open(&spring, None)
    });
    let on_move = on_move.clone();
    target.connect_drop(move |target, value, x, y| {
        spring_open(&spring, None);
        // The value is handed over here rather than read back off the target, which is the one
        // place it is certain to have arrived.
        let Some(dir) = dir(target, x, y) else {
            return false;
        };
        let moves = moves_into(&dragged(value), &dir);
        if moves.is_empty() {
            return false;
        }
        on_move(moves);
        true
    });
    target
}

/// What a drop of files from outside accent is handed to: the files, the vault-relative folder
/// they go into ("" being the root) and whether the drag was a move, which takes the originals
/// away.
type Import = Rc<dyn Fn(Vec<PathBuf>, String, bool)>;

/// A drop target for files dragged in from another application — GNOME Files, a browser's
/// downloads — onto the same three zones a tree-to-tree move has: a folder row, a file row (its
/// folder) and the blank area or the vault row (the root). `dir` answers with the folder for a
/// pointer position, exactly as [`move_target`]'s does.
///
/// `GdkFileList` is the type rather than `text/uri-list`: GDK deserialises the one into the other,
/// so this takes what every file manager offers without reading a stream by hand. A drop that
/// offers **only** move is moved — that is Shift held in the file manager — and anything else is
/// copied, which is what a plain drag between applications means.
fn import_target(
    on_import: &Import,
    dir: impl Fn(&gtk::DropTarget, f64, f64) -> Option<String> + 'static,
) -> gtk::DropTarget {
    let target = gtk::DropTarget::new(
        gdk::FileList::static_type(),
        gdk::DragAction::COPY | gdk::DragAction::MOVE,
    );
    let dir = Rc::new(dir);
    let spring = Spring::new(crate::widgets::Debounce::new(SPRING_OPEN));
    let answer = {
        let (dir, spring) = (dir.clone(), spring.clone());
        move |target: &gtk::DropTarget, x, y| match dir(target, x, y) {
            Some(_) => {
                spring_open(&spring, target_row(target));
                wanted(target)
            }
            None => {
                spring_open(&spring, None);
                gdk::DragAction::empty()
            }
        }
    };
    target.connect_enter({
        let answer = answer.clone();
        move |target, x, y| answer(target, x, y)
    });
    target.connect_motion(answer);
    target.connect_leave({
        let spring = spring.clone();
        move |_| spring_open(&spring, None)
    });
    let on_import = on_import.clone();
    target.connect_drop(move |target, value, x, y| {
        spring_open(&spring, None);
        let (Some(into), Some(files)) = (dir(target, x, y), dropped_paths(value)) else {
            return false;
        };
        on_import(files, into, wanted(target) == gdk::DragAction::MOVE);
        true
    });
    target
}

/// The files a drop from another application carries, as paths on this machine, or `None` where
/// it named none that way — an `ftp://` or a `trash://` URI has nothing here to copy from.
///
/// Public because it is the half of a cross-application drop a drill can drive: Xvfb carries a
/// drag inside one process and not between two, so `ACCENT_BENCH_DROP` builds the `GdkFileList`
/// itself and takes it from here.
pub fn dropped_paths(value: &glib::Value) -> Option<Vec<PathBuf>> {
    let files: Vec<PathBuf> = value
        .get::<gdk::FileList>()
        .ok()?
        .files()
        .iter()
        .filter_map(|f| f.path())
        .collect();
    (!files.is_empty()).then_some(files)
}

/// What a drop from another application is asking for: a move only where move is the one action
/// it offers, which is how a file manager reports Shift being held.
fn wanted(target: &gtk::DropTarget) -> gdk::DragAction {
    let offered = target
        .current_drop()
        .map(|drop| drop.actions())
        .unwrap_or(gdk::DragAction::COPY);
    match offered == gdk::DragAction::MOVE {
        true => gdk::DragAction::MOVE,
        false => gdk::DragAction::COPY,
    }
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
    let bind_marked = marked.clone();
    let drag_marked = marked.clone();
    let row_moves = moves.clone();
    let row_imports = imports.clone();
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
            // and the expander is the one widget here that spans the whole of it.
            set_class(
                expander,
                MARKED,
                is_marked(&bind_marked.borrow(), &item.rel),
            );
        },
    );

    let selection = gtk::SingleSelection::new(Some(model.clone()));
    selection.set_autoselect(false);
    selection.set_can_unselect(true);
    let view = gtk::ListView::new(None::<gtk::SingleSelection>, Some(factory));
    crate::widgets::set_model(&view, &selection);
    view.add_css_class("navigation-sidebar");
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
        let (marked, active, cache, model) =
            (marked.clone(), active.clone(), cache.clone(), model.clone());
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
                    // From the anchor while it is on screen, else from the open file's row,
                    // else the row alone; Shift leaves the anchor where it is.
                    let shown = |rel: &String| find_row(&model, rel).is_some();
                    let from = (anchor.borrow().clone().filter(shown))
                        .or_else(|| active.borrow().clone().filter(shown))
                        .unwrap_or_else(|| row.rel.clone());
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
            redraw_marks(&view, &marks);
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
                redraw_marks(view, &marks);
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
    let motion = gtk::EventControllerMotion::new();
    motion.connect_leave({
        let highlight = highlight.clone();
        move |controller| {
            let Some(view) = controller.widget().and_downcast::<gtk::ListView>() else {
                return;
            };
            highlight(&view);
        }
    });
    // The listing lands from a worker thread and expanding a folder inserts rows, so the open
    // file's row often is not there — or not there yet — at the moment the tab changed. Re-applied
    // whenever the model changes, but never while the pointer is in the list: the selection is
    // the hover highlight too, and a reindex must not pull it out from under the row being
    // pointed at.
    model.connect_items_changed({
        let motion = motion.clone();
        move |_, _, _, _| {
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
        active,
        pinned,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn ancestors_lists_the_directories_reveal_has_to_expand() {
        let dirs = |rel| ancestors(rel).collect::<Vec<_>>();
        assert_eq!(dirs("a/b/c.md"), ["a", "a/b"]);
        // A note at the vault root has nothing above it to expand.
        assert_eq!(dirs("c.md"), [] as [&str; 0]);
        assert_eq!(dirs(""), [] as [&str; 0]);
    }

    #[test]
    fn a_row_carries_whether_the_index_holds_it() {
        let row = |kind, rel, indexed, dep| decode_str(&encode(kind, rel, indexed, dep)).unwrap();
        let note = row(FileKind::Markdown, "Notes/A.md", true, false);
        assert_eq!(note.kind, 'f');
        assert_eq!(note.rel, "Notes/A.md");
        assert!(note.indexed);
        assert!(!note.dependency);
        // A row read off the disk keeps its kind — the icon and the expander must not change —
        // and says the index has never heard of it.
        let dep = row(FileKind::Dir, "node_modules", false, true);
        assert_eq!(dep.kind, 'd');
        assert!(dep.is_dir());
        assert_eq!(dep.rel, "node_modules");
        assert!(!dep.indexed);
        assert!(dep.dependency);
        // A gitignored folder is out of the index too, and is still the reader's own.
        let ignored = row(FileKind::Dir, "mlruns", false, false);
        assert!(!ignored.indexed);
        assert!(!ignored.dependency);
    }

    #[test]
    fn show_hidden_decides_the_dot_named_rows_the_index_holds() {
        use FileKind::{Conflict, Dir, Markdown, Other};
        // Shown with the toggle on, left out with it off.
        for rel in [".gitignore", ".obsidian/app.json", "Notes/.draft.md"] {
            assert!(!hidden(Other, rel, true, true), "{rel}");
            assert!(hidden(Other, rel, true, false), "{rel}");
        }
        assert!(!hidden(Markdown, "Notes/a.md", true, false));
        // A dot-named tree the walk refuses is listed off the disk either way, as it was before
        // the toggle existed.
        assert!(!hidden(Dir, ".venv", false, false));
        assert!(!hidden(Other, ".venv/pyvenv.cfg", false, false));
        // A conflict copy never is: resolving one is the conflict banner's business.
        let conflict = "a.sync-conflict-20260903-101500-ABCDEFG.md";
        assert!(hidden(Conflict, conflict, true, true));
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
    fn changed_span_reports_nothing_when_the_listing_is_unchanged() {
        let same = rows(&["dNotes", "ma.md", "mb.md"]);
        assert_eq!(changed_span(&same, &same), None);
        assert_eq!(changed_span(&[], &[]), None);
    }

    #[test]
    fn changed_span_covers_only_the_rows_that_moved() {
        let old = rows(&["dNotes", "ma.md", "mc.md"]);
        // Inserted in the middle: one row added, none removed.
        assert_eq!(
            changed_span(&old, &rows(&["dNotes", "ma.md", "mb.md", "mc.md"])),
            Some((2, 0, 1))
        );
        // Removed from the middle.
        assert_eq!(
            changed_span(&old, &rows(&["dNotes", "mc.md"])),
            Some((1, 1, 0))
        );
        // Renamed in place.
        assert_eq!(
            changed_span(&old, &rows(&["dNotes", "ma.md", "mz.md"])),
            Some((2, 1, 1))
        );
        // Appended at the end, so the head is everything that was already there.
        assert_eq!(
            changed_span(&old, &rows(&["dNotes", "ma.md", "mc.md", "md.md"])),
            Some((3, 0, 1))
        );
    }

    #[test]
    fn changed_span_handles_an_empty_side() {
        let listing = rows(&["dNotes", "ma.md"]);
        assert_eq!(changed_span(&[], &listing), Some((0, 0, 2)));
        assert_eq!(changed_span(&listing, &[]), Some((0, 2, 0)));
    }

    #[test]
    fn changed_span_keeps_a_repeated_row_from_widening_the_span() {
        // Equal head and tail must not overlap, or the span would remove more than there is.
        let old = rows(&["ma.md", "ma.md"]);
        assert_eq!(changed_span(&old, &rows(&["ma.md"])), Some((1, 1, 0)));
        assert_eq!(changed_span(&rows(&["ma.md"]), &old), Some((1, 0, 1)));
    }
}
