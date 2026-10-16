//! Serializable core errors for local, process and Android callers.
//!
//! Callers match the kind; messages complete `Cannot <what>: <why>`. Required context is
//! included at the failure site rather than carried in a source chain.

use std::fmt::Display;

use serde::{Deserialize, Serialize};

use crate::fs::Etag;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize, Deserialize)]
pub enum Error {
    /// The requested file or resource does not exist.
    #[error("{0} does not exist")]
    NotFound(String),
    #[error("{0} already exists")]
    AlreadyExists(String),
    /// A save refused because the file changed since it was read; `current` is what is there now,
    /// so the window can offer a comparison against it.
    #[error("the file changed on disk since it was read")]
    ChangedOnDisk { current: Etag },
    /// A branch delete refused because its commits are not merged: the one refusal worth
    /// offering to force. `kept` says where they would still be.
    #[error("{branch} is not fully merged")]
    NotMerged {
        branch: String,
        kept: crate::git::Kept,
    },
    /// The caller abandoned the request, such as a superseded search.
    #[error("cancelled")]
    Cancelled,
    #[error("{0}")]
    Io(String),
    #[error("{0}")]
    Index(String),
    /// git ran and refused, in its own words.
    #[error("{0}")]
    Git(String),
    #[error("{0}")]
    Config(String),
    #[error("{0}")]
    Pdf(String),
    /// Asked for something that cannot be: a path outside the vault, a page past the last.
    #[error("{0}")]
    Invalid(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

impl Error {
    /// Convert an I/O error for `what`, retaining missing and existing-file variants that callers
    /// can match. Other errors include `what` in their message.
    pub fn io(what: impl Display, e: std::io::Error) -> Error {
        use std::io::ErrorKind::*;
        match e.kind() {
            NotFound => Error::NotFound(what.to_string()),
            AlreadyExists => Error::AlreadyExists(what.to_string()),
            InvalidInput | InvalidData => Error::Invalid(format!("{what}: {e}")),
            _ => Error::Io(format!("{what}: {e}")),
        }
    }
}

/// An interrupted query is one nobody waits for; anything else the index said is the index's.
impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Error {
        match e.sqlite_error_code() {
            Some(rusqlite::ErrorCode::OperationInterrupted) => Error::Cancelled,
            _ => Error::Index(e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_io_error_keeps_the_kinds_callers_branch_on() {
        let io = |kind| std::io::Error::new(kind, "why");
        assert_eq!(
            Error::io("a.md", io(std::io::ErrorKind::NotFound)),
            Error::NotFound("a.md".into())
        );
        assert_eq!(
            Error::io("a.md", io(std::io::ErrorKind::AlreadyExists)),
            Error::AlreadyExists("a.md".into())
        );
        let denied = Error::io("a.md", io(std::io::ErrorKind::PermissionDenied));
        assert_eq!(denied.to_string(), "a.md: why");
    }

    #[test]
    fn an_interrupted_query_is_cancelled() {
        let code = |c| rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(c), None);
        assert_eq!(
            Error::from(code(rusqlite::ffi::SQLITE_INTERRUPT)),
            Error::Cancelled
        );
        assert!(matches!(
            Error::from(code(rusqlite::ffi::SQLITE_BUSY)),
            Error::Index(_)
        ));
    }
}
