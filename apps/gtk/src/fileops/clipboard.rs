//! Cut, Copy and Paste in the file tree.
//!
//! Hybrid, because a vault's files are not always on this machine. A **local** vault's are, so a
//! Copy writes the real GDK clipboard in the two forms a file manager reads — `text/uri-list`,
//! and `x-special/gnome-copied-files` for the verb, which is the pair GNOME Files writes and
//! reads back — and a Paste reads it, so a file crosses between accent and Files in either
//! direction. A **remote** vault's files are on the host, where no URI from here can point at
//! them: its Copy remembers vault-relative paths in [`Ops::clip`] and its Paste hands them to
//! [`Vault::copy`] or to the rename plan, both of which run where the files are, so duplicating a
//! folder costs no bytes over the link.
//!
//! What a Paste does with a path is decided by where it came from, not by which of the two vaults
//! it is: a path inside this vault is copied or moved there, and anything else is a file on this
//! machine that has to be carried in ([`transfer::import`]).

use super::paths::{free_path, move_dest};
use super::{Ops, batch_to, move_all, several, transfer};
use accent_core::path::basename;
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use std::path::PathBuf;
use std::rc::Rc;

/// The verb and the files under it, as GNOME Files writes them. Its own type rather than a
/// registered one: nothing but a file manager reads it, and both sides are plain text.
const GNOME: &str = "x-special/gnome-copied-files";
/// The cross-desktop half, which says where the files are and nothing about what to do with them.
const URIS: &str = "text/uri-list";

/// What a Cut or a Copy left behind.
#[derive(Clone, Default)]
pub struct Clip {
    /// Cut rather than Copy: a paste moves the files instead of duplicating them.
    cut: bool,
    /// What is in this vault already, each with whether it is a folder — which is what decides
    /// where a `(copy)` mark goes. Copied or moved where the files are.
    inside: Vec<(String, bool)>,
    /// Files on this machine that this vault does not hold, which a paste carries in.
    outside: Vec<PathBuf>,
}

impl Clip {
    fn is_empty(&self) -> bool {
        self.inside.is_empty() && self.outside.is_empty()
    }
}

/// Copy a row: a paste puts a duplicate of it wherever it lands.
pub fn copy(ops: &Rc<Ops>, rel: &str, is_dir: bool) {
    copy_all(ops, &[(rel.to_string(), is_dir)]);
}

/// Cut a row: a paste moves it, through the same plan a dragged row goes through.
pub fn cut(ops: &Rc<Ops>, rel: &str, is_dir: bool) {
    cut_all(ops, &[(rel.to_string(), is_dir)]);
}

/// [`copy`] for every row a Ctrl+click has marked, each with whether it is a directory.
pub fn copy_all(ops: &Rc<Ops>, rows: &[(String, bool)]) {
    take(ops, rows, false);
}

/// [`cut`] for every row a Ctrl+click has marked.
pub fn cut_all(ops: &Rc<Ops>, rows: &[(String, bool)]) {
    take(ops, rows, true);
}

/// Whether there is anything to paste, asked as the menu is built so an item that could do
/// nothing is never drawn. The clipboard answers what it holds without a read, which is the only
/// reason this can be a synchronous question at all.
pub fn can_paste(ops: &Ops) -> bool {
    let formats = ops.window.clipboard().formats();
    let on_this_machine = formats.contain_mime_type(GNOME) || formats.contain_mime_type(URIS);
    // A remote vault's own Copy never reached the real clipboard, so it has two places to look.
    on_this_machine || (ops.vault.is_remote() && ops.clip.borrow().is_some())
}

/// Put what the clipboard holds into `dir` ("" is the vault root).
pub fn paste(ops: &Rc<Ops>, dir: &str) {
    // A remote vault answers its own Copy from memory; everything else is on this machine and
    // has to be read off the real clipboard, which is asynchronous however local it is.
    //
    // Read out into a binding first, and not in an `if let`: the borrow would then still be live
    // inside the branch, where a Cut's own paste takes the clip back out again.
    let mine = match ops.vault.is_remote() {
        true => ops.clip.borrow().clone(),
        false => None,
    };
    if let Some(clip) = mine {
        return apply(ops, dir, clip);
    }
    let (ops, dir) = (ops.clone(), dir.to_string());
    glib::spawn_future_local(async move {
        match read_clipboard(&ops).await {
            Some(clip) => apply(&ops, &dir, clip),
            None => (ops.toast)("There are no files on the clipboard"),
        }
    });
}

/// [`copy`] and [`cut`], which differ in one flag and in what they leave dimmed. One row or
/// several: a marked set travels as a whole from here on, and the file managers' own formats
/// carry a list either way.
fn take(ops: &Rc<Ops>, rows: &[(String, bool)], cut: bool) {
    if rows.is_empty() {
        return;
    }
    if !ops.vault.is_remote() {
        publish(ops, rows, cut);
    }
    *ops.clip.borrow_mut() = Some(Clip {
        cut,
        inside: rows.to_vec(),
        outside: Vec::new(),
    });
    // The rows a Cut is waiting on read as dimmed until it is pasted, which is the only sign on
    // screen that anything is in flight at all. A Copy takes nothing away, so it dims nothing.
    let dimmed: Vec<String> = match cut {
        true => rows.iter().map(|(rel, _)| rel.clone()).collect(),
        false => Vec::new(),
    };
    (ops.cut)(&dimmed);
}

/// Put the rows on the real clipboard, in both forms, so a file manager can paste them.
fn publish(ops: &Ops, rows: &[(String, bool)], cut: bool) {
    let uris: Vec<String> = rows
        .iter()
        .map(|(rel, _)| gio::File::for_path(ops.vault.root().join(rel)).uri().into())
        .collect();
    let (gnome, list) = payload(&uris, cut);
    let provider = gdk::ContentProvider::new_union(&[
        gdk::ContentProvider::for_bytes(GNOME, &glib::Bytes::from(gnome.as_bytes())),
        gdk::ContentProvider::for_bytes(URIS, &glib::Bytes::from(list.as_bytes())),
    ]);
    if let Err(e) = ops.window.clipboard().set_content(Some(&provider)) {
        tracing::warn!("the clipboard would not take the files: {e}");
    }
}

/// What the two formats carry, which is what [`parse`] reads back out of them: GNOME's verb line
/// and then one URI per line, and the cross-desktop list, whose lines end CRLF and which says
/// nothing about the verb.
fn payload(uris: &[String], cut: bool) -> (String, String) {
    let verb = match cut {
        true => "cut",
        false => "copy",
    };
    let mut gnome = verb.to_string();
    let mut list = String::new();
    for uri in uris {
        gnome.push('\n');
        gnome.push_str(uri);
        list.push_str(uri);
        list.push_str("\r\n");
    }
    (gnome, list)
}

/// What the real clipboard holds, as a [`Clip`], or `None` when it holds no files.
async fn read_clipboard(ops: &Ops) -> Option<Clip> {
    let asked = ops
        .window
        .clipboard()
        .read_future(&[GNOME, URIS], glib::Priority::DEFAULT)
        .await;
    let (stream, mime) = match asked {
        Ok(answer) => answer,
        // What a clipboard holding text rather than files answers, which is not worth a toast of
        // its own: the Paste item is only drawn where the formats said there were files.
        Err(e) => {
            tracing::debug!("nothing on the clipboard to paste: {e}");
            return None;
        }
    };
    // One read of a generous buffer: a URI is a few dozen bytes, and a selection too large to fit
    // in 64 KiB is not one anybody made by hand in a file manager.
    let (buffer, read, _) = stream
        .read_all_future(vec![0u8; 64 * 1024], glib::Priority::DEFAULT)
        .await
        .ok()?;
    let (cut, uris) = parse(&mime, &String::from_utf8_lossy(&buffer[..read]));

    let root = ops.vault.root();
    let remote = ops.vault.is_remote();
    let mut clip = Clip {
        cut,
        ..Clip::default()
    };
    for uri in uris {
        let Some(path) = gio::File::for_uri(&uri).path() else {
            continue;
        };
        // A remote vault's root is a path on the *host*, so nothing this machine's clipboard
        // names is ever inside it, however alike the two spell their folders.
        match path.strip_prefix(&root) {
            Ok(rel) if !remote => {
                clip.inside
                    .push((rel.to_string_lossy().into_owned(), path.is_dir()));
            }
            _ => clip.outside.push(path),
        }
    }
    (!clip.is_empty()).then_some(clip)
}

/// What a file manager's clipboard says: whether it was a Cut, and the files under it.
///
/// `x-special/gnome-copied-files` is a verb line and then one URI per line; `text/uri-list` is
/// the same list with no verb at all, so a paste of one is always a copy. Blank lines and the
/// `#` comments the URI list allows are not files.
fn parse(mime: &str, text: &str) -> (bool, Vec<String>) {
    let mut lines = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'));
    let cut = mime == GNOME && lines.next() == Some("cut");
    (cut, lines.map(str::to_string).collect())
}

/// Put a clip into `dir`: the vault's own files first, then whatever has to be carried in.
fn apply(ops: &Rc<Ops>, dir: &str, clip: Clip) {
    match clip.cut {
        true => move_here(ops, &clip.inside, dir),
        false => copy_here(ops, clip.inside, dir),
    }
    if !clip.outside.is_empty() {
        transfer::import(ops, dir, clip.outside, clip.cut);
    }
    if clip.cut {
        // A Cut is spent by the paste that answered it: the rows stop being dimmed, and the
        // clipboard is emptied so a second paste does not go looking for files that have moved.
        ops.clip.borrow_mut().take();
        (ops.cut)(&[]);
        if !ops.vault.is_remote() {
            let _ = ops
                .window
                .clipboard()
                .set_content(gdk::ContentProvider::NONE);
        }
    }
}

/// A Cut pasted is a move, and a move is the tree's own: the same plan, the same Update Links?
/// question, and the same rewriting of every note that pointed at the files — once for all of
/// them, so two notes cut together that link each other are rewritten together.
fn move_here(ops: &Rc<Ops>, inside: &[(String, bool)], dir: &str) {
    let (mut moves, mut refused) = (Vec::new(), Vec::new());
    for (rel, _) in inside {
        match move_dest(rel, dir) {
            Some(to) => moves.push((rel.clone(), to)),
            None => refused.push(rel.clone()),
        }
    }
    // The three moves that are not moves: into the folder it is already in, a folder onto
    // itself, and a folder into something under it. One sentence for all three, because what the
    // reader did is the same gesture each time.
    if !refused.is_empty() {
        (ops.toast)(&format!("Cannot paste {} here", several(&refused)));
    }
    if !moves.is_empty() {
        move_all(ops, moves);
    }
}

/// A Copy pasted duplicates, each under a name the destination does not hold yet. One worker
/// copies them in turn, so two of one name take two names, and one toast says what landed once
/// every copy has; a copy that fails is named on its own, as Move to Trash names one.
fn copy_here(ops: &Rc<Ops>, inside: Vec<(String, bool)>, dir: &str) {
    if inside.is_empty() {
        return;
    }
    let (vault, ops, dir) = (ops.vault.clone(), ops.clone(), dir.to_string());
    let name = several(
        &inside
            .iter()
            .map(|(rel, _)| rel.clone())
            .collect::<Vec<_>>(),
    );
    glib::spawn_future_local(async move {
        let done = crate::work::off_thread("copy", move || {
            inside
                .into_iter()
                .map(|(rel, is_dir)| {
                    // Naming and copying on the same worker: each candidate name costs a `stat`,
                    // which on a remote vault is a round trip the window must not wait on.
                    let to = free_path(&dir, basename(&rel), is_dir, |rel| vault.exists(rel));
                    (vault.copy(&rel, &to).map(|()| to), rel)
                })
                .collect::<Vec<_>>()
        })
        .await;
        let Some(done) = done else {
            return (ops.toast)(&format!("Cannot copy {name}: the worker stopped"));
        };
        let (mut copied, mut landed) = (Vec::new(), Vec::new());
        for (answer, rel) in done {
            match answer {
                Ok(to) => {
                    copied.push(rel);
                    landed.push(to);
                }
                Err(e) => (ops.toast)(&format!("Cannot copy {}: {e}", basename(&rel))),
            }
        }
        // Neither the tree nor the index is poked: the watcher reports what landed, wherever the
        // files are, the same way it reports a new note.
        if let Some(first) = landed.first() {
            let to = batch_to(first, landed.len());
            (ops.toast)(&format!("Copied {} to {to}", several(&copied)));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_reads_the_verb_only_where_the_format_carries_one() {
        // What GNOME Files puts on the clipboard: the verb, then the files.
        let (cut, uris) = parse(GNOME, "cut\nfile:///a/b.md\nfile:///a/c.md");
        assert!(cut);
        assert_eq!(uris, ["file:///a/b.md", "file:///a/c.md"]);
        let (cut, uris) = parse(GNOME, "copy\nfile:///a/b.md");
        assert!(!cut, "the verb line is read, not only skipped");
        assert_eq!(uris, ["file:///a/b.md"]);

        // A plain URI list says nothing about moving, so it is a copy; its lines end CRLF and it
        // may carry comments.
        let (cut, uris) = parse(URIS, "#comment\r\nfile:///a/b.md\r\n");
        assert!(!cut);
        assert_eq!(uris, ["file:///a/b.md"]);
        assert!(parse(URIS, "").1.is_empty());
    }

    #[test]
    fn a_clip_of_several_files_is_read_back_as_it_was_written() {
        let uris = ["file:///a/b.md".to_string(), "file:///a/c d.md".to_string()];
        let (gnome, list) = payload(&uris, true);
        assert_eq!(parse(GNOME, &gnome), (true, uris.to_vec()));
        // The plain list carries the files and no verb, so a paste of it is always a copy.
        assert_eq!(parse(URIS, &list), (false, uris.to_vec()));
        assert_eq!(
            parse(GNOME, &payload(&uris, false).0),
            (false, uris.to_vec())
        );
    }
}
