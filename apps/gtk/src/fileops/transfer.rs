//! Copying files in and out of a vault on another machine. Bytes travel over ssh, so every one
//! of these runs on a worker thread, is said in the status bar while it does, and reports once,
//! when it is over.

use super::Ops;
use super::paths::{child_path, free_path};
use crate::dialogs::confirm;
use accent_core::path::basename;
use adw::prelude::*;
use gtk::{gio, glib};
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// How many file names an upload's toast or dialog spells out before it counts instead.
const NAMED: usize = 3;
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
        let busy = format!("Downloading {name}…");
        (ops.transferring)(&busy, true);
        // Bytes over ssh, so off the main thread: a large PDF would otherwise freeze the window
        // for as long as the copy takes.
        glib::spawn_future_local(async move {
            let done = gio::spawn_blocking(move || vault.download(&rel, &dest)).await;
            (ops.transferring)(&busy, false);
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
    let heading = match existing.len() {
        1 => "Replace File?",
        _ => "Replace Files?",
    };
    let (ops, dir, window) = (ops.clone(), dir.to_string(), ops.window.clone());
    confirm(
        &window,
        heading,
        &replace_body(existing),
        "Replace",
        true,
        move || send(&ops, &dir, chosen),
    );
}

/// Body of the "Replace Files?" dialog: what is already there, named, and that it cannot be got
/// back.
fn replace_body(existing: &[String]) -> String {
    match existing {
        [one] => format!("{one} is already in this folder. Replacing it cannot be undone."),
        many => format!(
            "{} of the chosen files are already in this folder: {}. Replacing them cannot be undone.",
            many.len(),
            listed_names(many)
        ),
    }
}

/// Send the chosen files, off the main thread, and report once.
fn send(ops: &Rc<Ops>, dir: &str, chosen: Vec<PathBuf>) {
    let (vault, dir, ops) = (ops.vault.clone(), dir.to_string(), ops.clone());
    let busy = busy_line("Uploading", &chosen);
    (ops.transferring)(&busy, true);
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
        (ops.transferring)(&busy, false);
        // Neither the tree nor the index is poked here: the watcher on the host reports what
        // landed, the same way it reports anything else written there.
        (ops.toast)(&match done {
            Ok((uploaded, failed)) => upload_message(uploaded, &failed),
            Err(_) => "Cannot upload".to_string(),
        });
    });
}

/// Carry files from this machine into `dir`, each under a name nothing there holds yet, and take
/// the originals away where they were cut.
///
/// What a paste of files the vault does not hold does, on a local vault as much as on a remote
/// one, and what a drag from another application onto the tree does: a file outside the vault has
/// to be copied in either way. It never replaces, where Upload Files… asks about it — an upload
/// has a chooser to ask in and a paste has nowhere to ask, so a name that is taken gets the same
/// `(copy)` mark an in-vault paste gets.
pub fn import(ops: &Rc<Ops>, dir: &str, files: Vec<PathBuf>, cut: bool) {
    let (vault, dir, ops) = (ops.vault.clone(), dir.to_string(), ops.clone());
    let busy = busy_line("Copying", &files);
    (ops.transferring)(&busy, true);
    glib::spawn_future_local(async move {
        let done = gio::spawn_blocking(move || {
            let (mut copied, mut refused) = (0, Vec::new());
            for file in &files {
                let Some(name) = local_name(file) else {
                    continue;
                };
                // A folder from out there would be a walk of this disk and a transfer per file in
                // it. `Vault::copy` takes one inside the vault; this side takes files, which is
                // what Upload Files… takes too.
                if file.is_dir() {
                    refused.push(name);
                    continue;
                }
                let to = free_path(&dir, &name, false, |rel| vault.exists(rel));
                match vault.upload(file, &to) {
                    // Cut means the file leaves where it was, and it is here now. A source that
                    // will not go is logged rather than toasted: the paste itself did land.
                    Ok(()) => {
                        copied += 1;
                        if cut && let Err(e) = std::fs::remove_file(file) {
                            tracing::warn!("{} stayed where it was: {e}", file.display());
                        }
                    }
                    Err(_) => refused.push(name),
                }
            }
            (copied, refused)
        })
        .await;
        (ops.transferring)(&busy, false);
        (ops.toast)(&match done {
            Ok((copied, refused)) => import_message(copied, &refused),
            Err(_) => "Cannot paste".to_string(),
        });
    });
}

/// What the toast says after a paste from outside the vault: how many landed, then the ones that
/// did not, by name — a folder among them being the likeliest reason.
fn import_message(copied: usize, refused: &[String]) -> String {
    if refused.is_empty() {
        return format!("Pasted {}", file_count(copied));
    }
    match copied {
        0 => format!("Cannot paste {}", listed_names(refused)),
        n => format!(
            "Pasted {}, but not {}",
            file_count(n),
            listed_names(refused)
        ),
    }
}

/// What the status bar says while a transfer runs: the file by name when there is one, otherwise
/// how many. The verb is the caller's — Upload Files… uploads, and a paste of files from outside
/// the vault copies them in, which on a local vault never leaves this machine.
fn busy_line(verb: &str, chosen: &[PathBuf]) -> String {
    let what = match chosen {
        [one] => local_name(one).unwrap_or_else(|| file_count(1)),
        many => file_count(many.len()),
    };
    format!("{verb} {what}…")
}

/// What the toast says after an upload: how many landed, then the ones that did not, by name.
/// Named rather than counted, because the user picked those files by hand and which of them to
/// try again is the only thing left to say.
fn upload_message(uploaded: usize, failed: &[String]) -> String {
    if failed.is_empty() {
        return format!("Uploaded {}", file_count(uploaded));
    }
    match uploaded {
        0 => format!("Cannot upload {}", listed_names(failed)),
        n => format!(
            "Uploaded {}, but not {}",
            file_count(n),
            listed_names(failed)
        ),
    }
}

/// A few names, then a count: enough to recognise which files are meant without a toast
/// growing to the width of the window.
fn listed_names(names: &[String]) -> String {
    let head = names[..names.len().min(NAMED)].join(", ");
    match names.len().saturating_sub(NAMED) {
        0 => head,
        rest => format!("{head} and {rest} more"),
    }
}

fn file_count(n: usize) -> String {
    match n {
        1 => "1 file".to_string(),
        n => format!("{n} files"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

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
    fn a_running_transfer_names_one_file_and_counts_several() {
        assert_eq!(
            busy_line("Uploading", &[PathBuf::from("/tmp/a.png")]),
            "Uploading a.png…"
        );
        let two = [PathBuf::from("/tmp/a.png"), PathBuf::from("/tmp/b.png")];
        assert_eq!(busy_line("Copying", &two), "Copying 2 files…");
    }

    #[test]
    fn import_message_counts_what_landed_and_names_what_did_not() {
        assert_eq!(import_message(2, &[]), "Pasted 2 files");
        assert_eq!(
            import_message(1, &["Folder".into()]),
            "Pasted 1 file, but not Folder"
        );
        assert_eq!(import_message(0, &["Folder".into()]), "Cannot paste Folder");
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
        assert_eq!(listed_names(&names[..1]), "f0.png");
        assert_eq!(listed_names(&names[..3]), "f0.png, f1.png, f2.png");
        assert_eq!(listed_names(&names), "f0.png, f1.png, f2.png and 2 more");
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
}
