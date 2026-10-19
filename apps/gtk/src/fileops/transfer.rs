//! Transfer files to or from a remote vault over SSH. Workers keep transfers off the UI thread;
//! the status bar shows progress and reports the result once.

use super::paths::{at_free_name, child_path, free_path, topmost};
use super::{Ops, batch_to, trash_all};
use crate::dialogs::confirm;
use crate::statusbar::Bytes;
use accent_core::fs::{Walked, walk_copy};
use accent_core::path::basename;
use adw::prelude::*;
use gtk::{gio, glib};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// How many file names an upload's toast or dialog spells out before it counts instead.
const NAMED: usize = 3;
/// Download a remote vault file to this machine. Local files already reside here and use the
/// file manager instead. The destination chooser handles replacement confirmation.
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
        if let Some(dest) = result.ok().and_then(|f| f.path()) {
            download_to(&ops, &rel, dest);
        }
    });
}

/// Download… once the chooser has answered: `rel` copied to `dest`, its bytes counted on the
/// status bar as they come ("Downloading paper.pdf… 12.3/80.0 MB"), and one toast at the end.
pub fn download_to(ops: &Rc<Ops>, rel: &str, dest: PathBuf) {
    let (vault, ops, rel) = (ops.vault.clone(), ops.clone(), rel.to_string());
    let name = basename(&rel).to_string();
    let bytes = Arc::new(Bytes::default());
    let what = format!("Downloading {name}");
    // Bytes over ssh, so off the main thread: a large PDF would otherwise freeze the window for
    // as long as the copy takes.
    glib::spawn_future_local(async move {
        let asked = format!("download {name}");
        let work = crate::work::attempt(&asked, {
            let bytes = bytes.clone();
            move || vault.download_with(&rel, &dest, &|done, total| bytes.set(done, total))
        });
        let line = move || Some(bytes.line(&what).unwrap_or_else(|| format!("{what}…")));
        let done = counting_on(&ops, line, work).await;
        (ops.toast)(&match done {
            Ok(()) => format!("Downloaded {name}"),
            Err(why) => why,
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
                true => send(&ops, &dir, chosen, Vec::new()),
                false => confirm_replace(&ops, existing, {
                    let ops = ops.clone();
                    move |existing| send(&ops, &dir, chosen, existing)
                }),
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
/// dozen paths, and a dozen dialogs is an obstacle rather than a question. `then` is handed the
/// names to write over on Replace; Cancel sends nothing.
fn confirm_replace(ops: &Rc<Ops>, existing: Vec<String>, then: impl FnOnce(Vec<String>) + 'static) {
    let heading = match existing.len() {
        1 => "Replace File?",
        _ => "Replace Files?",
    };
    let body = replace_body(&existing);
    confirm(&ops.window, heading, &body, "Replace", true, move || {
        then(existing)
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
            listed_names(many)
        ),
    }
}

/// Send the chosen files, off the main thread, and report once. Only the names in `replace`,
/// confirmed by the reader, are written over; a file that took any other name since is left be.
fn send(ops: &Rc<Ops>, dir: &str, chosen: Vec<PathBuf>, replace: Vec<String>) {
    let (vault, dir, ops) = (ops.vault.clone(), dir.to_string(), ops.clone());
    let count = Arc::new(Count::default());
    count.total.store(chosen.len(), Ordering::Relaxed);
    let line = batch_line("Uploading", &chosen, &count);
    glib::spawn_future_local(async move {
        let work = crate::work::off_thread("upload", {
            let count = count.clone();
            move || {
                let (mut uploaded, mut failed) = (0, Vec::new());
                for file in &chosen {
                    let Some(name) = local_name(file) else {
                        continue;
                    };
                    let to = child_path(&dir, &name);
                    let progress = |done, total| count.bytes.set(done, total);
                    match vault.upload_with(file, &to, replace.contains(&name), &progress) {
                        Ok(()) => uploaded += 1,
                        Err(_) => failed.push(name),
                    }
                    count.done.fetch_add(1, Ordering::Relaxed);
                }
                (uploaded, failed)
            }
        });
        let done = counting_on(&ops, line, work).await;
        // Neither the tree nor the index is poked here: the watcher on the host reports what
        // landed, the same way it reports anything else written there.
        (ops.toast)(&match done {
            Some((uploaded, failed)) => upload_message(uploaded, &failed),
            None => "Cannot upload".to_string(),
        });
    });
}

/// Copy external files or folders into `dir` under free names, removing originals after
/// successful Cut transfers. Used by paste and external drag-and-drop on local or remote vaults.
///
/// Never overwrite: occupied names gain `(copy)` suffixes, and a name taken between choosing and
/// writing moves on to the next ([`at_free_name`]). Recreate folders file by file through
/// [`Carry`], each in a folder made for it.
pub fn import(ops: &Rc<Ops>, dir: &str, files: Vec<PathBuf>, cut: bool) {
    let (vault, dir, ops) = (ops.vault.clone(), dir.to_string(), ops.clone());
    let count = Arc::new(Count::default());
    let line = batch_line("Copying", &files, &count);
    glib::spawn_future_local(async move {
        let work = crate::work::off_thread("copy", {
            let count = count.clone();
            move || {
                // Every name and every walk first, so the count has its total before the first
                // file goes. A name is taken once chosen, or two pasted files of one name would
                // both be given it.
                let free = |claimed: &[String], name: &str, is_dir: bool| {
                    free_path(&dir, name, is_dir, |rel| {
                        claimed.iter().any(|c| c == rel) || vault.exists(rel)
                    })
                };
                let mut claimed = Vec::new();
                let mut carries = Vec::new();
                let mut report = Imported::default();
                for file in &files {
                    let Some(name) = local_name(file) else {
                        continue;
                    };
                    let is_dir = file.is_dir();
                    let next = || free(&claimed, &name, is_dir);
                    let mut to = next();
                    let mut carry = Carry::of(file, &to);
                    // A folder that could be read is claimed now, made empty, so what was walked
                    // into it never joins a folder that took the name meanwhile; one taken since
                    // is walked again under the name it got.
                    if is_dir && !carry.unread.contains(&to) {
                        match at_free_name(to.clone(), next, |rel| vault.create_dir(rel, false)) {
                            Ok(made) if made == to => {}
                            Ok(made) => (carry, to) = (Carry::of(file, &made), made),
                            Err(_) => {
                                report.failed.push(name);
                                continue;
                            }
                        }
                    }
                    claimed.push(to.clone());
                    carries.push((carry, to, name, is_dir));
                }
                let total = carries.iter().map(|(carry, ..)| carry.files.len()).sum();
                count.total.store(total, Ordering::Relaxed);
                for (carry, to, name, is_dir) in carries {
                    // Where a file pasted alone landed: its name, or the next free one when that
                    // was taken since. What a folder holds goes into the folder made for it.
                    let landed = RefCell::new(to.clone());
                    let (sent, failed) = carry.send(
                        |rel| vault.create_dir(rel, true),
                        |from, rel| {
                            let progress = |done, total| count.bytes.set(done, total);
                            let upload = |rel: &str| vault.upload_with(from, rel, false, &progress);
                            let sent = match is_dir {
                                true => upload(rel),
                                false => at_free_name(
                                    rel.to_string(),
                                    || free(&claimed, &name, false),
                                    upload,
                                )
                                .map(|at| *landed.borrow_mut() = at),
                            };
                            count.done.fetch_add(1, Ordering::Relaxed);
                            // A Cut takes each file away once it is in the vault. A source that
                            // will not go is logged rather than toasted: the paste did land.
                            if cut
                                && sent.is_ok()
                                && let Err(e) = std::fs::remove_file(from)
                            {
                                tracing::warn!("{} stayed where it was: {e}", from.display());
                            }
                            sent
                        },
                    );
                    if cut {
                        carry.take_emptied();
                    }
                    if !failed.contains(&to) {
                        report.landed.push(landed.into_inner());
                        report.folders |= is_dir;
                    }
                    report.files += sent;
                    report
                        .failed
                        .extend(failed.iter().map(|rel| inside(&dir, rel)));
                }
                report
            }
        });
        let done = counting_on(&ops, line, work).await;
        (ops.toast)(&match done {
            Some(report) => report.message(cut),
            None => "Cannot paste".to_string(),
        });
    });
}

/// One path carried in from another vault: where it is there, where it lands here, whether it is
/// a folder, and what it takes.
type Carried = (String, String, bool, Carry<String>);

/// Rows dragged in from another window's tree: `rels` of `from`'s vault copied into `dir` of this
/// one, each under its own name, folders file by file, the bytes going straight from one machine to
/// the other ([`accent_api::Vault::copy_to`]). Names already taken here are asked about once for
/// the batch, as Upload Files… asks, and Cancel copies nothing; a folder already here takes what is
/// carried into it. `moving` (Shift) then takes the originals out of `from`'s vault as its Delete
/// does — to the trash, or on a host for good behind its question — the ones that all landed.
pub fn carry_in(ops: &Rc<Ops>, from: &Rc<Ops>, rels: Vec<String>, dir: &str, moving: bool) {
    // What lands should be what the reader last saw there.
    (from.flush)(&rels);
    let marked: Vec<(String, bool)> = rels.into_iter().map(|rel| (rel, false)).collect();
    let rels: Vec<String> = topmost(&marked).into_iter().map(|(rel, _)| rel).collect();
    let (source, vault, dir) = (from.vault.clone(), ops.vault.clone(), dir.to_string());
    let (ops, from) = (ops.clone(), from.clone());
    glib::spawn_future_local(async move {
        // Every walk and every taken name first, on the worker: each is a round trip on a host.
        let walked = crate::work::off_thread("copy", move || {
            let mut carried: Vec<Carried> = Vec::new();
            let mut existing = Vec::new();
            for rel in rels {
                let to = child_path(&dir, basename(&rel));
                let walked = source.list_tree(&rel).unwrap_or_else(|_| Walked {
                    unread: vec![String::new()],
                    ..Walked::default()
                });
                let is_dir = !walked.dirs.is_empty() || !walked.unread.is_empty();
                // A name nothing holds here has nothing under it to take.
                if vault.exists(&to) {
                    let here = match is_dir {
                        true => vault.list_tree(&to).map(|w| w.files).unwrap_or_default(),
                        false => vec![String::new()],
                    };
                    existing.extend(walked.files.iter().filter(|file| here.contains(file)).map(
                        |file| match file.as_str() {
                            "" => to.clone(),
                            file => child_path(&to, file),
                        },
                    ));
                }
                let at = |file: &str| match file {
                    "" => rel.clone(),
                    file => child_path(&rel, file),
                };
                let carry = Carry::landing(walked, &to, at);
                carried.push((rel, to, is_dir, carry));
            }
            (carried, existing, dir)
        })
        .await;
        let Some((carried, existing, dir)) = walked else {
            return (ops.toast)("Cannot copy");
        };
        if existing.is_empty() {
            return carry(&ops, &from, &dir, carried, Vec::new(), moving);
        }
        let shown = existing.iter().map(|rel| inside(&dir, rel)).collect();
        let asking = ops.clone();
        confirm_replace(&asking, shown, move |_| {
            carry(&ops, &from, &dir, carried, existing, moving)
        });
    });
}

/// [`carry_in`] once the names are settled: the copies on a worker, counted on the status bar in
/// the words of the side that is remote, one toast, and with `moving` the originals that all
/// landed taken out of `from`'s vault.
fn carry(
    ops: &Rc<Ops>,
    from: &Rc<Ops>,
    dir: &str,
    carried: Vec<Carried>,
    replace: Vec<String>,
    moving: bool,
) {
    let (source, vault) = (from.vault.clone(), ops.vault.clone());
    let verb = match (source.is_remote(), vault.is_remote()) {
        (true, false) => "Downloading",
        (false, true) => "Uploading",
        _ => "Copying",
    };
    let count = Arc::new(Count::default());
    let total = carried.iter().map(|(.., carry)| carry.files.len()).sum();
    count.total.store(total, Ordering::Relaxed);
    let names: Vec<PathBuf> = carried.iter().map(|(rel, ..)| rel.into()).collect();
    let line = batch_line(verb, &names, &count);
    let (ops, from, dir) = (ops.clone(), from.clone(), dir.to_string());
    glib::spawn_future_local(async move {
        let work = crate::work::off_thread("copy", {
            let count = count.clone();
            move || {
                let (mut report, mut whole) = (Imported::default(), Vec::new());
                for (rel, to, is_dir, carry) in carried {
                    let (sent, failed) = carry.send(
                        |rel| vault.create_dir(rel, true),
                        |from, rel| {
                            let progress = |done, total| count.bytes.set(done, total);
                            let over = replace.iter().any(|r| r == rel);
                            let sent = source.copy_to(from, &vault, rel, over, &progress);
                            count.done.fetch_add(1, Ordering::Relaxed);
                            sent
                        },
                    );
                    if !failed.contains(&to) {
                        report.landed.push(to);
                        report.folders |= is_dir;
                    }
                    if failed.is_empty() {
                        whole.push(rel);
                    }
                    report.files += sent;
                    report
                        .failed
                        .extend(failed.iter().map(|rel| inside(&dir, rel)));
                }
                (report, whole)
            }
        });
        let done = counting_on(&ops, line, work).await;
        let Some((report, whole)) = done else {
            return (ops.toast)("Cannot copy");
        };
        (ops.toast)(&report.message(false));
        if moving && !whole.is_empty() {
            trash_all(&from, whole);
        }
    });
}

/// `rel` named from the folder `dir` a batch went into, which its toast has already said.
fn inside(dir: &str, rel: &str) -> String {
    match rel.strip_prefix(&format!("{dir}/")) {
        Some(rest) => rest.to_string(),
        None => rel.to_string(),
    }
}

/// How far a batch has got, counted on the worker and read on the main loop: the files tried so
/// far and how many there are, 0 until the walk has counted them, and the bytes of the one file
/// in flight, which is what a batch of one says instead.
#[derive(Default)]
struct Count {
    done: AtomicUsize,
    total: AtomicUsize,
    bytes: Bytes,
}

/// What the status bar says for a batch of `chosen` while `count` counts it: that it runs
/// ("Copying Photos…"); from two files on, how far by file, as indexing counts ("Copying Photos…
/// 12/120 files", "Uploading… 3/12 files"); and for one file sent over the link, how far by byte
/// ("Uploading big.pdf… 12.3/80.0 MB"), a local copy being the disk's speed and saying nothing.
fn batch_line(
    verb: &str,
    chosen: &[PathBuf],
    count: &Arc<Count>,
) -> impl Fn() -> Option<String> + 'static {
    let (verb, chosen, count) = (verb.to_string(), chosen.to_vec(), count.clone());
    move || {
        let (done, total) = (
            count.done.load(Ordering::Relaxed),
            count.total.load(Ordering::Relaxed),
        );
        Some(match (chosen.as_slice(), total) {
            (_, 0 | 1) => {
                let what = busy_what(&verb, &chosen);
                count
                    .bytes
                    .line(&what)
                    .unwrap_or_else(|| format!("{what}…"))
            }
            ([one], total) => format!(
                "{verb} {}… {done}/{total} files",
                local_name(one).unwrap_or_default()
            ),
            (_, total) => format!("{verb}… {done}/{total} files"),
        })
    }
}

/// [`crate::statusbar::counting`] on the window's status bar, through `ops`.
async fn counting_on<T>(
    ops: &Rc<Ops>,
    line: impl Fn() -> Option<String> + 'static,
    work: impl std::future::Future<Output = T>,
) -> T {
    let (say, retell) = (ops.clone(), ops.clone());
    crate::statusbar::counting(
        move |text, running| (say.transferring)(text, running),
        move |from, to| (retell.transfer_count)(from, to),
        line,
        work,
    )
    .await
}

/// What carrying one path into the vault takes: the folders to make, parents first, and the files
/// to send, each from where `S` names it on the source side to the path it lands at. A file is the
/// carry of one.
#[derive(Debug, Default, PartialEq)]
struct Carry<S> {
    dirs: Vec<(S, String)>,
    files: Vec<(S, String)>,
    /// Folders the walk could not read, by where they would have landed.
    unread: Vec<String>,
}

impl<S> Carry<S> {
    /// `walked` landing at `to`, each of its paths named on the source side by `at`.
    fn landing(walked: Walked, to: &str, at: impl Fn(&str) -> S) -> Carry<S> {
        let land = |rel: &str| match rel {
            "" => to.to_string(),
            rel => child_path(to, rel),
        };
        let pair = |rel: String| (at(&rel), land(&rel));
        Carry {
            dirs: walked.dirs.into_iter().map(pair).collect(),
            files: walked.files.into_iter().map(pair).collect(),
            unread: walked.unread.iter().map(|rel| land(rel)).collect(),
        }
    }

    /// Make the folders, then send the files, keeping whatever lands when a later step fails, as a
    /// file manager does. Answers how many files landed and what did not, by where it would have
    /// landed: a folder that could not be made is named once, and nothing under it is tried.
    fn send(
        &self,
        make_dir: impl Fn(&str) -> accent_api::Result<()>,
        send: impl Fn(&S, &str) -> accent_api::Result<()>,
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
                Ok(()) => sent += 1,
                Err(_) => failed.push(rel.clone()),
            }
        }
        (sent, failed)
    }
}

impl Carry<PathBuf> {
    /// Walk `src` on this machine into `to`, a path nothing in the vault holds yet, so nothing
    /// under it can clash ([`walk_copy`]).
    fn of(src: &Path, to: &str) -> Carry<PathBuf> {
        Carry::landing(walk_copy(src), to, |rel| match rel {
            "" => src.to_path_buf(),
            rel => src.join(rel),
        })
    }

    /// A Cut's end, its files taken away: every folder left empty goes too, deepest first; what
    /// stays behind is what did not go.
    fn take_emptied(&self) {
        for (dir, _) in self.dirs.iter().rev() {
            let _ = std::fs::remove_dir(dir);
        }
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

/// What a running transfer is, for the status bar: the file by name when there is one, otherwise
/// how many. The verb is the caller's — Upload Files… uploads, and a paste of files from outside
/// the vault copies them in, which on a local vault never leaves this machine.
fn busy_what(verb: &str, chosen: &[PathBuf]) -> String {
    let what = match chosen {
        [one] => local_name(one).unwrap_or_else(|| file_count(1)),
        many => file_count(many.len()),
    };
    format!("{verb} {what}")
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
        let count = Arc::new(Count::default());
        let folder = batch_line("Copying", &[PathBuf::from("/tmp/Photos")], &count);
        assert_eq!(folder().as_deref(), Some("Copying Photos…"));
        count.total.store(120, Ordering::Relaxed);
        count.done.store(12, Ordering::Relaxed);
        assert_eq!(folder().as_deref(), Some("Copying Photos… 12/120 files"));

        // One file counts its bytes instead, once they move.
        let count = Arc::new(Count::default());
        count.total.store(1, Ordering::Relaxed);
        let one = batch_line("Uploading", &[PathBuf::from("/tmp/big.pdf")], &count);
        assert_eq!(one().as_deref(), Some("Uploading big.pdf…"));
        count.bytes.set(1024 * 1024, 4 * 1024 * 1024);
        assert_eq!(one().as_deref(), Some("Uploading big.pdf… 1.0/4.0 MB"));

        let two = [PathBuf::from("/tmp/a.png"), PathBuf::from("/tmp/b.png")];
        let count = Arc::new(Count::default());
        count.total.store(2, Ordering::Relaxed);
        count.done.store(1, Ordering::Relaxed);
        let many = batch_line("Uploading", &two, &count);
        assert_eq!(many().as_deref(), Some("Uploading… 1/2 files"));
    }

    #[test]
    fn a_running_transfer_names_one_file_and_counts_several() {
        assert_eq!(
            busy_what("Uploading", &[PathBuf::from("/tmp/a.png")]),
            "Uploading a.png"
        );
        let two = [PathBuf::from("/tmp/a.png"), PathBuf::from("/tmp/b.png")];
        assert_eq!(busy_what("Copying", &two), "Copying 2 files");
    }

    /// A walk lands under the path chosen for it, each path named on the source side as the
    /// caller names it: a file of this disk, or a path in another vault.
    #[test]
    fn a_walk_lands_under_the_path_chosen_for_it() {
        let walked = || Walked {
            dirs: vec!["".into(), "sub".into()],
            files: vec!["a.png".into(), "sub/b.png".into()],
            unread: vec!["locked".into()],
        };
        let carry = Carry::landing(walked(), "Media/Photos (copy)", |rel| rel.to_string());
        assert_eq!(
            carry.dirs,
            [
                ("".to_string(), "Media/Photos (copy)".to_string()),
                ("sub".into(), "Media/Photos (copy)/sub".into())
            ]
        );
        assert_eq!(
            carry.files[1],
            ("sub/b.png".into(), "Media/Photos (copy)/sub/b.png".into())
        );
        assert_eq!(carry.unread, ["Media/Photos (copy)/locked"]);

        // A file is the carry of one.
        let src = Path::new("/x/Photos");
        let one = Carry::of(&src.join("a.png"), "a.png");
        assert!(one.dirs.is_empty());
        assert_eq!(one.files, [(src.join("a.png"), "a.png".to_string())]);
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
            |rel| {
                made.borrow_mut().push(rel.to_string());
                match rel {
                    "P/bad" => Err(accent_core::Error::Io("no".into()).into()),
                    _ => Ok(()),
                }
            },
            |_, rel| match rel {
                "P/c" => Err(accent_core::Error::Io("no".into()).into()),
                _ => Ok(()),
            },
        );
        assert_eq!(sent, 1, "P/a landed and stays");
        assert_eq!(*made.borrow(), ["P", "P/bad"]);
        assert_eq!(failed, ["P/locked", "P/bad", "P/c"]);
    }

    /// A Cut takes away the folders its files left empty, and what did not go stays where it was,
    /// with the folders it is in.
    #[test]
    fn a_cut_folder_leaves_only_what_did_not_go() {
        let tmp = std::env::temp_dir().join(format!("accent-cut-{}", std::process::id()));
        let src = tmp.join("P");
        std::fs::create_dir_all(src.join("gone")).unwrap();
        std::fs::create_dir_all(src.join("kept")).unwrap();
        std::fs::write(src.join("kept/b"), "b").unwrap();

        Carry::of(&src, "P").take_emptied();
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
