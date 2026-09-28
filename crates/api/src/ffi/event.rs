//! What the vault tells the app about, narrowed to what a phone can act on.
//!
//! The façade's own [`crate::Event`] also carries git news, the remote connection's state and
//! language-server diagnostics. None of those exist on Android, so they are dropped here rather
//! than crossing as variants nothing ever matches.

use crate::ffi::convert::Progress;

#[derive(uniffi::Enum)]
pub enum Event {
    /// How far the first walk of a cold vault has got.
    Progress {
        progress: Progress,
    },
    /// The index is level with the files again, or as level as a walk that was `stopped` left it:
    /// a partial index, and a vault that walks again on [`Vault::resume_indexing`] alone.
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
    /// Somebody else changed this file. Never our own saves.
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

/// The ones that mean something here. `None` is an event Android has no use for.
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
        // Git, the remote connection, language servers and their background jobs, and the watch
        // on unindexed folders: none of them run on a phone, so nothing here ever has to match
        // them.
        crate::Event::GitChanged
        | crate::Event::UnindexedChanged(_)
        | crate::Event::Connecting { .. }
        | crate::Event::Connected
        | crate::Event::Disconnected(_)
        | crate::Event::Refused(_)
        | crate::Event::Diagnostics { .. }
        | crate::Event::Busy { .. } => return None,
    })
}
