//! The file tree's context menu: what it offers over a row, over a folder and over nothing, and
//! the action group its items resolve through.

use super::{Ops, download, new_file, new_folder, rename, trash, upload};
use super::{copy_absolute_path, copy_name, copy_relative_path, show_in_files};
use accent_core::path::parent_dir;
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use std::rc::Rc;

/// The action group the context menu's items resolve through, inserted on the tree widget.
const GROUP: &str = "fileops";
/// Right-click and Menu-key context menu for the file tree. `host` is the tree's outer box, and
/// `anchor` is where to point the popover in that box's coordinates.
///
/// `row` is the row that was clicked, as (path, is a directory), or `None` where the click landed
/// on no row at all — the blank area below the last one. Creating is offered in all three cases,
/// and everything else but [`listing`] needs a path, so a menu opened over nothing holds the create
/// items and that alone.
///
/// ponytail: the popover is parented to the box rather than to the `GtkListView` inside it,
/// because GTK only re-presents a popover from its parent's `allocate_native_children`, which a
/// widget with a custom `size_allocate` such as `GtkListView` never reaches. A menu parented to
/// the list keeps its first-frame size and `GtkPopoverMenu`'s internal scrolled window turns the
/// rest into a scrollbar. Fallback if one ever reappears: set that scrolled window's policies to
/// Never.
pub fn context_menu(
    ops: &Rc<Ops>,
    host: &gtk::Widget,
    row: Option<(&str, bool)>,
    anchor: gdk::Rectangle,
) {
    // On the host, not the list: an action resolves up the widget tree from the popover's parent.
    // Re-inserted per menu: the group holds a clone of `ops` and nothing else, and replacing it
    // costs a handful of small objects, which is less than remembering whether it is already there.
    host.insert_action_group(GROUP, Some(&actions(ops)));

    let menu = gio::Menu::new();
    if let Some((rel, false)) = row {
        menu.append_item(&item(GROUP, "Open", "open", rel));
    }
    // Everything that puts something in a folder shares one target, so a right-click anywhere in
    // the tree can create: in the folder clicked, beside the file clicked, or in the vault root.
    let dir = row_dir(row);
    menu.append_item(&item(GROUP, "New File", "new-file", dir));
    menu.append_item(&item(GROUP, "New Folder", "new-folder", dir));
    // Putting files in is only worth offering where they are not here already; a folder of a
    // local vault is one the file manager can be dropped onto.
    if ops.vault.is_remote() {
        menu.append_item(&item(GROUP, "Upload Files…", "upload", dir));
    }
    // Everything below names one file or folder, so none of it belongs on a menu opened over
    // blank space. Splitting is not here at all: it opens a note beside the active tab, which is
    // what the tab's own menu and `win.split-*` are for, not something done to a path.
    let Some((rel, is_dir)) = row else {
        menu.append_section(None, &listing());
        return popup(host, &menu, anchor, None);
    };
    // Rename is the move as well as the name: a path typed into it carries the file, which is
    // what replaced Move to… when the tree learned to take a drop. A folder's is an action of its
    // own because the dialog selects a folder's whole name and only a file's stem.
    let rename = if is_dir { "rename-folder" } else { "rename" };
    menu.append_item(&item(GROUP, "Rename", rename, rel));
    // Only a directory can be left out: `[search] exclude` is a list of folders, and the path is
    // right here, which is why this beats a preferences row nobody can point at a folder from.
    if is_dir {
        menu.append_item(&item(GROUP, "Leave Out of Search", "exclude", rel));
    }
    // Reading the name or the path out and leaving the app are neither edits nor deletions, so
    // they get a section of their own between the two. The name first: it is the shortest of the
    // three answers to "what is this file called", and the one a note's own prose wants.
    let elsewhere = gio::Menu::new();
    elsewhere.append_item(&item(GROUP, "Copy Name", "copy-name", rel));
    elsewhere.append_item(&item(GROUP, "Copy Relative Path", "copy-rel", rel));
    elsewhere.append_item(&item(GROUP, "Copy Absolute Path", "copy-abs", rel));
    // Download… takes Show in Files' place when the file is on a host: no file manager here can
    // show it, and a copy here is the only way to reach it with anything but accent.
    if !ops.vault.is_remote() {
        elsewhere.append_item(&item(GROUP, "Show in Files", "show", rel));
    } else if !is_dir {
        elsewhere.append_item(&item(GROUP, "Download…", "download", rel));
    }
    menu.append_section(None, &elsewhere);
    // Its own section, so the one destructive item is never next to Rename by accident.
    let danger = gio::Menu::new();
    danger.append_item(&item(GROUP, "Move to Trash", "trash", rel));
    menu.append_section(None, &danger);
    menu.append_section(None, &listing());
    popup(host, &menu, anchor, None);
}

/// The section every tree menu ends with, blank area included, as GTK's own file chooser has it:
/// how the listing looks, which is no single row's business. A window action, so it resolves up
/// from the host and is the same preference the palette toggles.
fn listing() -> gio::Menu {
    let action = "win.show-hidden-files";
    let section = gio::Menu::new();
    section.append(Some(crate::actions::label_of(action)), Some(action));
    section
}

/// The folder a row stands for: the folder itself, the one holding the file, and the vault root
/// where there is no row at all — the blank area below the last one, or the root label above the
/// first. It is where New File, New Folder and Upload put what they create, and where a drop
/// moves what was dragged, so the two ways of putting a file somewhere agree by construction.
pub fn row_dir(row: Option<(&str, bool)>) -> &str {
    match row {
        Some((rel, true)) => rel,
        Some((rel, false)) => parent_dir(rel),
        None => "",
    }
}

/// Hang the menu off `host` and show it. `class` is a style class for the popover, for a host
/// that gives it no background of its own.
pub fn popup(host: &gtk::Widget, menu: &gio::Menu, anchor: gdk::Rectangle, class: Option<&str>) {
    let popover = gtk::PopoverMenu::from_model(Some(menu));
    if let Some(class) = class {
        popover.add_css_class(class);
    }
    popover.set_parent(host);
    popover.set_has_arrow(false);
    popover.set_pointing_to(Some(&anchor));
    // A popover parented by hand stays parented until it is unparented by hand — but not while it
    // is closing. `closed` is emitted from inside the item's own `clicked`, and an unparented
    // widget has no path to the action group on the host, so unparenting there dropped whatever
    // the click had just asked for — every item in this menu, not only the ones that open a
    // dialog, exactly as it dropped the status bar's Fit Height. The idle runs once the click is
    // over.
    popover.connect_closed(|p| {
        let p = p.clone();
        glib::idle_add_local_once(move || p.unparent());
    });
    popover.popup();
}

/// One menu item of `group` carrying its target as a `String` rather than in a detailed-action
/// string, where an apostrophe in a note name would break the quoting. The Git pane's history
/// menu builds its items through here too, its targets being commit ids and branch names.
pub fn item(group: &str, label: &str, action: &str, target: &str) -> gio::MenuItem {
    let item = gio::MenuItem::new(Some(label), None);
    item.set_action_and_target_value(
        Some(&format!("{group}.{action}")),
        Some(&target.to_variant()),
    );
    item
}

/// What one context-menu action does with the path it was handed.
type Run = Box<dyn Fn(&Rc<Ops>, &str)>;

/// The actions the menu items name, each taking the row's path as its parameter.
fn actions(ops: &Rc<Ops>) -> gio::SimpleActionGroup {
    let group = gio::SimpleActionGroup::new();
    let add = |name: &str, run: Run| {
        let action = gio::SimpleAction::new(name, Some(glib::VariantTy::STRING));
        let ops = ops.clone();
        action.connect_activate(move |_, target| {
            if let Some(rel) = target.and_then(|t| t.str()) {
                run(&ops, rel);
            }
        });
        group.add_action(&action);
    };
    add("open", Box::new(|ops, rel| (ops.open)(rel, &[])));
    add("new-file", Box::new(new_file));
    add("new-folder", Box::new(new_folder));
    add("rename", Box::new(|ops, rel| rename(ops, rel, false)));
    add("rename-folder", Box::new(|ops, rel| rename(ops, rel, true)));
    add("copy-name", Box::new(copy_name));
    add("copy-rel", Box::new(copy_relative_path));
    add("copy-abs", Box::new(copy_absolute_path));
    add("show", Box::new(show_in_files));
    add("download", Box::new(download));
    add("upload", Box::new(upload));
    add("trash", Box::new(trash));
    add("exclude", Box::new(|ops, rel| (ops.exclude)(rel)));
    group
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_dir_answers_for_all_three_kinds_of_row() {
        assert_eq!(row_dir(Some(("Notes/Daily", true))), "Notes/Daily");
        assert_eq!(row_dir(Some(("Notes/Daily/mon.md", false))), "Notes/Daily");
        // A file at the vault root, and no row at all: both the root.
        assert_eq!(row_dir(Some(("todo.md", false))), "");
        assert_eq!(row_dir(None), "");
    }
}
