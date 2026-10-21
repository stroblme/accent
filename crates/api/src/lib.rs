//! UI-facing façade with serde types independent of GTK and Android. Desktop links directly,
//! Android uses UniFFI, and CLI serves JSON-RPC over stdio. [`Vault`] keeps indexing and caller
//! reads on separate SQLite connections.

use std::sync::{Mutex, MutexGuard};

use serde::{Deserialize, Serialize};

/// Android UniFFI bindings. Declare scaffolding at the crate root because
/// UniFFI macros look for its tag there.
#[cfg(feature = "android")]
pub mod ffi;
#[cfg(feature = "android")]
uniffi::setup_scaffolding!();
pub mod debug;
mod error;
pub mod git_remote;
pub mod kernel;
pub mod language;
pub mod link;
mod local;
mod paths;
pub mod python;
pub mod remote;
pub mod rpc;
pub mod ssh;
mod vault;
mod worker;

#[cfg(test)]
mod tests;

pub use error::{Error, Result};
pub(crate) use local::Local;
pub use paths::conflict_original_rel;
pub use vault::Vault;
// The module too: a tab matches on `fs::Read`, and the façade hands one back.
pub use accent_core::fs;

pub use accent_core::config::{Config, LspConfig, Session, VaultConfig};
pub use accent_core::diff::{DiffLine, Op};
pub use accent_core::fs::{Etag, Read, Text};
// The module as well as its types: the git operations take a `Repo`, not a `Vault`, so callers
// reach them as `accent_api::git::status(&repo)` after asking the vault which repos there are.
pub use accent_core::code::SymbolKind;
pub use accent_core::git;
pub use accent_core::git::{Branch, Commit, Entry, LogRow, Repo, Status, Submodule};
pub use accent_core::index::{
    Backlink, CodeSymbol, FileRow, HeadingHit, Match, Mention, OutLink, PdfLink, Progress,
    ReconcileStats, SearchHit, Stats, SymbolName,
};
pub use accent_core::page_edit::PageEdit;
pub use accent_core::search::{self, Options, Regex};
pub use accent_core::walk::FileKind;
pub use language::{
    Call, Completion, Completions, Diagnostic, Fold, Goals, Hover, Kind, Location, PdfPages, Pos,
    Range, Severity, Signature, Support, Symbol, Task, TextEdit, Waits,
};

// ---------------------------------------------------------------- public data

/// Vault events with vault-relative paths for the UI. No `PartialEq` because
/// `index::Progress` lacks it; consumers match event variants.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Event {
    Progress(Progress),
    Reconciled(ReconcileStats),
    /// A walk ended without finishing. What it had indexed stays, nothing is paused, and the
    /// error is an [`Error`](Self::Error), said once per vault; this is sent on every failed walk,
    /// since it is what ends the walk on screen.
    WalkFailed,
    /// The vault's worker ended by a panic, its reason an [`Error`](Self::Error) sent just before:
    /// nothing indexes or watches the vault until it is opened again. The language providers send
    /// on the same channel, so it never closes and this is how the end is heard.
    WorkerGone,
    /// The direct children of these directories changed ("" is the vault root). The tree refills
    /// exactly these levels instead of dropping its whole cache.
    DirsChanged(Vec<String>),
    /// Direct children changed in watched unindexed folders
    /// ([`Vault::watch_unindexed`]). Refresh listings only; the index is unaffected.
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
    /// Repository `.git` state changed, prompting the git pane to refresh
    /// for commits, checkouts or staging done outside the app.
    GitChanged,
    /// Remote connection progress, sharing the indexing-progress display.
    /// `fraction` is 0–1 for server uploads; `None` for steps of unknown duration.
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
    /// The host refused the vault, for example because its folder or index
    /// could not open. Unlike [`Disconnected`](Self::Disconnected), retrying
    /// requires a change on the host.
    Refused(String),
    Error(String),
    /// What a language provider has to say about an open document, whole: an empty list clears.
    Diagnostics {
        rel: String,
        items: Vec<Diagnostic>,
    },
    /// Language-provider background progress, currently ghost-index rebuilds.
    /// `what` labels the job; `message` carries its progress text (for example,
    /// "1200 files"). Display yields to vault indexing progress.
    Busy {
        what: String,
        busy: bool,
        message: Option<String>,
    },
    /// Pending checks of `rel` as sorted, separate zero-based line ranges;
    /// empty when complete. Lean's `$/lean/fileProgress` reports parallel checks
    /// whose lines may lack answers while Mathlib loads.
    Checking {
        rel: String,
        lines: Vec<std::ops::Range<u32>>,
    },
    /// What debug session `session`, the number [`Vault::debug_start`] was given, has to say.
    Debug {
        session: u64,
        news: debug::News,
    },
    /// What kernel `session`, the number [`Vault::kernel_start`] was given, has to say.
    Kernel {
        session: u64,
        news: kernel::News,
    },
}

/// What a rename or a move would do, so the UI can confirm before anything is written.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenamePlan {
    /// `(from, to)` for each file or folder, done in this order.
    pub moves: Vec<(String, String)>,
    /// The notes whose links the moves would leave naming the wrong place, by their paths now.
    pub rewrites: Vec<String>,
    /// Language-server edits needed by the moves, such as import updates,
    /// keyed by each file's current path.
    pub imports: Vec<FileEdits>,
    /// Moved source files whose imports no running language server checked.
    pub unchecked: Vec<String>,
}

/// Edits to text identified by its etag: byte ranges and replacement strings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileEdits {
    pub rel: String,
    pub etag: Etag,
    pub edits: Vec<(usize, usize, String)>,
}

/// Global-replace results: successful counts and per-file failures, following
/// [`RenameReport`]. A failed file does not fail the whole call.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ReplaceReport {
    pub rewritten: Vec<String>,
    pub matches: usize,
    pub failed: Vec<(String, String)>,
    /// Whether [`Vault::undo_replace`] can restore the rewritten files. False
    /// if nothing changed or original contents exceeded the undo budget.
    pub undoable: bool,
}

/// Replace All undo results: restored files, files changed since replacement
/// and left alone, and write failures.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UndoReport {
    pub restored: Vec<String>,
    pub skipped: Vec<String>,
    pub failed: Vec<(String, String)>,
}

/// Completed moves and rewrites, including failures.
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

/// Note-link rewrites after a PDF page edit, following [`RenameReport`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RepageReport {
    /// The notes rewritten.
    pub rewritten: Vec<String>,
    /// How many links now name another page.
    pub moved: usize,
    /// Links still naming deleted pages, retained so undo can restore them.
    pub left: Vec<KeptLink>,
    pub failed: Vec<(String, String)>,
}

/// A link unchanged by a page edit ([`accent_core::markdown::Repaged::left`]):
/// its note, `page=N&…` anchor and occurrence among that note's matching PDF links.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeptLink {
    pub note: String,
    pub anchor: String,
    pub nth: usize,
}

/// Acquire a lock despite poisoning. These locks protect rebuildable caches,
/// so a query panic must not make the whole vault unusable.
pub(crate) fn locked<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}
