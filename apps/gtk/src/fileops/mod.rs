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

pub(crate) mod clipboard;
mod menu;
mod paths;
mod transfer;

pub use clipboard::Clip;
#[cfg(feature = "bench")]
pub use menu::labels;
pub use menu::{context_menu, row_dir};
pub use paths::{move_dest, topmost};
#[cfg(feature = "bench")]
pub use transfer::download_to;
pub use transfer::{download, import, upload};

use self::paths::{
    already_exists, is_markdown, levels, moved_path, moves_to, renamed_part, renamed_path,
    split_typed, typed_dir, typed_path, verb,
};
use crate::dialogs::{
    CONFIRM, alert, choose, confirm, focus_entry, form, labelled, name_dialog, name_entry,
};
use crate::pathfield::{completions, look_again, path_field};
use accent_api::{FileKind, FileRow, RenamePlan, Vault};
use accent_core::path::{basename, linked_path, parent_dir};
use adw::prelude::*;
use gtk::{gio, glib};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

/// How many linking notes the rename dialog lists before it starts counting instead.
const LISTED: usize = 20;

/// The one line of an `anyhow` chain a toast has room for. `{e:#}` writes every layer of context
/// the call collected — three or four clauses by the time it reaches here — where what the reader
/// can act on is the root: "permission denied", "no space left on device".
fn why(e: &anyhow::Error) -> String {
    e.root_cause().to_string()
}

/// What a toast adds when a file lands where the tree does not list it, so that one just made or
/// renamed does not seem to have vanished.
const HIDDEN: &str = "it is hidden while Show Hidden Files is off";

/// Whether the tree leaves `rel` out right now: a dot-named path while Show Hidden Files is off,
/// read off the window's own toggle.
fn hidden_now(ops: &Ops, rel: &str) -> bool {
    let showing = ops
        .window
        .lookup_action("show-hidden-files")
        .and_then(|action| action.state())
        .and_then(|state| state.get::<bool>());
    showing == Some(false) && crate::tree::dot_named(rel)
}

/// Everything the operations need from the app, without depending on it.
// Boxed closures are the whole point of this struct; a type alias per field would only hide the
// signature the caller has to write anyway.
#[allow(clippy::type_complexity)]
pub struct Ops {
    pub vault: Arc<Vault>,
    pub window: adw::ApplicationWindow,
    pub toast: Box<dyn Fn(&str)>,
    /// Say in the status bar that a copy to or from the host is running (`true`) or over.
    pub transferring: Box<dyn Fn(&str, bool)>,
    /// Change a running copy's line in the status bar, from the first text to the second: a
    /// batch's count of the files it has sent.
    pub transfer_count: Box<dyn Fn(&str, &str)>,
    /// Open a note in a tab, putting the caret at the first of these byte offsets and making the
    /// rest Tab stops — which is where a template's `{{cursor}}`s land.
    pub open: Box<dyn Fn(&str, &[usize])>,
    /// Whether the first reconcile has finished, i.e. whether the index can be trusted to know
    /// which notes link to which.
    pub reconciled: Box<dyn Fn() -> bool>,
    /// Save any dirty tab at or under these paths before the file moves under them, and reload
    /// the ones listed afterwards. Called with the notes a rename is about to rewrite, and with
    /// the folder a trash or a move is about to take: a path here is a subtree, not only a key,
    /// and "" is the vault root, every tab.
    pub flush: Box<dyn Fn(&[String])>,
    /// Reload these paths' tabs from disk, returning how many were left alone because their
    /// buffer still holds unsaved edits (those get the changed-on-disk banner instead).
    pub reload: Box<dyn Fn(&[String]) -> usize>,
    /// Point every tab at or under the first path at the same place under the second: a move of
    /// our own, followed before the notes it rewrote are reloaded by the paths they have now.
    pub moved: Box<dyn Fn(&str, &str)>,
    /// Let the tree's marks go, after any move or trash: it may have taken what they named, and
    /// marks naming paths that are gone would come back on whatever takes those names next.
    pub unmark: Box<dyn Fn()>,
    /// Close every document at or under a path that has stopped existing — a folder in the trash
    /// takes the notes inside it. Only called once the file is really gone, so there is nothing
    /// left to write the buffer into and nothing to ask about.
    pub close: Box<dyn Fn(&str)>,
    /// Open a drawing that was just created and put the pen down in it, which is the whole point
    /// of having made it. Not [`Ops::open`]: that one waits for a text tab and reports a PDF as
    /// "not a text file".
    pub draw: Box<dyn Fn(&str)>,
    /// Add a directory to the vault's `[search] exclude` list, save it and refresh what search
    /// leaves out. Offered on directory rows alone.
    pub exclude: Box<dyn Fn(&str)>,
    /// Open in New Window: the file in a window of its own with no vault.
    pub apart: Box<dyn Fn(&str)>,
    /// Reload on a folder: list it and every folder open under it again, and bring the index up
    /// to date where it holds the folder.
    pub relist: Box<dyn Fn(&str)>,
    /// Dim these tree rows and undim the rest: what a Cut is waiting to move. Cleared with an
    /// empty slice by the paste that answers it.
    pub cut: Box<dyn Fn(&[String])>,
    /// What the last Cut or Copy in this window left. A local vault writes the real clipboard as
    /// well and reads a paste back off it, so this is a remote vault's whole clipboard and a
    /// local one's memory of which rows are dimmed. See [`clipboard`].
    pub clip: RefCell<Option<Clip>>,
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
    new_file_named(ops, dir, "");
}

/// Following a link to a note that is not there: [`new_file`] with the name already typed in.
///
/// The path is the link's own, from the vault root — `[[Notes/Foo]]` is `Notes/Foo.md` — and the
/// dialog's path field is how the reader puts it somewhere else instead.
pub fn new_linked_note(ops: &Rc<Ops>, target: &str) {
    new_file_named(ops, "", &linked_path(target));
}

/// [`new_file`] with `name` already in the entry, which is the one thing the two differ in.
fn new_file_named(ops: &Rc<Ops>, dir: &str, name: &str) {
    let (dir, name) = (dir.to_string(), name.to_string());
    with_templates(ops, false, move |ops, templates| {
        new_file_with(ops, &dir, &name, templates)
    });
}

/// [`new_file`] once the templates are in.
fn new_file_with(ops: &Rc<Ops>, dir: &str, name: &str, templates: Vec<String>) {
    let entry = name_entry("File name", name);
    let form = form();
    form.append(&vault_path_field(&entry, &ops.vault, dir));

    form.append(&name_preview(&entry, {
        let dir = dir.to_string();
        move |typed| typed_path(&dir, typed)
    }));

    let picker = template_picker(&templates);
    if let Some(picker) = &picker {
        let row = labelled("Template", picker);
        // A prefilled name may already say the file will be markdown, and nothing has changed yet.
        row.set_visible(is_markdown(name.trim()));
        entry.connect_changed({
            let row = row.clone();
            move |e| row.set_visible(is_markdown(e.text().trim()))
        });
        form.append(&row);
    }

    let dialog = name_dialog("New File", "Create", &form);
    let (ops, dir, window) = (ops.clone(), dir.to_string(), ops.window.clone());
    let typed = entry.clone();
    choose(&dialog, Some(&window), move |response| {
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
            .and_then(|i| templates.get(i))
            .cloned();
        // The folders and then the file, on a worker: each is a round trip on a remote vault.
        let vault = ops.vault.clone();
        glib::spawn_future_local(async move {
            let made = crate::work::off_thread("create", {
                let name = name.clone();
                move || {
                    make_parents(&vault, &rel)?;
                    vault.create_note(&rel, template.as_deref()).map_err(|e| {
                        match already_exists(&e) {
                            true => format!("Cannot create {name}: it already exists"),
                            false => format!("Cannot create {name}: {}", why(&e)),
                        }
                    })
                }
            })
            .await
            .unwrap_or_else(|| Err(format!("Cannot create {name}")));
            match made {
                Ok((created, stops)) => {
                    (ops.open)(&created, &stops);
                    // The tab is the report, unless the tree is not going to list what it holds.
                    if hidden_now(&ops, &created) {
                        (ops.toast)(&format!("Created {name}; {HIDDEN}"));
                    }
                }
                Err(why) => (ops.toast)(&why),
            }
        });
    });
    focus_name(&entry, None);
}

/// New folder inside `dir` ("" is the vault root).
///
/// A name carrying `/` is a path relative to `dir`, as it is in New File and in Rename, and the
/// levels it names are made with it — `Vault::create_dir` is `mkdir -p`. This was the one dialog
/// left refusing a slash, so `a/b` took two trips through it.
pub fn new_folder(ops: &Rc<Ops>, dir: &str) {
    let entry = name_entry("Folder name", "");
    let form = form();
    form.append(&vault_path_field(&entry, &ops.vault, dir));
    form.append(&name_preview(&entry, {
        let dir = dir.to_string();
        move |typed| typed_path(&dir, typed)
    }));

    let dialog = name_dialog("New Folder", "Create", &form);
    let (ops, dir, window) = (ops.clone(), dir.to_string(), ops.window.clone());
    let typed = entry.clone();
    choose(&dialog, Some(&window), move |response| {
        if response != CONFIRM {
            return;
        }
        let rel = match typed_path(&dir, &typed.text()) {
            Ok(rel) => rel,
            Err(why) => return (ops.toast)(why),
        };
        let name = basename(&rel).to_string();
        // Both questions on a worker, each being a round trip on a remote vault.
        let vault = ops.vault.clone();
        glib::spawn_future_local(async move {
            let made = crate::work::off_thread("create", {
                let (rel, name) = (rel.clone(), name.clone());
                move || {
                    // `create_dir_all` is happy to find the directory already there, so the
                    // collision the user cares about has to be asked about before the call rather
                    // than read off its error. Asked of the vault and not of this disk: `root()` is
                    // a path on the *remote* host, so the local `exists` there was always false and
                    // every clash went through as "Created".
                    if vault.exists(&rel) {
                        return Err(format!("Cannot create {name}: it already exists"));
                    }
                    vault
                        .create_dir(&rel)
                        .map_err(|e| made_what_it_could(&vault, &rel, &e.to_string()))
                }
            })
            .await
            .unwrap_or_else(|| Err(format!("Cannot create {name}")));
            match made {
                Ok(()) if hidden_now(&ops, &rel) => {
                    (ops.toast)(&format!("Created {name}; {HIDDEN}"))
                }
                Ok(()) => (ops.toast)(&format!("Created {name}")),
                Err(why) => (ops.toast)(&why),
            }
        });
    });
    focus_name(&entry, None);
}

/// New blank PDF in `dir` ("" is the vault root), to draw on rather than to read.
///
/// The size is picked here and fixed afterwards: a page is paper, and paper does not grow. When
/// a drawing runs off the end, Add Page After puts another page of the same size under it — which
/// is what makes the file a notebook every PDF reader shows correctly, rather than one growing
/// `/MediaBox` only we understand.
///
/// Local vaults only, for the reason the pen and Insert Sketch refuse on a remote one: a PDF
/// there is read from the ssh cache copy, so what was drawn would never reach the host. The tree
/// menu leaves the item out on a remote vault; this is the palette's way in, and it says why.
pub fn new_drawing(ops: &Rc<Ops>, dir: &str) {
    if ops.vault.is_remote() {
        return (ops.toast)("Open a local folder to create a drawing");
    }
    let current = free_drawing_name(&ops.vault, dir);
    let entry = name_entry("Drawing name", &current);
    let form = form();
    form.append(&vault_path_field(&entry, &ops.vault, dir));
    form.append(&name_preview(&entry, {
        let dir = dir.to_string();
        move |typed| typed_path(&dir, typed).map(drawing_path)
    }));
    let picker = gtk::DropDown::from_strings(SIZES);
    form.append(&labelled("Size", &picker));

    let dialog = name_dialog("New Drawing", "Create", &form);
    let (ops, dir, window) = (ops.clone(), dir.to_string(), ops.window.clone());
    let (typed, parent) = (entry.clone(), window.clone());
    choose(&dialog, Some(&parent), move |response| {
        if response != CONFIRM {
            return;
        }
        let rel = match typed_path(&dir, &typed.text()) {
            Ok(rel) => drawing_path(rel),
            Err(why) => return (ops.toast)(why),
        };
        let name = basename(&rel).to_string();
        // Read now rather than in the worker: the window is the main thread's.
        let size = drawing_size(
            picker.selected() as usize,
            (window.width(), window.height()),
        );
        // The folders, the document and the write, on a worker: making a PDF is pdfium work
        // behind the process-wide lock, which a render thread may be holding.
        let vault = ops.vault.clone();
        glib::spawn_future_local(async move {
            let made = crate::work::off_thread("create", {
                let (rel, name) = (rel.clone(), name.clone());
                move || {
                    if vault.exists(&rel) {
                        return Err(format!("Cannot create {name}: it already exists"));
                    }
                    make_parents(&vault, &rel)?;
                    let cannot = |e: String| format!("Cannot create {name}: {e}");
                    let bytes = accent_core::pdf::blank_pdf(size).map_err(|e| cannot(why(&e)))?;
                    let path = vault.resolve(&rel).map_err(|e| cannot(e.to_string()))?;
                    accent_core::fs::write_bytes(&path, &bytes, None)
                        .map(|_| ())
                        .map_err(|e| cannot(e.to_string()))
                }
            })
            .await
            .unwrap_or_else(|| Err(format!("Cannot create {name}")));
            match made {
                // The tab with the pen down is the report, unless the tree will not list the file.
                Ok(()) => {
                    (ops.draw)(&rel);
                    if hidden_now(&ops, &rel) {
                        (ops.toast)(&format!("Created {name}; {HIDDEN}"));
                    }
                }
                Err(why) => (ops.toast)(&why),
            }
        });
    });
    focus_name(
        &entry,
        Some(renamed_part(&current, false).chars().count() as i32),
    );
}

/// The page shapes New Drawing offers, in the order the dropdown lists them.
const SIZES: &[&str] = &["A4 Portrait", "A4 Landscape", "Square", "This Window"];

/// The size [`SIZES`]`[at]` names, in points. `window` is the window's size in pixels and only
/// the last shape reads it: A4's width with the window's own proportions, so one page is one
/// screenful of what the reader is looking at. An unmapped window (0 px either way) is A4.
fn drawing_size(at: usize, window: (i32, i32)) -> (f32, f32) {
    let (w, h) = accent_core::pdf::A4;
    match at {
        1 => (h, w),
        2 => (w, w),
        3 if window.0 > 0 && window.1 > 0 => (w, w * window.1 as f32 / window.0 as f32),
        _ => (w, h),
    }
}

/// A drawing is a PDF whatever it is called: the extension is what opens it in a PDF tab here
/// and in a reader everywhere else, so a typed name that leaves it out gains it.
fn drawing_path(rel: String) -> String {
    match Path::new(&rel)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("pdf"))
    {
        true => rel,
        false => format!("{rel}.pdf"),
    }
}

/// What the dialog opens on: the first `Drawing N.pdf` the folder does not already hold, so
/// Enter alone is another page every time.
fn free_drawing_name(vault: &Vault, dir: &str) -> String {
    (1..)
        .map(|n| format!("Drawing {n}.pdf"))
        .find(|name| typed_path(dir, name).is_ok_and(|rel| !vault.exists(&rel)))
        .unwrap_or_default()
}

/// New note from a template that says where its notes go.
///
/// Only the templates carrying an `accent-target:` are listed: the rest have no destination to
/// create anything at, and New File is where they are picked with a name typed by hand. A target
/// naming a note that already exists opens it untouched, which is what makes a dated one a daily
/// note.
pub fn new_from_template(ops: &Rc<Ops>) {
    with_templates(ops, true, new_from_template_with);
}

/// [`new_from_template`] once the templates that name a target are in.
fn new_from_template_with(ops: &Rc<Ops>, templates: Vec<String>) {
    if templates.is_empty() {
        let dir = ops.vault.config().templates_dir;
        return (ops.toast)(&format!(
            "No template has a destination. Add `accent-target:` to one in {dir}"
        ));
    }

    let labels: Vec<&str> = templates.iter().map(|t| basename(t)).collect();
    let picker = gtk::DropDown::from_strings(&labels);
    let form = form();
    form.append(&labelled("Template", &picker));

    let dialog = name_dialog("New from Template", "Create", &form);
    let (ops, window) = (ops.clone(), ops.window.clone());
    choose(&dialog, Some(&window), move |response| {
        if response != CONFIRM {
            return;
        }
        let Some(template) = templates.get(picker.selected() as usize).cloned() else {
            return;
        };
        let (name, vault) = (basename(&template).to_string(), ops.vault.clone());
        glib::spawn_future_local(async move {
            let made = crate::work::attempt(&format!("create a note from {name}"), move || {
                vault.note_from_template(&template).map_err(|e| why(&e))
            })
            .await;
            match made {
                Ok(Some((rel, stops))) => (ops.open)(&rel, &stops),
                // The file changed under the dialog; nothing was created, so nothing to undo.
                Ok(None) => (ops.toast)(&format!("{name} no longer has a destination")),
                Err(why) => (ops.toast)(&why),
            }
        });
    });
}

/// Put rendered template text into the open note, its `{{cursor}}` stops as byte offsets.
type Insert = Box<dyn Fn(&str, &[usize])>;

/// Put a template into the open note at the caret: `title` is that note's stem, which is what
/// its `{{title}}` means here, and `insert` is handed the rendered text with its `{{cursor}}`
/// stops. Every template is offered, target or not; a Meeting is something typed into the day's
/// note, not a note of its own.
pub fn insert_template(ops: &Rc<Ops>, title: &str, insert: Insert) {
    let title = title.to_string();
    with_templates(ops, false, move |ops, templates| {
        insert_template_with(ops, &title, insert, templates)
    });
}

/// Carry on with the vault's templates once a worker has them, all of them or only the ones that
/// name a target: on a remote vault the asking is a round trip, and the dialog waits for it
/// rather than the window.
fn with_templates(
    ops: &Rc<Ops>,
    targets_only: bool,
    then: impl FnOnce(&Rc<Ops>, Vec<String>) + 'static,
) {
    let (vault, ops) = (ops.vault.clone(), ops.clone());
    glib::spawn_future_local(async move {
        let listed = crate::work::off_thread("template list", move || match targets_only {
            true => vault.template_targets(),
            false => vault.templates(),
        })
        .await;
        then(&ops, listed.and_then(Result::ok).unwrap_or_default());
    });
}

/// [`insert_template`] once the templates are in.
fn insert_template_with(ops: &Rc<Ops>, title: &str, insert: Insert, templates: Vec<String>) {
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
    choose(&dialog, Some(&window), move |response| {
        if response != CONFIRM {
            return;
        }
        let Some(template) = templates.get(picker.selected() as usize).cloned() else {
            return;
        };
        let (name, vault) = (basename(&template).to_string(), ops.vault.clone());
        glib::spawn_future_local(async move {
            let rendered = crate::work::attempt(&format!("insert {name}"), move || {
                vault
                    .render_template(&template, &title)
                    .map_err(|e| why(&e))
            })
            .await;
            match rendered {
                Ok((text, stops)) => insert(&text, &stops),
                Err(why) => (ops.toast)(&why),
            }
        });
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
/// A file's extension starts outside the selection, so typing replaces the stem only, and a
/// folder's name is selected whole, which is what every file manager does.
pub fn rename(ops: &Rc<Ops>, rel: &str, is_dir: bool) {
    let note = is_markdown(basename(rel));
    let from = rel.to_string();
    path_dialog(ops, rel, is_dir, ("Rename", "Rename"), move |ops, to| {
        if to == from {
            return;
        }
        match note && !is_markdown(basename(&to)) {
            true => confirm_demote(ops, &from, &to),
            false => plan(ops, vec![(from.clone(), to.clone())], verb(&from, &to)),
        }
    });
}

/// Save As…'s dialog: Rename's, whose Save hands `then` the path typed. What is there already is
/// the caller's question.
pub fn save_as(ops: &Rc<Ops>, rel: &str, then: impl FnOnce(String) + 'static) {
    path_dialog(ops, rel, false, ("Save As", "Save"), move |_, to| then(to));
}

/// The dialog Rename and Save As share: `rel`'s name in the vault's path field, and `then` handed
/// the path it resolves to once `verb` is pressed.
fn path_dialog(
    ops: &Rc<Ops>,
    rel: &str,
    is_dir: bool,
    (title, verb): (&str, &str),
    then: impl FnOnce(&Rc<Ops>, String) + 'static,
) {
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

    let dialog = name_dialog(title, verb, &form);
    let (ops, rel, window) = (ops.clone(), rel.to_string(), ops.window.clone());
    let typed = entry.clone();
    choose(&dialog, Some(&window), move |response| {
        if response != CONFIRM {
            return;
        }
        match renamed_path(&rel, &typed.text()) {
            Ok(to) => then(&ops, to),
            Err(why) => (ops.toast)(why),
        }
    });
    let selected = renamed_part(&current, is_dir).chars().count() as i32;
    focus_name(&entry, Some(selected));
}

/// A note that loses its `.md` keeps its place in the vault and is still searched and opened, but
/// it stops being a note: it opens as plain text, and its own links and tags are no longer read.
///
/// It gets a dialog of its own because the Update Links one cannot cover it: the links that name
/// the note by its stem still find it, so there is nothing to rewrite and the rename would
/// otherwise go through in silence.
fn confirm_demote(ops: &Rc<Ops>, from: &str, to: &str) {
    let body = format!(
        "Links to {} keep pointing at it, but it opens as plain text: its own links and tags are no longer read.",
        basename(to)
    );
    let (ops, from, to, window) = (
        ops.clone(),
        from.to_string(),
        to.to_string(),
        ops.window.clone(),
    );
    // Not destructive: nothing is lost, the note simply stops being read as one.
    confirm(
        &window,
        "No Longer a Note?",
        &body,
        "Rename",
        false,
        move || {
            let verb = verb(&from, &to);
            plan(&ops, vec![(from.clone(), to.clone())], verb);
        },
    );
}

/// Move notes and folders into another directory of the same vault, keeping their names: a row
/// dragged in the tree, the marked set dragged with it, or a Cut pasted. However many there are,
/// it is one plan and one Update Links? question, because the links between them are rewritten
/// together.
///
/// Where a drop may land at all is [`move_dest`]'s decision, taken while the pointer is still
/// moving so a row that cannot take what is over it never lights up. What is left to refuse here
/// is a name the destination already holds, and a batch is refused whole for it rather than moved
/// in part.
pub fn move_all(ops: &Rc<Ops>, moves: Vec<(String, String)>) {
    let (vault, ops) = (ops.vault.clone(), ops.clone());
    glib::spawn_future_local(async move {
        // Asked before the move rather than read off its error, as `new_folder` does: the error
        // names an absolute path, which is not what anyone dropped anything on. On a worker, as
        // the plan is, being a `stat` each and a round trip each on a remote vault. Two files of
        // one name from two folders would take the same place, so the batch asks for it too.
        let dests: Vec<String> = moves.iter().map(|(_, to)| to.clone()).collect();
        let taken = crate::work::off_thread("move", move || {
            let mut seen = std::collections::HashSet::new();
            dests
                .into_iter()
                .find(|to| !seen.insert(to.clone()) || vault.exists(to))
        })
        .await;
        match taken {
            Some(None) => plan(&ops, moves, "Moved"),
            Some(Some(to)) => (ops.toast)(&format!(
                "Cannot move {}: it is already there",
                basename(&to)
            )),
            None => (ops.toast)(&format!("Cannot move {}", several(&sources(&moves)))),
        }
    });
}

/// Move to…: `rels` (each with whether it is a folder) into a folder typed into the vault's path
/// field, keeping their names — what a drop onto a folder does, for a folder that is not on screen
/// or not there yet. It opens on the folder the first of them is in; one that does not exist is
/// made with the move, once any Update Links? question is answered, as Rename makes the folders a
/// typed path names. Through [`move_all`], so a name the folder already holds refuses the batch.
pub fn move_to(ops: &Rc<Ops>, rels: Vec<(String, bool)>) {
    let rels: Vec<String> = topmost(&rels).into_iter().map(|(rel, _)| rel).collect();
    let Some(first) = rels.first().cloned() else {
        return;
    };
    let entry = name_entry("Vault root", parent_dir(&first));
    let form = form();
    form.append(&vault_path_field(&entry, &ops.vault, ""));
    let count = rels.len();
    form.append(&name_preview(&entry, move |typed| {
        typed_dir(typed).map(|dir| batch_to(&moved_path(&first, &dir), count))
    }));
    let title = match rels.as_slice() {
        [one] => format!("Move {}", basename(one)),
        many => format!("Move {} Items", many.len()),
    };
    let dialog = name_dialog(&title, "Move", &form);
    let (ops, window) = (ops.clone(), ops.window.clone());
    let typed = entry.clone();
    choose(&dialog, Some(&window), move |response| {
        if response != CONFIRM {
            return;
        }
        let dir = match typed_dir(&typed.text()) {
            Ok(dir) => dir,
            Err(why) => return (ops.toast)(why),
        };
        match moves_to(&rels, &dir) {
            Ok(moves) if moves.is_empty() => (ops.toast)(&format!(
                "Already in {}",
                match dir.as_str() {
                    "" => "the vault root",
                    dir => dir,
                }
            )),
            Ok(moves) => move_all(&ops, moves),
            Err(rel) => (ops.toast)(&format!("Cannot move {} into itself", basename(&rel))),
        }
    });
    // The caret at the end, where a folder inside this one is typed on.
    focus_entry(&entry, |entry| entry.set_position(-1));
}

/// Ask the vault what the moves would touch, then either do them or confirm the link rewrites
/// first.
///
/// The question is index reads and, on a remote vault, a round trip, so it is asked on a worker
/// thread the way [`download`] and [`upload`] send their bytes: a rename must not freeze the
/// window for as long as the host takes to answer.
fn plan(ops: &Rc<Ops>, moves: Vec<(String, String)>, verb: &'static str) {
    // `plan_moves` reads the backlinks out of the index, so during the first reconcile it finds
    // none — and an empty rewrite list is also what skips the confirmation dialog, so the rename
    // would go through in silence and break every wikilink pointing at the note. Rename and a
    // dropped row both land here, which is why the check sits at the top rather than in either.
    if !(ops.reconciled)() {
        return (ops.toast)("Cannot rename yet: the vault is still being indexed");
    }
    // Every unsaved buffer is written out first: the links are read off the disk, and so is
    // what a language server answers about the imports — positions in the file, not the tab.
    (ops.flush)(&[String::new()]);
    let (vault, ops, name) = (ops.vault.clone(), ops.clone(), several(&sources(&moves)));
    glib::spawn_future_local(async move {
        let planned = crate::work::attempt(&format!("rename {name}"), move || {
            vault.plan_moves(&moves).map_err(|e| why(&e))
        })
        .await;
        match planned {
            Ok(plan) if asks_nothing(&plan) => apply(&ops, plan, false, verb),
            Ok(plan) => confirm_update(&ops, plan, verb),
            Err(why) => (ops.toast)(&why),
        }
    });
}

/// Whether a plan goes through without a question: nothing to rewrite in a note and no import
/// to change.
fn asks_nothing(plan: &RenamePlan) -> bool {
    plan.rewrites.is_empty() && plan.imports.is_empty()
}

/// What an unchecked plan says, in the dialog or, with no dialog, in the toast.
const UNCHECKED: &str = "Imports not checked: no language server is running";

/// Rewriting other people's notes and code is a data-losing choice, so it is an `AlertDialog`
/// and the files it would touch are named rather than counted: the notes, then the source
/// files. One question and one Update for both, since a move is one thing to agree to.
fn confirm_update(ops: &Rc<Ops>, plan: RenamePlan, verb: &'static str) {
    let what = match (plan.rewrites.is_empty(), plan.imports.is_empty()) {
        (false, true) => "Links",
        (true, false) => "Imports",
        _ => "Links and Imports",
    };
    let update = format!("Update {what}");
    let dialog = alert(
        &format!("{update}?"),
        &update_body(&plan),
        &[
            ("cancel", "Cancel", adw::ResponseAppearance::Default),
            ("keep", "Rename Only", adw::ResponseAppearance::Default),
            ("update", &update, adw::ResponseAppearance::Suggested),
        ],
        "update",
    );

    let (ops, window) = (ops.clone(), ops.window.clone());
    choose(&dialog, Some(&window), move |response| {
        match response.as_str() {
            "keep" => apply(&ops, plan, false, verb),
            "update" => apply(&ops, plan, true, verb),
            _ => {}
        }
    });
}

/// Body of the update dialog: for the notes and then for the source files, the count, the
/// paths, and what it stopped listing; last, whether some imports went unchecked.
fn update_body(plan: &RenamePlan) -> String {
    let what = match plan.moves.len() {
        1 => "this one",
        _ => "these files",
    };
    let code: Vec<String> = plan.imports.iter().map(|f| f.rel.clone()).collect();
    let mut parts = Vec::new();
    if !plan.rewrites.is_empty() {
        let n = plan.rewrites.len();
        let count = match n {
            1 => format!("1 note links to {what}."),
            _ => format!("{n} notes link to {what}."),
        };
        parts.push(listed(count, &plan.rewrites));
    }
    if !code.is_empty() {
        let count = match code.len() {
            1 => format!("1 source file imports {what}."),
            n => format!("{n} source files import {what}."),
        };
        parts.push(listed(count, &code));
    }
    if !plan.unchecked.is_empty() {
        parts.push(format!("{UNCHECKED}."));
    }
    parts.join("\n\n")
}

/// A count, then the paths under it, then what it stopped listing.
fn listed(count: String, rels: &[String]) -> String {
    let mut body = count;
    body.push('\n');
    for rel in rels.iter().take(LISTED) {
        body.push('\n');
        body.push_str(rel);
    }
    if rels.len() > LISTED {
        body.push_str(&format!("\nand {} more", rels.len() - LISTED));
    }
    body
}

/// Move the files, then report. A partly rewritten vault is a real outcome, so the files that
/// could not be updated are said out loud instead of being logged and forgotten.
fn apply(ops: &Rc<Ops>, plan: RenamePlan, update: bool, verb: &'static str) {
    // What is being moved is flushed with the files about to be rewritten: their tabs are about
    // to point at paths that no longer exist, and an unsaved buffer must not be the casualty.
    let mut dirty = plan.rewrites.clone();
    dirty.extend(plan.imports.iter().map(|f| f.rel.clone()));
    dirty.extend(sources(&plan.moves));
    (ops.flush)(&dirty);
    // With no dialog to have said so, the toast says the imports went unchecked.
    let unchecked = asks_nothing(&plan) && !plan.unchecked.is_empty();

    // The write itself is N notes rewritten, one fsync each, and on a remote vault a round trip
    // per note: the same worker thread the plan was made on.
    let (vault, ops, name) = (
        ops.vault.clone(),
        ops.clone(),
        several(&sources(&plan.moves)),
    );
    let to = batch_to(&plan.moves[0].1, plan.moves.len());
    let first = plan.moves[0].1.clone();
    glib::spawn_future_local(async move {
        let done = crate::work::off_thread("rename", {
            let name = name.clone();
            move || {
                // Rename is the keyboard's move and a typed path may name folders that are not
                // there yet, so they are made here — after the confirmation, so nothing exists
                // until the move really happens. A dropped row never needs one: every destination
                // the tree offers is a row that is already there.
                for (_, to) in &plan.moves {
                    make_parents(&vault, to)?;
                }
                vault
                    .rename(&plan, update)
                    .map_err(|e| format!("Cannot rename {name}: {}", why(&e)))
            }
        })
        .await;
        match done {
            Some(Ok(report)) => {
                // Tabs follow first: a rewritten note is named by where it is now.
                for (from, to) in &report.moved {
                    (ops.moved)(from, to);
                }
                if !report.moved.is_empty() {
                    (ops.unmark)();
                }
                let unsaved = (ops.reload)(&report.rewritten);
                let mut message = match &report.not_moved {
                    Some((from, why)) => format!("Cannot move {}: {why}", basename(from)),
                    None => rename_message(verb, &name, &to, report.failed.len(), unsaved),
                };
                if hidden_now(&ops, &first) {
                    message.push_str(&format!("; {HIDDEN}"));
                }
                if unchecked {
                    message.push_str(&format!(". {UNCHECKED}"));
                }
                (ops.toast)(&message);
            }
            Some(Err(why)) => (ops.toast)(&why),
            None => (ops.toast)(&format!("Cannot rename {name}")),
        }
    });
}

/// What the toast says after a rename: where it went, what could not be rewritten, and what is
/// still showing the old text because its tab has unsaved edits.
fn rename_message(verb: &str, name: &str, to: &str, failed: usize, unsaved: usize) -> String {
    let mut message = match failed {
        0 => format!("{verb} {name} to {to}"),
        n => format!("{verb} {name}, but {n} files could not be updated"),
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
    trash_all(ops, vec![rel.to_string()]);
}

/// [`trash`] for several paths at once: one toast for all of them and, where there is no trash,
/// one question. The Git pane's Discard on a folder takes its untracked files away through this,
/// and a toast or a dialog per file would be one per file.
pub fn trash_all(ops: &Rc<Ops>, rels: Vec<String>) {
    // What lands in the trash should be what the user last saw, so every dirty tab under it is
    // written out before the file moves — a folder takes the notes inside it, and their unsaved
    // edits used to go with it in silence. Whatever cannot be written stays in its tab's banner.
    (ops.flush)(&rels);
    (ops.unmark)();
    // A vault on another machine has no session bus to ask and no trash to ask it about, so the
    // only delete there is is the permanent one — which is exactly the case this already has a
    // dialog for, and it says so in the same words.
    if ops.vault.is_remote() {
        return confirm_delete(ops, rels);
    }
    let ops = ops.clone();
    // ponytail: the tree and the index catch up through the watcher rather than being told here.
    // Post the removal explicitly if a trashed file is ever seen lingering in the sidebar.
    glib::spawn_future_local(async move {
        let (mut moved, mut refused) = (Vec::new(), Vec::new());
        for rel in rels {
            let file = gio::File::for_path(ops.vault.root().join(&rel));
            match file.trash_future(glib::Priority::DEFAULT).await {
                Ok(()) => {
                    (ops.close)(&rel);
                    moved.push(rel);
                }
                // What a sandbox without a working trash portal answers. There is nothing to fall
                // back to but a permanent delete, and that has to be asked about.
                Err(e) if e.matches(gio::IOErrorEnum::NotSupported) => refused.push(rel),
                Err(e) => (ops.toast)(&format!("Cannot trash {}: {e}", basename(&rel))),
            }
        }
        if !moved.is_empty() {
            (ops.toast)(&format!("Moved {} to Trash", several(&moved)));
        }
        if !refused.is_empty() {
            confirm_delete(&ops, refused);
        }
    });
}

/// The paths a batch of moves takes from.
fn sources(moves: &[(String, String)]) -> Vec<String> {
    moves.iter().map(|(from, _)| from.clone()).collect()
}

/// Where a batch of `count` files went, `first` being one's new path, as its toast names it: one
/// file by where it is now, several by the folder they all went into.
fn batch_to(first: &str, count: usize) -> String {
    match (count, parent_dir(first)) {
        (1, _) => first.to_string(),
        (_, "") => "the vault root".to_string(),
        (_, dir) => dir.to_string(),
    }
}

/// How a toast or a dialog names the paths it is about: a single one by its name, several by
/// their number.
fn several(rels: &[String]) -> String {
    match rels {
        [one] => basename(one).to_string(),
        many => format!("{} files", many.len()),
    }
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
fn confirm_delete(ops: &Rc<Ops>, rels: Vec<String>) {
    let body = format!(
        "{} cannot be moved to the trash{}. Deleting {} cannot be undone.",
        several(&rels),
        match ops.vault.is_remote() {
            true => " on the remote",
            false => " on this system",
        },
        match rels.len() {
            1 => "it",
            _ => "them",
        }
    );

    let (ops, window) = (ops.clone(), ops.window.clone());
    confirm(
        &window,
        "Delete Permanently?",
        &body,
        "Delete",
        true,
        move || {
            // A round trip per path on a remote vault, so on a worker; each tab closes once its file
            // is gone.
            let vault = ops.vault.clone();
            glib::spawn_future_local(async move {
                let done = crate::work::off_thread("delete", move || {
                    rels.into_iter()
                        .map(|rel| (vault.delete(&rel), rel))
                        .collect::<Vec<_>>()
                })
                .await;
                let Some(done) = done else {
                    return (ops.toast)("Cannot delete");
                };
                let mut deleted = Vec::new();
                for (answer, rel) in done {
                    match answer {
                        Ok(()) => {
                            (ops.close)(&rel);
                            deleted.push(rel);
                        }
                        Err(e) => (ops.toast)(&format!("Cannot delete {}: {e}", basename(&rel))),
                    }
                }
                if !deleted.is_empty() {
                    (ops.toast)(&format!("Deleted {}", several(&deleted)));
                }
            });
        },
    );
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
        // Only the failure is worth saying: a file manager that opened is its own report, and a
        // chooser the user closed was their answer.
        if let Err(e) = result
            && !declined(&e)
        {
            toast(&format!("Cannot show {name}: {e}"));
        }
    });
}

/// Whether a launcher's error is the user saying no rather than something failing. GTK reports
/// both ways of saying it in its own domain: the portal's chooser dismissed, and a cancelled call
/// (the portal and the `FileManager1` fallback turn `G_IO_ERROR_CANCELLED` into the latter).
fn declined(e: &glib::Error) -> bool {
    e.matches(gtk::DialogError::Dismissed) || e.matches(gtk::DialogError::Cancelled)
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
/// levels it did manage. They are named rather than cleaned up — deleting a directory because a
/// deeper one could not be made is the more dangerous of the two guesses, and one of the levels
/// may have been there all along.
pub(crate) fn make_parents(vault: &Vault, rel: &str) -> Result<(), String> {
    let dir = parent_dir(rel);
    if dir.is_empty() || vault.exists(dir) {
        return Ok(());
    }
    match vault.create_dir(dir) {
        Ok(()) => Ok(()),
        Err(e) => Err(made_what_it_could(vault, dir, &e.to_string())),
    }
}

/// What a path Save As was given holds already.
pub(crate) enum Taken {
    Free,
    File,
    Folder,
}

/// Asked of the vault, a round trip on a remote one: a stat, and for something that is there the
/// listing the tree is drawn from, since a stat answers for a folder as well.
pub(crate) fn taken(vault: &Vault, rel: &str) -> Taken {
    if !vault.exists(rel) {
        return Taken::Free;
    }
    let folder = vault.list_dir(parent_dir(rel)).is_ok_and(|rows| {
        rows.iter()
            .any(|row| row.rel_path == rel && row.kind == FileKind::Dir)
    });
    match folder {
        true => Taken::Folder,
        false => Taken::File,
    }
}

/// A failed `mkdir -p`, and the levels of it that are on disk now. Asked afterwards rather than
/// tracked as it went: the vault is the only thing that knows how far the call got.
fn made_what_it_could(vault: &Vault, dir: &str, why: &str) -> String {
    let made: Vec<&str> = levels(dir).filter(|level| vault.exists(level)).collect();
    match made.is_empty() {
        true => format!("Cannot create {dir}: {why}"),
        false => format!(
            "Cannot create {dir}: {why}; {} was created",
            made.join(", ")
        ),
    }
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

/// Put the caret in the entry, optionally selecting only the first `stem` characters. Called
/// after `choose` has presented the dialog, so the entry is in a window that can focus it.
fn focus_name(entry: &gtk::Entry, stem: Option<i32>) {
    match stem {
        Some(stem) => focus_entry(entry, move |entry| entry.select_region(0, stem)),
        None => {
            entry.grab_focus();
        }
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
                let listed = crate::work::off_thread("path completion", {
                    let (vault, dir) = (vault.clone(), dir.clone());
                    move || vault.list_dir(&dir)
                })
                .await;
                let names = match listed {
                    Some(Ok(rows)) => folder_names(rows),
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

/// The folder names directly inside a listing, dot-named ones included: a typed path may name them
/// whatever Show Hidden Files says. `.git` is never in a listing to begin with.
fn folder_names(rows: Vec<FileRow>) -> Vec<String> {
    rows.into_iter()
        .filter(|row| row.kind == FileKind::Dir)
        .map(|row| basename(&row.rel_path).to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dismissed_chooser_is_no_failure() {
        assert!(declined(&glib::Error::new(gtk::DialogError::Dismissed, "")));
        assert!(declined(&glib::Error::new(gtk::DialogError::Cancelled, "")));
        assert!(!declined(&glib::Error::new(gtk::DialogError::Failed, "")));
        assert!(!declined(&glib::Error::new(gio::IOErrorEnum::NotFound, "")));
    }

    #[test]
    fn drawing_size_is_a4_turned_four_ways() {
        let (w, h) = accent_core::pdf::A4;
        let window = (1600, 900);
        assert_eq!(drawing_size(0, window), (w, h));
        assert_eq!(drawing_size(1, window), (h, w));
        assert_eq!(drawing_size(2, window), (w, w));
        // The window's own proportions at A4's width: a landscape window, a landscape page.
        let (pw, ph) = drawing_size(3, window);
        assert_eq!(pw, w);
        assert!((ph / pw - 900.0 / 1600.0).abs() < 1e-4, "{pw}x{ph}");
        // A window with no size yet — the drill's, or one asked before it is mapped — is A4.
        assert_eq!(drawing_size(3, (0, 0)), (w, h));
    }

    #[test]
    fn a_drawing_is_named_pdf_whatever_was_typed() {
        assert_eq!(drawing_path("Notes/Sketch".into()), "Notes/Sketch.pdf");
        assert_eq!(drawing_path("Notes/Sketch.pdf".into()), "Notes/Sketch.pdf");
        assert_eq!(drawing_path("Notes/Sketch.PDF".into()), "Notes/Sketch.PDF");
        // Another extension is not an extension a PDF reader will open, so `.pdf` goes on top.
        assert_eq!(drawing_path("plan.v2".into()), "plan.v2.pdf");
    }

    #[test]
    fn rename_message_names_both_kinds_of_leftover() {
        // The subject is named in every shape, as "Moved {name} to Trash" names it.
        assert_eq!(
            rename_message("Renamed", "a.md", "b.md", 0, 0),
            "Renamed a.md to b.md"
        );
        assert_eq!(
            rename_message("Moved", "a.md", "x/b.md", 2, 0),
            "Moved a.md, but 2 files could not be updated"
        );
        assert_eq!(
            rename_message("Renamed", "a.md", "b.md", 0, 1),
            "Renamed a.md to b.md; 1 note has unsaved changes and was not reloaded"
        );
        assert_eq!(
            rename_message("Renamed", "a.md", "b.md", 2, 3),
            "Renamed a.md, but 2 files could not be updated; 3 notes have unsaved changes and were not reloaded"
        );
    }

    #[test]
    fn a_batch_is_named_by_its_path_or_by_the_folder_it_went_into() {
        assert_eq!(batch_to("Notes/a (copy).md", 1), "Notes/a (copy).md");
        assert_eq!(batch_to("Notes/a.md", 3), "Notes");
        assert_eq!(batch_to("a.md", 2), "the vault root");
    }

    #[test]
    fn several_names_one_path_and_counts_more() {
        assert_eq!(several(&["a/b.md".to_string()]), "b.md");
        assert_eq!(several(&["a.md".to_string(), "b/".to_string()]), "2 files");
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
    fn update_body_names_the_notes_then_the_code_then_what_went_unchecked() {
        let plan =
            |moves: usize, rewrites: &[&str], imports: &[&str], unchecked: &[&str]| RenamePlan {
                moves: vec![("a".to_string(), "b".to_string()); moves],
                rewrites: rewrites.iter().map(|r| r.to_string()).collect(),
                imports: imports
                    .iter()
                    .map(|rel| accent_api::FileEdits {
                        rel: rel.to_string(),
                        etag: accent_api::Etag {
                            mtime_ns: 0,
                            size: 0,
                            ino: 0,
                        },
                        edits: Vec::new(),
                    })
                    .collect(),
                unchecked: unchecked.iter().map(|r| r.to_string()).collect(),
            };
        assert_eq!(
            update_body(&plan(1, &["a.md"], &[], &[])),
            "1 note links to this one.\n\na.md"
        );

        let body = update_body(&plan(2, &["a.md", "b/c.md"], &["src/lib.rs"], &["x.rs"]));
        assert!(body.starts_with("2 notes link to these files."));
        assert!(body.contains("\nb/c.md\n\n1 source file imports these files.\n\nsrc/lib.rs"));
        assert!(body.ends_with("\n\nImports not checked: no language server is running."));
        assert!(!body.contains("more"));

        let many: Vec<String> = (0..25).map(|i| format!("n{i}.md")).collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        let body = update_body(&plan(1, &many, &[], &[]));
        assert!(body.contains("n19.md"));
        assert!(!body.contains("n20.md"));
        assert!(body.ends_with("and 5 more"));
    }
}
