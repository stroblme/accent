//! What waits on a document still being opened, by its key, until the document lands, its open
//! fails, or its bytes say it is another kind than the one asked for.
//!
//! Each request keeps a lifetime of its own:
//! - work for a text tab ([`App::with_tab`]) runs once that tab lands, and goes when the open
//!   fails or the file opens as something else;
//! - a [`Reveal`] is taken by whichever document lands, or by the terminal editor that opens the
//!   file, and goes when the open fails; a PDF's print or export waits on for its pages
//!   (`App::sync_pdf_links`);
//! - a double click's keep is taken by the landing tab, and goes when the open fails;
//! - Open as Text and Open in Accent's Editor are taken where the text read decides what the
//!   file opens as, so a failed open leaves them standing;
//! - a restored tab's pane is taken when its tab is made, and dropped for every tab no longer on
//!   its way once the restore has asked for them all ([`Pending::settle`]).

use super::*;
use open::{OnDoc, Reveal};
use std::cell::Ref;

/// The work itself, once there is a tab to run it on.
pub type Work = Box<dyn FnOnce(&Rc<App>, &Rc<Tab>)>;

/// Something to do with a tab once it is open: see [`App::with_tab`].
pub struct Waiting {
    /// Failure context for pending tab work ([`App::cannot`]); session restores use
    /// `None` because `session::Asked` tracks failed opens itself.
    pub what: Option<String>,
    pub run: Work,
}

/// The requests waiting on documents still being opened, by key.
///
/// Each kind sits in a `RefCell` of its own, so looking at one (the reveal a terminal editor
/// reads, the work [`settle`](Self::settle) asks after) never holds up another.
#[derive(Default)]
pub struct Pending {
    work: RefCell<HashMap<String, Waiting>>,
    reveals: RefCell<HashMap<String, Reveal>>,
    /// Double clicks in the Files tree whose second press came before the tab did.
    keeps: RefCell<HashSet<String>>,
    /// Notebooks the reader asked to open as text (Open as Text).
    as_text: RefCell<HashSet<String>>,
    /// Files to open in accent's own editor once rather than the terminal editor picked (Open in
    /// Accent's Editor, or an editor the shell did not find).
    in_accent: RefCell<HashSet<String>>,
    /// The pane a restored tab goes into ([`App::tabs_for`]).
    panes: RefCell<HashMap<String, std::rc::Weak<Pane>>>,
}

impl Pending {
    // Registration: each replaces what the same key had waiting of its kind.

    pub fn wait(&self, key: &str, waiting: Waiting) {
        self.work.borrow_mut().insert(key.to_string(), waiting);
    }

    pub fn reveal(&self, key: &str, reveal: Reveal) {
        self.reveals.borrow_mut().insert(key.to_string(), reveal);
    }

    pub fn keep(&self, key: &str) {
        self.keeps.borrow_mut().insert(key.to_string());
    }

    pub fn as_text(&self, key: &str) {
        self.as_text.borrow_mut().insert(key.to_string());
    }

    pub fn in_accent(&self, key: &str) {
        self.in_accent.borrow_mut().insert(key.to_string());
    }

    pub fn place(&self, key: &str, pane: &Rc<Pane>) {
        self.panes
            .borrow_mut()
            .insert(key.to_string(), Rc::downgrade(pane));
    }

    // Completion: each takes its request out, for the caller to carry out.

    pub fn take_work(&self, key: &str) -> Option<Waiting> {
        self.work.borrow_mut().remove(key)
    }

    pub fn take_reveal(&self, key: &str) -> Option<Reveal> {
        self.reveals.borrow_mut().remove(key)
    }

    /// The print or export waiting on `key` ([`Reveal::Then`]), leaving any other reveal.
    pub fn take_then(&self, key: &str) -> Option<OnDoc> {
        if !matches!(self.reveal_of(key).as_deref(), Some(Reveal::Then(_))) {
            return None;
        }
        match self.take_reveal(key) {
            Some(Reveal::Then(run)) => Some(run),
            _ => None,
        }
    }

    pub fn take_keep(&self, key: &str) -> bool {
        self.keeps.borrow_mut().remove(key)
    }

    pub fn take_as_text(&self, key: &str) -> bool {
        self.as_text.borrow_mut().remove(key)
    }

    pub fn take_in_accent(&self, key: &str) -> bool {
        self.in_accent.borrow_mut().remove(key)
    }

    pub fn take_pane(&self, key: &str) -> Option<Rc<Pane>> {
        self.panes.borrow_mut().remove(key)?.upgrade()
    }

    /// The open of `key` failed: its work goes unsaid (the open says why it failed), and so do
    /// its reveal and a double click's keep.
    pub fn fail(&self, key: &str) {
        self.work.borrow_mut().remove(key);
        self.reveals.borrow_mut().remove(key);
        self.keeps.borrow_mut().remove(key);
    }

    /// `key` turned out to be a diagram or a notebook, which its text-tab work cannot run on: the
    /// work goes, and what it was for comes back to be said, unless a reveal waits, which the
    /// tab it opens as takes instead.
    pub fn retarget(&self, key: &str) -> Option<String> {
        let work = self.work.borrow_mut().remove(key);
        work.filter(|_| !self.reveals.borrow().contains_key(key))?
            .what
    }

    /// Drop the pane of every restored tab no longer on its way: landed, or failed to open.
    pub fn settle(&self) {
        self.panes.borrow_mut().retain(|key, _| self.awaited(key));
    }

    /// Whether work waits for `key`'s text tab, which keeps it from a terminal editor.
    pub fn waits(&self, key: &str) -> bool {
        self.work.borrow().contains_key(key)
    }

    /// Whether the tab `key` is still on its way: its read, or its note's for a preview tab.
    pub fn awaited(&self, key: &str) -> bool {
        self.waits(key) || rendered::note_of(key).is_some_and(|note| self.waits(note))
    }

    pub fn reveal_of(&self, key: &str) -> Option<Ref<'_, Reveal>> {
        Ref::filter_map(self.reveals.borrow(), |reveals| reveals.get(key)).ok()
    }

    /// The pane restored tab `key` is to go into, left for the tab to take.
    pub fn pane_for(&self, key: &str) -> Option<Rc<Pane>> {
        self.panes.borrow().get(key)?.upgrade()
    }

    /// The keys whose text-tab work is waiting.
    #[cfg(feature = "bench")]
    pub fn waiting(&self) -> Vec<String> {
        self.work.borrow().keys().cloned().collect()
    }
}
