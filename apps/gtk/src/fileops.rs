//! Creating, renaming, moving and trashing notes and folders from the tree.
//!
//! Every call that touches the vault lives in [`accent_api::Vault`]; what is here is the dialogs
//! and the wiring between them. Like `sidebar` and `palette`, this module never sees the app:
//! everything it needs arrives as closures in [`Ops`], so `main` can hand it tabs and toasts
//! without a dependency cycle.
//!
//! DESIGN.md decides the shapes. A toast reports something that happened and is over; an
//! `AdwAlertDialog` appears only where the choice can lose data (rewriting links, deleting for
//! good); buttons and titles use header capitalisation, and only "Move to…" takes an ellipsis
//! because it is the one item that needs more input before it can act.

use accent_api::{RenamePlan, Vault};
use adw::prelude::*;
use gtk::{gdk, gio, glib};
use std::path::Path;
use std::rc::Rc;

/// The response id the three name dialogs confirm with.
const CONFIRM: &str = "confirm";
/// The action group the context menu's items resolve through, inserted on the tree widget.
const GROUP: &str = "fileops";
/// How many linking notes the rename dialog lists before it starts counting instead.
const LISTED: usize = 20;

/// Everything the operations need from the app, without depending on it.
// Boxed closures are the whole point of this struct; a type alias per field would only hide the
// signature the caller has to write anyway.
#[allow(clippy::type_complexity)]
pub struct Ops {
    pub vault: Rc<Vault>,
    pub window: adw::ApplicationWindow,
    pub toast: Box<dyn Fn(&str)>,
    /// Open a note in a tab.
    pub open: Box<dyn Fn(&str)>,
    /// Save any dirty tab for these paths before the file moves under them, and reload the ones
    /// listed afterwards. Called with the notes a rename is about to rewrite.
    pub flush: Box<dyn Fn(&[String])>,
    /// Reload these paths' tabs from disk, returning how many were left alone because their
    /// buffer still holds unsaved edits (those get the changed-on-disk banner instead).
    pub reload: Box<dyn Fn(&[String]) -> usize>,
    /// Close the tab for a path that has stopped existing. Only called once the file is really
    /// gone, so there is nothing left to write the buffer into and nothing to ask about.
    pub close: Box<dyn Fn(&str)>,
}

// --------------------------------------------------------------------------------- creating

/// New note in `dir` ("" is the vault root), from a template when the vault has any.
pub fn new_note(ops: &Rc<Ops>, dir: &str) {
    let entry = name_entry("Note name", "");
    let form = form();
    form.append(&entry);

    form.append(&name_preview(&entry));

    let templates = ops.vault.templates().unwrap_or_default();
    let picker = template_picker(&templates);
    if let Some(picker) = &picker {
        form.append(&labelled("Template", picker));
    }

    let dialog = name_dialog("New Note", "Create", &form);
    let (ops, dir, window) = (ops.clone(), dir.to_string(), ops.window.clone());
    let typed = entry.clone();
    dialog.choose(Some(&window), gio::Cancellable::NONE, move |response| {
        if response != CONFIRM {
            return;
        }
        let name = match sanitise_name(&typed.text()) {
            Ok(name) => name,
            Err(why) => return (ops.toast)(why),
        };
        let template = picker
            .as_ref()
            .and_then(|p| (p.selected() as usize).checked_sub(1))
            .and_then(|i| templates.get(i));
        match ops
            .vault
            .create_note(&child_path(&dir, &name), template.map(String::as_str))
        {
            Ok((created, _cursor)) => (ops.open)(&created),
            Err(e) if already_exists(&e) => {
                (ops.toast)(&format!("{} already exists", with_extension(&name)))
            }
            Err(e) => (ops.toast)(&format!("Cannot create {name}: {e:#}")),
        }
    });
    focus_name(&entry, None);
}

/// New folder inside `dir` ("" is the vault root).
pub fn new_folder(ops: &Rc<Ops>, dir: &str) {
    let entry = name_entry("Folder name", "");
    let form = form();
    form.append(&entry);

    let dialog = name_dialog("New Folder", "Create", &form);
    let (ops, dir, window) = (ops.clone(), dir.to_string(), ops.window.clone());
    let typed = entry.clone();
    dialog.choose(Some(&window), gio::Cancellable::NONE, move |response| {
        if response != CONFIRM {
            return;
        }
        let name = match sanitise_name(&typed.text()) {
            Ok(name) => name,
            Err(why) => return (ops.toast)(why),
        };
        let rel = child_path(&dir, &name);
        // `create_dir_all` is happy to find the directory already there, so the collision the
        // user cares about has to be asked about before the call rather than read off its error.
        if ops.vault.root().join(&rel).exists() {
            return (ops.toast)(&format!("{name} already exists"));
        }
        match ops.vault.create_dir(&rel) {
            Ok(()) => (ops.toast)(&format!("Created {name}")),
            Err(e) => (ops.toast)(&format!("Cannot create {name}: {e}")),
        }
    });
    focus_name(&entry, None);
}

/// "None" plus one row per template, or `None` when the vault has no templates.
fn template_picker(templates: &[String]) -> Option<gtk::DropDown> {
    if templates.is_empty() {
        return None;
    }
    let mut labels = vec!["None".to_string()];
    labels.extend(templates.iter().map(|t| basename(t).to_string()));
    let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    Some(gtk::DropDown::from_strings(&refs))
}

// --------------------------------------------------------------------------------- renaming

/// Rename a note or folder in place. The extension starts outside the selection, so typing
/// replaces the stem only, which is what every file manager does.
pub fn rename(ops: &Rc<Ops>, rel: &str) {
    let current = basename(rel).to_string();
    let entry = name_entry("Name", &current);
    let form = form();
    form.append(&entry);
    // A note that loses its `.md` drops out of the index and stops being a note, so renaming one
    // follows the same extension policy `create_note` does, and says so as the name is typed.
    // A folder or a PDF keeps whatever the user types.
    let note = is_markdown(&current);
    if note {
        form.append(&name_preview(&entry));
    }

    let dialog = name_dialog("Rename", "Rename", &form);
    let (ops, rel, window) = (ops.clone(), rel.to_string(), ops.window.clone());
    let typed = entry.clone();
    dialog.choose(Some(&window), gio::Cancellable::NONE, move |response| {
        if response != CONFIRM {
            return;
        }
        let name = match sanitise_name(&typed.text()) {
            Ok(name) => name,
            Err(why) => return (ops.toast)(why),
        };
        let to = sibling_path(&rel, &renamed_to(&name, note));
        if to != rel {
            plan(&ops, &rel, &to, "Renamed");
        }
    });
    let stem = split_ext(&current).0.chars().count() as i32;
    focus_name(&entry, Some(stem));
}

/// Move a note or folder to another directory of the same vault, keeping its name: a wikilink
/// resolves by basename, so nothing that points at it has to be rewritten.
pub fn move_to(ops: &Rc<Ops>, rel: &str) {
    let root = ops.vault.root().to_path_buf();
    let dialog = gtk::FileDialog::builder()
        .title("Move To")
        .initial_folder(&gio::File::for_path(&root))
        .modal(true)
        .build();

    let (ops, rel, window) = (ops.clone(), rel.to_string(), ops.window.clone());
    dialog.select_folder(Some(&window), gio::Cancellable::NONE, move |result| {
        // The error case is almost always "the user closed the chooser", which needs no toast.
        let Some(chosen) = result.ok().and_then(|f| f.path()) else {
            return;
        };
        let Some(dir) = inside_vault(&root, &chosen) else {
            return (ops.toast)("Choose a folder inside this vault.");
        };
        let to = moved_path(&rel, &dir);
        if to != rel {
            plan(&ops, &rel, &to, "Moved");
        }
    });
}

/// Ask the vault what the move would touch, then either do it or confirm the link rewrites first.
fn plan(ops: &Rc<Ops>, from: &str, to: &str, verb: &'static str) {
    match ops.vault.plan_rename(from, to) {
        Ok(plan) if plan.rewrites.is_empty() => apply(ops, &plan, false, verb),
        Ok(plan) => confirm_links(ops, plan, verb),
        Err(e) => (ops.toast)(&format!("Cannot rename: {e:#}")),
    }
}

/// Rewriting other people's notes is a data-losing choice, so it is an `AlertDialog` and the
/// notes it would touch are named rather than counted.
fn confirm_links(ops: &Rc<Ops>, plan: RenamePlan, verb: &'static str) {
    let dialog = adw::AlertDialog::new(Some("Update Links?"), Some(&link_body(&plan.rewrites)));
    dialog.add_responses(&[
        ("cancel", "Cancel"),
        ("keep", "Rename Only"),
        ("update", "Update Links"),
    ]);
    dialog.set_response_appearance("update", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("update"));
    dialog.set_close_response("cancel");

    let (ops, window) = (ops.clone(), ops.window.clone());
    dialog.choose(
        Some(&window),
        gio::Cancellable::NONE,
        move |response| match response.as_str() {
            "keep" => apply(&ops, &plan, false, verb),
            "update" => apply(&ops, &plan, true, verb),
            _ => {}
        },
    );
}

/// Body of the "Update Links?" dialog: the count, then the paths, then what it stopped listing.
fn link_body(rewrites: &[String]) -> String {
    let n = rewrites.len();
    let mut body = match n {
        1 => "1 note links to this one.".to_string(),
        _ => format!("{n} notes link to this one."),
    };
    body.push('\n');
    for rel in rewrites.iter().take(LISTED) {
        body.push('\n');
        body.push_str(rel);
    }
    if n > LISTED {
        body.push_str(&format!("\nand {} more", n - LISTED));
    }
    body
}

/// Move the file, then report. A partly rewritten vault is a real outcome, so the notes that
/// could not be updated are said out loud instead of being logged and forgotten.
fn apply(ops: &Rc<Ops>, plan: &RenamePlan, update_links: bool, verb: &str) {
    // The note being moved is flushed with the ones about to be rewritten: its own tab is about
    // to point at a path that no longer exists, and an unsaved buffer must not be the casualty.
    let mut dirty = plan.rewrites.clone();
    dirty.push(plan.from.clone());
    (ops.flush)(&dirty);

    match ops.vault.rename(plan, update_links) {
        Ok(report) => {
            let unsaved = (ops.reload)(&report.rewritten);
            (ops.toast)(&rename_message(
                verb,
                &plan.to,
                report.failed.len(),
                unsaved,
            ));
        }
        Err(e) => (ops.toast)(&format!("Cannot rename: {e:#}")),
    }
}

/// What the toast says after a rename: where it went, what could not be rewritten, and what is
/// still showing the old text because its tab has unsaved edits.
fn rename_message(verb: &str, to: &str, failed: usize, unsaved: usize) -> String {
    let mut message = match failed {
        0 => format!("{verb} to {to}"),
        n => format!("{verb}, but {n} notes could not be updated"),
    };
    match unsaved {
        0 => {}
        1 => message.push_str("; 1 note has unsaved changes and was not reloaded"),
        n => message.push_str(&format!(
            "; {n} notes have unsaved changes and were not reloaded"
        )),
    }
    message
}

// --------------------------------------------------------------------------------- deleting

/// Move to the system trash, which is the only reversible delete there is.
///
/// The tab closes after the file is gone, not before: a trash that fails, or a permanent delete
/// the user then cancels, must not leave the note open nowhere.
pub fn trash(ops: &Rc<Ops>, rel: &str) {
    let path = ops.vault.root().join(rel);
    let name = basename(rel).to_string();
    let (ops, rel) = (ops.clone(), rel.to_string());
    // What lands in the trash should be what the user last saw, so a dirty tab is written out
    // before the file moves. Whatever cannot be written stays visible in its tab's banner.
    (ops.flush)(std::slice::from_ref(&rel));
    // ponytail: the tree and the index catch up through the watcher rather than being told here.
    // Post the removal explicitly if a trashed file is ever seen lingering in the sidebar.
    gio::File::for_path(&path).trash_async(
        glib::Priority::DEFAULT,
        gio::Cancellable::NONE,
        move |result| match result {
            Ok(()) => {
                (ops.close)(&rel);
                (ops.toast)(&format!("Moved {name} to Trash"));
            }
            // What a sandbox without a working trash portal answers. There is nothing to fall
            // back to but a permanent delete, and that has to be asked about.
            Err(e) if e.matches(gio::IOErrorEnum::NotSupported) => {
                confirm_delete(&ops, &path, &name, &rel)
            }
            Err(e) => (ops.toast)(&format!("Cannot trash {name}: {e}")),
        },
    );
}

/// There is no Undo: `gio` has no untrash, so the toast never offers a button that cannot work
/// (NOTEPAD.md records it). Deleting for good is therefore asked about, every time.
fn confirm_delete(ops: &Rc<Ops>, path: &Path, name: &str, rel: &str) {
    let dialog = adw::AlertDialog::new(
        Some("Delete Permanently?"),
        Some(&format!(
            "{name} cannot be moved to the trash on this system. Deleting it cannot be undone."
        )),
    );
    dialog.add_responses(&[("cancel", "Cancel"), ("delete", "Delete")]);
    dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");

    let (ops, path, name, rel, window) = (
        ops.clone(),
        path.to_path_buf(),
        name.to_string(),
        rel.to_string(),
        ops.window.clone(),
    );
    dialog.choose(Some(&window), gio::Cancellable::NONE, move |response| {
        if response != "delete" {
            return;
        }
        let removed = match path.is_dir() {
            true => std::fs::remove_dir_all(&path),
            false => std::fs::remove_file(&path),
        };
        match removed {
            Ok(()) => {
                (ops.close)(&rel);
                (ops.toast)(&format!("Deleted {name}"));
            }
            Err(e) => (ops.toast)(&format!("Cannot delete {name}: {e}")),
        }
    });
}

// ------------------------------------------------------------------------------ context menu

/// Right-click and Menu-key context menu for a tree row. `anchor` is where to point the popover,
/// in the coordinates of `list`.
pub fn context_menu(
    ops: &Rc<Ops>,
    list: &gtk::ListView,
    rel: &str,
    is_dir: bool,
    anchor: gdk::Rectangle,
) {
    // Re-inserted per menu: the group holds a clone of `ops` and nothing else, and replacing it
    // costs six small objects, which is less than remembering whether it is already there.
    list.insert_action_group(GROUP, Some(&actions(ops)));

    let menu = gio::Menu::new();
    if is_dir {
        menu.append_item(&item("New Note", "new-note", rel));
        menu.append_item(&item("New Folder", "new-folder", rel));
    } else {
        menu.append_item(&item("Open", "open", rel));
    }
    menu.append_item(&item("Rename", "rename", rel));
    menu.append_item(&item("Move to…", "move", rel));
    // Its own section, so the one destructive item is never next to Rename by accident.
    let danger = gio::Menu::new();
    danger.append_item(&item("Move to Trash", "trash", rel));
    menu.append_section(None, &danger);

    let popover = gtk::PopoverMenu::from_model(Some(&menu));
    popover.set_parent(list);
    popover.set_has_arrow(false);
    popover.set_pointing_to(Some(&anchor));
    // A popover parented by hand stays parented: without this every right-click would leave
    // another one hanging off the tree.
    popover.connect_closed(|p| p.unparent());
    popover.popup();
}

/// One menu item carrying its path as a `String` target rather than in a detailed-action string,
/// where an apostrophe in a note name would break the quoting.
fn item(label: &str, action: &str, rel: &str) -> gio::MenuItem {
    let item = gio::MenuItem::new(Some(label), None);
    item.set_action_and_target_value(Some(&format!("{GROUP}.{action}")), Some(&rel.to_variant()));
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
    add("open", Box::new(|ops, rel| (ops.open)(rel)));
    add("new-note", Box::new(new_note));
    add("new-folder", Box::new(new_folder));
    add("rename", Box::new(rename));
    add("move", Box::new(move_to));
    add("trash", Box::new(trash));
    group
}

// ------------------------------------------------------------------------------------ paths

/// Trim the typed name and refuse the ones that would not stay where they were put. A note
/// called `../x` escapes the vault, and a leading dot hides the file from the tree.
fn sanitise_name(raw: &str) -> Result<String, &'static str> {
    let name = raw.trim();
    if name.is_empty() {
        return Err("Enter a name.");
    }
    if name.contains('/') {
        return Err("Names cannot contain a slash.");
    }
    if name.starts_with('.') {
        return Err("Names cannot start with a dot.");
    }
    Ok(name.to_string())
}

/// Whether the name already carries a markdown extension.
fn is_markdown(name: &str) -> bool {
    name.rsplit_once('.').is_some_and(|(_, ext)| {
        ext.eq_ignore_ascii_case("md") || ext.eq_ignore_ascii_case("markdown")
    })
}

/// The name the vault will actually create. Mirrors `Vault::create_note`, which appends `.md` to
/// anything that is not already markdown, so the dialog cannot promise a file it will not make.
fn with_extension(name: &str) -> String {
    match is_markdown(name) {
        true => name.to_string(),
        false => format!("{name}.md"),
    }
}

/// The name a rename lands on: the same extension policy for a note, the typed name for anything
/// else (a folder called `Notes` must not become `Notes.md`).
fn renamed_to(name: &str, note: bool) -> String {
    match note {
        true => with_extension(name),
        false => name.to_string(),
    }
}

/// Split a file name into stem and extension, dot included. A leading dot belongs to the name.
fn split_ext(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(i) if i > 0 => name.split_at(i),
        _ => (name, ""),
    }
}

fn basename(rel: &str) -> &str {
    rel.rsplit('/').next().unwrap_or(rel)
}

/// The directory `rel` sits in; "" for a file at the vault root.
fn parent_dir(rel: &str) -> &str {
    rel.rsplit_once('/').map_or("", |(dir, _)| dir)
}

/// `name` inside `dir`, where "" is the vault root.
fn child_path(dir: &str, name: &str) -> String {
    match dir.trim_end_matches('/') {
        "" => name.to_string(),
        dir => format!("{dir}/{name}"),
    }
}

/// `rel` renamed to `name`, staying in the same directory.
fn sibling_path(rel: &str, name: &str) -> String {
    child_path(parent_dir(rel), name)
}

/// `rel` moved into `dest_dir`, keeping its name.
fn moved_path(rel: &str, dest_dir: &str) -> String {
    child_path(dest_dir, basename(rel))
}

/// `chosen` as a vault-relative directory, or `None` when it is not inside the vault at all.
/// Both ends are canonicalised first, or a symlinked or `..`-laden path would sneak past.
fn inside_vault(root: &Path, chosen: &Path) -> Option<String> {
    let (root, chosen) = (root.canonicalize().ok()?, chosen.canonicalize().ok()?);
    let rel = chosen.strip_prefix(&root).ok()?;
    Some(rel.to_string_lossy().into_owned())
}

/// Whether the file was already there, from an `anyhow` chain that has wrapped the `io::Error`.
fn already_exists(e: &anyhow::Error) -> bool {
    e.chain().any(|c| {
        c.downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == std::io::ErrorKind::AlreadyExists)
    })
}

// --------------------------------------------------------------------------------- widgetry

/// The shared shape of the three name dialogs: Cancel, one verb, no OK button (DESIGN.md).
fn name_dialog(title: &str, verb: &str, form: &gtk::Box) -> adw::AlertDialog {
    let dialog = adw::AlertDialog::new(Some(title), None);
    dialog.set_extra_child(Some(form));
    dialog.add_responses(&[("cancel", "Cancel"), (CONFIRM, verb)]);
    dialog.set_response_appearance(CONFIRM, adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some(CONFIRM));
    dialog.set_close_response("cancel");
    dialog
}

/// The dim line under a name entry showing the file name that will really be used, since the
/// vault appends the `.md` the user did not type.
fn name_preview(entry: &gtk::Entry) -> gtk::Label {
    let preview = gtk::Label::builder().xalign(0.0).build();
    preview.add_css_class("dim-label");
    entry.connect_changed({
        let preview = preview.clone();
        move |e| {
            let typed = e.text();
            let typed = typed.trim();
            preview.set_label(&match typed.is_empty() {
                true => String::new(),
                false => with_extension(typed),
            });
        }
    });
    let initial = entry.text();
    preview.set_label(&match initial.trim() {
        "" => String::new(),
        typed => with_extension(typed),
    });
    preview
}

fn name_entry(placeholder: &str, text: &str) -> gtk::Entry {
    gtk::Entry::builder()
        .placeholder_text(placeholder)
        .text(text)
        .activates_default(true)
        .build()
}

/// 12 px between related widgets, per DESIGN.md's spacing scale.
fn form() -> gtk::Box {
    gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .build()
}

fn labelled(text: &str, child: &impl IsA<gtk::Widget>) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    row.append(
        &gtk::Label::builder()
            .label(text)
            .xalign(0.0)
            .hexpand(true)
            .build(),
    );
    row.append(child);
    row
}

/// Put the caret in the entry, optionally selecting only the first `stem` characters.
///
/// Called after `choose` has presented the dialog: the entry is mapped by then, and grabbing
/// focus is what selects the whole name (`gtk-entry-select-on-focus`), so the narrower selection
/// has to be set afterwards or it would be thrown away.
fn focus_name(entry: &gtk::Entry, stem: Option<i32>) {
    entry.grab_focus();
    if let Some(stem) = stem {
        entry.select_region(0, stem);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A unique empty directory under the system temp dir. `apps/gtk` has no `tempfile`
    /// dev-dependency and one test does not earn one.
    fn tempdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("accent-fileops-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    #[test]
    fn sanitise_name_trims_and_accepts_a_normal_name() {
        assert_eq!(
            sanitise_name("  Meeting notes  "),
            Ok("Meeting notes".into())
        );
        assert_eq!(
            sanitise_name("Übung 1 – Rückblick"),
            Ok("Übung 1 – Rückblick".into())
        );
        assert_eq!(sanitise_name("note.md"), Ok("note.md".into()));
    }

    #[test]
    fn sanitise_name_rejects_what_would_escape_the_vault() {
        assert!(sanitise_name("").is_err());
        assert!(sanitise_name("   ").is_err());
        assert!(sanitise_name("a/b").is_err());
        assert!(sanitise_name("../x").is_err());
        assert!(sanitise_name("..").is_err());
        assert!(sanitise_name(".hidden").is_err());
    }

    #[test]
    fn with_extension_adds_md_only_where_it_is_missing() {
        assert_eq!(with_extension("note"), "note.md");
        assert_eq!(with_extension("note.md"), "note.md");
        assert_eq!(with_extension("NOTE.MD"), "NOTE.MD");
        assert_eq!(with_extension("note.markdown"), "note.markdown");
        // What `Vault::create_note` does with a non-markdown extension: notes are markdown.
        assert_eq!(with_extension("chart.pdf"), "chart.pdf.md");
    }

    #[test]
    fn renamed_to_keeps_a_note_a_note_and_leaves_everything_else_alone() {
        // Renaming `note.md` to `x` used to demote it out of the note index.
        assert_eq!(renamed_to("x", true), "x.md");
        assert_eq!(renamed_to("x.md", true), "x.md");
        assert_eq!(renamed_to("Archive", false), "Archive");
        assert_eq!(renamed_to("chart.pdf", false), "chart.pdf");
    }

    #[test]
    fn is_markdown_only_matches_a_markdown_extension() {
        assert!(is_markdown("note.md"));
        assert!(is_markdown("NOTE.MD"));
        assert!(is_markdown("note.markdown"));
        assert!(!is_markdown("note"));
        assert!(!is_markdown("chart.pdf"));
        assert!(!is_markdown("Notes"));
    }

    #[test]
    fn rename_message_names_both_kinds_of_leftover() {
        assert_eq!(rename_message("Renamed", "b.md", 0, 0), "Renamed to b.md");
        assert_eq!(
            rename_message("Moved", "x/b.md", 2, 0),
            "Moved, but 2 notes could not be updated"
        );
        assert_eq!(
            rename_message("Renamed", "b.md", 0, 1),
            "Renamed to b.md; 1 note has unsaved changes and was not reloaded"
        );
        assert_eq!(
            rename_message("Renamed", "b.md", 2, 3),
            "Renamed, but 2 notes could not be updated; 3 notes have unsaved changes and were not reloaded"
        );
    }

    #[test]
    fn split_ext_keeps_the_last_dot_and_ignores_a_leading_one() {
        assert_eq!(split_ext("note.md"), ("note", ".md"));
        assert_eq!(split_ext("archive.tar.gz"), ("archive.tar", ".gz"));
        assert_eq!(split_ext("README"), ("README", ""));
        assert_eq!(split_ext(".gitignore"), (".gitignore", ""));
    }

    #[test]
    fn sibling_path_stays_in_the_same_directory() {
        assert_eq!(sibling_path("a/b/c.md", "d.md"), "a/b/d.md");
        assert_eq!(sibling_path("c.md", "d.md"), "d.md");
        assert_eq!(sibling_path("a/b", "c"), "a/c");
    }

    #[test]
    fn moved_path_keeps_the_basename() {
        assert_eq!(moved_path("a/b/c.md", "x/y"), "x/y/c.md");
        assert_eq!(moved_path("a/b/c.md", ""), "c.md");
        assert_eq!(moved_path("c.md", "x"), "x/c.md");
    }

    #[test]
    fn inside_vault_accepts_only_the_tree_below_the_root() {
        let base = tempdir("inside");
        let root = base.join("vault");
        std::fs::create_dir_all(root.join("Notes/Daily")).expect("vault tree");
        std::fs::create_dir_all(base.join("elsewhere")).expect("sibling");

        assert_eq!(inside_vault(&root, &root), Some(String::new()));
        assert_eq!(
            inside_vault(&root, &root.join("Notes")),
            Some("Notes".into())
        );
        assert_eq!(
            inside_vault(&root, &root.join("Notes/Daily")),
            Some("Notes/Daily".into())
        );
        assert_eq!(inside_vault(&root, &base.join("elsewhere")), None);
        assert_eq!(inside_vault(&root, &base), None);
        // A path that walks back out again is caught, which is the point of canonicalising.
        assert_eq!(
            inside_vault(&root, &root.join("Notes/../../elsewhere")),
            None
        );

        std::fs::remove_dir_all(&base).expect("cleanup");
    }

    #[test]
    fn link_body_counts_then_names_then_stops() {
        assert!(link_body(&["a.md".into()]).starts_with("1 note links to this one."));

        let body = link_body(&["a.md".into(), "b/c.md".into()]);
        assert!(body.starts_with("2 notes link to this one."));
        assert!(body.contains("\nb/c.md"));
        assert!(!body.contains("more"));

        let many: Vec<String> = (0..25).map(|i| format!("n{i}.md")).collect();
        let body = link_body(&many);
        assert!(body.contains("n19.md"));
        assert!(!body.contains("n20.md"));
        assert!(body.ends_with("and 5 more"));
    }
}
