//! File menus: the Files tree's rows, folders and empty space, and the file part a tab's menu and
//! a Git row's menu share with them, backed by one action group on the window.

use super::clipboard::{self, can_paste};
use super::{
    Ops, download, move_to, new_drawing, new_file, new_folder, rename, trash, trash_all, upload,
};
use super::{copy_absolute_path, copy_name, copy_relative_path, show_in_files};
use crate::actions::{label_of, print_actions};
use crate::doc::Kind;
use crate::export::{Export, printable_name};
use accent_core::path::parent_dir;
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use std::rc::Rc;

/// The action group every file menu's items resolve through, inserted on the window ([`actions`]).
pub const GROUP: &str = "fileops";

/// Where a file's menu is opened, which decides what it offers beside the file itself.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum At {
    /// A Files tree row, which is where the file already is.
    #[default]
    Tree,
    /// A tab, which has the file open.
    Tab,
    /// A Git pane row, which is about the file's change and changes the file only through git.
    Git,
}

/// What the part of a file's menu every location shares ([`file_sections`]) is built from.
#[derive(Clone, Copy, Debug, Default)]
pub struct Facts {
    pub at: At,
    /// A folder: Leave Out of Search and Reload, and nothing printed.
    pub dir: bool,
    /// On a host, where Download… stands in for Show in Files.
    pub remote: bool,
    /// What the file can be printed and exported as.
    pub prints: Option<Kind>,
    /// Gone from the working tree (a Git row's deletion): only its name and paths are left.
    pub gone: bool,
    /// The clipboard holds files, so a tree row offers Paste.
    pub paste: bool,
    /// Inside a dependency tree (`tree::Row::dependency`), which is read and never changed: only
    /// its names and the ways out of the app, as its tree row has them.
    pub dependency: bool,
}

/// Each verb's label, a `win.` action's where the palette has the same command so the two cannot
/// disagree; [`label_of`] hands back any other as it is.
const LABELS: &[(&str, &str)] = &[
    ("open", "Open"),
    ("open-apart", "Open in New Window"),
    ("open-accent", "win.open-in-accent"),
    ("terminal", "Open in Terminal"),
    ("new-file", "win.new-file"),
    ("new-folder", "win.new-folder"),
    ("new-drawing", "win.new-drawing"),
    ("upload", "win.upload"),
    ("rename", "win.rename"),
    ("rename-folder", "win.rename"),
    ("exclude", "Leave Out of Search"),
    ("reload", "Reload"),
    ("cut", "Cut"),
    ("cut-folder", "Cut"),
    ("copy", "Copy"),
    ("copy-folder", "Copy"),
    ("move-to", "Move to…"),
    ("paste", "Paste"),
    ("copy-name", "win.copy-name"),
    ("copy-rel", "win.copy-relative-path"),
    ("copy-abs", "win.copy-absolute-path"),
    ("show", "win.show-in-files"),
    ("download", "Download…"),
    ("reveal", "win.reveal-in-sidebar"),
    ("print", "win.print"),
    ("export-pdf", "win.export-pdf"),
    ("export-html", "win.export-html"),
    ("export-png", "win.export-png"),
    ("export-svg", "win.export-svg"),
    ("trash", "win.trash"),
    ("delete", "win.delete"),
];

/// The label of the menu item for `verb`.
fn label(verb: &'static str) -> &'static str {
    let named = LABELS.iter().find(|(v, _)| *v == verb);
    label_of(named.map_or(verb, |(_, name)| name))
}

/// The item for `verb`, aimed at `target`.
fn verb(verb: &'static str, target: &str) -> gio::MenuItem {
    item(GROUP, label(verb), verb, target)
}

/// The verbs of a file's menu from Rename on, a section each, in DESIGN.md's order (Context
/// menus): its name, the clipboard, the names it can be copied as and the ways out of the app,
/// what it is printed as, and the trash. A Git row changes a file only through git, so it has
/// neither the first two nor the last.
fn file_verbs(f: &Facts) -> Vec<Vec<&'static str>> {
    if f.dependency {
        return vec![names(f)];
    }
    let changes = f.at != At::Git;
    let mut sections = Vec::new();
    if changes {
        sections.push(match f.dir {
            true => vec!["rename-folder", "exclude", "reload"],
            false => vec!["rename"],
        });
        let mut clip = match f.dir {
            true => vec!["cut-folder", "copy-folder", "move-to"],
            false => vec!["cut", "copy", "move-to"],
        };
        if f.paste {
            clip.push("paste");
        }
        sections.push(clip);
    }
    sections.push(names(f));
    let prints: Vec<&str> = print_actions(f.prints)
        .iter()
        .filter_map(|action| action.strip_prefix("win."))
        .collect();
    if !f.dir && !f.gone && !prints.is_empty() {
        sections.push(prints);
    }
    if changes {
        sections.push(vec![trash_verb(f.remote)]);
    }
    sections
}

/// Move to Trash, or on a remote vault, which has no trash, Delete…, which asks first.
fn trash_verb(remote: bool) -> &'static str {
    match remote {
        true => "delete",
        false => "trash",
    }
}

/// Copy Name, the two Copy Paths and the way out of the app: Show in Files, or Download… for a
/// host's file, which no file manager here can show. Reveal in Sidebar ends it on a menu that is
/// not the tree's own, and a file deleted from the working tree has only its name and paths left.
/// The name first: it is the shortest answer to "what is this file called".
fn names(f: &Facts) -> Vec<&'static str> {
    let mut verbs = vec!["copy-name", "copy-rel", "copy-abs"];
    if f.gone {
        return verbs;
    }
    match (f.remote, f.dir) {
        (false, _) => verbs.push("show"),
        (true, false) => verbs.push("download"),
        (true, true) => {}
    }
    if f.at != At::Tree {
        verbs.push("reveal");
    }
    verbs
}

/// The part of a file's menu the tree, a tab and a Git row share, from Rename on, each item a
/// [`GROUP`] action on `rel`.
pub fn file_sections(rel: &str, f: &Facts) -> Vec<gio::Menu> {
    file_verbs(f)
        .into_iter()
        .map(|verbs| {
            let section = gio::Menu::new();
            for name in verbs {
                section.append_item(&match name {
                    "move-to" => many(label(name), name, &[(rel.to_string(), f.dir)]),
                    // Paste puts things in the folder the row is or is in.
                    "paste" => verb(name, row_dir(Some((rel, f.dir)))),
                    _ => verb(name, rel),
                });
            }
            section
        })
        .collect()
}

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
    if !marked.is_empty() {
        let menu = marked_menu(
            marked,
            row_dir(row),
            can_paste(ops),
            ops.vault.is_remote(),
            false,
        );
        return crate::widgets::popup_menu(host, &menu, Some(anchor));
    }
    let remote = ops.vault.is_remote();
    let menu = gio::Menu::new();
    if let Some((rel, false)) = row {
        menu.append_item(&verb("open", rel));
        // Where a terminal editor is picked, and this file would open in it.
        if (ops.terminal_editor)() && matches!(crate::doc::kind_of(rel), Kind::Note | Kind::Text) {
            menu.append_item(&verb("open-accent", rel));
        }
        // A window with no vault opens a file by its path on this machine, which a remote vault's
        // file is not.
        if !remote {
            menu.append_item(&verb("open-apart", rel));
        }
    }
    // A folder opens as a shell there, where a file has Open.
    if let Some((rel, true)) = row {
        menu.append_item(&verb("terminal", rel));
    }
    // Everything that puts something in a folder shares one target, so a right-click anywhere in
    // the tree can create: in the folder clicked, beside the file clicked, or in the vault root.
    let dir = row_dir(row);
    menu.append_item(&verb("new-file", dir));
    menu.append_item(&verb("new-folder", dir));
    // A blank PDF to draw on.
    menu.append_item(&verb("new-drawing", dir));
    // Putting files in is only worth offering where they are not here already; a folder of a
    // local vault is one the file manager can be dropped onto.
    if remote {
        menu.append_item(&verb("upload", dir));
    }
    // Everything below names one file or folder, so none of it belongs on a menu opened over
    // blank space. Splitting is not here at all: it opens a note beside the active tab, which is
    // what the tab's own menu and `win.split-*` are for, not something done to a path.
    let Some((rel, dir)) = row else {
        // Nothing else names a path, but Paste names the folder it puts things in, and here that
        // is the vault root — the same target New File has just been given.
        if can_paste(ops) {
            let paste = gio::Menu::new();
            paste.append_item(&verb("paste", ""));
            menu.append_section(None, &paste);
        }
        return crate::widgets::popup_menu(host, &menu, Some(anchor));
    };
    let facts = Facts {
        dir,
        remote,
        prints: printable_name(rel),
        paste: can_paste(ops),
        ..Facts::default()
    };
    for section in file_sections(rel, &facts) {
        menu.append_section(None, &section);
    }
    crate::widgets::popup_menu(host, &menu, Some(anchor))
}

/// The menu of a row inside a dependency tree (`tree::Row::dependency`): only what reads it,
/// since the tree is somebody else's to change. The tree's `top` folder itself can also be
/// renamed, moved or trashed, which takes the tree whole.
pub fn dependency_menu(
    ops: &Rc<Ops>,
    host: &gtk::Widget,
    (rel, is_dir): (&str, bool),
    top: bool,
    anchor: gdk::Rectangle,
) -> gtk::PopoverMenu {
    let menu = dependency_model(rel, is_dir, ops.vault.is_remote(), top);
    crate::widgets::popup_menu(host, &menu, Some(anchor))
}

/// [`dependency_menu`]'s items: Open, or Open in Terminal and Reload, then [`names`]; on the
/// `top` folder Rename, Move to… and Move to Trash where [`context_menu`] has them.
fn dependency_model(rel: &str, is_dir: bool, remote: bool, top: bool) -> gio::Menu {
    let menu = gio::Menu::new();
    let open = gio::Menu::new();
    if is_dir {
        open.append_item(&verb("terminal", rel));
    }
    if top {
        open.append_item(&verb("rename-folder", rel));
    }
    match is_dir {
        true => open.append_item(&verb("reload", rel)),
        false => {
            open.append_item(&verb("open", rel));
            if !remote {
                open.append_item(&verb("open-apart", rel));
            }
        }
    }
    menu.append_section(None, &open);
    if top {
        let moves = gio::Menu::new();
        moves.append_item(&many(
            label("move-to"),
            "move-to",
            &[(rel.to_string(), is_dir)],
        ));
        menu.append_section(None, &moves);
    }
    let reads = Facts {
        dir: is_dir,
        remote,
        ..Facts::default()
    };
    let section = gio::Menu::new();
    for name in names(&reads) {
        section.append_item(&verb(name, rel));
    }
    menu.append_section(None, &section);
    if top {
        let danger = gio::Menu::new();
        danger.append_item(&verb(trash_verb(remote), rel));
        menu.append_section(None, &danger);
    }
    menu
}

/// The menu of a marked set holding a dependency tree, marked whole through its top folder:
/// [`marked_menu`]'s `whole`.
pub fn whole_menu(
    ops: &Rc<Ops>,
    host: &gtk::Widget,
    marked: &[(String, bool)],
    anchor: gdk::Rectangle,
) -> gtk::PopoverMenu {
    let menu = marked_menu(marked, "", false, ops.vault.is_remote(), true);
    crate::widgets::popup_menu(host, &menu, Some(anchor))
}

/// Multi-file menu containing only actions that apply to the whole marked set. `dir` is the
/// paste destination from [`row_dir`]; `paste` indicates available clipboard content, and
/// `remote` a vault with no trash, where the set is deleted. A set holding a dependency tree
/// (`whole`) offers only what the tree's top folder's own menu does, Move to… and Move to Trash
/// ([`dependency_model`]). Omit single-file actions rather than silently using one selected row.
fn marked_menu(
    marked: &[(String, bool)],
    dir: &str,
    paste: bool,
    remote: bool,
    whole: bool,
) -> gio::Menu {
    let menu = gio::Menu::new();
    let clip = gio::Menu::new();
    if !whole {
        clip.append_item(&many(label("cut"), "cut-many", marked));
        clip.append_item(&many(label("copy"), "copy-many", marked));
    }
    clip.append_item(&many(label("move-to"), "move-to", marked));
    if paste && !whole {
        clip.append_item(&verb("paste", dir));
    }
    menu.append_section(None, &clip);
    // Its own section, so the one destructive item is never next to Copy by accident.
    let danger = gio::Menu::new();
    danger.append_item(&many(label(trash_verb(remote)), "trash-many", marked));
    menu.append_section(None, &danger);
    menu
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

/// What the action `verb` does with the one path it is handed, `None` for a verb that is no such
/// action: Move to…, which takes a list.
fn run(verb: &str) -> Option<Run> {
    let run: Run = match verb {
        "open" => Box::new(|ops, rel| (ops.open)(rel, &[])),
        "open-apart" => Box::new(|ops, rel| (ops.apart)(rel)),
        "open-accent" => Box::new(|ops, rel| (ops.in_accent)(rel)),
        "terminal" => Box::new(|ops, dir| (ops.terminal)(dir)),
        "new-file" => Box::new(new_file),
        "new-folder" => Box::new(new_folder),
        "new-drawing" => Box::new(new_drawing),
        "upload" => Box::new(upload),
        "rename" => Box::new(|ops, rel| rename(ops, rel, false)),
        "rename-folder" => Box::new(|ops, rel| rename(ops, rel, true)),
        "exclude" => Box::new(|ops, rel| (ops.exclude)(rel)),
        "reload" => Box::new(|ops, dir| (ops.relist)(dir)),
        "cut" => Box::new(|ops, rel| clipboard::cut(ops, rel, false)),
        "cut-folder" => Box::new(|ops, rel| clipboard::cut(ops, rel, true)),
        "copy" => Box::new(|ops, rel| clipboard::copy(ops, rel, false)),
        "copy-folder" => Box::new(|ops, rel| clipboard::copy(ops, rel, true)),
        "paste" => Box::new(clipboard::paste),
        "copy-name" => Box::new(copy_name),
        "copy-rel" => Box::new(copy_relative_path),
        "copy-abs" => Box::new(copy_absolute_path),
        "show" => Box::new(show_in_files),
        "download" => Box::new(download),
        "reveal" => Box::new(|ops, rel| (ops.reveal)(rel)),
        "print" => Box::new(|ops, rel| (ops.make)(rel, None)),
        "export-pdf" => Box::new(|ops, rel| (ops.make)(rel, Some(Export::Pdf))),
        "export-html" => Box::new(|ops, rel| (ops.make)(rel, Some(Export::Html))),
        "export-png" => Box::new(|ops, rel| (ops.make)(rel, Some(Export::Png))),
        "export-svg" => Box::new(|ops, rel| (ops.make)(rel, Some(Export::Svg))),
        "trash" | "delete" => Box::new(trash),
        _ => return None,
    };
    Some(run)
}

/// The actions every file menu's items name, each taking the row's path as its parameter: one
/// group for the window, put on it once (`build::adopt_vault`). Each holds `ops` weakly, since
/// `ops` holds the window.
pub fn actions(ops: &Rc<Ops>) -> gio::SimpleActionGroup {
    let group = gio::SimpleActionGroup::new();
    for (name, _) in LABELS {
        let Some(run) = run(name) else { continue };
        let action = gio::SimpleAction::new(name, Some(glib::VariantTy::STRING));
        let ops = Rc::downgrade(ops);
        action.connect_activate(move |_, target| {
            if let (Some(ops), Some(rel)) = (ops.upgrade(), target.and_then(|t| t.str())) {
                run(&ops, rel);
            }
        });
        group.add_action(&action);
    }

    // The marked set's four, which take the whole list rather than one path; Move to… takes a
    // single row's as a list of one.
    let add_many = |name: &str, run: RunMany| {
        let ty = glib::VariantTy::new(PATHS).expect("a valid variant type");
        let action = gio::SimpleAction::new(name, Some(ty));
        let ops = Rc::downgrade(ops);
        action.connect_activate(move |_, target| {
            let marked = target.and_then(|t| t.get::<Vec<(String, bool)>>());
            if let (Some(ops), Some(marked)) = (ops.upgrade(), marked) {
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
            labels(marked_menu(&marked, "", true, false, false).upcast_ref()),
            ["Cut", "Copy", "Move to…", "Paste", "Move to Trash"]
        );
        // Paste is drawn only where the clipboard holds something, as it is on the single-row menu.
        assert_eq!(
            labels(marked_menu(&marked, "", false, false, false).upcast_ref()),
            ["Cut", "Copy", "Move to…", "Move to Trash"]
        );
        // A set holding a dependency tree does only what the tree's top folder's menu does.
        assert_eq!(
            labels(marked_menu(&marked, "", true, false, true).upcast_ref()),
            ["Move to…", "Move to Trash"]
        );
    }

    #[test]
    fn a_dependency_row_offers_only_what_reads_it() {
        let menu =
            |rel, is_dir, remote| labels(dependency_model(rel, is_dir, remote, false).upcast_ref());
        let paths = ["Copy Name", "Copy Relative Path", "Copy Absolute Path"];
        assert_eq!(
            menu(".ruff_cache/0.6.9", true, false),
            [
                &["Open in Terminal", "Reload"][..],
                &paths,
                &["Show in Files"]
            ]
            .concat()
        );
        // The tree's own folder goes whole: renamed, moved or trashed.
        assert_eq!(
            labels(dependency_model(".ruff_cache", true, false, true).upcast_ref()),
            [
                &["Open in Terminal", "Rename…", "Reload", "Move to…"][..],
                &paths,
                &["Show in Files", "Move to Trash"]
            ]
            .concat()
        );
        assert_eq!(
            menu("node_modules/a/index.js", false, false),
            [
                &["Open", "Open in New Window"][..],
                &paths,
                &["Show in Files"]
            ]
            .concat()
        );
        assert_eq!(
            menu(".venv/pyvenv.cfg", false, true),
            [&["Open"][..], &paths, &["Download…"]].concat()
        );
    }

    /// The labels of a file's shared part at `rel`, every section in order.
    fn part(rel: &str, f: Facts) -> Vec<String> {
        file_sections(rel, &f)
            .iter()
            .flat_map(|section| labels(section.upcast_ref()))
            .collect()
    }

    /// One file part for the tree, a tab and a Git row (DESIGN.md, Context menus), cut to what
    /// each place can do: a tab adds Reveal in Sidebar, a host's file has Download…, a folder
    /// prints nothing, and a Git row changes the file only through git.
    #[test]
    fn one_file_part_for_every_location() {
        let names = ["Copy Name", "Copy Relative Path", "Copy Absolute Path"];
        let changes = ["Rename…", "Cut", "Copy", "Move to…"];
        let note = Facts {
            prints: Some(Kind::Note),
            ..Facts::default()
        };
        let html = ["Print…", "Export as HTML…", "Move to Trash"];
        assert_eq!(
            part("a.md", note),
            [&changes[..], &names, &["Show in Files"], &html].concat()
        );
        let tab = Facts {
            at: At::Tab,
            ..note
        };
        let shown = ["Show in Files", "Reveal in Sidebar"];
        assert_eq!(
            part("a.md", tab),
            [&changes[..], &names, &shown, &html].concat()
        );
        let pdf = Facts {
            at: At::Tab,
            remote: true,
            prints: Some(Kind::Pdf),
            ..Facts::default()
        };
        assert_eq!(
            part("a.pdf", pdf),
            [
                &changes[..],
                &names,
                &[
                    "Download…",
                    "Reveal in Sidebar",
                    "Print…",
                    "Export as PDF…",
                    "Delete…"
                ]
            ]
            .concat()
        );
        let folder = Facts {
            dir: true,
            paste: true,
            ..Facts::default()
        };
        assert_eq!(
            part("Notes", folder),
            [
                &[
                    "Rename…",
                    "Leave Out of Search",
                    "Reload",
                    "Cut",
                    "Copy",
                    "Move to…",
                    "Paste"
                ][..],
                &names,
                &["Show in Files", "Move to Trash"]
            ]
            .concat()
        );
        let git = Facts {
            at: At::Git,
            prints: Some(Kind::Diagram),
            ..Facts::default()
        };
        let diagram = [
            "Print…",
            "Export as PDF…",
            "Export as PNG…",
            "Export as SVG…",
        ];
        assert_eq!(
            part("a.drawio", git),
            [&names[..], &shown, &diagram].concat()
        );
        assert_eq!(part("a.drawio", Facts { gone: true, ..git }), names);
    }

    /// A tab on a file inside a dependency tree offers what that file's row offers, which reads it
    /// and changes nothing; on a host the names' way out is Download….
    #[test]
    fn a_dependency_file_is_only_read() {
        let tab = Facts {
            at: At::Tab,
            dependency: true,
            prints: Some(Kind::Note),
            ..Facts::default()
        };
        let names = ["Copy Name", "Copy Relative Path", "Copy Absolute Path"];
        assert_eq!(
            part("node_modules/a/README.md", tab),
            [&names[..], &["Show in Files", "Reveal in Sidebar"]].concat()
        );
        let remote = Facts {
            remote: true,
            ..tab
        };
        assert_eq!(
            part("node_modules/a/README.md", remote),
            [&names[..], &["Download…", "Reveal in Sidebar"]].concat()
        );
        // The tree's own top folder of one goes whole, deleted on a host.
        assert_eq!(
            labels(dependency_model("node_modules", true, true, true).upcast_ref()).last(),
            Some(&"Delete…".to_string())
        );
        let marked = [("a.md".to_string(), false)];
        assert_eq!(
            labels(marked_menu(&marked, "", false, true, false).upcast_ref()).last(),
            Some(&"Delete…".to_string())
        );
    }

    /// An item whose verb is no action is greyed for good, and one with no label shows its verb.
    #[test]
    fn every_verb_is_an_action_with_a_label() {
        for (verb, _) in LABELS {
            assert!(*verb == "move-to" || run(verb).is_some(), "{verb}");
        }
        for at in [At::Tree, At::Tab, At::Git] {
            for (dir, remote, gone, paste) in
                [(false, false, false, true), (true, true, true, false)]
            {
                for prints in [Some(Kind::Note), Some(Kind::Pdf), Some(Kind::Diagram)] {
                    let f = Facts {
                        at,
                        dir,
                        remote,
                        prints,
                        gone,
                        paste,
                        dependency: false,
                    };
                    for verb in file_verbs(&f).concat() {
                        assert!(LABELS.iter().any(|(v, _)| *v == verb), "{verb}");
                    }
                }
            }
        }
    }
}
