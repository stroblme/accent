//! One error for the whole surface.
//!
//! Kotlin gains nothing from the shape of a failure it cannot act on, so everything that is not
//! the save conflict — the one case with a real answer, since the caller holds an edit and the
//! file has moved under it — arrives as a sentence.

use accent_core::fs::Etag;

use crate::Error;

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum AccentError {
    /// The file changed since it was read, so the save was refused. `current` is what is on disk
    /// now: the caller either reloads from it or saves again against it.
    #[error("the file changed on disk")]
    ChangedOnDisk { current: Etag },
    #[error("{reason}")]
    Failed { reason: String },
}

impl From<Error> for AccentError {
    fn from(e: Error) -> Self {
        match e {
            Error::Core(accent_core::Error::ChangedOnDisk { current }) => {
                AccentError::ChangedOnDisk { current }
            }
            other => AccentError::Failed {
                reason: other.to_string(),
            },
        }
    }
}

/// The PDF session's calls reach the core directly.
impl From<accent_core::Error> for AccentError {
    fn from(e: accent_core::Error) -> Self {
        Error::from(e).into()
    }
}

pub type Answer<T> = Result<T, AccentError>;

#[cfg(test)]
mod tests {
    use super::*;

    /// Kotlin matches the conflict alone, with the etag on disk; everything else is its sentence.
    #[test]
    fn only_the_save_conflict_keeps_its_shape() {
        let current = Etag {
            mtime_ns: 1,
            size: 2,
            ino: 3,
        };
        let conflict = Error::Core(accent_core::Error::ChangedOnDisk { current });
        assert!(matches!(
            AccentError::from(conflict),
            AccentError::ChangedOnDisk { current: c } if c == current
        ));
        for e in [
            Error::Core(accent_core::Error::NotFound("a.md".into())),
            Error::Offline("not connected".into()),
        ] {
            let reason = e.to_string();
            assert!(
                matches!(AccentError::from(e), AccentError::Failed { reason: r } if r == reason)
            );
        }
    }
}
