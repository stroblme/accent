//! Lazy vault file tree: `gtk::ListView` over a `gtk::TreeListModel` whose children come from
//! `Vault::list_dir(prefix)`, one directory level per expansion.

use accent_api::Vault;
use accent_core::fs::is_sync_conflict;
use accent_core::markdown::is_image;
use accent_core::walk::FileKind;
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

/// ponytail: rows are `gtk::StringObject`s holding `"<kind char><rel_path>"` instead of a custom
/// GObject with typed properties. Saves ~40 lines of subclass boilerplate; if the tree ever needs
/// more per-row state (git status, unsaved marker) define a real `FileItem` GObject then.
fn encode(kind: FileKind, rel: &str) -> String {
    let c = match kind {
        FileKind::Dir => 'd',
        FileKind::Markdown => 'm',
        FileKind::Pdf => 'p',
        // The walk has no image kind, but the tree needs one: an image gets its own icon and
        // opens in a picture tab rather than being turned away as an unknown file.
        _ if is_image(rel) => 'i',
        _ => 'o',
    };
    format!("{c}{rel}")
}

pub fn decode(item: &glib::Object) -> Option<(char, String)> {
    let s = item.downcast_ref::<gtk::StringObject>()?.string();
    let mut cs = s.chars();
    let kind = cs.next()?;
    Some((kind, cs.as_str().to_string()))
}

/// `.obsidian`, `.stfolder`, `.git`, … and Syncthing conflicts never belong in the tree.
pub fn hidden(row_kind: FileKind, rel: &str) -> bool {
    row_kind == FileKind::Conflict
        || rel.split('/').any(|c| c.starts_with('.'))
        || rel.rsplit('/').next().is_some_and(is_sync_conflict)
}

/// Bring `store` in step with the direct children of `prefix`. `list_dir` already returns
/// directories first, then names case-insensitively.
pub fn fill(store: &gio::ListStore, vault: &Arc<Vault>, prefix: &str) {
    let rows = match vault.list_dir(prefix) {
        Ok(rows) => rows,
        // Leaving the rows alone beats blanking a directory the index simply could not answer for.
        Err(e) => return tracing::warn!(dir = prefix, "listing the directory failed: {e:#}"),
    };
    let items: Vec<String> = rows
        .into_iter()
        .filter(|r| !hidden(r.kind, &r.rel_path))
        .map(|r| encode(r.kind, &r.rel_path))
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
}

impl Tree {
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
            // A directory that is gone keeps no model: a same-named one created later must be
            // listed afresh instead of re-expanding into the files this one used to hold.
            if !dir.is_empty() && !self.vault.root().join(dir).is_dir() {
                self.cache.borrow_mut().remove(dir);
                continue;
            }
            fill(&store, &self.vault, dir);
        }
    }

    /// The selected row, as (kind char, rel path).
    pub fn selected(&self) -> Option<(char, String)> {
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
    pub fn row_at(&self, x: f64, y: f64) -> Option<(char, String)> {
        let mut widget = self.view.pick(x, y, gtk::PickFlags::DEFAULT)?;
        // `pick` lands on the label or the icon; the row identity hangs off the expander above it.
        let expander = loop {
            match widget.downcast::<gtk::TreeExpander>() {
                Ok(expander) => break expander,
                Err(w) => widget = w.parent()?,
            }
        };
        expander.list_row()?.item().as_ref().and_then(decode)
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
fn ancestors(rel: &str) -> impl Iterator<Item = &str> {
    rel.match_indices('/').map(|(at, _)| &rel[..at])
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
        let (_, r) = row.item().as_ref().and_then(decode)?;
        (r == rel).then_some(row)
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

/// Build the tree. `on_activate` is called with the rel_path of an activated non-directory row,
/// `on_drag` with `true` while a row is being dragged out of the tree and `false` when it is over,
/// so the panes can put their drop zones up for the duration.
pub fn build(
    vault: Arc<Vault>,
    root: &gio::ListStore,
    on_activate: impl Fn(char, &str) + 'static,
    on_drag: impl Fn(bool) + 'static,
) -> Tree {
    let cache: Rc<RefCell<HashMap<String, gio::ListStore>>> = Rc::new(RefCell::new(HashMap::new()));
    let model = gtk::TreeListModel::new(root.clone(), false, false, {
        let (vault, cache) = (vault.clone(), cache.clone());
        move |obj| {
            let (kind, rel) = decode(obj)?;
            (kind == 'd').then(|| children_model(&vault, &cache, &rel).upcast())
        }
    });

    // Abbreviated once rather than per row: neither the vault root nor `$HOME` moves while the
    // window is open, and the label is only ever a prefix of a tooltip.
    let root_label = crate::fileops::display_path(vault.root(), "");
    // Shared, because `setup` runs once per recycled row widget and both ends of every drag
    // report through the same closure.
    let dragging: Rc<dyn Fn(bool)> = Rc::new(on_drag);
    let factory = gtk::SignalListItemFactory::new();
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
            let Some((_, rel)) = row.as_ref().and_then(decode) else {
                return false;
            };
            tooltip.set_text(Some(&format!("{root_label}/{rel}")));
            true
        });
        // A row can be dragged into a pane, which opens the note there, or onto a pane's edge,
        // which splits it. The path travels as a plain string: it is what every drop handler
        // wants, and it survives the row being recycled under the drag. Directories are not
        // draggable, having no single note to open.
        let source = gtk::DragSource::builder()
            .actions(gdk::DragAction::COPY)
            .build();
        source.connect_prepare(|source, _, _| {
            let expander = source.widget()?.downcast::<gtk::TreeExpander>().ok()?;
            let (kind, rel) = expander.list_row()?.item().as_ref().and_then(decode)?;
            (kind != 'd').then(|| gdk::ContentProvider::for_value(&rel.to_value()))
        });
        let begin = dragging.clone();
        source.connect_drag_begin(move |_, _| begin(true));
        let end = dragging.clone();
        source.connect_drag_end(move |_, _, _| end(false));
        expander.add_controller(source);
        item.downcast_ref::<gtk::ListItem>()
            .expect("list item")
            .set_child(Some(&expander));
    });
    // Widget lookups and two setters only: no database access on the bind path.
    factory.connect_bind(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().expect("list item");
        let Some(expander) = item.child().and_downcast::<gtk::TreeExpander>() else {
            return;
        };
        let Some(row) = item.item().and_downcast::<gtk::TreeListRow>() else {
            return;
        };
        let Some((kind, rel)) = row.item().as_ref().and_then(decode) else {
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
        icon.set_icon_name(Some(icon_name(kind)));
        let label = icon
            .next_sibling()
            .and_downcast::<gtk::Label>()
            .expect("label");
        label.set_text(rel.rsplit('/').next().unwrap_or(&rel));
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
        let Some((kind, rel)) = row.item().as_ref().and_then(decode) else {
            return;
        };
        if kind == 'd' {
            row.set_expanded(!row.is_expanded());
        } else {
            on_activate(kind, &rel);
        }
    });
    let scroller = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&view)
        .build();
    // ponytail: a plain `GtkBox` around the scroller, purely so the context menu has a
    // layout-managed widget to hang off. GTK re-presents a popover from its parent's
    // `allocate_native_children`, which only runs for widgets that use a layout manager;
    // `GtkListView` has a custom `size_allocate` and never re-presents its popover children, so a
    // menu parented to the list is frozen at its first-frame size and `GtkPopoverMenu`'s inner
    // scrolled window turns everything that grows afterwards into a scrollbar. If a scrollbar
    // ever comes back, the next dial is setting that inner scrolled window's policies to Never.
    let host = gtk::Box::new(gtk::Orientation::Vertical, 0);
    host.append(&scroller);
    Tree {
        host,
        view,
        model,
        vault,
        root: root.clone(),
        cache,
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
