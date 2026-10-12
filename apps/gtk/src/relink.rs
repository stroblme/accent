//! Carry PDF page edits into note links and Back/Forward locations.
//!
//! Notes are rewritten through `Vault::repage_links` after the page edit. Remote notes update
//! only after upload succeeds: the host still has old pages until then, and an `(edited)`
//! conflict copy does not replace them.
//!
//! Pending edits belong to the document, surviving tab closes; remote edits are stored beside
//! the cached file so the next window can finish them.

use crate::doc;
use crate::pdftab::PdfTab;
use crate::toasts::Toast;
use crate::{App, pdfview};
use accent_api::{KeptLink, PageEdit, RepageReport};
use gtk::glib;
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::rc::Rc;

/// Serialize link rewrites in edit order because each reads the previous rewrite's output.
#[derive(Default)]
pub(crate) struct Relinks {
    /// Pending edits in sequence order, with undo-history steps.
    queue: VecDeque<(u64, PageEdit, u32)>,
    /// The number the next edit takes.
    next: u64,
    /// The edits numbered below this are in the file on disk, so an upload starting now carries
    /// them.
    saved: u64,
    /// The edits numbered below this are in the vault, on a remote one once their upload has
    /// landed: the notes follow those.
    landed: u64,
    /// A rewrite is out on a worker.
    running: bool,
    /// Deleted-page links by history step, preserved for undo to restore their targets.
    left: HashMap<u32, Vec<KeptLink>>,
}

/// One edit handed out to follow: its number, the edit, its step and the links it keeps.
type Taken = (u64, PageEdit, u32, Vec<KeptLink>);

impl Relinks {
    /// A page edit was made. `landed` on a local vault, where the file is the vault's own.
    fn made(&mut self, edit: PageEdit, step: u32, landed: bool) {
        self.queue.push_back((self.next, edit, step));
        self.next += 1;
        if landed {
            self.landed = self.next;
        }
    }

    /// Take the oldest eligible edit unless busy; restored pages retain deleted-page links, new blank pages have none.
    fn take(&mut self) -> Option<Taken> {
        let &(n, edit, step) = self.queue.front().filter(|(n, ..)| *n < self.landed)?;
        if self.running {
            return None;
        }
        self.queue.pop_front();
        self.running = true;
        let keep = match edit {
            PageEdit::Insert { .. } => self.left.remove(&step).unwrap_or_default(),
            _ => Vec::new(),
        };
        Some((n, edit, step, keep))
    }

    /// An edit [`take`](Self::take) handed out was never asked of the host: back to the front.
    fn put_back(&mut self, (n, edit, step, keep): Taken) {
        self.running = false;
        self.queue.push_front((n, edit, step));
        if let PageEdit::Insert { .. } = edit {
            self.left.insert(step, keep);
        }
    }

    /// Discard edits uploaded as an `(edited)` conflict copy; original-page links must keep their targets.
    fn refused(&mut self) {
        let landed = self.landed;
        self.queue.retain(|(n, ..)| *n < landed);
    }

    /// Encode pending edits and deleted-page links for undo; `None` removes completed queue state.
    fn encode(&self) -> Option<Vec<u8>> {
        if self.queue.is_empty() {
            return None;
        }
        let edits: Vec<(PageEdit, u32)> = self.queue.iter().map(|&(_, e, s)| (e, s)).collect();
        let undone = |step: &u32| {
            (edits.iter()).any(|(e, s)| s == step && matches!(e, PageEdit::Insert { .. }))
        };
        let left: Vec<_> = self.left.iter().filter(|(s, _)| undone(s)).collect();
        serde_json::to_vec(&(edits, left)).ok()
    }

    /// Restore cached edits as written but not uploaded; assign fresh history steps because prior undo history is gone.
    fn decode(bytes: &[u8]) -> Relinks {
        type Kept = (Vec<(PageEdit, u32)>, Vec<(u32, Vec<KeptLink>)>);
        let Ok((edits, left)) = serde_json::from_slice::<Kept>(bytes) else {
            return Relinks::default();
        };
        let mut steps = HashMap::new();
        let mut step = |s: u32| *steps.entry(s).or_insert_with(accent_core::pdf::fresh_id);
        let queue: VecDeque<_> = (0..)
            .zip(edits)
            .map(|(n, (e, s))| (n, e, step(s)))
            .collect();
        let left = left.into_iter().map(|(s, kept)| (step(s), kept)).collect();
        let next = queue.len() as u64;
        Relinks {
            queue,
            next,
            saved: next,
            left,
            ..Relinks::default()
        }
    }
}

/// `key` after `from` was renamed to `to`, if it is at or under it.
fn renamed(key: &str, from: &str, to: &str) -> Option<String> {
    match key.strip_prefix(from)? {
        "" => Some(to.to_string()),
        rest => rest.starts_with('/').then(|| format!("{to}{rest}")),
    }
}

impl App {
    /// Update reading locations immediately; queue note-link rewrites after remote upload ([`pages_landed`](Self::pages_landed)).
    pub(crate) fn repaged(self: &Rc<Self>, pdf: &Rc<PdfTab>, edit: PageEdit, step: u32) {
        let (key, count) = (pdf.key(), pdf.page_count());
        // A place on the page that went is on the one that took its place, as the reader is.
        let map = |at: pdfview::Anchor| {
            let page = edit.map(at.page).unwrap_or(at.page);
            pdfview::Anchor { page, ..at }.clamped(count)
        };
        for pane in self.panes.borrow().iter() {
            pane.nav.borrow_mut().repage(&key, map);
        }
        // A file opened from outside the vault has no notes that link into it.
        let Some(vault) = self.vault().filter(|_| !doc::is_loose_key(&key)) else {
            return;
        };
        let relinks = self.relinks_of(&key);
        relinks.borrow_mut().made(edit, step, !vault.is_remote());
        self.keep_relinks(&key, &relinks.borrow());
        if !self.reconciled.get() {
            let name = doc::file_name(&key);
            let wait = format!("The links into {name} follow once the vault is indexed");
            self.relinked(&key, &wait);
        }
        self.relink(&key);
    }

    /// The document at `key` was written out: the page edits made so far are in the file.
    pub(crate) fn pages_saved(&self, key: &str) {
        if let Some(relinks) = self.relinks.borrow().get(key) {
            let mut relinks = relinks.borrow_mut();
            relinks.saved = relinks.next;
        }
    }

    /// What an upload of `key` starting now carries: the page edits numbered below this, which
    /// were in the file before it was read.
    pub(crate) fn pages_carried(&self, key: &str) -> u64 {
        self.relinks_of(key).borrow().saved
    }

    /// An upload of `key` landed on the host carrying the page edits numbered below `carried`:
    /// their notes follow now.
    pub(crate) fn pages_landed(self: &Rc<Self>, key: &str, carried: u64) {
        {
            let relinks = self.relinks_of(key);
            let mut relinks = relinks.borrow_mut();
            relinks.landed = relinks.landed.max(carried);
        }
        self.relink(key);
    }

    /// An upload of `key` went beside it as `(edited)`: see [`Relinks::refused`].
    pub(crate) fn pages_refused(&self, key: &str) {
        let relinks = self.relinks_of(key);
        relinks.borrow_mut().refused();
        self.keep_relinks(key, &relinks.borrow());
    }

    /// The link to the host is back, or a walk of the vault has finished: the page edits made
    /// meanwhile rewrite the notes now, a closed tab's too.
    pub(crate) fn relink_all(self: &Rc<Self>) {
        let keys: Vec<String> = self.relinks.borrow().keys().cloned().collect();
        for key in keys {
            self.relink(&key);
        }
    }

    /// Carry pending edits through rename/move, then resolve deleted-page links by their renamed note paths for undo.
    pub(crate) fn relinks_renamed(&self, from: &str, to: &str) {
        let mut all = self.relinks.borrow_mut();
        let moved: Vec<_> = (all.keys())
            .filter_map(|key| Some((key.clone(), renamed(key, from, to)?)))
            .collect();
        for (old, new) in moved {
            if let Some(relinks) = all.remove(&old) {
                all.insert(new, relinks);
            }
        }
        for (key, relinks) in all.iter() {
            let mut relinks = relinks.borrow_mut();
            for kept in relinks.left.values_mut().flatten() {
                accent_core::config::rename_in(std::slice::from_mut(&mut kept.note), from, to);
            }
            if !relinks.queue.is_empty() {
                self.keep_relinks(key, &relinks);
            }
        }
    }

    /// The page edits of the document at `key` still to follow, read back from beside its
    /// cached copy the first time on a remote vault.
    fn relinks_of(&self, key: &str) -> Rc<RefCell<Relinks>> {
        let kept = || {
            let bytes = self
                .relink_path(key)
                .and_then(|path| std::fs::read(path).ok());
            Rc::new(RefCell::new(
                bytes.map_or_else(Relinks::default, |b| Relinks::decode(&b)),
            ))
        };
        let mut all = self.relinks.borrow_mut();
        all.entry(key.to_string()).or_insert_with(kept).clone()
    }

    /// Where a remote vault keeps the edits of `key` still to follow; `None` on a local one.
    fn relink_path(&self, key: &str) -> Option<PathBuf> {
        let remote = self.vault()?.remote()?;
        accent_api::ssh::relink_path(remote.url(), key)
    }

    /// Keep `relinks` beside the cached copy of `key`, or take it away once nothing is left.
    fn keep_relinks(&self, key: &str, relinks: &Relinks) {
        let Some(path) = self.relink_path(key) else {
            return;
        };
        let kept = match relinks.encode() {
            Some(bytes) => (path.parent().map_or(Ok(()), std::fs::create_dir_all))
                .and_then(|()| std::fs::write(&path, bytes)),
            None => match std::fs::remove_file(&path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            },
        };
        if let Err(e) = kept {
            tracing::debug!("keeping the page edits of {key}: {e}");
        }
    }

    /// The name `relinks` is under now: a rename while its rewrite was out moved it.
    fn key_of(&self, relinks: &Rc<RefCell<Relinks>>) -> Option<String> {
        let all = self.relinks.borrow();
        let found = all.iter().find(|(_, r)| Rc::ptr_eq(r, relinks));
        found.map(|(key, _)| key.clone())
    }

    /// Rewrite notes for the oldest eligible page edit of `key`, one rewrite at a time.
    ///
    /// Unsent disconnected requests return to the queue for [`relink_all`](Self::relink_all).
    /// Requests interrupted after dispatch are not retried because they may have applied,
    /// and repeating them would shift links twice.
    ///
    /// Wait for a complete vault walk so every linking note is indexed before page renumbering.
    fn relink(self: &Rc<Self>, key: &str) {
        let (Some(vault), Some(ops)) = (self.vault().cloned(), self.ops().cloned()) else {
            return;
        };
        if !self.reconciled.get() {
            return;
        }
        let Some(relinks) = self.relinks.borrow().get(key).cloned() else {
            return;
        };
        let Some(taken) = relinks.borrow_mut().take() else {
            return;
        };
        self.keep_relinks(key, &relinks.borrow());
        let (_, edit, step, ref keep) = taken;
        // The links are read off the disk, so a buffer with unsaved edits is written out first
        // and reloaded after, as a rename does.
        (ops.flush)(&[String::new()]);
        let (app, rel, asked) = (self.clone(), key.to_string(), keep.clone());
        glib::spawn_future_local(async move {
            let name = doc::file_name(&rel).to_string();
            let done = crate::work::off_thread("relink", {
                let rel = rel.clone();
                move || vault.repage_links(&rel, edit, &asked)
            })
            .await;
            let key = app.key_of(&relinks).unwrap_or(rel);
            match done {
                Some(Ok(report)) => {
                    if let PageEdit::Delete { .. } = edit {
                        relinks.borrow_mut().left.insert(step, report.left.clone());
                        app.keep_relinks(&key, &relinks.borrow());
                    }
                    let unsaved = (ops.reload)(&report.rewritten);
                    if let Some(message) = relink_message(&report, unsaved) {
                        app.relinked(&key, &message);
                    }
                }
                Some(Err(e)) if e.unasked() => {
                    relinks.borrow_mut().put_back(taken);
                    app.keep_relinks(&key, &relinks.borrow());
                    return;
                }
                Some(Err(e)) => app.cannot(&format!("update the links into {name}"), e),
                None => app.toast(&format!("Cannot update the links into {name}")),
            }
            relinks.borrow_mut().running = false;
            app.relink(&key);
        });
    }

    /// Replace each document's rewrite toast so bursts of undo cannot stack stale reports.
    fn relinked(&self, key: &str, message: &str) {
        self.add_toast(Toast::new(message).key(format!("relink {key}")));
    }
}

/// Report affected links/notes, deleted-page targets and rewrite/reload failures; `None` if no link changed.
fn relink_message(report: &RepageReport, unsaved: usize) -> Option<String> {
    let plural = |n: usize, one: &str, many: &str| match n {
        1 => format!("1 {one}"),
        n => format!("{n} {many}"),
    };
    let mut parts = Vec::new();
    if report.moved > 0 {
        parts.push(format!(
            "Updated {} in {}",
            plural(report.moved, "link", "links"),
            plural(report.rewritten.len(), "note", "notes")
        ));
    }
    match report.left.len() {
        0 => {}
        1 => parts.push("1 link points into the deleted page".to_string()),
        n => parts.push(format!("{n} links point into the deleted page")),
    }
    match report.failed.len() {
        0 => {}
        1 => parts.push("1 note could not be updated".to_string()),
        n => parts.push(format!("{n} notes could not be updated")),
    }
    match unsaved {
        0 => {}
        1 => parts.push("1 note has unsaved changes and was not reloaded".to_string()),
        n => parts.push(format!(
            "{n} notes have unsaved changes and were not reloaded"
        )),
    }
    (!parts.is_empty()).then(|| parts.join("; "))
}

#[cfg(test)]
mod tests {
    use super::{Relinks, relink_message};
    use accent_api::{KeptLink, PageEdit, RepageReport};

    /// On a remote vault the notes follow an edit once an upload that started after the edit was
    /// written out has landed; one that went beside the file as `(edited)` drops what it carried,
    /// and on a local vault an edit follows at once.
    #[test]
    fn a_remote_edit_follows_once_its_upload_lands() {
        let mut relinks = Relinks::default();
        relinks.made(PageEdit::delete(1), 7, false);
        assert!(relinks.take().is_none(), "nothing has reached the host");
        relinks.saved = relinks.next;
        let carried = relinks.saved;
        // Made while the upload is out: not in what it read.
        relinks.made(PageEdit::insert(0), 8, false);
        relinks.landed = relinks.landed.max(carried);
        let taken = relinks.take().map(|(_, edit, step, _)| (edit, step));
        assert_eq!(taken, Some((PageEdit::delete(1), 7)));
        relinks.running = false;
        assert!(relinks.take().is_none(), "the insert has not landed");
        relinks.refused();
        assert!(relinks.queue.is_empty());

        let mut local = Relinks::default();
        local.made(PageEdit::insert(2), 9, true);
        assert!(local.take().is_some());
    }

    /// The queue kept beside the cached copy reads back in order, as written out and not yet on
    /// the host, an Undo of a delete still finding the links that delete left.
    #[test]
    fn the_kept_queue_reads_back() {
        let kept = KeptLink {
            note: "a.md".into(),
            anchor: "page=2".into(),
            nth: 0,
        };
        let mut relinks = Relinks::default();
        assert_eq!(relinks.encode(), None);
        relinks.made(PageEdit::delete(1), 5, false);
        relinks.made(PageEdit::insert(1), 5, false);
        relinks.made(PageEdit::Move { from: 0, to: 2 }, 6, false);
        relinks.left.insert(5, vec![kept.clone()]);
        relinks.left.insert(4, vec![kept.clone()]);

        let back = Relinks::decode(&relinks.encode().unwrap());
        let edits: Vec<_> = back.queue.iter().map(|&(n, e, _)| (n, e)).collect();
        assert_eq!(
            edits,
            [
                (0, PageEdit::delete(1)),
                (1, PageEdit::insert(1)),
                (2, PageEdit::Move { from: 0, to: 2 })
            ]
        );
        let steps: Vec<_> = back.queue.iter().map(|&(.., s)| s).collect();
        assert_eq!(steps[0], steps[1]);
        assert_ne!(steps[1], steps[2]);
        assert_eq!(back.left.len(), 1);
        assert_eq!(back.left.get(&steps[1]), Some(&vec![kept]));
        assert_eq!((back.next, back.saved, back.landed), (3, 3, 0));
    }

    #[test]
    fn the_toast_says_what_moved_and_what_was_left() {
        let left = |n| KeptLink {
            note: "a.md".into(),
            anchor: "page=2".into(),
            nth: n,
        };
        let report = |moved, notes: &[&str], left: Vec<KeptLink>| RepageReport {
            rewritten: notes.iter().map(|n| n.to_string()).collect(),
            moved,
            left,
            failed: Vec::new(),
        };
        assert_eq!(relink_message(&report(0, &[], vec![]), 0), None);
        assert_eq!(
            relink_message(&report(3, &["a.md", "b.md"], vec![]), 0).as_deref(),
            Some("Updated 3 links in 2 notes")
        );
        assert_eq!(
            relink_message(&report(1, &["a.md"], vec![left(0)]), 1).as_deref(),
            Some(
                "Updated 1 link in 1 note; 1 link points into the deleted page; \
                 1 note has unsaved changes and was not reloaded"
            )
        );
        assert_eq!(
            relink_message(&report(0, &[], vec![left(0), left(1)]), 0).as_deref(),
            Some("2 links point into the deleted page")
        );
    }
}
