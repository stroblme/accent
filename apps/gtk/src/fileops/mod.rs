//! Creating, renaming, moving and trashing notes and folders from the tree.
//!
//! Every call that touches the vault lives in [`accent_api::Vault`]; what is here is the dialogs
//! and the wiring between them. Like `sidebar` and `palette`, this module never sees the app:
//! everything it needs arrives as closures in [`Ops`], so `main` can hand it tabs and toasts
//! without a dependency cycle.
//!
//! DESIGN.md decides the shapes. A toast reports something that happened and is over; an
//! `AdwAlertDialog` appears only where the choice can lose data (rewriting links, deleting for
//! good); buttons and titles use header capitalisation, and an item takes an ellipsis only where
//! it needs more input before it can act (Upload Files…, Download…).

mod menu;
mod paths;
mod transfer;

pub use menu::{context_menu, row_dir};
pub use paths::move_dest;
pub use transfer::{download, upload};

use self::paths::{
    already_exists, child_path, is_markdown, renamed_path, sanitise_name, split_ext, split_typed,
    typed_path, verb,
};
use crate::dialogs::{alert, form, labelled};
use crate::pathfield::{completions, look_again, path_field};
// Re-exported rather than imported plainly: the Git pane's Create Branch asks for them through
// this module, which is where they used to live.
pub(crate) use crate::dialogs::{CONFIRM, name_dialog, name_entry};
use accent_api::{FileKind, FileRow, RenamePlan, Vault};
use accent_core::path::{basename, parent_dir};
use adw::prelude::*;
use gtk::{gio, glib};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

/// How many linking notes the rename dialog lists before it starts counting instead.
const LISTED: usize = 20;
/// Everything the operations need from the app, without depending on it.
// Boxed closures are the whole point of this struct; a type alias per field would only hide the
// signature the caller has to write anyway.
#[allow(clippy::type_complexity)]
pub struct Ops {
    pub vault: Arc<Vault>,
    pub window: adw::ApplicationWindow,
    pub toast: Box<dyn Fn(&str)>,
    /// Open a note in a tab, putting the caret at the first of these byte offsets and making the
    /// rest Tab stops — which is where a template's `{{cursor}}`s land.
    pub open: Box<dyn Fn(&str, &[usize])>,
    /// Whether the first reconcile has finished, i.e. whether the index can be trusted to know
    /// which notes link to which.
    pub reconciled: Box<dyn Fn() -> bool>,
    /// Save any dirty tab at or under these paths before the file moves under them, and reload
    /// the ones listed afterwards. Called with the notes a rename is about to rewrite, and with
    /// the folder a trash or a move is about to take: a path here is a subtree, not only a key.
    pub flush: Box<dyn Fn(&[String])>,
    /// Reload these paths' tabs from disk, returning how many were left alone because their
    /// buffer still holds unsaved edits (those get the changed-on-disk banner instead).
    pub reload: Box<dyn Fn(&[String]) -> usize>,
    /// Close every document at or under a path that has stopped existing — a folder in the trash
    /// takes the notes inside it. Only called once the file is really gone, so there is nothing
    /// left to write the buffer into and nothing to ask about.
    pub close: Box<dyn Fn(&str)>,
}

// --------------------------------------------------------------------------------- creating

/// New file in `dir` ("" is the vault root), from a template when the vault has any.
///
/// The name is created exactly as typed: `notes` is a file called `notes`, `main.rs` is a source
/// file, and only `notes.md` is a note. A template is markdown, so the picker is on screen only
/// while the typed name says the file will be one.
///
/// A name carrying `/` is a path relative to `dir`, exactly as it is in Rename, and the folders it
/// names are created with it. The line under the entry says where the file will really land.
pub fn new_file(ops: &Rc<Ops>, dir: &str) {
    let entry = name_entry("File name", "");
    let form = form();
    form.append(&vault_path_field(&entry, &ops.vault, dir));

    form.append(&name_preview(&entry, {
        let dir = dir.to_string();
        move |typed| typed_path(&dir, typed)
    }));

    let templates = ops.vault.templates().unwrap_or_default();
    let picker = template_picker(&templates);
    if let Some(picker) = &picker {
        let row = labelled("Template", picker);
        row.set_visible(false);
        entry.connect_changed({
            let row = row.clone();
            move |e| row.set_visible(is_markdown(e.text().trim()))
        });
        form.append(&row);
    }

    let dialog = name_dialog("New File", "Create", &form);
    let (ops, dir, window) = (ops.clone(), dir.to_string(), ops.window.clone());
    let typed = entry.clone();
    dialog.choose(Some(&window), gio::Cancellable::NONE, move |response| {
        if response != CONFIRM {
            return;
        }
        let rel = match typed_path(&dir, &typed.text()) {
            Ok(rel) => rel,
            Err(why) => return (ops.toast)(why),
        };
        let name = basename(&rel).to_string();
        // Read only where the picker is on screen, so a name that stopped being markdown after a
        // template was picked does not carry the leftover selection into a non-note file.
        let template = picker
            .as_ref()
            .filter(|_| is_markdown(&name))
            .and_then(|p| (p.selected() as usize).checked_sub(1))
            .and_then(|i| templates.get(i));
        if let Err(why) = make_parents(&ops, &rel) {
            return (ops.toast)(&why);
        }
        match ops.vault.create_note(&rel, template.map(String::as_str)) {
            Ok((created, stops)) => (ops.open)(&created, &stops),
            Err(e) if already_exists(&e) => (ops.toast)(&format!("{name} already exists")),
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
        // Asked of the vault and not of this disk: `root()` is a path on the *remote* host, so
        // the local `exists` there was always false and every clash went through as "Created".
        if ops.vault.exists(&rel) {
            return (ops.toast)(&format!("{name} already exists"));
        }
        match ops.vault.create_dir(&rel) {
            Ok(()) => (ops.toast)(&format!("Created {name}")),
            Err(e) => (ops.toast)(&format!("Cannot create {name}: {e}")),
        }
    });
    focus_name(&entry, None);
}

/// New note from a template that says where its notes go.
///
/// Only the templates carrying an `accent-target:` are listed: the rest have no destination to
/// create anything at, and New File is where they are picked with a name typed by hand. A target
/// naming a note that already exists opens it untouched, which is what makes a dated one a daily
/// note.
pub fn new_from_template(ops: &Rc<Ops>) {
    let templates: Vec<String> = ops
        .vault
        .templates()
        .unwrap_or_default()
        .into_iter()
        .filter(|t| matches!(ops.vault.template_target(t), Ok(Some(_))))
        .collect();
    if templates.is_empty() {
        let dir = ops.vault.config().templates_dir;
        return (ops.toast)(&format!(
            "No template says where its notes go. Add `accent-target:` to one in {dir}"
        ));
    }

    let labels: Vec<&str> = templates.iter().map(|t| basename(t)).collect();
    let picker = gtk::DropDown::from_strings(&labels);
    let form = form();
    form.append(&labelled("Template", &picker));

    let dialog = name_dialog("New from Template", "Create", &form);
    let (ops, window) = (ops.clone(), ops.window.clone());
    dialog.choose(Some(&window), gio::Cancellable::NONE, move |response| {
        if response != CONFIRM {
            return;
        }
        let Some(template) = templates.get(picker.selected() as usize) else {
            return;
        };
        let name = basename(template);
        match ops.vault.note_from_template(template) {
            Ok(Some((rel, stops))) => (ops.open)(&rel, &stops),
            // The file changed under the dialog; nothing was created, so nothing to undo.
            Ok(None) => (ops.toast)(&format!("{name} no longer says where its notes go")),
            Err(e) => (ops.toast)(&format!("Cannot create a note from {name}: {e:#}")),
        }
    });
}

/// Put rendered template text into the open note, its `{{cursor}}` stops as byte offsets.
type Insert = Box<dyn Fn(&str, &[usize])>;

/// Put a template into the open note at the caret: `title` is that note's stem, which is what
/// its `{{title}}` means here, and `insert` is handed the rendered text with its `{{cursor}}`
/// stops. Every template is offered, target or not; a Meeting is something typed into the day's
/// note, not a note of its own.
pub fn insert_template(ops: &Rc<Ops>, title: &str, insert: Insert) {
    let templates = ops.vault.templates().unwrap_or_default();
    if templates.is_empty() {
        let dir = ops.vault.config().templates_dir;
        return (ops.toast)(&format!("No templates in {dir}"));
    }
    let labels: Vec<&str> = templates.iter().map(|t| basename(t)).collect();
    let picker = gtk::DropDown::from_strings(&labels);
    let form = form();
    form.append(&labelled("Template", &picker));

    let dialog = name_dialog("Insert Template", "Insert", &form);
    let (ops, window, title) = (ops.clone(), ops.window.clone(), title.to_string());
    dialog.choose(Some(&window), gio::Cancellable::NONE, move |response| {
        if response != CONFIRM {
            return;
        }
        let Some(template) = templates.get(picker.selected() as usize) else {
            return;
        };
        match ops.vault.render_template(template, &title) {
            Ok((text, stops)) => insert(&text, &stops),
            Err(e) => (ops.toast)(&format!("Cannot insert {}: {e:#}", basename(template))),
        }
    });
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

/// Rename a note or folder, and move it: a typed name carrying `/` is a path relative to the
/// folder the file is in, `..` included, so `../Archive/note.md` moves the file as well as names
/// it. That is the keyboard's move — `F2`, and the only one an assistive technology can drive now
/// that Move to… is gone — and it goes through the same [`plan`] every other move does.
///
/// The name is used exactly as typed, as New File's is: `note.md` becomes `main.rs` if that is
/// what was asked for. A note that loses its `.md` stops being a note, which is the one rename
/// that asks first ([`confirm_demote`]). The folders a path names are created with it.
///
/// The extension starts outside the selection, so typing replaces the stem only, which is what
/// every file manager does.
pub fn rename(ops: &Rc<Ops>, rel: &str) {
    let current = basename(rel).to_string();
    let entry = name_entry("Name", &current);
    let form = form();
    form.append(&vault_path_field(&entry, &ops.vault, parent_dir(rel)));
    // Rename is the keyboard's move as well, so the line under the entry is where the file lands
    // rather than what it will be called: `../moved.md` says which folder that is.
    form.append(&name_preview(&entry, {
        let rel = rel.to_string();
        move |typed| renamed_path(&rel, typed)
    }));

    let dialog = name_dialog("Rename", "Rename", &form);
    let note = is_markdown(&current);
    let (ops, rel, window) = (ops.clone(), rel.to_string(), ops.window.clone());
    let typed = entry.clone();
    dialog.choose(Some(&window), gio::Cancellable::NONE, move |response| {
        if response != CONFIRM {
            return;
        }
        let to = match renamed_path(&rel, &typed.text()) {
            Ok(to) => to,
            Err(why) => return (ops.toast)(why),
        };
        if to == rel {
            return;
        }
        match note && !is_markdown(basename(&to)) {
            true => confirm_demote(&ops, &rel, &to),
            false => plan(&ops, &rel, &to, verb(&rel, &to)),
        }
    });
    let stem = split_ext(&current).0.chars().count() as i32;
    focus_name(&entry, Some(stem));
}

/// A note that loses its `.md` keeps its place in the vault and is still searched and opened, but
/// it stops being a note: no backlinks, and every `[[wikilink]]` pointing at it stops resolving.
///
/// It gets a dialog of its own because the Update Links one cannot cover it: `plan_rename`
/// compares stems, so a rename that changes only the extension finds nothing to rewrite and would
/// otherwise go through in silence.
fn confirm_demote(ops: &Rc<Ops>, from: &str, to: &str) {
    let dialog = alert(
        "No Longer a Note?",
        &format!(
            "{} stays in the vault and stays searchable, but links to it will no longer resolve.",
            basename(to)
        ),
        &[
            ("cancel", "Cancel", adw::ResponseAppearance::Default),
            ("rename", "Rename", adw::ResponseAppearance::Default),
        ],
        "cancel",
    );

    let (ops, from, to, window) = (
        ops.clone(),
        from.to_string(),
        to.to_string(),
        ops.window.clone(),
    );
    dialog.choose(Some(&window), gio::Cancellable::NONE, move |response| {
        if response == "rename" {
            plan(&ops, &from, &to, verb(&from, &to));
        }
    });
}

/// Move a note or folder into another directory of the same vault, keeping its name, from a drag
/// in the tree. A wikilink resolves by basename, so nothing pointing at it has to be rewritten —
/// but a folder full of notes still can be, which is why this goes through [`plan`] like the rest.
///
/// Where the drop may land at all is [`move_dest`]'s decision, taken while the pointer is still
/// moving so a row that cannot take what is over it never lights up. What is left to refuse here
/// is a name the destination already holds.
pub fn move_dropped(ops: &Rc<Ops>, from: &str, to: &str) {
    // Asked before the move rather than read off its error, as `new_folder` does: the error names
    // an absolute path, which is not what anyone dropped anything on.
    if ops.vault.exists(to) {
        return (ops.toast)(&format!("{} is already there", basename(to)));
    }
    plan(ops, from, to, "Moved");
}

/// Ask the vault what the move would touch, then either do it or confirm the link rewrites first.
///
/// The question is index reads and, on a remote vault, a round trip, so it is asked on a worker
/// thread the way [`download`] and [`upload`] send their bytes: a rename must not freeze the
/// window for as long as the host takes to answer.
fn plan(ops: &Rc<Ops>, from: &str, to: &str, verb: &'static str) {
    // `plan_rename` reads the backlinks out of the index, so during the first reconcile it finds
    // none — and an empty rewrite list is also what skips the confirmation dialog, so the rename
    // would go through in silence and break every wikilink pointing at the note. Rename and a
    // dropped row both land here, which is why the check sits at the top rather than in either.
    if !(ops.reconciled)() {
        return (ops.toast)("Still indexing, try again in a moment");
    }
    let (vault, from, to) = (ops.vault.clone(), from.to_string(), to.to_string());
    let ops = ops.clone();
    glib::spawn_future_local(async move {
        let planned = gio::spawn_blocking(move || vault.plan_rename(&from, &to)).await;
        match planned {
            Ok(Ok(plan)) if plan.rewrites.is_empty() => apply(&ops, plan, false, verb),
            Ok(Ok(plan)) => confirm_links(&ops, plan, verb),
            Ok(Err(e)) => (ops.toast)(&format!("Cannot rename: {e:#}")),
            Err(_) => (ops.toast)("Cannot rename"),
        }
    });
}

/// Rewriting other people's notes is a data-losing choice, so it is an `AlertDialog` and the
/// notes it would touch are named rather than counted.
fn confirm_links(ops: &Rc<Ops>, plan: RenamePlan, verb: &'static str) {
    let dialog = alert(
        "Update Links?",
        &link_body(&plan.rewrites),
        &[
            ("cancel", "Cancel", adw::ResponseAppearance::Default),
            ("keep", "Rename Only", adw::ResponseAppearance::Default),
            ("update", "Update Links", adw::ResponseAppearance::Suggested),
        ],
        "update",
    );

    let (ops, window) = (ops.clone(), ops.window.clone());
    dialog.choose(
        Some(&window),
        gio::Cancellable::NONE,
        move |response| match response.as_str() {
            "keep" => apply(&ops, plan, false, verb),
            "update" => apply(&ops, plan, true, verb),
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
fn apply(ops: &Rc<Ops>, plan: RenamePlan, update_links: bool, verb: &'static str) {
    // Rename is the keyboard's move and a typed path may name folders that are not there yet, so
    // they are made here — after the confirmation, so nothing exists until the move really
    // happens. A dropped row never reaches it: every destination the tree offers is a row that is
    // already there.
    if let Err(why) = make_parents(ops, &plan.to) {
        return (ops.toast)(&why);
    }
    // The note being moved is flushed with the ones about to be rewritten: its own tab is about
    // to point at a path that no longer exists, and an unsaved buffer must not be the casualty.
    let mut dirty = plan.rewrites.clone();
    dirty.push(plan.from.clone());
    (ops.flush)(&dirty);

    // The write itself is N notes rewritten, one fsync each, and on a remote vault a round trip
    // per note: the same worker thread the plan was made on.
    let (vault, to, ops) = (ops.vault.clone(), plan.to.clone(), ops.clone());
    glib::spawn_future_local(async move {
        let done = gio::spawn_blocking(move || vault.rename(&plan, update_links)).await;
        match done {
            Ok(Ok(report)) => {
                let unsaved = (ops.reload)(&report.rewritten);
                (ops.toast)(&rename_message(verb, &to, report.failed.len(), unsaved));
            }
            Ok(Err(e)) => (ops.toast)(&format!("Cannot rename: {e:#}")),
            Err(_) => (ops.toast)("Cannot rename"),
        }
    });
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
    // What lands in the trash should be what the user last saw, so every dirty tab under it is
    // written out before the file moves — a folder takes the notes inside it, and their unsaved
    // edits used to go with it in silence. Whatever cannot be written stays in its tab's banner.
    (ops.flush)(std::slice::from_ref(&rel));
    // A vault on another machine has no session bus to ask and no trash to ask it about, so the
    // only delete there is is the permanent one — which is exactly the case this already has a
    // dialog for, and it says so in the same words.
    if ops.vault.is_remote() {
        return confirm_delete(&ops, &name, &rel);
    }
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
                confirm_delete(&ops, &name, &rel)
            }
            Err(e) => (ops.toast)(&format!("Cannot trash {name}: {e}")),
        },
    );
}

/// Whether an open document's key goes with `trashed`: the path itself, or anything inside it
/// when a whole folder is deleted. The key of a file outside the vault is absolute and the key of
/// a diff or a terminal is not a path at all, so neither ever matches a vault-relative one.
///
/// An empty `trashed` is the vault root, which has no Move to Trash item and must never be read
/// as "everything": the guard is what keeps a bug there from closing every tab in the window.
pub fn trashed_with(trashed: &str, key: &str) -> bool {
    !trashed.is_empty()
        && key
            .strip_prefix(trashed)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// There is no Undo: `gio` has no untrash, so the toast never offers a button that cannot work
/// (NOTEPAD.md records it). Deleting for good is therefore asked about, every time.
fn confirm_delete(ops: &Rc<Ops>, name: &str, rel: &str) {
    let dialog = alert(
        "Delete Permanently?",
        &format!(
            "{name} cannot be moved to the trash{}. Deleting it cannot be undone.",
            match ops.vault.is_remote() {
                true => " on the remote",
                false => " on this system",
            }
        ),
        &[
            ("cancel", "Cancel", adw::ResponseAppearance::Default),
            ("delete", "Delete", adw::ResponseAppearance::Destructive),
        ],
        "cancel",
    );

    let (ops, name, rel, window) = (
        ops.clone(),
        name.to_string(),
        rel.to_string(),
        ops.window.clone(),
    );
    dialog.choose(Some(&window), gio::Cancellable::NONE, move |response| {
        if response != "delete" {
            return;
        }
        match ops.vault.delete(&rel) {
            Ok(()) => {
                (ops.close)(&rel);
                (ops.toast)(&format!("Deleted {name}"));
            }
            Err(e) => (ops.toast)(&format!("Cannot delete {name}: {e}")),
        }
    });
}

// ------------------------------------------------------------- clipboard and the file manager

/// Copy the file's own name, the extension included: `notes.md`, not `notes`. A folder's name is
/// copied the same way, being the same last segment of the path.
pub fn copy_name(ops: &Rc<Ops>, rel: &str) {
    ops.window.clipboard().set_text(basename(rel));
}

/// Copy the vault-relative path, which is what a wikilink and every accent path notation use.
///
/// No toast, here or in either of its neighbours: the clipboard is the feedback, and DESIGN.md
/// keeps toasts for what the user cannot otherwise see.
pub fn copy_relative_path(ops: &Rc<Ops>, rel: &str) {
    ops.window.clipboard().set_text(rel);
}

/// Copy the real filesystem path, for pasting into a terminal or another application. Written
/// out in full rather than `~`-abbreviated: a tilde is a shell convenience, not a path.
pub fn copy_absolute_path(ops: &Rc<Ops>, rel: &str) {
    let path = ops.vault.root().join(rel);
    ops.window.clipboard().set_text(&path.to_string_lossy());
}

/// Open the file manager on the containing folder with the file selected, through the portal.
pub fn show_in_files(ops: &Rc<Ops>, rel: &str) {
    let ops = ops.clone();
    let path = ops.vault.root().join(rel);
    reveal(&ops.window.clone(), &path, move |message| {
        (ops.toast)(message)
    });
}

/// Reveal `path` in the file manager.
///
/// Takes a path rather than a vault-relative one, and its own way of complaining, so a window
/// with no vault can still show where a loose file lives.
pub fn reveal(window: &adw::ApplicationWindow, path: &Path, toast: impl Fn(&str) + 'static) {
    let launcher = gtk::FileLauncher::new(Some(&gio::File::for_path(path)));
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    launcher.open_containing_folder(Some(window), gio::Cancellable::NONE, move |result| {
        // Only the failure is worth saying: a file manager that opened is its own report.
        if let Err(e) = result {
            toast(&format!("Cannot show {name}: {e}"));
        }
    });
}

/// The vault path of `rel` as GNOME writes it, `$HOME` abbreviated to `~`. Read-only decoration
/// for tooltips and labels; nothing opens a file by it.
pub fn display_path(root: &Path, rel: &str) -> String {
    let home = glib::home_dir();
    with_home(root, rel, Some(&home))
}

fn with_home(root: &Path, rel: &str, home: Option<&Path>) -> String {
    // `join("")` leaves a trailing separator behind, and the vault root is a real caller: the
    // tree builds every row's tooltip by appending to it.
    let path = match rel.is_empty() {
        true => root.to_path_buf(),
        false => root.join(rel),
    };
    crate::start::abbreviate(&path, home)
}

/// Make the folders a typed path names, `mkdir -p` style, so a destination that does not exist yet
/// is created the way New Folder would rather than reported as a bare OS error.
///
/// `Vault::create_dir` resolves through the same guard the typed path already passed, so a `../`
/// path that leaves the vault is still refused.
///
/// ponytail: `create_dir_all` is not transactional, so a failure part way leaves behind whatever
/// levels it did manage. The toast names the folder it stopped on, which is all New Folder offers
/// either; make it clean up after itself if that is ever seen.
fn make_parents(ops: &Ops, rel: &str) -> Result<(), String> {
    let dir = parent_dir(rel);
    if dir.is_empty() || ops.vault.exists(dir) {
        return Ok(());
    }
    ops.vault
        .create_dir(dir)
        .map_err(|e| format!("Cannot create {dir}: {e}"))
}

// --------------------------------------------------------------------------------- widgetry

/// The dim line under a name entry showing where the file will really land, vault-relative.
///
/// Both dialogs resolve a typed path, so the line says what the entry cannot: which folder
/// `../notes/x.md` walks out to, and which folder a plain name is created in. A path that does not
/// resolve leaves the line empty — the toast on Rename or Create is what says why.
fn name_preview(
    entry: &gtk::Entry,
    dest: impl Fn(&str) -> Result<String, &'static str> + 'static,
) -> gtk::Label {
    let preview = gtk::Label::builder()
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::Middle)
        .build();
    preview.add_css_class("dim-label");
    let show = {
        let preview = preview.clone();
        move |e: &gtk::Entry| preview.set_label(&dest(&e.text()).unwrap_or_default())
    };
    show(entry);
    entry.connect_changed(show);
    preview
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

/// A refresh that is only known once the field holding it exists, which is the knot a completion
/// answering later has to tie: the field is built from the completion, and the completion has to
/// be able to say "look again".
/// The path entry the two name dialogs share, completing against the vault's own listing.
///
/// `base` is the folder a typed path is relative to. Listings are asked for on a worker thread and
/// cached per folder, so a vault on another machine completes without a round trip on the
/// keystroke and a local one answers off the index; a folder that has not answered yet offers
/// nothing until it does.
fn vault_path_field(entry: &gtk::Entry, vault: &Arc<Vault>, base: &str) -> gtk::Widget {
    let folders: Rc<RefCell<HashMap<String, Option<Vec<String>>>>> =
        Rc::new(RefCell::new(HashMap::new()));

    path_field(entry, "Folders in this vault", {
        // Weak: the entry owns this closure through its own `changed` handler, so holding it
        // strongly would be a cycle that outlives the dialog.
        let asked = entry.downgrade();
        let (vault, base, folders) = (vault.clone(), base.to_string(), folders.clone());
        move |typed| {
            let Ok((dir, _)) = split_typed(&base, typed) else {
                return Vec::new();
            };
            let known = folders.borrow().get(&dir).cloned();
            if let Some(known) = known {
                return known.map_or_else(Vec::new, |f| completions(typed, &f));
            }
            // `None` while the listing is out, so it is asked for once however fast the typing is.
            folders.borrow_mut().insert(dir.clone(), None);
            let (vault, folders, asked) = (vault.clone(), folders.clone(), asked.clone());
            glib::spawn_future_local(async move {
                let listed = gio::spawn_blocking({
                    let (vault, dir) = (vault.clone(), dir.clone());
                    move || vault.list_dir(&dir)
                })
                .await;
                let names = match listed {
                    Ok(Ok(rows)) => folder_names(rows),
                    // Offering nothing beats offering a wrong list, which is what the tree says
                    // about a directory the index could not answer for either.
                    answer => {
                        tracing::debug!(dir, "no completions: {answer:?}");
                        Vec::new()
                    }
                };
                folders.borrow_mut().insert(dir, Some(names));
                if let Some(entry) = asked.upgrade() {
                    look_again(&entry);
                }
            });
            Vec::new()
        }
    })
}

/// The folder names directly inside a listing. Hidden ones are left out because the tree hides
/// them and a typed path refuses them anyway.
fn folder_names(rows: Vec<FileRow>) -> Vec<String> {
    rows.into_iter()
        .filter(|row| row.kind == FileKind::Dir)
        .map(|row| basename(&row.rel_path).to_string())
        .filter(|name| !name.starts_with('.'))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn trashed_with_takes_a_folder_but_not_its_neighbours() {
        assert!(trashed_with("a/b.md", "a/b.md"));
        assert!(!trashed_with("a/b.md", "a/c.md"));
        // A folder takes what is under it, however deep.
        assert!(trashed_with("Notes", "Notes"));
        assert!(trashed_with("Notes", "Notes/Daily/mon.md"));
        // Not a folder whose name merely starts the same way.
        assert!(!trashed_with("Notes", "Notestore/x.md"));
        // Absolute keys (a file outside the vault) and non-path keys never match.
        assert!(!trashed_with("Notes", "/home/me/Notes/x.md"));
        assert!(!trashed_with("Notes", "terminal:1"));
        // The vault root is never "everything".
        assert!(!trashed_with("", "a/b.md"));
    }

    #[test]
    fn with_home_abbreviates_the_vault_path_and_survives_an_empty_rel() {
        let home = Path::new("/home/me");
        let root = Path::new("/home/me/Vault");
        assert_eq!(with_home(root, "a/b.md", Some(home)), "~/Vault/a/b.md");
        // The vault root itself, which is what the tree's tooltip prefix is built from: joining
        // an empty rel path would otherwise leave a trailing slash on it.
        assert_eq!(with_home(root, "", Some(home)), "~/Vault");
        assert_eq!(
            with_home(Path::new("/mnt/Vault"), "", Some(home)),
            "/mnt/Vault"
        );
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
