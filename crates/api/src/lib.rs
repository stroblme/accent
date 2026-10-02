//! accent-api: the UI-facing façade. Plain serde data types only; no GTK, no Android types.
//! Desktop links this directly; Android gets uniffi bindings of this crate; the CLI renders it as JSON-RPC over stdio.
//!
//! [`Vault`] owns the lifecycle of one open vault: three SQLite connections, the filesystem
//! watcher, and the batching that turns a Syncthing pull of 500 files into a single [`Event`].
//! The caller reads on its own connection and never waits for the worker, which is what keeps a
//! UI thread free while the vault is being indexed.

use std::sync::{Mutex, MutexGuard};

use serde::{Deserialize, Serialize};

/// The uniffi bindings Android calls this crate through. See `ffi::`.
///
/// The scaffolding is declared here rather than in the module because uniffi's macros look for
/// the tag it defines in the crate root.
#[cfg(feature = "android")]
pub mod ffi;
#[cfg(feature = "android")]
uniffi::setup_scaffolding!();
pub mod language;
pub mod link;
mod local;
mod paths;
pub mod remote;
pub mod rpc;
pub mod ssh;
mod vault;
mod worker;

#[cfg(test)]
mod tests;

pub(crate) use local::Local;
pub use paths::conflict_original_rel;
pub use vault::Vault;
// The module too: a tab matches on `fs::Read`, and the façade hands one back.
pub use accent_core::fs;

pub use accent_core::config::{Config, LspConfig, Session, VaultConfig};
pub use accent_core::diff::{DiffLine, Op};
pub use accent_core::fs::{Etag, Read, SaveError, Text};
// The module as well as its types: the git operations take a `Repo`, not a `Vault`, so callers
// reach them as `accent_api::git::status(&repo)` after asking the vault which repos there are.
pub use accent_core::git;
pub use accent_core::git::{Branch, Commit, Entry, LogRow, Repo, Status, Submodule};
pub use accent_core::index::{
    Backlink, FileRow, Match, PdfLink, Progress, ReconcileStats, SearchHit, Stats,
};
pub use accent_core::page_edit::PageEdit;
pub use accent_core::search::{self, Options, Regex};
pub use accent_core::walk::FileKind;
pub use language::{
    Completion, Completions, Diagnostic, Fold, Hover, Kind, Location, PdfPages, Pos, Range,
    Severity, Signature, Support, Symbol, Task, TextEdit,
};

// ---------------------------------------------------------------- public data

/// What happened in the vault, in vault-relative paths the UI can use directly.
///
/// No `PartialEq`: `index::Progress` has none, and an event type is matched, not compared.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Event {
    Progress(Progress),
    Reconciled(ReconcileStats),
    /// The direct children of these directories changed ("" is the vault root). The tree refills
    /// exactly these levels instead of dropping its whole cache.
    DirsChanged(Vec<String>),
    /// The direct children of these folders changed: folders the index does not walk, watched
    /// because the window lists them ([`Vault::watch_unindexed`]). Nothing the index holds moved,
    /// so listing them again is all there is to do.
    UnindexedChanged(Vec<String>),
    /// Someone else changed this note's content. Never fires for our own saves.
    FileChanged(String),
    FileRemoved(String),
    FileRenamed {
        from: String,
        to: String,
    },
    Conflict {
        original: String,
        conflict: String,
    },
    /// Something under a repository's `.git` moved: a commit, a checkout, a stage. The git pane
    /// refreshes on it, which is how a `git commit` typed in a shell reaches the UI.
    GitChanged,
    /// A remote vault is still getting ready, and this is what it is doing. Shown where the
    /// indexing progress is shown, because to the reader it is the same wait.
    ///
    /// `fraction` is how far the step has got, 0 to 1, for the one step that can measure itself:
    /// uploading the server binary, which is most of a first connection's wait. The others are
    /// waits of unknown length, and say so with `None` rather than with a number nobody computed.
    Connecting {
        what: String,
        fraction: Option<f64>,
    },
    /// The remote vault is answering. A local vault never sends this: it is connected from the
    /// moment it opens.
    Connected,
    /// The remote vault is not answering, and why. Reads stay served from whatever the UI already
    /// has; writes fail until [`Vault::reconnect`] succeeds.
    Disconnected(String),
    /// The host answered and will not serve the vault, and why: its folder is not there, or its
    /// index would not open. Otherwise [`Disconnected`](Self::Disconnected), except that trying
    /// again meets the same answer until someone changes the host.
    Refused(String),
    Error(String),
    /// What a language provider has to say about an open document, whole: an empty list clears.
    Diagnostics {
        rel: String,
        items: Vec<Diagnostic>,
    },
    /// A language provider started or finished a background job worth waiting for — the
    /// ghost-text index rebuilding, and nothing else today. `what` is what to call it on screen,
    /// and `message` how far the job says it has got, in its own words ("1200 files"), while it
    /// says so. Shown where the vault's own indexing is shown, and yielding to it: this one is
    /// optional.
    Busy {
        what: String,
        busy: bool,
        message: Option<String>,
    },
}

/// What a rename or a move would do, so the UI can confirm before anything is written.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenamePlan {
    /// `(from, to)` for each file or folder, done in this order.
    pub moves: Vec<(String, String)>,
    /// The notes whose links the moves would leave naming the wrong place, by their paths now.
    pub rewrites: Vec<String>,
    /// What the language servers already running asked to have changed because of the moves —
    /// an import naming a moved module — by each file's path now. `default`, as are the fields
    /// below, so a plan from a host that predates them still reads.
    #[serde(default)]
    pub imports: Vec<FileEdits>,
    /// Moved source files no running language server was asked about, whose imports nothing
    /// checked.
    #[serde(default)]
    pub unchecked: Vec<String>,
}

/// Edits to one file: byte ranges of its text as it was when they were asked for, which the
/// etag says, and what to put in each.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileEdits {
    pub rel: String,
    pub etag: Etag,
    pub edits: Vec<(usize, usize, String)>,
}

/// What a global replace wrote, in the shape [`RenameReport`] has: what worked is counted, what
/// failed is named, and a file the replace could not write never fails the whole call.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ReplaceReport {
    pub rewritten: Vec<String>,
    pub matches: usize,
    pub failed: Vec<(String, String)>,
    /// Whether [`Vault::undo_replace`] can put the rewritten files back: false when nothing was
    /// rewritten, or when what they held was more than an undo keeps. Defaulted, so a host that
    /// predates the undo still reports its rewrite, as one that cannot be undone.
    #[serde(default)]
    pub undoable: bool,
}

/// What undoing a Replace All did: the files it put back, the ones changed since the rewrite and
/// so left alone, and the ones it could not write.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UndoReport {
    pub restored: Vec<String>,
    pub skipped: Vec<String>,
    pub failed: Vec<(String, String)>,
}

/// What it actually did.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RenameReport {
    /// The moves that happened: all of the plan's, unless one failed.
    pub moved: Vec<(String, String)>,
    /// The move that failed and why. Nothing after it was tried.
    pub not_moved: Option<(String, String)>,
    /// The notes whose links were rewritten, and the files whose imports were, by their paths
    /// after the moves.
    pub rewritten: Vec<String>,
    pub failed: Vec<(String, String)>,
}

/// What following a PDF's page edit did to the notes that link into it, in the shape
/// [`RenameReport`] has.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RepageReport {
    /// The notes rewritten.
    pub rewritten: Vec<String>,
    /// How many links now name another page.
    pub moved: usize,
    /// The links left naming the page a delete took out, which the Undo of that delete hands back
    /// to keep.
    pub left: Vec<KeptLink>,
    pub failed: Vec<(String, String)>,
}

/// A link a page edit left where it was ([`accent_core::markdown::Repaged::left`]): its note,
/// its markup, and which of that note's links into the PDF reading the same it is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeptLink {
    pub note: String,
    pub link: String,
    pub nth: usize,
}

/// Take a lock, ignoring poison.
///
/// A panic in one query must not take the whole vault down with it, so a poisoned lock is used
/// rather than propagated: everything behind one here is a cache the next query rebuilds.
pub(crate) fn locked<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}
