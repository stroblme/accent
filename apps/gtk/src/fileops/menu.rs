//! File-tree context menus for rows, folders and empty space, backed by an action group.

use super::clipboard::{self, can_paste};
use super::{
    Ops, download, move_to, new_drawing, new_file, new_folder, rename, trash, trash_all, upload,
};
use super::{copy_absolute_path, copy_name, copy_relative_path, show_in_files};
use accent_core::path::parent_dir;
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use std::rc::Rc;

/// The action group the context menu's items resolve through, inserted on the tree widget.
const GROUP: &str = "fileops";
/// File-tree menu anchored in `host` coordinates. `row` supplies a path and directory flag;
/// blank space offers creation and paste only. `marked` selects the multi-file [`marked_menu`].
///
/// Parent the popover to the outer box: `GtkListView`'s custom allocation prevents re-presenting
/// it, leaving a stale first-frame size and an internal scrollbar.
pub fn context_menu(
    ops: &Rc<Ops>,
    host: &gtk::Widget,
    row: Option<(&str, bool)>,
    marked: &[(String, bool)],
    anchor: gdk::Rectangle,
) -> gtk::PopoverMenu {
    // On the host, not the list: an action resolves up the widget tree from the popover's parent.
    // Re-inserted per menu: the group holds a clone of `ops` and nothing else, and replacing it
    // costs a handful of small objects, which is less than remembering whether it is already there.
    host.insert_action_group(GROUP, Some(&actions(ops)));

    if !marked.is_empty() {
        let menu = marked_menu(marked, row_dir(row), can_paste(ops));
        return crate::widgets::popup_menu(host, &menu, Some(anchor));
    }
    let menu = gio::Menu::new();
    if let Some((rel, false)) = row {
        menu.append_item(&item(GROUP, "Open", "open", rel));
        // A window with no vault opens a file by its path on this machine, which a remote vault's
        // file is not.
        if !ops.vault.is_remote() {
            menu.append_item(&item(GROUP, "Open in New Window", "open-apart", rel));
        }
    }
    // Everything that puts something in a folder shares one target, so a right-click anywhere in
    // the tree can create: in the folder clicked, beside the file clicked, or in the vault root.
    let dir = row_dir(row);
    menu.append_item(&item(GROUP, "New File", "new-file", dir));
    menu.append_item(&item(GROUP, "New Folder", "new-folder", dir));
    // A blank PDF to draw on.
    menu.append_item(&item(GROUP, "New Drawing", "new-drawing", dir));
    // Putting files in is only worth offering where they are not here already; a folder of a
    // local vault is one the file manager can be dropped onto.
    if ops.vault.is_remote() {
        menu.append_item(&item(GROUP, "Upload Files…", "upload", dir));
    }
    // Everything below names one file or folder, so none of it belongs on a menu opened over
    // blank space. Splitting is not here at all: it opens a note beside the active tab, which is
    // what the tab's own menu and `win.split-*` are for, not something done to a path.
    let Some((rel, is_dir)) = row else {
        // Nothing else names a path, but Paste names the folder it puts things in, and here that
        // is the vault root — the same target New File has just been given.
        menu.append_section(None, &clip_section(ops, None, dir));
        return crate::widgets::popup_menu(host, &menu, Some(anchor));
    };
    // Rename is the move as well as the name: a path typed into it carries the file. A folder's is
    // an action of its own because the dialog selects a folder's whole name and only a file's
    // stem.
    let rename = if is_dir { "rename-folder" } else { "rename" };
    menu.append_item(&item(GROUP, "Rename", rename, rel));
    // Only a directory can be left out: `[search] exclude` is a list of folders, and the path is
    // right here, which is why this beats a preferences row nobody can point at a folder from.
    if is_dir {
        menu.append_item(&item(GROUP, "Leave Out of Search", "exclude", rel));
        // For a listing that has fallen behind the disk: what the watcher never reported.
        menu.append_item(&item(GROUP, "Reload", "reload", rel));
    }
    // A file manager's own three, in a section of their own between what changes the file and
    // what reads its name out.
    menu.append_section(None, &clip_section(ops, Some((rel, is_dir)), dir));
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
    crate::widgets::popup_menu(host, &menu, Some(anchor))
}

/// Multi-file menu containing only actions that apply to the whole marked set. `dir` is the
/// paste destination from [`row_dir`]; `paste` indicates available clipboard content. Omit
/// single-file actions rather than silently using one selected row.
fn marked_menu(marked: &[(String, bool)], dir: &str, paste: bool) -> gio::Menu {
    let menu = gio::Menu::new();
    let clip = gio::Menu::new();
    clip.append_item(&many("Cut", "cut-many", marked));
    clip.append_item(&many("Copy", "copy-many", marked));
    clip.append_item(&many("Move to…", "move-to", marked));
    if paste {
        clip.append_item(&item(GROUP, "Paste", "paste", dir));
    }
    menu.append_section(None, &clip);
    // Its own section, so the one destructive item is never next to Copy by accident.
    let danger = gio::Menu::new();
    danger.append_item(&many("Move to Trash", "trash-many", marked));
    menu.append_section(None, &danger);
    menu
}

/// Cut, Copy and Move to… act on `row`; Paste targets `dir` from [`row_dir`]. Omit Paste without
/// clipboard files. Pass directory identity to Cut/Copy so duplicate-name suffixes preserve file
/// extensions but treat folder names whole.
fn clip_section(ops: &Rc<Ops>, row: Option<(&str, bool)>, dir: &str) -> gio::Menu {
    let section = gio::Menu::new();
    if let Some((rel, is_dir)) = row {
        let folder = match is_dir {
            true => "-folder",
            false => "",
        };
        section.append_item(&item(GROUP, "Cut", &format!("cut{folder}"), rel));
        section.append_item(&item(GROUP, "Copy", &format!("copy{folder}"), rel));
        section.append_item(&many("Move to…", "move-to", &[(rel.to_string(), is_dir)]));
    }
    if can_paste(ops) {
        section.append_item(&item(GROUP, "Paste", "paste", dir));
    }
    section
}

/// Shared destination for creation, paste and drops: the row's directory, a file's parent, or
/// the vault root on blank space.
pub fn row_dir(row: Option<(&str, bool)>) -> &str {
    match row {
        Some((rel, true)) => rel,
        Some((rel, false)) => parent_dir(rel),
        None => "",
    }
}

/// The type a marked set travels as: the paths and, for each, whether it is a directory, which is
/// what decides where a `(copy)` mark goes.
const PATHS: &str = "a(sb)";

/// One item of the marked menu, carrying the whole set as its target. Its own builder because the
/// target is a list rather than the one path [`item`] takes.
fn many(label: &str, action: &str, marked: &[(String, bool)]) -> gio::MenuItem {
    let item = gio::MenuItem::new(Some(label), None);
    item.set_action_and_target_value(
        Some(&format!("{GROUP}.{action}")),
        Some(&marked.to_variant()),
    );
    item
}

/// Every label a menu model offers, sections walked through, for the drills and the tests: a
/// `GtkPopoverMenu` keeps its model, so what is on screen can be read back out of it.
#[cfg(any(test, feature = "bench"))]
pub fn labels(menu: &gio::MenuModel) -> Vec<String> {
    let mut out = Vec::new();
    for i in 0..menu.n_items() {
        if let Some(section) = menu.item_link(i, gio::MENU_LINK_SECTION) {
            out.extend(labels(&section));
        }
        if let Some(label) = menu
            .item_attribute_value(i, gio::MENU_ATTRIBUTE_LABEL, Some(glib::VariantTy::STRING))
            .and_then(|v| v.str().map(str::to_string))
        {
            out.push(label);
        }
    }
    out
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

/// What one of the marked menu's actions does with the whole set it was handed.
type RunMany = Box<dyn Fn(&Rc<Ops>, Vec<(String, bool)>)>;

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
    add("open-apart", Box::new(|ops, rel| (ops.apart)(rel)));
    add("reload", Box::new(|ops, dir| (ops.relist)(dir)));
    add("new-file", Box::new(new_file));
    add("new-folder", Box::new(new_folder));
    add("new-drawing", Box::new(new_drawing));
    add("rename", Box::new(|ops, rel| rename(ops, rel, false)));
    add("rename-folder", Box::new(|ops, rel| rename(ops, rel, true)));
    add("cut", Box::new(|ops, rel| clipboard::cut(ops, rel, false)));
    add(
        "cut-folder",
        Box::new(|ops, rel| clipboard::cut(ops, rel, true)),
    );
    add(
        "copy",
        Box::new(|ops, rel| clipboard::copy(ops, rel, false)),
    );
    add(
        "copy-folder",
        Box::new(|ops, rel| clipboard::copy(ops, rel, true)),
    );
    add("paste", Box::new(clipboard::paste));
    add("copy-name", Box::new(copy_name));
    add("copy-rel", Box::new(copy_relative_path));
    add("copy-abs", Box::new(copy_absolute_path));
    add("show", Box::new(show_in_files));
    add("download", Box::new(download));
    add("upload", Box::new(upload));
    add("trash", Box::new(trash));
    add("exclude", Box::new(|ops, rel| (ops.exclude)(rel)));

    // The marked set's four, which take the whole list rather than one path; Move to… takes a
    // single row's as a list of one.
    let add_many = |name: &str, run: RunMany| {
        let ty = glib::VariantTy::new(PATHS).expect("a valid variant type");
        let action = gio::SimpleAction::new(name, Some(ty));
        let ops = ops.clone();
        action.connect_activate(move |_, target| {
            if let Some(marked) = target.and_then(|t| t.get::<Vec<(String, bool)>>()) {
                run(&ops, marked);
            }
        });
        group.add_action(&action);
    };
    add_many(
        "cut-many",
        Box::new(|ops, marked| clipboard::cut_all(ops, &marked)),
    );
    add_many(
        "copy-many",
        Box::new(|ops, marked| clipboard::copy_all(ops, &marked)),
    );
    add_many("move-to", Box::new(move_to));
    add_many(
        "trash-many",
        Box::new(|ops, marked| trash_all(ops, marked.into_iter().map(|(rel, _)| rel).collect())),
    );
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

    #[test]
    fn a_marked_set_offers_only_what_can_act_on_several_paths() {
        let marked = [("a.md".to_string(), false), ("Notes".to_string(), true)];
        assert_eq!(
            labels(marked_menu(&marked, "", true).upcast_ref()),
            ["Cut", "Copy", "Move to…", "Paste", "Move to Trash"]
        );
        // Paste is drawn only where the clipboard holds something, as it is on the single-row menu.
        assert_eq!(
            labels(marked_menu(&marked, "", false).upcast_ref()),
            ["Cut", "Copy", "Move to…", "Move to Trash"]
        );
    }
}
