//! Save, upload and reload the file behind a PDF tab.

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

    /// Save on tab close while retaining the tab until completion, so `connect_saved` can upload
    /// a remote copy. [`Request::Close`] first removes ink hidden by the ending history.
    pub fn flush_closing(self: &Rc<Self>) {
        let (tx, rx) = channel();
        self.ask(Request::Close(Some(tx)));
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
    /// write still on the render thread's queue would go with it. With `last`, the history ends
    /// here, as when the tab closes.
    ///
    // ponytail: up to a second of the main loop, and only on the way out. The thread answers as
    // soon as it finishes whatever tile it is on, so in practice this is a few milliseconds.
    pub fn flush_blocking(self: &Rc<Self>, last: bool) {
        let (tx, rx) = channel();
        self.ask(match last {
            true => Request::Close(Some(tx)),
            false => Request::Save(Some(tx)),
        });
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

    /// Claim an upload, or return `false` and schedule another pass if one is running. Coalesce
    /// saves during transfer to avoid concurrent uploads and keep the remote at most one
    /// transfer behind.
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

    /// Reload after [`SETTLE`] without losing page, scroll or zoom, supporting PDF rebuilds
    /// during LaTeX editing. Each change postpones the reload until writing settles.
    ///
    /// Ignore the tab's own saved etag: its in-memory document already contains that write, and
    /// reloading could discard newer strokes.
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
