//! FFI errors preserve actionable save conflicts; other failures become
//! display messages because Kotlin callers cannot act on their variants.

use accent_core::fs::Etag;

use crate::Error;

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum AccentError {
    /// Save refused because the file changed. `current` is the disk etag
    /// for reloading or retrying the save.
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
