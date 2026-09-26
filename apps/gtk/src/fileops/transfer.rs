//! Copying files in and out of a vault on another machine. Bytes travel over ssh, so every one
//! of these runs on a worker thread, is said in the status bar while it does, and reports once,
//! when it is over.

use super::paths::{child_path, free_path};
use super::{Ops, batch_to};
use crate::dialogs::confirm;
use accent_core::path::basename;
use adw::prelude::*;
use gtk::{gio, glib};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

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
            let done = crate::work::attempt(&format!("download {name}"), move || {
                vault.download(&rel, &dest)
            })
            .await;
            (ops.transferring)(&busy, false);
            (ops.toast)(&match done {
                Ok(()) => format!("Downloaded {name}"),
                Err(why) => why,
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
            let checked = crate::work::off_thread("upload", move || {
                let existing = clashes(&dir, &chosen, |rel| vault.exists(rel));
                (dir, chosen, existing)
            })
            .await;
            let Some((dir, chosen, existing)) = checked else {
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
    let count = Arc::new(Count::default());
    count.total.store(chosen.len(), Ordering::Relaxed);
    let line = counting("Uploading", &chosen);
    glib::spawn_future_local(async move {
        let work = crate::work::off_thread("upload", {
            let count = count.clone();
            move || {
                let (mut uploaded, mut failed) = (0, Vec::new());
                for file in &chosen {
                    let Some(name) = local_name(file) else {
                        continue;
                    };
                    match vault.upload(file, &child_path(&dir, &name)) {
                        Ok(()) => uploaded += 1,
                        Err(_) => failed.push(name),
                    }
                    count.done.fetch_add(1, Ordering::Relaxed);
                }
                (uploaded, failed)
            }
        });
        let done = counted(&ops, line, &count, work).await;
        // Neither the tree nor the index is poked here: the watcher on the host reports what
        // landed, the same way it reports anything else written there.
        (ops.toast)(&match done {
            Some((uploaded, failed)) => upload_message(uploaded, &failed),
            None => "Cannot upload".to_string(),
        });
    });
}

/// Carry files and folders from this machine into `dir`, each under a name nothing there holds
/// yet, and take the originals away where they were cut.
///
/// What a paste of files the vault does not hold does, on a local vault as much as on a remote
/// one, and what a drag from another application onto the tree does: a file outside the vault has
/// to be copied in either way. It never replaces, where Upload Files… asks about it — an upload
/// has a chooser to ask in and a paste has nowhere to ask, so a name that is taken gets the same
/// `(copy)` mark an in-vault paste gets. A folder is walked on this disk and made again in the
/// vault, file by file ([`Carry`]).
pub fn import(ops: &Rc<Ops>, dir: &str, files: Vec<PathBuf>, cut: bool) {
    let (vault, dir, ops) = (ops.vault.clone(), dir.to_string(), ops.clone());
    let count = Arc::new(Count::default());
    let line = counting("Copying", &files);
    glib::spawn_future_local(async move {
        let work = crate::work::off_thread("copy", {
            let count = count.clone();
            move || {
                // Every name and every walk first, so the count has its total before the first
                // file goes. A name is taken once chosen, or two pasted files of one name would
                // both be given it.
                let mut claimed = Vec::new();
                let mut carries = Vec::new();
                for file in &files {
                    let Some(name) = local_name(file) else {
                        continue;
                    };
                    let is_dir = file.is_dir();
                    let to = free_path(&dir, &name, is_dir, |rel| {
                        claimed.iter().any(|c| c == rel) || vault.exists(rel)
                    });
                    claimed.push(to.clone());
                    carries.push((Carry::of(file, &to), to, is_dir));
                }
                let total = carries.iter().map(|(carry, ..)| carry.files.len()).sum();
                count.total.store(total, Ordering::Relaxed);
                let mut report = Imported::default();
                for (carry, to, is_dir) in carries {
                    let (sent, failed) = carry.send(
                        cut,
                        |rel| vault.create_dir(rel),
                        |from, rel| {
                            let sent = vault.upload(from, rel);
                            count.done.fetch_add(1, Ordering::Relaxed);
                            sent
                        },
                    );
                    if !failed.contains(&to) {
                        report.landed.push(to);
                        report.folders |= is_dir;
                    }
                    report.files += sent;
                    // Named from the folder pasted into, which the toast has already said.
                    let inside = |rel: String| match rel.strip_prefix(&format!("{dir}/")) {
                        Some(rest) => rest.to_string(),
                        None => rel,
                    };
                    report.failed.extend(failed.into_iter().map(inside));
                }
                report
            }
        });
        let done = counted(&ops, line, &count, work).await;
        (ops.toast)(&match done {
            Some(report) => report.message(cut),
            None => "Cannot paste".to_string(),
        });
    });
}

/// How far a batch has got, counted on the worker and read on the main loop: the files tried so
/// far, and how many there are, 0 until the walk has counted them.
#[derive(Default)]
struct Count {
    done: AtomicUsize,
    total: AtomicUsize,
}

/// How often a running batch's count on the status bar is brought up to date.
const TICK: Duration = Duration::from_millis(200);

/// What the status bar says for a batch of `chosen` while `count` is `(done, total)`: that it
/// runs ("Copying Photos…"), and from two files on how far it has got, as indexing counts
/// ("Copying Photos… 12/120 files", "Uploading… 3/12 files").
fn counting(verb: &str, chosen: &[PathBuf]) -> impl Fn(usize, usize) -> String + 'static {
    let (verb, chosen) = (verb.to_string(), chosen.to_vec());
    move |done, total| match (chosen.as_slice(), total) {
        (_, 0 | 1) => busy_line(&verb, &chosen),
        ([one], total) => format!(
            "{verb} {}… {done}/{total} files",
            local_name(one).unwrap_or_default()
        ),
        (_, total) => format!("{verb}… {done}/{total} files"),
    }
}

/// Wait for `work`, its line on the status bar from start to end, kept to what `count` says every
/// [`TICK`]. The timer goes with the work, which is what ends it.
async fn counted<T>(
    ops: &Rc<Ops>,
    line: impl Fn(usize, usize) -> String + 'static,
    count: &Arc<Count>,
    work: impl std::future::Future<Output = T>,
) -> T {
    let shown = Rc::new(RefCell::new(line(0, 0)));
    (ops.transferring)(&shown.borrow(), true);
    let tick = glib::timeout_add_local(TICK, {
        let (ops, shown, count) = (ops.clone(), shown.clone(), count.clone());
        move || {
            let next = line(
                count.done.load(Ordering::Relaxed),
                count.total.load(Ordering::Relaxed),
            );
            if *shown.borrow() != next {
                (ops.transfer_count)(&shown.borrow(), &next);
                *shown.borrow_mut() = next;
            }
            glib::ControlFlow::Continue
        }
    });
    let answer = work.await;
    tick.remove();
    (ops.transferring)(&shown.borrow(), false);
    answer
}

/// What carrying one path from this machine into the vault takes: the folders to make, parents
/// first, and the files to send, each with the path it lands at. A file is the carry of one.
#[derive(Debug, Default, PartialEq)]
struct Carry {
    dirs: Vec<(PathBuf, String)>,
    files: Vec<(PathBuf, String)>,
    /// Folders the walk could not read, by where they would have landed.
    unread: Vec<String>,
}

impl Carry {
    /// Walk `src` into `to`, a path nothing in the vault holds yet, so nothing under it can clash.
    ///
    /// Everything that is there is carried, dot-named files included, as GNOME Files copies a
    /// folder. A symbolic link is carried only as the file it names inside `src`: one leading out
    /// of it would copy what nobody selected, and one to a folder may be a loop, its folder being
    /// walked where it really is.
    fn of(src: &Path, to: &str) -> Carry {
        let mut carry = Carry::default();
        match src.is_dir() {
            true => {
                let root = src.canonicalize().unwrap_or_else(|_| src.to_path_buf());
                carry.walk(&root, src, to);
            }
            false => carry.files.push((src.to_path_buf(), to.to_string())),
        }
        carry
    }

    fn walk(&mut self, root: &Path, dir: &Path, to: &str) {
        let mut entries: Vec<PathBuf> = match std::fs::read_dir(dir) {
            Ok(entries) => entries.flatten().map(|e| e.path()).collect(),
            Err(_) => return self.unread.push(to.to_string()),
        };
        entries.sort();
        self.dirs.push((dir.to_path_buf(), to.to_string()));
        for path in entries {
            let (Some(name), Ok(meta)) = (local_name(&path), std::fs::symlink_metadata(&path))
            else {
                continue;
            };
            let rel = child_path(to, &name);
            let linked_inside = || {
                path.canonicalize()
                    .is_ok_and(|target| target.starts_with(root) && target.is_file())
            };
            if meta.is_dir() {
                self.walk(root, &path, &rel);
            } else if meta.is_file() || (meta.is_symlink() && linked_inside()) {
                self.files.push((path, rel));
            }
        }
    }

    /// Make the folders, then send the files, keeping whatever lands when a later step fails, as a
    /// file manager does. Answers how many files landed and what did not, by where it would have
    /// landed: a folder that could not be made is named once, and nothing under it is tried.
    ///
    /// A Cut takes each file away once it is in the vault, then every folder left empty; what
    /// stays behind is what did not go.
    fn send(
        &self,
        cut: bool,
        make_dir: impl Fn(&str) -> std::io::Result<()>,
        send: impl Fn(&Path, &str) -> std::io::Result<()>,
    ) -> (usize, Vec<String>) {
        let mut failed = self.unread.clone();
        let under = |rel: &str, failed: &[String]| {
            failed.iter().any(|f| {
                rel.strip_prefix(f.as_str())
                    .is_some_and(|r| r.starts_with('/'))
            })
        };
        for (_, rel) in &self.dirs {
            if !under(rel, &failed) && make_dir(rel).is_err() {
                failed.push(rel.clone());
            }
        }
        let mut sent = 0;
        for (from, rel) in &self.files {
            if under(rel, &failed) {
                continue;
            }
            match send(from, rel) {
                // A source that will not go is logged rather than toasted: the paste did land.
                Ok(()) => {
                    sent += 1;
                    if cut && let Err(e) = std::fs::remove_file(from) {
                        tracing::warn!("{} stayed where it was: {e}", from.display());
                    }
                }
                Err(_) => failed.push(rel.clone()),
            }
        }
        if cut {
            for (dir, _) in self.dirs.iter().rev() {
                let _ = std::fs::remove_dir(dir);
            }
        }
        (sent, failed)
    }
}

/// What a paste from this machine did, for its one toast.
#[derive(Debug, Default)]
struct Imported {
    /// Where each file or folder that landed is now.
    landed: Vec<String>,
    /// How many files landed in all, a folder's included.
    files: usize,
    /// Whether a folder was among what landed.
    folders: bool,
    /// What did not land, by its path under the folder pasted into.
    failed: Vec<String>,
}

impl Imported {
    /// "Copied Photos (120 files) to Media/Photos", or "Copied 3 files to Media" for several, in
    /// the words an in-vault paste uses ([`batch_to`]), then what did not land by name.
    fn message(&self, cut: bool) -> String {
        let Some(first) = self.landed.first() else {
            return match self.failed.is_empty() {
                true => "Cannot paste".to_string(),
                false => format!("Cannot paste {}", listed_names(&self.failed)),
            };
        };
        let what = match (self.landed.len(), self.folders) {
            (1, true) => format!("{} ({})", basename(first), file_count(self.files)),
            (1, false) => basename(first).to_string(),
            _ => file_count(self.files),
        };
        let verb = if cut { "Moved" } else { "Copied" };
        let to = batch_to(first, self.landed.len());
        match self.failed.is_empty() {
            true => format!("{verb} {what} to {to}"),
            false => format!(
                "{verb} {what} to {to}, but not {}",
                listed_names(&self.failed)
            ),
        }
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

    /// A batch says that it runs until it knows how many files it has, then how far it has got,
    /// the way indexing counts; one file is never counted.
    #[test]
    fn a_batch_counts_its_files_once_it_knows_how_many() {
        let folder = counting("Copying", &[PathBuf::from("/tmp/Photos")]);
        assert_eq!(folder(0, 0), "Copying Photos…");
        assert_eq!(folder(12, 120), "Copying Photos… 12/120 files");
        assert_eq!(folder(0, 1), "Copying Photos…");
        let two = [PathBuf::from("/tmp/a.png"), PathBuf::from("/tmp/b.png")];
        assert_eq!(counting("Uploading", &two)(1, 2), "Uploading… 1/2 files");
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

    /// A folder from this disk is walked into folders to make and files to send, each landing
    /// under the path the paste chose for the folder. A link is carried only as the file it names
    /// inside the folder: one out of it, or to a folder, which may be a loop, is left out.
    #[test]
    fn a_folder_is_carried_as_its_folders_and_files() {
        // `std::env::temp_dir`, as `start.rs`'s test does: apps/gtk has no `tempfile`.
        let tmp = std::env::temp_dir().join(format!("accent-carry-{}", std::process::id()));
        let src = tmp.join("Photos");
        std::fs::create_dir_all(src.join("sub")).unwrap();
        std::fs::create_dir(src.join("empty")).unwrap();
        std::fs::write(src.join("a.png"), "a").unwrap();
        std::fs::write(src.join("sub/b.png"), "b").unwrap();
        std::fs::write(tmp.join("outside.png"), "o").unwrap();
        std::os::unix::fs::symlink("a.png", src.join("inside.png")).unwrap();
        std::os::unix::fs::symlink("../outside.png", src.join("out.png")).unwrap();
        std::os::unix::fs::symlink(".", src.join("loop")).unwrap();

        let carry = Carry::of(&src, "Media/Photos (copy)");
        let dirs: Vec<&str> = carry.dirs.iter().map(|(_, rel)| rel.as_str()).collect();
        assert_eq!(
            dirs,
            [
                "Media/Photos (copy)",
                "Media/Photos (copy)/empty",
                "Media/Photos (copy)/sub"
            ]
        );
        let files: Vec<(&Path, &str)> = carry
            .files
            .iter()
            .map(|(from, to)| (from.strip_prefix(&src).unwrap(), to.as_str()))
            .collect();
        assert_eq!(
            files,
            [
                (Path::new("a.png"), "Media/Photos (copy)/a.png"),
                (Path::new("inside.png"), "Media/Photos (copy)/inside.png"),
                (Path::new("sub/b.png"), "Media/Photos (copy)/sub/b.png"),
            ]
        );

        // A file is the carry of one.
        let one = Carry::of(&src.join("a.png"), "a.png");
        assert!(one.dirs.is_empty());
        assert_eq!(one.files, [(src.join("a.png"), "a.png".to_string())]);
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    /// What lands stays when something after it fails, as in a file manager, and what did not is
    /// named once: a folder that could not be made, not every file under it.
    #[test]
    fn a_failure_keeps_what_landed_and_names_what_did_not() {
        let carry = Carry {
            dirs: vec![
                (PathBuf::from("/x/P"), "P".to_string()),
                (PathBuf::from("/x/P/bad"), "P/bad".to_string()),
            ],
            files: vec![
                (PathBuf::from("/x/P/a"), "P/a".to_string()),
                (PathBuf::from("/x/P/bad/b"), "P/bad/b".to_string()),
                (PathBuf::from("/x/P/c"), "P/c".to_string()),
            ],
            unread: vec!["P/locked".to_string()],
        };
        let made = std::cell::RefCell::new(Vec::new());
        let (sent, failed) = carry.send(
            false,
            |rel| {
                made.borrow_mut().push(rel.to_string());
                match rel {
                    "P/bad" => Err(std::io::Error::other("no")),
                    _ => Ok(()),
                }
            },
            |_, rel| match rel {
                "P/c" => Err(std::io::Error::other("no")),
                _ => Ok(()),
            },
        );
        assert_eq!(sent, 1, "P/a landed and stays");
        assert_eq!(*made.borrow(), ["P", "P/bad"]);
        assert_eq!(failed, ["P/locked", "P/bad", "P/c"]);
    }

    /// A Cut takes away what went, and what did not stays where it was, with the folders it is in.
    #[test]
    fn a_cut_folder_leaves_only_what_did_not_go() {
        let tmp = std::env::temp_dir().join(format!("accent-cut-{}", std::process::id()));
        let src = tmp.join("P");
        std::fs::create_dir_all(src.join("gone")).unwrap();
        std::fs::create_dir_all(src.join("kept")).unwrap();
        std::fs::write(src.join("gone/a"), "a").unwrap();
        std::fs::write(src.join("kept/b"), "b").unwrap();

        let carry = Carry::of(&src, "P");
        let (sent, failed) = carry.send(
            true,
            |_| Ok(()),
            |_, rel| match rel {
                "P/kept/b" => Err(std::io::Error::other("no")),
                _ => Ok(()),
            },
        );
        assert_eq!((sent, failed), (1, vec!["P/kept/b".to_string()]));
        assert!(!src.join("gone").exists(), "emptied, so taken away");
        assert!(src.join("kept/b").exists());
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn the_toast_says_what_went_where_and_names_what_did_not() {
        let one = |landed: &str, files, folders, failed: &[&str]| Imported {
            landed: vec![landed.to_string()],
            files,
            folders,
            failed: failed.iter().map(|f| f.to_string()).collect(),
        };
        assert_eq!(
            one("Media/a.png", 1, false, &[]).message(false),
            "Copied a.png to Media/a.png"
        );
        assert_eq!(
            one("Media/Photos", 120, true, &["Photos/raw.cr2"]).message(false),
            "Copied Photos (120 files) to Media/Photos, but not Photos/raw.cr2"
        );
        let many = Imported {
            landed: vec!["Photos".into(), "a.png".into()],
            files: 3,
            folders: true,
            failed: Vec::new(),
        };
        assert_eq!(many.message(true), "Moved 3 files to the vault root");
        let none = Imported {
            failed: vec!["Photos".into()],
            ..Imported::default()
        };
        assert_eq!(none.message(false), "Cannot paste Photos");
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
