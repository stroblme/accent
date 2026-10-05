//! The file behind a PDF tab: the drawing written out, a remote vault's copy sent on and what the
//! far end said about it, and the file read again when it changes on disk.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::mpsc::channel;

use gtk::glib;

use super::protocol::Request;
use super::tab::PdfTab;

/// How long a changed file is left alone before it is read again. A PDF being written — a LaTeX
/// run, a copy — changes every few hundred milliseconds until it is done, and read half-way it
/// will not open, or opens with the pages written so far.
const SETTLE: std::time::Duration = std::time::Duration::from_millis(500);

impl PdfTab {
    /// Write out whatever has been drawn, if anything has.
    ///
    /// The thread answers when it gets there; nothing waits for it, because the write is atomic
    /// and the channel is drained before the thread ends, so a tab closing still saves.
    pub fn flush(self: &Rc<Self>) {
        self.ask(Request::Save(None));
    }

    /// The same as the tab closes, holding the tab until the write is done: its answer is what
    /// sends a remote vault's copy back to the host (`connect_saved`), and a tab already gone
    /// would never hear it.
    pub fn flush_closing(self: &Rc<Self>) {
        let (tx, rx) = channel();
        self.ask(Request::Save(Some(tx)));
        let tab = self.clone();
        glib::spawn_future_local(async move {
            while let Err(std::sync::mpsc::TryRecvError::Empty) = rx.try_recv() {
                glib::timeout_future(std::time::Duration::from_millis(50)).await;
            }
            // The thread posts its answer as an idle before it lets go of `tx`, and idles of one
            // priority run in the order they were added: this one runs after it.
            glib::idle_add_local_once(move || drop(tab));
        });
    }

    /// The same, but wait for it — the window is closing and the process is about to end, so a
    /// write still on the render thread's queue would go with it.
    ///
    // ponytail: up to a second of the main loop, and only on the way out. The thread answers as
    // soon as it finishes whatever tile it is on, so in practice this is a few milliseconds.
    pub fn flush_blocking(self: &Rc<Self>) {
        let (tx, rx) = channel();
        self.ask(Request::Save(Some(tx)));
        let _ = rx.recv_timeout(std::time::Duration::from_secs(1));
    }

    /// Write out a second after a stroke, once for a burst of them: the first stroke's second,
    /// so drawing on and on still writes once a second.
    pub(super) fn save_soon(self: &Rc<Self>) {
        self.autosave.call_once(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move || tab.flush()
        ));
    }

    /// Take the right to start sending the written-out file somewhere. `false` when one is
    /// already on its way: that one is marked to go again rather than a second starting beside
    /// it, so a burst of strokes costs two transfers and the far end is never more than one
    /// behind.
    pub fn claim_upload(&self) -> bool {
        if self.uploading.replace(true) {
            self.upload_again.set(true);
            return false;
        }
        true
    }

    /// The transfer came back: `true` when a save landed while it was out and the file has to go
    /// once more.
    pub fn upload_done(&self) -> bool {
        self.uploading.set(false);
        self.upload_again.replace(false)
    }

    /// The far end refused this document and the copy beside it, so what was written is kept on
    /// this machine. `true` the first time, which is the one the reader is told about: a reader
    /// who keeps drawing writes once a second and every one of those is refused for the same
    /// reason.
    pub fn told_conflict(&self) -> bool {
        !self.conflict_told.replace(true)
    }

    /// An upload of this document failed. `true` the first time, which is the one the reader is
    /// told about: the saves after it fail the same way until one lands.
    pub fn told_failure(&self) -> bool {
        self.unsent.set(true);
        !self.failure_told.replace(true)
    }

    /// An upload the dropped link stopped: nothing to tell, the banner says the link went.
    pub fn lost_upload(&self) {
        self.unsent.set(true);
    }

    /// Whether the last upload failed, so the file here has to go again once it can: when the
    /// link is back.
    pub fn unsent(&self) -> bool {
        self.unsent.get()
    }

    /// The reader asked for another try: a refusal or a failure after it is news again.
    pub fn forget_told(&self) {
        self.conflict_told.set(false);
        self.failure_told.set(false);
    }

    /// The conflict is over — an upload landed, the tab moved onto the copy it went into, or the
    /// document was re-read from what the far end now holds — so the next refusal or failure is
    /// news again.
    pub fn clear_conflict(&self) {
        self.conflict_told.set(false);
        self.failure_told.set(false);
        self.unsent.set(false);
    }

    /// Re-read the file, keeping the page, the scroll and the zoom. A rebuilt PDF is the reason
    /// this exists: a LaTeX loop should not send the reader back to page one.
    ///
    /// Once the file has been left alone for [`SETTLE`], not at once: every report of a change
    /// pushes the reload back, so a file still being written is read once, when it is done.
    ///
    /// A write of our own is not a reason: the document in memory *is* what was written, and
    /// re-reading it would drop annotations made since. There is no `own: true` to ride on the
    /// way a note's save has one, because the bytes never went through the vault — so the etag
    /// of what we wrote is what tells the two apart.
    pub fn refresh(self: &Rc<Self>) {
        if self.saved.get().is_some()
            && accent_core::fs::Etag::of(&self.path()).ok() == self.saved.get()
        {
            return;
        }
        self.changed.set(std::time::Instant::now());
        if !self.reload_due.replace(true) {
            self.reload_when_settled(SETTLE);
        }
    }

    /// Ask for the file again `wait` from now, or later if it has been reported changed since.
    fn reload_when_settled(self: &Rc<Self>, wait: std::time::Duration) {
        glib::timeout_add_local_once(
            wait,
            glib::clone!(
                #[weak(rename_to = tab)]
                self,
                move || match SETTLE.checked_sub(tab.changed.get().elapsed()) {
                    Some(left) if !left.is_zero() => tab.reload_when_settled(left),
                    _ => {
                        tab.reload_due.set(false);
                        tab.ask(Request::Reload);
                    }
                }
            ),
        );
    }

    /// Look at the file every [`SETTLE`] while it will not open, and read it again once it has
    /// changed. The watcher reports a vault file's changes too; this is for a file no watcher
    /// reports on — outside the vault, or in a folder git ignores — which would wait for good.
    pub(super) fn watch_while_failed(self: &Rc<Self>) {
        let seen = Cell::new(accent_core::fs::Etag::of(&self.path()).ok());
        glib::timeout_add_local(
            SETTLE,
            glib::clone!(
                #[weak(rename_to = tab)]
                self,
                #[upgrade_or]
                glib::ControlFlow::Break,
                move || {
                    if !tab.failed.get() {
                        return glib::ControlFlow::Break;
                    }
                    let now = accent_core::fs::Etag::of(&tab.path()).ok();
                    if seen.replace(now) != now {
                        tab.refresh();
                    }
                    glib::ControlFlow::Continue
                }
            ),
        );
    }
}
