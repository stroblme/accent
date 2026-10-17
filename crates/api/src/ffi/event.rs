//! Android vault events. Drop git, remote-connection and language-server
//! variants that mobile does not use.

use crate::ffi::convert::Progress;

#[derive(uniffi::Enum)]
pub enum Event {
    /// Progress through the initial vault scan.
    Progress {
        progress: Progress,
    },
    /// A scan failed without pausing indexing ([`crate::Event::WalkFailed`]).
    /// Its error is reported separately through [`Event::Error`] if not already reported.
    WalkFailed,
    /// The vault's worker ended by a panic ([`crate::Event::WorkerGone`]), after its reason as an
    /// [`Event::Error`]: nothing indexes the vault until it is opened again.
    WorkerGone,
    /// Index reconciliation completed, possibly with a partial index if
    /// `stopped`. Only [`Vault::resume_indexing`] resumes a paused vault.
    ///
    /// [`Vault::resume_indexing`]: crate::ffi::Vault::resume_indexing
    Reconciled {
        scanned: u64,
        added: u64,
        updated: u64,
        removed: u64,
        scan_ms: u64,
        stopped: bool,
    },
    /// The direct children of these vault-relative directories changed; `""` is the root.
    DirsChanged {
        dirs: Vec<String>,
    },
    /// Another process changed this file; our own saves are excluded.
    FileChanged {
        rel: String,
    },
    FileRemoved {
        rel: String,
    },
    FileRenamed {
        from: String,
        to: String,
    },
    /// Syncthing left a conflict copy beside a note.
    Conflict {
        original: String,
        conflict: String,
    },
    Error {
        message: String,
    },
}

/// Convert supported events; `None` means Android does not use the event.
pub(crate) fn narrow(e: crate::Event) -> Option<Event> {
    Some(match e {
        crate::Event::Progress(p) => Event::Progress { progress: p.into() },
        crate::Event::Reconciled(s) => Event::Reconciled {
            scanned: s.scanned as u64,
            added: s.added as u64,
            updated: s.updated as u64,
            removed: s.removed as u64,
            scan_ms: s.scan_ms,
            stopped: s.stopped,
        },
        crate::Event::DirsChanged(dirs) => Event::DirsChanged { dirs },
        crate::Event::FileChanged(rel) => Event::FileChanged { rel },
        crate::Event::FileRemoved(rel) => Event::FileRemoved { rel },
        crate::Event::FileRenamed { from, to } => Event::FileRenamed { from, to },
        crate::Event::Conflict { original, conflict } => Event::Conflict { original, conflict },
        crate::Event::Error(message) => Event::Error { message },
        crate::Event::WalkFailed => Event::WalkFailed,
        crate::Event::WorkerGone => Event::WorkerGone,
        // Git, the remote connection, language servers and their background jobs, debug sessions
        // and the watch on unindexed folders: none of them run on a phone, so nothing here ever
        // has to match them.
        crate::Event::GitChanged
        | crate::Event::UnindexedChanged(_)
        | crate::Event::Connecting { .. }
        | crate::Event::Connected
        | crate::Event::Disconnected(_)
        | crate::Event::Refused(_)
        | crate::Event::Diagnostics { .. }
        | crate::Event::Busy { .. }
        | crate::Event::Checking { .. }
        | crate::Event::Debug { .. } => return None,
    })
}
