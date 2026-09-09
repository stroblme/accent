//! Lazy vault file tree: `gtk::ListView` over a `gtk::TreeListModel` whose children come from
//! `Vault::list_dir(prefix)`, one directory level per expansion.

use crate::widgets::{scroller, set_class};
use accent_api::Vault;
use accent_core::fs::is_sync_conflict;
use accent_core::markdown::is_image;
use accent_core::path::basename;
use accent_core::walk::FileKind;
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

/// One row of the tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// `d`, `m`, `p`, `i` or `o` — see [`encode`].
    pub kind: char,
    /// Vault-relative path.
    pub rel: String,
    /// Whether the index holds this row.
    ///
    /// False inside the trees the walk refuses — `node_modules`, a `.venv`, a cargo `target/`.
    /// Those are listed because a file tree that leaves a folder out is one nobody can trust, but
    /// they are read off the disk and nothing else in the app knows they are there: they are not
    /// searched, not watched and not stored. So they list and open, and nothing may be created,
    /// renamed, moved into or dragged out of them — a change the index never hears of would leave
    /// the tree and the index disagreeing until the next rescan.
    pub indexed: bool,
}

impl Row {
    pub fn is_dir(&self) -> bool {
        self.kind == 'd'
    }
}

/// ponytail: rows are `gtk::StringObject`s holding `"<kind char><rel_path>"` instead of a custom
/// GObject with typed properties. Saves ~40 lines of subclass boilerplate; if the tree ever needs
/// more per-row state (git status, unsaved marker) define a real `FileItem` GObject then.
///
/// The kind letter is upper case for a row the index does not hold, which is the one extra bit
/// [`Row::indexed`] needs and costs no extra byte.
fn encode(kind: FileKind, rel: &str, indexed: bool) -> String {
    let c = match kind {
        FileKind::Dir => 'd',
        FileKind::Markdown => 'm',
        FileKind::Pdf => 'p',
        // The walk has no image kind, but the tree needs one: an image gets its own icon and
        // opens in a picture tab rather than being turned away as an unknown file.
        _ if is_image(rel) => 'i',
        _ => 'o',
    };
    let c = match indexed {
        true => c,
        false => c.to_ascii_uppercase(),
    };
    format!("{c}{rel}")
}

pub fn decode(item: &glib::Object) -> Option<Row> {
    decode_str(&item.downcast_ref::<gtk::StringObject>()?.string())
}

/// The pure half of [`decode`], so the encoding is a test rather than a running window.
fn decode_str(s: &str) -> Option<Row> {
    let mut cs = s.chars();
    let kind = cs.next()?;
    Some(Row {
        kind: kind.to_ascii_lowercase(),
        rel: cs.as_str().to_string(),
        indexed: kind.is_ascii_lowercase(),
    })
}

/// `.obsidian`, `.stfolder`, `.git`, … and Syncthing conflicts never belong in the tree.
///
/// Only asked of the rows the index holds: a row read off the disk is one of the skipped trees,
/// which the walk has already filtered (`.git` and `.trash` are out of reach there too), and
/// hiding the dot-named ones would hide `.venv` and four of the six names in `SKIP_DIRS`.
pub fn hidden(row_kind: FileKind, rel: &str) -> bool {
    row_kind == FileKind::Conflict
        || rel.split('/').any(|c| c.starts_with('.'))
        || rel.rsplit('/').next().is_some_and(is_sync_conflict)
}

/// Bring `store` in step with the direct children of `prefix`.
///
/// The listing is asked for on a worker thread and spliced in when it lands, so the store this
/// returns to is empty for a frame or two. That is what lets a vault on another machine expand a
/// directory without the click waiting for a round trip; on a local vault the index answers in
/// well under a frame and nobody sees the gap. `list_dir` already returns directories first, then
/// names case-insensitively.
pub fn fill(store: &gio::ListStore, vault: &Arc<Vault>, prefix: &str) {
    let (store, vault, dir) = (store.clone(), vault.clone(), prefix.to_string());
    glib::spawn_future_local(async move {
        let listed = gio::spawn_blocking(move || vault.list_dir(&dir)).await;
        match listed {
            Ok(Ok(rows)) => splice(&store, rows),
            // Leaving the rows alone beats blanking a directory the index simply could not answer
            // for — or, on a remote vault, one the connection could not reach.
            Ok(Err(e)) => tracing::warn!("listing a directory failed: {e:#}"),
            Err(_) => tracing::warn!("the tree worker panicked"),
        }
    });
}

/// The rows the listing produced, against the ones the store already holds.
fn splice(store: &gio::ListStore, rows: Vec<accent_api::FileRow>) {
    let items: Vec<String> = rows
        .into_iter()
        // `id == 0` is `Vault::list_dir` saying this row came off the disk rather than out of
        // the index.
        .filter(|r| r.id == 0 || !hidden(r.kind, &r.rel_path))
        .map(|r| encode(r.kind, &r.rel_path, r.id != 0))
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

fn icon_name(kind: char) -> &'static str {
    match kind {
        'd' => "folder-symbolic",
        'm' => "text-x-generic-symbolic",
        'p' => "x-office-document-symbolic",
        'i' => "image-x-generic-symbolic",
        _ => "application-x-addon-symbolic",
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
    /// What git ignores, shared with the row factory so binding a row is still two setters and a
    /// set lookup rather than a question for the index.
    ignored: Rc<RefCell<HashSet<String>>>,
    /// The open file, which the selection follows. Shared with the pointer-leave handler: the
    /// list selects rows on hover (see `build`), so the selection has to be put back whenever
    /// the pointer goes away again.
    active: Rc<RefCell<Option<String>>>,
}

impl Tree {
    /// Tell the tree what git ignores, and redraw the rows on screen.
    ///
    /// The factory is reset rather than the model spliced: a splice recreates every
    /// `GtkTreeListRow` and collapses the directories the reader had opened, while re-binding
    /// only touches the handful of rows actually visible.
    pub fn set_ignored(&self, ignored: HashSet<String>) {
        if *self.ignored.borrow() == ignored {
            return;
        }
        *self.ignored.borrow_mut() = ignored;
        let factory = self.view.factory();
        self.view.set_factory(None::<&gtk::ListItemFactory>);
        self.view.set_factory(factory.as_ref());
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
                fill(&store, &self.vault, dir);
                continue;
            }
            // A directory that is gone keeps no model: a same-named one created later must be
            // listed afresh instead of re-expanding into the files this one used to hold. Asked
            // of the index rather than of the disk, which on a remote vault is not here at all —
            // and asked on a worker thread, because on that vault it is one round trip per
            // invalidated directory and a reindex invalidates a handful at a time.
            let (vault, cache, dir) = (self.vault.clone(), self.cache.clone(), dir.clone());
            glib::spawn_future_local(async move {
                let there = gio::spawn_blocking({
                    let (vault, dir) = (vault.clone(), dir.clone());
                    move || vault.exists(&dir)
                })
                .await;
                match there {
                    Ok(true) => fill(&store, &vault, &dir),
                    Ok(false) => {
                        cache.borrow_mut().remove(&dir);
                    }
                    Err(_) => tracing::warn!("the tree worker panicked"),
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
        select(&self.view, rel);
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

/// Every directory above `rel`, outermost first: `a/b/c.md` yields `a` then `a/b`.
/// Whether git ignores `rel`. The set holds paths as git reports them, so a wholly ignored
/// directory is one entry with a trailing slash and everything under it is ignored with it;
/// a partly ignored directory has its files listed one by one instead.
pub fn is_ignored(set: &HashSet<String>, rel: &str) -> bool {
    if set.is_empty() {
        return false;
    }
    set.contains(rel)
        || set.contains(&format!("{rel}/"))
        || ancestors(rel).any(|dir| set.contains(&format!("{dir}/")))
}

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

fn children_model(
    vault: &Arc<Vault>,
    cache: &Rc<RefCell<HashMap<String, gio::ListStore>>>,
    rel: &str,
) -> gio::ListStore {
    // Cloned out so the cache borrow cannot still be live during `fill`.
    let hit = cache.borrow().get(rel).cloned();
    if let Some(store) = hit {
        return store;
    }
    let t0 = Instant::now();
    let store = gio::ListStore::new::<gtk::StringObject>();
    fill(&store, vault, rel);
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

/// What a dropped row is handed to: the path it came from, and the path it goes to.
type Move = Rc<dyn Fn(&str, &str)>;

/// The path a tree drag is carrying, if it is one.
fn dragged(value: &glib::Value) -> Option<String> {
    Some(value.get::<gtk::StringObject>().ok()?.string().to_string())
}

/// A drop target that moves the dragged file into the directory `dir` answers with for the
/// pointer position — `Some("")` being the vault root — and refuses the drop where it answers
/// `None`.
///
/// The refusal happens while the pointer is still moving rather than after the drop, so a row
/// that cannot take what is over it never lights up: a folder onto itself, into what is under it,
/// or into the folder it is already in are simply not targets. GTK's own `:drop(active)` outline
/// on the row is then the whole of the feedback, and there is nothing else to draw.
fn move_target(
    on_move: &Move,
    dir: impl Fn(&gtk::DropTarget, f64, f64) -> Option<String> + 'static,
) -> gtk::DropTarget {
    let target = gtk::DropTarget::new(gtk::StringObject::static_type(), gdk::DragAction::MOVE);
    // The dragged path has to be readable while the drag is still in flight, or the decision
    // could only be taken once the drop had already happened.
    target.set_preload(true);
    let dir = Rc::new(dir);
    let planned = {
        let dir = dir.clone();
        move |target: &gtk::DropTarget, x, y| {
            let from = target.value().as_ref().and_then(dragged)?;
            let to = crate::fileops::move_dest(&from, &dir(target, x, y)?)?;
            Some((from, to))
        }
    };
    let planned = Rc::new(planned);
    // Both, because `enter` is what decides whether the row highlights at all and `motion` is
    // what corrects it once the preloaded value has arrived.
    let answer = {
        let planned = planned.clone();
        move |target: &gtk::DropTarget, x, y| match planned(target, x, y) {
            Some(_) => gdk::DragAction::MOVE,
            None => gdk::DragAction::empty(),
        }
    };
    target.connect_enter({
        let answer = answer.clone();
        move |target, x, y| answer(target, x, y)
    });
    target.connect_motion(answer);
    let on_move = on_move.clone();
    target.connect_drop(move |target, value, x, y| {
        // The value is handed over here rather than read back off the target, which is the one
        // place it is certain to have arrived.
        let (Some(from), Some(dir)) = (dragged(value), dir(target, x, y)) else {
            return false;
        };
        match crate::fileops::move_dest(&from, &dir) {
            Some(to) => {
                on_move(&from, &to);
                true
            }
            None => false,
        }
    });
    target
}

/// The row above the tree naming the vault, and the drop zone for "put it in the vault root".
///
/// The blank area below the last row is the other one, and a tree scrolled deep in a large vault
/// has none, which is what this is for: it is always on screen. A label rather than a list row,
/// because it stands for what the whole listing is of — there is nothing to open or expand.
fn root_row(label: &str) -> gtk::Box {
    let row = gtk::Box::builder()
        .spacing(6)
        .margin_start(12)
        .margin_end(12)
        .margin_top(6)
        .margin_bottom(6)
        .tooltip_text(label)
        .build();
    row.append(&gtk::Image::from_icon_name(icon_name('d')));
    row.append(
        &gtk::Label::builder()
            .label(basename(label))
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .css_classes(["heading"])
            .build(),
    );
    row
}

/// Build the tree. `on_activate` is called with the rel_path of an activated non-directory row,
/// `on_drag` with `true` while a row is being dragged out of the tree and `false` when it is over,
/// so the panes can put their drop zones up for the duration, and `on_move` with the path a row
/// was dragged from and the path it was dropped onto.
pub fn build(
    vault: Arc<Vault>,
    root: &gio::ListStore,
    on_activate: impl Fn(char, &str) + 'static,
    on_drag: impl Fn(bool) + 'static,
    on_move: impl Fn(&str, &str) + 'static,
) -> Tree {
    let cache: Rc<RefCell<HashMap<String, gio::ListStore>>> = Rc::new(RefCell::new(HashMap::new()));
    let ignored: Rc<RefCell<HashSet<String>>> = Rc::new(RefCell::new(HashSet::new()));
    let model = gtk::TreeListModel::new(root.clone(), false, false, {
        let (vault, cache) = (vault.clone(), cache.clone());
        move |obj| {
            let row = decode(obj)?;
            row.is_dir()
                .then(|| children_model(&vault, &cache, &row.rel).upcast())
        }
    });

    // Abbreviated once rather than per row: neither the vault root nor `$HOME` moves while the
    // window is open, and the label is only ever a prefix of a tooltip.
    let root_label = crate::fileops::display_path(&vault.root(), "");
    // Shared, because `setup` runs once per recycled row widget and both ends of every drag
    // report through the same closure.
    let dragging: Rc<dyn Fn(bool)> = Rc::new(on_drag);
    let moves: Move = Rc::new(on_move);
    let vault_row = root_row(&root_label);
    vault_row.add_controller(move_target(&moves, |_, _, _| Some(String::new())));
    let factory = gtk::SignalListItemFactory::new();
    let bind_ignored = ignored.clone();
    let row_moves = moves.clone();
    factory.connect_setup(move |_, item| {
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
        source.connect_prepare(|source, _, _| {
            let expander = source.widget()?.downcast::<gtk::TreeExpander>().ok()?;
            let row = expander.list_row()?.item().as_ref().and_then(decode)?;
            // Nothing is dragged out of a tree the index does not hold: the move would happen on
            // disk and the index would go on listing the file where it used to be.
            if !row.indexed {
                return None;
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
        expander.add_controller(move_target(&row_moves, |target, _, _| {
            let expander = target.widget()?.downcast::<gtk::TreeExpander>().ok()?;
            let row = expander.list_row()?.item().as_ref().and_then(decode)?;
            // And nothing is dropped into one either, for the same reason. The row simply never
            // lights up.
            row.indexed
                .then(|| crate::fileops::row_dir(Some((&row.rel, row.is_dir()))).to_string())
        }));
        item.downcast_ref::<gtk::ListItem>()
            .expect("list item")
            .set_child(Some(&expander));
    });
    // Widget lookups and two setters only: no database access on the bind path.
    factory.connect_bind(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().expect("list item");
        let Some(expander) = item.child().and_downcast::<gtk::TreeExpander>() else {
            return;
        };
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
        icon.set_icon_name(Some(icon_name(item.kind)));
        let label = icon
            .next_sibling()
            .and_downcast::<gtk::Label>()
            .expect("label");
        label.set_text(basename(&item.rel));
        // Both branches, always: row widgets are recycled, so a row that stops being ignored has
        // to have the class taken off it again. A row the index does not hold is dimmed by the
        // same rule and for the same reason the ignored ones are: search does not reach it.
        let dim = !item.indexed || is_ignored(&bind_ignored.borrow(), &item.rel);
        for widget in [icon.upcast_ref::<gtk::Widget>(), label.upcast_ref()] {
            set_class(widget, "dim-label", dim);
        }
    });

    let selection = gtk::SingleSelection::new(Some(model.clone()));
    selection.set_autoselect(false);
    selection.set_can_unselect(true);
    let view = gtk::ListView::new(Some(selection), Some(factory));
    view.add_css_class("navigation-sidebar");
    // One click opens, as GNOME's own sidebars do. A folder still toggles rather than opening,
    // so a click never costs anything you did not ask for.
    view.set_single_click_activate(true);
    view.connect_activate(move |view, pos| {
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
    let active = Rc::new(RefCell::new(None::<String>));
    let motion = gtk::EventControllerMotion::new();
    motion.connect_leave({
        let active = active.clone();
        move |controller| {
            let Some(view) = controller.widget().and_downcast::<gtk::ListView>() else {
                return;
            };
            select(&view, active.borrow().as_deref());
        }
    });
    // The listing lands from a worker thread and expanding a folder inserts rows, so the open
    // file's row often is not there — or not there yet — at the moment the tab changed. Re-applied
    // whenever the model changes, but never while the pointer is in the list: the selection is
    // the hover highlight too, and a reindex must not pull it out from under the row being
    // pointed at.
    model.connect_items_changed({
        let (active, motion) = (active.clone(), motion.clone());
        move |_, _, _, _| {
            if motion.contains_pointer() {
                return;
            }
            let Some(view) = motion.widget().and_downcast::<gtk::ListView>() else {
                return;
            };
            select(&view, active.borrow().as_deref());
        }
    });
    view.add_controller(motion);
    // The blank area below the last row is the vault root, the same place a right-click there
    // creates in. A drop that landed on a row is that row's own business — its target has already
    // accepted or refused it — so this one has to answer for the blank area alone, or a refusal
    // bubbling up out of a row would turn into a move to the root.
    view.add_controller(move_target(&moves, |target, x, y| {
        let view = target.widget()?.downcast::<gtk::ListView>().ok()?;
        row_at(&view, x, y).is_none().then(String::new)
    }));

    let scroller = scroller(&view);
    // ponytail: a plain `GtkBox` around the scroller, purely so the context menu has a
    // layout-managed widget to hang off. GTK re-presents a popover from its parent's
    // `allocate_native_children`, which only runs for widgets that use a layout manager;
    // `GtkListView` has a custom `size_allocate` and never re-presents its popover children, so a
    // menu parented to the list is frozen at its first-frame size and `GtkPopoverMenu`'s inner
    // scrolled window turns everything that grows afterwards into a scrollbar. If a scrollbar
    // ever comes back, the next dial is setting that inner scrolled window's policies to Never.
    let host = gtk::Box::new(gtk::Orientation::Vertical, 0);
    host.append(&vault_row);
    host.append(&scroller);
    Tree {
        host,
        view,
        model,
        vault,
        root: root.clone(),
        cache,
        ignored,
        active,
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
        let row = |kind, rel, indexed| decode_str(&encode(kind, rel, indexed)).unwrap();
        let note = row(FileKind::Markdown, "Notes/A.md", true);
        assert_eq!(note.kind, 'm');
        assert_eq!(note.rel, "Notes/A.md");
        assert!(note.indexed);
        // A row read off the disk keeps its kind — the icon and the expander must not change —
        // and says the index has never heard of it.
        let dep = row(FileKind::Dir, "node_modules", false);
        assert_eq!(dep.kind, 'd');
        assert!(dep.is_dir());
        assert_eq!(dep.rel, "node_modules");
        assert!(!dep.indexed);
    }

    #[test]
    fn is_ignored_covers_a_file_a_directory_and_what_is_under_it() {
        let set: HashSet<String> = ["build/".to_string(), "notes/a.log".to_string()]
            .into_iter()
            .collect();
        assert!(is_ignored(&set, "notes/a.log"));
        assert!(is_ignored(&set, "build"));
        assert!(is_ignored(&set, "build/deep/thing.o"));
        assert!(!is_ignored(&set, "notes/b.log"));
        // A directory whose name merely starts the same is a different directory.
        assert!(!is_ignored(&set, "builder/x"));
        assert!(!is_ignored(&HashSet::new(), "build/x"));
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
