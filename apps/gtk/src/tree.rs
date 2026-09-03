//! Lazy vault file tree: `gtk::ListView` over a `gtk::TreeListModel` whose children come from
//! `Index::list_files(prefix)`, one directory level per expansion.

use accent_core::fs::is_sync_conflict;
use accent_core::index::Index;
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

/// Replace `store`'s contents with the direct children of `prefix`. `list_files` already returns
/// directories first, then names case-insensitively.
pub fn fill(store: &gio::ListStore, index: &Rc<RefCell<Index>>, prefix: &str) {
    // The borrow ends with this statement: `list_files` hands back owned rows, so no `Index`
    // borrow is ever live across the GTK calls below.
    let rows = index.borrow().list_files(prefix).unwrap_or_default();
    let items: Vec<gtk::StringObject> = rows
        .into_iter()
        .filter(|r| !hidden(r.kind, &r.rel_path))
        .map(|r| gtk::StringObject::new(&encode(r.kind, &r.rel_path)))
        .collect();
    // One splice, one `items-changed`. Appending row by row made a 2 400-child directory emit
    // 2 400 signals out through TreeListModel -> SingleSelection -> ListView.
    store.splice(0, store.n_items(), &items);
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
/// scrolling into view ran a fresh `list_files` query.
pub struct Tree {
    view: gtk::ListView,
    model: gtk::TreeListModel,
    index: Rc<RefCell<Index>>,
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

    /// Re-read the vault root after a reconcile.
    ///
    /// ponytail: cached child models are dropped rather than re-filled, so a directory that is
    /// expanded *right now* keeps showing pre-reconcile children until it is collapsed and opened
    /// again (which is what the tree did before it cached at all). Re-filling every cached store
    /// would be a query per directory the user has ever scrolled past; do that behind the file
    /// watcher instead, where the changed paths are known.
    pub fn refresh(&self) {
        self.cache.borrow_mut().clear();
        fill(&self.root, &self.index, "");
    }
}

fn children_model(
    index: &Rc<RefCell<Index>>,
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
    fill(&store, index, rel);
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
    index: Rc<RefCell<Index>>,
    root: &gio::ListStore,
    on_activate: impl Fn(char, &str) + 'static,
) -> Tree {
    let cache: Rc<RefCell<HashMap<String, gio::ListStore>>> = Rc::new(RefCell::new(HashMap::new()));
    let model = gtk::TreeListModel::new(root.clone(), false, false, {
        let (index, cache) = (index.clone(), cache.clone());
        move |obj| {
            let (kind, rel) = decode(obj)?;
            (kind == 'd').then(|| children_model(&index, &cache, &rel).upcast())
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
        index,
        root: root.clone(),
        cache,
    }
}
