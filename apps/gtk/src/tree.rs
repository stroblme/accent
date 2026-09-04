//! Lazy vault file tree: `gtk::ListView` over a `gtk::TreeListModel` whose children come from
//! `Vault::list_dir(prefix)`, one directory level per expansion.

use accent_api::Vault;
use accent_core::fs::is_sync_conflict;
use accent_core::walk::FileKind;
use gtk::prelude::*;
use gtk::{gio, glib};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Instant;

/// ponytail: rows are `gtk::StringObject`s holding `"<kind char><rel_path>"` instead of a custom
/// GObject with typed properties. Saves ~40 lines of subclass boilerplate; if the tree ever needs
/// more per-row state (git status, unsaved marker) define a real `FileItem` GObject then.
fn encode(kind: FileKind, rel: &str) -> String {
    let c = match kind {
        FileKind::Dir => 'd',
        FileKind::Markdown => 'm',
        FileKind::Pdf => 'p',
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
pub fn fill(store: &gio::ListStore, vault: &Rc<Vault>, prefix: &str) {
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
    view: gtk::ListView,
    model: gtk::TreeListModel,
    vault: Rc<Vault>,
    root: gio::ListStore,
    cache: Rc<RefCell<HashMap<String, gio::ListStore>>>,
}

impl Tree {
    pub fn view(&self) -> &gtk::ListView {
        &self.view
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
}

fn children_model(
    vault: &Rc<Vault>,
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

/// Build the tree. `on_activate` is called with the rel_path of an activated non-directory row.
pub fn build(
    vault: Rc<Vault>,
    root: &gio::ListStore,
    on_activate: impl Fn(char, &str) + 'static,
) -> Tree {
    let cache: Rc<RefCell<HashMap<String, gio::ListStore>>> = Rc::new(RefCell::new(HashMap::new()));
    let model = gtk::TreeListModel::new(root.clone(), false, false, {
        let (vault, cache) = (vault.clone(), cache.clone());
        move |obj| {
            let (kind, rel) = decode(obj)?;
            (kind == 'd').then(|| children_model(&vault, &cache, &rel).upcast())
        }
    });

    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
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
    Tree {
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
