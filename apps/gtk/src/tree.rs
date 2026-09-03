//! Lazy vault file tree: `gtk::ListView` over a `gtk::TreeListModel` whose children come from
//! `Index::list_files(prefix)`, one directory level per expansion.

use accent_core::fs::is_sync_conflict;
use accent_core::index::Index;
use accent_core::walk::FileKind;
use gtk::prelude::*;
use gtk::{gio, glib};
use std::cell::RefCell;
use std::rc::Rc;

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

fn children(index: &Rc<RefCell<Index>>, prefix: &str) -> gio::ListStore {
    let store = gio::ListStore::new::<gtk::StringObject>();
    fill(&store, index, prefix);
    store
}

/// Replace `store`'s contents with the direct children of `prefix`. `list_files` already returns
/// directories first, then names case-insensitively.
pub fn fill(store: &gio::ListStore, index: &Rc<RefCell<Index>>, prefix: &str) {
    store.remove_all();
    let rows = index.borrow().list_files(prefix).unwrap_or_default();
    for row in rows {
        if hidden(row.kind, &row.rel_path) {
            continue;
        }
        store.append(&gtk::StringObject::new(&encode(row.kind, &row.rel_path)));
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

/// Build the tree. `on_activate` is called with the rel_path of an activated non-directory row.
pub fn build(
    index: Rc<RefCell<Index>>,
    root: &gio::ListStore,
    on_activate: impl Fn(char, &str) + 'static,
) -> gtk::ListView {
    let tree = gtk::TreeListModel::new(root.clone(), false, false, {
        let index = index.clone();
        move |obj| {
            let (kind, rel) = decode(obj)?;
            (kind == 'd').then(|| children(&index, &rel).upcast())
        }
    });

    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let icon = gtk::Image::new();
        let label = gtk::Label::builder().xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::Middle).build();
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        row.append(&icon);
        row.append(&label);
        let expander = gtk::TreeExpander::new();
        expander.set_child(Some(&row));
        item.downcast_ref::<gtk::ListItem>()
            .expect("list item")
            .set_child(Some(&expander));
    });
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
        let hbox = expander.child().and_downcast::<gtk::Box>().expect("row box");
        let icon = hbox.first_child().and_downcast::<gtk::Image>().expect("icon");
        icon.set_icon_name(Some(icon_name(kind)));
        let label = icon.next_sibling().and_downcast::<gtk::Label>().expect("label");
        label.set_text(rel.rsplit('/').next().unwrap_or(&rel));
    });

    let selection = gtk::SingleSelection::new(Some(tree));
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
    view
}

/// Every markdown note in the vault, for the file switcher. Walks the index one directory level at
/// a time via the same lazy query the tree uses, so the hidden-path rules stay in one place.
pub fn all_markdown(index: &RefCell<Index>) -> Vec<String> {
    let mut out = Vec::new();
    let mut queue = vec![String::new()];
    while let Some(prefix) = queue.pop() {
        for row in index.borrow().list_files(&prefix).unwrap_or_default() {
            if hidden(row.kind, &row.rel_path) {
                continue;
            }
            match row.kind {
                FileKind::Dir => queue.push(row.rel_path),
                FileKind::Markdown => out.push(row.rel_path),
                _ => {}
            }
        }
    }
    out.sort();
    out
}
