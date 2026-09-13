//! One error for the whole surface.
//!
//! Kotlin gains nothing from the shape of a failure it cannot act on, so everything that is not
//! the save conflict — the one case with a real answer, since the caller holds an edit and the
//! file has moved under it — arrives as a sentence.

use accent_core::fs::{Etag, SaveError};

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum AccentError {
    /// The file changed since it was read, so the save was refused. `current` is what is on disk
    /// now: the caller either reloads from it or saves again against it.
    #[error("the file changed on disk")]
    ChangedOnDisk { current: Etag },
    #[error("{reason}")]
    Failed { reason: String },
}

impl From<std::io::Error> for AccentError {
    fn from(e: std::io::Error) -> Self {
        AccentError::Failed {
            reason: e.to_string(),
        }
    }
}

impl From<anyhow::Error> for AccentError {
    fn from(e: anyhow::Error) -> Self {
        AccentError::Failed {
            reason: format!("{e:#}"),
        }
    }
}

impl From<SaveError> for AccentError {
    fn from(e: SaveError) -> Self {
        match e {
            SaveError::ChangedOnDisk { current } => AccentError::ChangedOnDisk { current },
            other => AccentError::Failed {
                reason: other.to_string(),
            },
        }
    }
}

pub type Answer<T> = Result<T, AccentError>;
