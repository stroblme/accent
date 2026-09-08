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

use accent_api::{FileKind, FileRow, RenamePlan, Vault};
use adw::prelude::*;
use gtk::{gdk, gio, glib};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

/// The response id the three name dialogs confirm with.
pub(crate) const CONFIRM: &str = "confirm";
/// The action group the context menu's items resolve through, inserted on the tree widget.
const GROUP: &str = "fileops";
/// How many linking notes the rename dialog lists before it starts counting instead.
const LISTED: usize = 20;
/// How many file names an upload's toast or dialog spells out before it counts instead.
const NAMED: usize = 3;
/// How many folders a path entry offers at once before the list stops.
const COMPLETIONS: usize = 12;

/// Everything the operations need from the app, without depending on it.
// Boxed closures are the whole point of this struct; a type alias per field would only hide the
// signature the caller has to write anyway.
#[allow(clippy::type_complexity)]
pub struct Ops {
    pub vault: Arc<Vault>,
    pub window: adw::ApplicationWindow,
    pub toast: Box<dyn Fn(&str)>,
    /// Open a note in a tab.
    pub open: Box<dyn Fn(&str)>,
    /// Whether the first reconcile has finished, i.e. whether the index can be trusted to know
    /// which notes link to which.
    pub reconciled: Box<dyn Fn() -> bool>,
    /// Save any dirty tab for these paths before the file moves under them, and reload the ones
    /// listed afterwards. Called with the notes a rename is about to rewrite.
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
            Ok((created, _cursor)) => (ops.open)(&created),
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
    let dialog = adw::AlertDialog::new(
        Some("No Longer a Note?"),
        Some(&format!(
            "{} stays in the vault and stays searchable, but links to it will no longer resolve.",
            basename(to)
        )),
    );
    dialog.add_responses(&[("cancel", "Cancel"), ("rename", "Rename")]);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");

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
fn plan(ops: &Rc<Ops>, from: &str, to: &str, verb: &'static str) {
    // `plan_rename` reads the backlinks out of the index, so during the first reconcile it finds
    // none — and an empty rewrite list is also what skips the confirmation dialog, so the rename
    // would go through in silence and break every wikilink pointing at the note. Rename and a
    // dropped row both land here, which is why the check sits at the top rather than in either.
    if !(ops.reconciled)() {
        return (ops.toast)("Still indexing, try again in a moment");
    }
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
    let dialog = adw::AlertDialog::new(
        Some("Delete Permanently?"),
        Some(&format!(
            "{name} cannot be moved to the trash{}. Deleting it cannot be undone.",
            match ops.vault.is_remote() {
                true => " on the remote",
                false => " on this system",
            }
        )),
    );
    dialog.add_responses(&[("cancel", "Cancel"), ("delete", "Delete")]);
    dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");

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

// ------------------------------------------------------------------ downloading and uploading

/// Copy `rel` out of the vault to somewhere on this machine.
///
/// Offered on a remote vault only. On a local one the file is already on this disk, where the
/// file manager reaches it, so a chooser that copied it next to itself would be a way of making
/// a second copy rather than of getting at the first.
///
/// The chooser asks about replacing the file it is pointed at, so nothing here does.
pub fn download(ops: &Rc<Ops>, rel: &str) {
    let name = basename(rel).to_string();
    let dialog = gtk::FileDialog::builder()
        .title("Download")
        .initial_name(&name)
        .modal(true)
        .build();

    let (ops, rel, window) = (ops.clone(), rel.to_string(), ops.window.clone());
    dialog.save(Some(&window), gio::Cancellable::NONE, move |result| {
        // The error is almost always "the user closed the chooser", which needs no toast.
        let Some(dest) = result.ok().and_then(|f| f.path()) else {
            return;
        };
        let vault = ops.vault.clone();
        // Bytes over ssh, so off the main thread: a large PDF would otherwise freeze the window
        // for as long as the copy takes.
        glib::spawn_future_local(async move {
            let done = gio::spawn_blocking(move || vault.download(&rel, &dest)).await;
            (ops.toast)(&match done {
                Ok(Ok(())) => format!("Downloaded {name}"),
                Ok(Err(e)) => format!("Cannot download {name}: {e}"),
                Err(_) => format!("Cannot download {name}"),
            });
        });
    });
}

/// Copy files from this machine into `dir` ("" is the vault root). Remote vaults only, for the
/// same reason [`download`] is.
pub fn upload(ops: &Rc<Ops>, dir: &str) {
    let dialog = gtk::FileDialog::builder()
        .title("Upload Files")
        .modal(true)
        .build();

    let (ops, dir, window) = (ops.clone(), dir.to_string(), ops.window.clone());
    dialog.open_multiple(Some(&window), gio::Cancellable::NONE, move |result| {
        let Ok(chosen) = result else { return };
        let chosen: Vec<PathBuf> = chosen
            .iter::<gio::File>()
            .flatten()
            .filter_map(|file| file.path())
            .collect();
        if chosen.is_empty() {
            return;
        }
        let vault = ops.vault.clone();
        glib::spawn_future_local(async move {
            // The chooser could only ask about this machine's files, so what is already on the
            // host has to be asked about here — once, before anything is sent. Each answer is a
            // `stat` over ssh, so the asking happens on the worker with the copies.
            let checked = gio::spawn_blocking(move || {
                let existing = clashes(&dir, &chosen, |rel| vault.exists(rel));
                (dir, chosen, existing)
            })
            .await;
            let Ok((dir, chosen, existing)) = checked else {
                return (ops.toast)("Cannot upload");
            };
            match existing.is_empty() {
                true => send(&ops, &dir, chosen),
                false => confirm_replace(&ops, &dir, chosen, &existing),
            }
        });
    });
}

/// Which of `chosen` would land on something the vault already has.
///
/// Takes the lookup rather than the vault, so the partition can be decided without one.
fn clashes(dir: &str, chosen: &[PathBuf], exists: impl Fn(&str) -> bool) -> Vec<String> {
    chosen
        .iter()
        .filter_map(|file| local_name(file))
        .filter(|name| exists(&child_path(dir, name)))
        .collect()
}

/// What a chosen file will be called in the vault: its own name, in the folder that was clicked.
fn local_name(path: &Path) -> Option<String> {
    path.file_name().map(|n| n.to_string_lossy().into_owned())
}

/// Overwriting is the one thing an upload does that can lose data, so it is asked about
/// (DESIGN.md, States). Once for the batch rather than once per file: a chooser can return a
/// dozen paths, and a dozen dialogs is an obstacle rather than a question.
fn confirm_replace(ops: &Rc<Ops>, dir: &str, chosen: Vec<PathBuf>, existing: &[String]) {
    let dialog = adw::AlertDialog::new(
        Some(match existing.len() {
            1 => "Replace File?",
            _ => "Replace Files?",
        }),
        Some(&replace_body(existing)),
    );
    dialog.add_responses(&[("cancel", "Cancel"), ("replace", "Replace")]);
    dialog.set_response_appearance("replace", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");

    let (ops, dir, window) = (ops.clone(), dir.to_string(), ops.window.clone());
    dialog.choose(Some(&window), gio::Cancellable::NONE, move |response| {
        if response == "replace" {
            send(&ops, &dir, chosen);
        }
    });
}

/// Body of the "Replace Files?" dialog: what is already there, named, and that it cannot be got
/// back.
fn replace_body(existing: &[String]) -> String {
    match existing {
        [one] => format!("{one} is already in this folder. Replacing it cannot be undone."),
        many => format!(
            "{} of the chosen files are already in this folder: {}. Replacing them cannot be undone.",
            many.len(),
            listed(many)
        ),
    }
}

/// Send the chosen files, off the main thread, and report once.
fn send(ops: &Rc<Ops>, dir: &str, chosen: Vec<PathBuf>) {
    let (vault, dir, ops) = (ops.vault.clone(), dir.to_string(), ops.clone());
    glib::spawn_future_local(async move {
        let done = gio::spawn_blocking(move || {
            let (mut uploaded, mut failed) = (0, Vec::new());
            for file in &chosen {
                let Some(name) = local_name(file) else {
                    continue;
                };
                match vault.upload(file, &child_path(&dir, &name)) {
                    Ok(()) => uploaded += 1,
                    Err(_) => failed.push(name),
                }
            }
            (uploaded, failed)
        })
        .await;
        // Neither the tree nor the index is poked here: the watcher on the host reports what
        // landed, the same way it reports anything else written there.
        (ops.toast)(&match done {
            Ok((uploaded, failed)) => upload_message(uploaded, &failed),
            Err(_) => "Cannot upload".to_string(),
        });
    });
}

/// What the toast says after an upload: how many landed, then the ones that did not, by name.
/// Named rather than counted, because the user picked those files by hand and which of them to
/// try again is the only thing left to say.
fn upload_message(uploaded: usize, failed: &[String]) -> String {
    if failed.is_empty() {
        return format!("Uploaded {}", file_count(uploaded));
    }
    match uploaded {
        0 => format!("Cannot upload {}", listed(failed)),
        n => format!("Uploaded {}, but not {}", file_count(n), listed(failed)),
    }
}

fn file_count(n: usize) -> String {
    match n {
        1 => "1 file".to_string(),
        n => format!("{n} files"),
    }
}

/// A few names, then a count: enough to recognise which files are meant without a toast growing
/// to the width of the window.
fn listed(names: &[String]) -> String {
    let head = names[..names.len().min(NAMED)].join(", ");
    match names.len().saturating_sub(NAMED) {
        0 => head,
        rest => format!("{head} and {rest} more"),
    }
}

// ------------------------------------------------------------- clipboard and the file manager

/// Copy the vault-relative path, which is what a wikilink and every accent path notation use.
///
/// No toast: the clipboard is the feedback, and DESIGN.md keeps toasts for what the user cannot
/// otherwise see.
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

// ------------------------------------------------------------------------------ context menu

/// Right-click and Menu-key context menu for the file tree. `host` is the tree's outer box, and
/// `anchor` is where to point the popover in that box's coordinates.
///
/// `row` is the row that was clicked, as (path, is a directory), or `None` where the click landed
/// on no row at all — the blank area below the last one. Creating is offered in all three cases,
/// and everything else needs a path, so a menu opened over nothing holds the create items alone.
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
        menu.append_item(&item("Open", "open", rel));
    }
    // Everything that puts something in a folder shares one target, so a right-click anywhere in
    // the tree can create: in the folder clicked, beside the file clicked, or in the vault root.
    let dir = row_dir(row);
    menu.append_item(&item("New File", "new-file", dir));
    menu.append_item(&item("New Folder", "new-folder", dir));
    // Putting files in is only worth offering where they are not here already; a folder of a
    // local vault is one the file manager can be dropped onto.
    if ops.vault.is_remote() {
        menu.append_item(&item("Upload Files…", "upload", dir));
    }
    // Everything below names one file or folder, so none of it belongs on a menu opened over
    // blank space. Splitting is not here at all: it opens a note beside the active tab, which is
    // what the tab's own menu and `win.split-*` are for, not something done to a path.
    let Some((rel, is_dir)) = row else {
        return popup(host, &menu, anchor);
    };
    // Rename is the move as well as the name: a path typed into it carries the file, which is
    // what replaced Move to… when the tree learned to take a drop.
    menu.append_item(&item("Rename", "rename", rel));
    // Reading the path out and leaving the app are neither edits nor deletions, so they get a
    // section of their own between the two.
    let elsewhere = gio::Menu::new();
    elsewhere.append_item(&item("Copy Relative Path", "copy-rel", rel));
    elsewhere.append_item(&item("Copy Absolute Path", "copy-abs", rel));
    elsewhere.append_item(&item("Show in Files", "show", rel));
    // The other half of Show in Files when the file is on a host: getting a copy of it here is
    // the only way to reach it with anything but accent.
    if !is_dir && ops.vault.is_remote() {
        elsewhere.append_item(&item("Download…", "download", rel));
    }
    menu.append_section(None, &elsewhere);
    // Its own section, so the one destructive item is never next to Rename by accident.
    let danger = gio::Menu::new();
    danger.append_item(&item("Move to Trash", "trash", rel));
    menu.append_section(None, &danger);
    popup(host, &menu, anchor);
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

/// Hang the menu off `host` and show it.
fn popup(host: &gtk::Widget, menu: &gio::Menu, anchor: gdk::Rectangle) {
    let popover = gtk::PopoverMenu::from_model(Some(menu));
    popover.set_parent(host);
    popover.set_has_arrow(false);
    popover.set_pointing_to(Some(&anchor));
    // A popover parented by hand stays parented until it is unparented by hand — but not while it
    // is closing. `closed` is emitted from inside the item's own `clicked`, and an unparented
    // widget has no path to the action group on the host, so unparenting there dropped whatever
    // the click had just asked for — every item in this menu, not only the ones that open a
    // dialog, exactly as it dropped the status bar's Fit Page. The idle runs once the click is
    // over.
    popover.connect_closed(|p| {
        let p = p.clone();
        glib::idle_add_local_once(move || p.unparent());
    });
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
    add("new-file", Box::new(new_file));
    add("new-folder", Box::new(new_folder));
    add("rename", Box::new(rename));
    add("copy-rel", Box::new(copy_relative_path));
    add("copy-abs", Box::new(copy_absolute_path));
    add("show", Box::new(show_in_files));
    add("download", Box::new(download));
    add("upload", Box::new(upload));
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

/// `rel` moved into `dest_dir`, keeping its name.
fn moved_path(rel: &str, dest_dir: &str) -> String {
    child_path(dest_dir, basename(rel))
}

/// Where dropping `from` on a row of `dir` would put it, or `None` where there is no such move.
///
/// The three refusals are what a drag can ask for and a rename cannot: a folder onto itself, a
/// folder into something under it (which would move it inside its own new self), and a drop back
/// into the folder it is already in, which is a no-op and not worth a confirmation dialog. `dir`
/// is "" for the vault root.
pub fn move_dest(from: &str, dir: &str) -> Option<String> {
    let inside_itself = dir == from || dir.starts_with(&format!("{from}/"));
    if inside_itself || parent_dir(from) == dir {
        return None;
    }
    Some(moved_path(from, dir))
}

/// The folder a half-typed path points into and the last segment, which is the file's own name.
///
/// `base` is the folder the path is typed in: the file's own for Rename, the clicked row's for
/// New File. `..` walks back up out of it and stops at the vault root, and a segment starting with
/// a dot is refused because the tree hides one. The name comes back as typed, empty included, so
/// that completion can read a path that is still being written.
fn split_typed(base: &str, typed: &str) -> Result<(String, String), &'static str> {
    let typed = typed.trim();
    let (dirs, name) = typed.rsplit_once('/').unwrap_or(("", typed));
    let mut parts: Vec<&str> = base.split('/').filter(|s| !s.is_empty()).collect();
    // Empty segments and `.` mean nothing here, so `a//b` and `./a` are the paths they look like.
    for dir in dirs
        .split('/')
        .map(str::trim)
        .filter(|d| !d.is_empty() && *d != ".")
    {
        match dir {
            ".." => {
                parts.pop().ok_or("That path leaves this vault.")?;
            }
            dir if dir.starts_with('.') => return Err("Names cannot start with a dot."),
            dir => parts.push(dir),
        }
    }
    Ok((parts.join("/"), name.trim().to_string()))
}

/// Where a typed name puts the file, vault-relative: a plain name lands in `dir`, and one carrying
/// `/` is a path relative to it, `..` walking back up out of it. `Err` where the path would leave
/// the vault or name something the tree hides.
///
/// The extension is whatever was typed, in both dialogs. A note renamed out of `.md` stops being
/// one, which the dialog asks about rather than quietly preventing.
fn typed_path(dir: &str, typed: &str) -> Result<String, &'static str> {
    let (dest, name) = split_typed(dir, typed)?;
    if name.is_empty() || name == ".." {
        return Err("Enter a name.");
    }
    if name.starts_with('.') {
        return Err("Names cannot start with a dot.");
    }
    Ok(child_path(&dest, &name))
}

/// [`typed_path`] from the folder `rel` is in, which is what Rename types against.
fn renamed_path(rel: &str, typed: &str) -> Result<String, &'static str> {
    typed_path(parent_dir(rel), typed)
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

/// What the toast calls it: a file that stayed in its folder was renamed, one that left it moved.
fn verb(from: &str, to: &str) -> &'static str {
    match parent_dir(from) == parent_dir(to) {
        true => "Renamed",
        false => "Moved",
    }
}

/// Whether the file was already there, from an `anyhow` chain that has wrapped the `io::Error`.
fn already_exists(e: &anyhow::Error) -> bool {
    e.chain().any(|c| {
        c.downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == std::io::ErrorKind::AlreadyExists)
    })
}

// --------------------------------------------------------------------------------- widgetry

/// The shared shape of the name dialogs: Cancel, one verb, no OK button (DESIGN.md). Also what
/// the Git pane's Create Branch uses, which is why this and [`name_entry`] are crate-visible.
pub(crate) fn name_dialog(title: &str, verb: &str, form: &gtk::Box) -> adw::AlertDialog {
    let dialog = adw::AlertDialog::new(Some(title), None);
    dialog.set_extra_child(Some(form));
    dialog.add_responses(&[("cancel", "Cancel"), (CONFIRM, verb)]);
    dialog.set_response_appearance(CONFIRM, adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some(CONFIRM));
    dialog.set_close_response("cancel");
    dialog
}

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

pub(crate) fn name_entry(placeholder: &str, text: &str) -> gtk::Entry {
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

// ------------------------------------------------------------------------------- completion

/// The whole texts a half-typed path could be completed to: every name in `folders` that carries
/// on from the last segment, with the rest of the path kept in front of it and a `/` on the end so
/// the next segment can be typed straight away.
///
/// Folders only. The last segment is the file's own name, which is being invented rather than
/// looked up, so nothing can complete it and a file would only be a name to collide with.
pub(crate) fn completions(typed: &str, folders: &[String]) -> Vec<String> {
    let typed = typed.trim();
    let (head, leaf) = match typed.rsplit_once('/') {
        Some((_, leaf)) => (&typed[..typed.len() - leaf.len()], leaf),
        None => ("", typed),
    };
    let leaf = leaf.to_lowercase();
    folders
        .iter()
        .filter(|name| name.to_lowercase().starts_with(&leaf))
        .take(COMPLETIONS)
        .map(|name| format!("{head}{name}/"))
        .collect()
}

/// A path entry with the folders it could go into listed under it as it is typed.
///
/// GTK4 deprecated `GtkEntryCompletion` and shipped nothing in its place. A popover is the shape
/// `start::host_field` reaches for, but not here: a completion list stays up while the keyboard is
/// still in the entry, and in a dialog this small every popover GTK will fit lands on top of
/// Cancel and Rename. So the list is a revealer inside the form — it pushes the buttons down
/// instead of covering them, it is reachable by Tab, and it needs no grab, no hand-parenting and
/// no guessing about where there is room. The folder button beside the entry is the same list on
/// demand, for an entry nothing is typed in yet.
///
/// `complete` answers with whole texts the entry could hold, so every bit of path arithmetic stays
/// with the caller. It may answer with nothing while it is still finding out — a listing on a
/// worker thread, or a host that has not replied — and [`look_again`] is how it comes back once it
/// knows.
pub(crate) fn path_field(
    entry: &gtk::Entry,
    tooltip: &str,
    complete: impl Fn(&str) -> Vec<String> + 'static,
) -> gtk::Widget {
    let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
    // A deep vault is a list that scrolls rather than a dialog taller than the window.
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .max_content_height(160)
        .child(&list)
        .build();
    scroller.add_css_class("card");
    let revealer = gtk::Revealer::builder().child(&scroller).build();
    let button = gtk::ToggleButton::builder()
        .icon_name("folder-symbolic")
        .tooltip_text(tooltip)
        .build();
    button
        .bind_property("active", &revealer, "reveal-child")
        .bidirectional()
        .sync_create()
        .build();

    entry.connect_changed({
        let (list, button) = (list.clone(), button.clone());
        move |entry| {
            while let Some(row) = list.first_child() {
                list.remove(&row);
            }
            let text = entry.text();
            let offers = complete(&text);
            for candidate in &offers {
                let row = gtk::Button::builder()
                    .child(&gtk::Label::builder().label(candidate).xalign(0.0).build())
                    .build();
                row.add_css_class("flat");
                row.connect_clicked({
                    // Weak: the entry owns this list through its own handler, so a row holding it
                    // back would be a cycle that outlives the dialog.
                    let (asked, candidate) = (entry.downgrade(), candidate.clone());
                    move |_| {
                        // Setting the text rebuilds this very list, so it happens once the click
                        // is over — the same reason `popup` unparents its menu from an idle. The
                        // focus goes back first, so the rebuilt list is the new folder's.
                        let (asked, candidate) = (asked.clone(), candidate.clone());
                        glib::idle_add_local_once(move || {
                            if let Some(entry) = asked.upgrade() {
                                entry.grab_focus_without_selecting();
                                entry.set_text(&candidate);
                                entry.set_position(-1);
                            }
                        });
                    }
                });
                list.append(&row);
            }
            show_completions(&button, entry, offers.is_empty());
        }
    });

    let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    row.add_css_class("linked");
    row.append(entry);
    row.append(&button);
    // 6 px inside a control group, per DESIGN.md's spacing scale: the list belongs to the entry.
    let field = gtk::Box::new(gtk::Orientation::Vertical, 6);
    field.append(&row);
    field.append(&revealer);
    field.upcast()
}

/// Run a path entry's completion again, for an answer that arrived after the keystroke that asked
/// for it. The entry's own `changed` is the one path everything watching it already takes — the
/// list, and in the connect dialog the check that enables Connect — so a late answer needs no
/// second channel, and nothing has to hold a closure that would hold it back.
pub(crate) fn look_again(entry: &gtk::Entry) {
    entry.emit_by_name::<()>("changed", &[]);
}

/// `FOCUS_WITHIN` rather than `has_focus`, which is always false here: a GTK4 `GtkEntry` is a
/// wrapper whose inner `GtkText` is the widget that actually takes the keyboard.
pub(crate) fn typing_here(entry: &gtk::Entry) -> bool {
    entry.state_flags().contains(gtk::StateFlags::FOCUS_WITHIN)
}

/// The list shows itself in answer to typing, not over a dialog nobody has touched: it opens only
/// while the entry has the keyboard and holds something to complete. An entry that is still empty
/// keeps its folders behind the button, which is insensitive when there are none.
fn show_completions(button: &gtk::ToggleButton, entry: &gtk::Entry, empty: bool) {
    button.set_sensitive(!empty);
    button.set_active(!empty && typing_here(entry) && !entry.text().trim().is_empty());
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
    use std::path::PathBuf;

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
    fn row_dir_answers_for_all_three_kinds_of_row() {
        assert_eq!(row_dir(Some(("Notes/Daily", true))), "Notes/Daily");
        assert_eq!(row_dir(Some(("Notes/Daily/mon.md", false))), "Notes/Daily");
        // A file at the vault root, and no row at all: both the root.
        assert_eq!(row_dir(Some(("todo.md", false))), "");
        assert_eq!(row_dir(None), "");
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
    fn move_dest_refuses_the_three_moves_a_drop_can_ask_for() {
        // Into a folder, and out to the vault root.
        assert_eq!(move_dest("a/b.md", "x"), Some("x/b.md".into()));
        assert_eq!(move_dest("a/deep/b.md", ""), Some("b.md".into()));
        assert_eq!(move_dest("a/Notes", "x"), Some("x/Notes".into()));
        // Onto itself, and into what is under it.
        assert_eq!(move_dest("a/Notes", "a/Notes"), None);
        assert_eq!(move_dest("a/Notes", "a/Notes/Daily"), None);
        // Into the folder it is already in, which includes the root row over a root-level file.
        assert_eq!(move_dest("a/b.md", "a"), None);
        assert_eq!(move_dest("b.md", ""), None);
        // A folder whose name merely starts the same way is a different folder.
        assert_eq!(
            move_dest("a/Notes", "a/Notestore"),
            Some("a/Notestore/Notes".into())
        );
    }

    #[test]
    fn renamed_path_renames_in_place_and_moves_on_a_slash() {
        let to = |typed| renamed_path("Notes/Daily/mon.md", typed);
        assert_eq!(to("tue.md"), Ok("Notes/Daily/tue.md".into()));
        assert_eq!(
            to("Archive/tue.md"),
            Ok("Notes/Daily/Archive/tue.md".into())
        );
        // `..` walks up, which is the only way the keyboard reaches the vault root.
        assert_eq!(to("../tue.md"), Ok("Notes/tue.md".into()));
        assert_eq!(to("../../tue.md"), Ok("tue.md".into()));
        assert_eq!(to("../Archive/tue.md"), Ok("Notes/Archive/tue.md".into()));
        // One `..` too many leaves the vault, and so does one from a file already at the root.
        assert!(to("../../../tue.md").is_err());
        assert!(renamed_path("mon.md", "../tue.md").is_err());
        assert_eq!(renamed_path("a/x.pdf", "b/y.pdf"), Ok("a/b/y.pdf".into()));
        // Nothing typed, nothing but separators, and a hidden name at either end.
        assert!(to("").is_err());
        assert!(to("  ").is_err());
        assert!(to("..").is_err());
        assert!(to(".hidden.md").is_err());
        assert!(to(".config/tue.md").is_err());
    }

    #[test]
    fn renamed_path_lands_on_the_extension_that_was_typed() {
        let to = |typed| renamed_path("Notes/mon.md", typed);
        // Rename used to force `.md` back on, so a note could not become a source file.
        assert_eq!(to("main.rs"), Ok("Notes/main.rs".into()));
        // A bare name stays extensionless rather than becoming a note again.
        assert_eq!(to("notes"), Ok("Notes/notes".into()));
        assert_eq!(to("tue.md"), Ok("Notes/tue.md".into()));
        // A folder called `Notes` was never at risk, and still is not.
        assert_eq!(renamed_path("Notes", "Archive"), Ok("Archive".into()));
    }

    #[test]
    fn typed_path_names_a_folder_that_does_not_exist_yet() {
        // The dialog resolves the destination and `make_parents` creates what is missing; the
        // path arithmetic is the same whether the folder is there or not.
        assert_eq!(renamed_path("a.md", "New/x.md"), Ok("New/x.md".to_string()));
        assert_eq!(
            renamed_path("Notes/a.md", "New/Deep/x.md"),
            Ok("Notes/New/Deep/x.md".to_string())
        );
        // New File types against the clicked row's folder instead of the file's own.
        assert_eq!(
            typed_path("Notes", "Inbox/x.md"),
            Ok("Notes/Inbox/x.md".to_string())
        );
        assert_eq!(typed_path("", "x.md"), Ok("x.md".to_string()));
        assert!(typed_path("", "../x.md").is_err());
    }

    #[test]
    fn completions_offer_folders_and_keep_the_path_in_front_of_them() {
        let folders: Vec<String> = ["Archive", "Attachments", "Notes", "notes-old"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        // The last segment is what is being completed; a `/` says the next one is starting.
        assert_eq!(completions("Arc", &folders), ["Archive/".to_string()]);
        assert_eq!(completions("At", &folders), ["Attachments/".to_string()]);
        // Case-insensitive, and every match is offered.
        assert_eq!(
            completions("NOT", &folders),
            ["Notes/".to_string(), "notes-old/".to_string()]
        );
        // The path already typed is kept, so a click leaves a whole path in the entry.
        assert_eq!(
            completions("../Deep/Arc", &folders),
            ["../Deep/Archive/".to_string()]
        );
        assert_eq!(completions("Deep/", &folders).len(), folders.len());
        // Nothing typed offers every folder; a name nothing starts with offers none.
        assert_eq!(completions("", &folders).len(), folders.len());
        assert!(completions("zzz", &folders).is_empty());
        // A folder whose name was typed in full still earns its trailing slash, beside anything
        // else that carries on from it.
        assert_eq!(
            completions("Notes", &folders),
            ["Notes/".to_string(), "notes-old/".to_string()]
        );
    }

    #[test]
    fn moved_path_keeps_the_basename() {
        assert_eq!(moved_path("a/b/c.md", "x/y"), "x/y/c.md");
        assert_eq!(moved_path("a/b/c.md", ""), "c.md");
        assert_eq!(moved_path("c.md", "x"), "x/c.md");
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
    fn clashes_names_only_what_the_vault_already_has() {
        let files = [
            PathBuf::from("/tmp/a.png"),
            PathBuf::from("/tmp/b.png"),
            PathBuf::from("/tmp/c.png"),
        ];
        let have = |rel: &str| rel == "Media/a.png" || rel == "Media/c.png";
        assert_eq!(clashes("Media", &files, have), ["a.png", "c.png"]);
        // The same names one folder over collide with nothing.
        assert!(clashes("Other", &files, have).is_empty());
        // "" is the vault root, and must not become a leading slash.
        assert_eq!(clashes("", &files, |rel| rel == "b.png"), ["b.png"]);
    }

    #[test]
    fn upload_message_counts_what_landed_and_names_what_did_not() {
        assert_eq!(upload_message(1, &[]), "Uploaded 1 file");
        assert_eq!(upload_message(3, &[]), "Uploaded 3 files");
        assert_eq!(
            upload_message(2, &["a.png".into()]),
            "Uploaded 2 files, but not a.png"
        );
        assert_eq!(
            upload_message(0, &["a.png".into(), "b.png".into()]),
            "Cannot upload a.png, b.png"
        );
    }

    #[test]
    fn listed_names_a_few_then_counts_the_rest() {
        let names: Vec<String> = (0..5).map(|i| format!("f{i}.png")).collect();
        assert_eq!(listed(&names[..1]), "f0.png");
        assert_eq!(listed(&names[..3]), "f0.png, f1.png, f2.png");
        assert_eq!(listed(&names), "f0.png, f1.png, f2.png and 2 more");
    }

    #[test]
    fn replace_body_says_what_is_already_there() {
        assert_eq!(
            replace_body(&["a.png".into()]),
            "a.png is already in this folder. Replacing it cannot be undone."
        );
        let body = replace_body(&["a.png".into(), "b.png".into()]);
        assert!(
            body.starts_with("2 of the chosen files are already in this folder: a.png, b.png.")
        );
        assert!(body.ends_with("cannot be undone."));
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
