//! A PDF's page edits carried into what names its pages by number: the notes' links into it, and
//! the places Back and Forward keep in it.
//!
//! The notes are rewritten through the vault, where the files are, as a rename's links are
//! (`Vault::repage_links`); the page edit is already made by then, so there is nothing to ask.

use crate::doc::{self, Doc};
use crate::pdftab::PdfTab;
use crate::toasts::Toast;
use crate::{App, pdfview};
use accent_api::{PageEdit, RepageReport};
use gtk::glib;
use std::rc::Rc;

impl App {
    /// The pages of `pdf` were edited, or an edit walked back or forward by Undo or Redo (`step`
    /// names it either way). The places in it follow at once; the notes linking into it are
    /// queued to follow behind the rewrites already out.
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
        if doc::is_loose_key(&key) || self.vault().is_none() {
            return;
        }
        pdf.relinks.borrow_mut().queue.push_back((edit, step));
        if !self.reconciled.get() {
            let name = doc::file_name(&key);
            let wait = format!("The links into {name} follow once the vault is indexed");
            self.relinked(pdf, &wait);
        }
        self.relink(pdf);
    }

    /// The link to the host is back, or a walk of the vault has finished: the page edits made
    /// meanwhile rewrite the notes now.
    pub(crate) fn relink_all(self: &Rc<Self>) {
        for pdf in self.docs().iter().filter_map(Doc::pdf) {
            self.relink(pdf);
        }
    }

    /// Rewrite the notes for the oldest page edit not yet followed, unless a rewrite is already
    /// out, whose landing runs this again.
    ///
    /// One the host was never asked, the link being down, goes back to the front of the queue
    /// for [`relink_all`](Self::relink_all), and nothing is said: the banner says the link went.
    /// One the link dropped under may have been carried out, and is not asked again: twice would
    /// move the links twice.
    ///
    /// None goes out before a walk of the vault has finished, the first or one stopped from the
    /// status bar: the index has not got every note linking in, and a note it missed would keep
    /// its old numbers for the next edit to move from there. A rename is refused until then.
    fn relink(self: &Rc<Self>, pdf: &Rc<PdfTab>) {
        let (Some(vault), Some(ops)) = (self.vault().cloned(), self.ops().cloned()) else {
            return;
        };
        if !self.reconciled.get() {
            return;
        }
        let (edit, step, keep) = {
            let mut relinks = pdf.relinks.borrow_mut();
            if relinks.running {
                return;
            }
            let Some((edit, step)) = relinks.queue.pop_front() else {
                return;
            };
            relinks.running = true;
            // An insert that puts back the page a delete took out keeps the links that delete
            // left naming it; a new blank page has none.
            let keep = match edit {
                PageEdit::Insert { .. } => relinks.left.remove(&step).unwrap_or_default(),
                _ => Vec::new(),
            };
            (edit, step, keep)
        };
        // The links are read off the disk, so a buffer with unsaved edits is written out first
        // and reloaded after, as a rename does.
        (ops.flush)(&[String::new()]);
        let (app, pdf, rel) = (self.clone(), pdf.clone(), pdf.key());
        glib::spawn_future_local(async move {
            let name = doc::file_name(&rel).to_string();
            let asked = keep.clone();
            let done =
                crate::work::off_thread("relink", move || vault.repage_links(&rel, edit, &asked))
                    .await;
            match done {
                Some(Ok(report)) => {
                    if let PageEdit::Delete { .. } = edit {
                        pdf.relinks
                            .borrow_mut()
                            .left
                            .insert(step, report.left.clone());
                    }
                    let unsaved = (ops.reload)(&report.rewritten);
                    if let Some(message) = relink_message(&report, unsaved) {
                        app.relinked(&pdf, &message);
                    }
                }
                Some(Err(e)) if e.unasked() => {
                    let mut relinks = pdf.relinks.borrow_mut();
                    relinks.running = false;
                    relinks.queue.push_front((edit, step));
                    if let PageEdit::Insert { .. } = edit {
                        relinks.left.insert(step, keep);
                    }
                    return;
                }
                Some(Err(e)) => app.cannot(&format!("update the links into {name}"), e),
                None => app.toast(&format!("Cannot update the links into {name}")),
            }
            pdf.relinks.borrow_mut().running = false;
            app.relink(&pdf);
        });
    }

    /// Say what a rewrite did or waits for, in place of what the last one for this document said:
    /// a burst of Undos would otherwise stack a toast each, all but the last out of date.
    fn relinked(&self, pdf: &PdfTab, message: &str) {
        self.add_toast(Toast::new(message).key(format!("relink {}", pdf.key())));
    }
}

/// What the toast says after the notes followed a page edit: how many links moved in how many
/// notes, how many name the page that went, and what could not be rewritten or reloaded. `None`
/// when the edit touched no link.
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
    use super::relink_message;
    use accent_api::{KeptLink, RepageReport};

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
