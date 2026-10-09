//! The one error the core hands back.
//!
//! Plain data rather than a chain of sources: a caller matches on the kind, the message reads as
//! the end of "Cannot <what>: <why>", and the whole of it crosses a process or the Android
//! boundary as it is. Whatever context a failure needs is formatted into it where it is made.

use std::fmt::Display;

use serde::{Deserialize, Serialize};

use crate::fs::Etag;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize, Deserialize)]
pub enum Error {
    /// The file, or whatever was looked for, is not there.
    #[error("{0} does not exist")]
    NotFound(String),
    #[error("{0} already exists")]
    AlreadyExists(String),
    /// A save refused because the file changed since it was read; `current` is what is there now,
    /// so the window can offer a comparison against it.
    #[error("the file changed on disk since it was read")]
    ChangedOnDisk { current: Etag },
    /// git would not delete this branch because its commits are merged nowhere: the one refusal
    /// worth offering to force.
    #[error("{0} is not fully merged")]
    NotMerged(String),
    /// Nobody waits for the answer any more: a search a newer one superseded, a query interrupted.
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
    /// An io error met on `what`, a path or the thing being done. A missing file and a taken name
    /// keep their kind, which callers branch on; anything else reads as `what` and the reason.
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
