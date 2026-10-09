//! The one error the façade hands back, whichever backend answered.
//!
//! What the core can say is [`Error::Core`], unchanged; the rest is what only a vault reached over
//! a link or a language server can meet. Plain data, as the core's is, so it crosses the wire
//! whole: a failure on the host arrives here as the same value it was there.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize, Deserialize)]
pub enum Error {
    #[error(transparent)]
    Core(#[from] accent_core::Error),
    /// There is no link to the host: it went, or it is still being made. Nothing was asked of it.
    #[error("{0}")]
    Offline(String),
    /// The link went while the call was out, so the host may have done what it was asked: the
    /// answer is what was lost.
    #[error("{0}")]
    Lost(String),
    /// The host will not serve the vault, its folder not there or its index not opening. Asking
    /// again meets the same answer until someone changes the host.
    #[error("{0}")]
    Refused(String),
    /// ssh, a forward, a transfer or a deadline failed in a way none of the above names.
    #[error("{0}")]
    Remote(String),
    /// The host's `accent-cli` has no such method: it is older than this window.
    #[error("the host's accent-cli has no method {0}")]
    UnknownMethod(String),
    /// The two ends disagree on what was sent: an argument or an answer that does not read.
    #[error("{0}")]
    Protocol(String),
    /// A language server failed, or there is none to ask.
    #[error("{0}")]
    Language(String),
    /// A language server cannot answer yet, as one still loading the project says.
    #[error("{0}")]
    NotYet(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

impl Error {
    /// Whether the link, rather than the call, is what failed.
    pub fn is_offline(&self) -> bool {
        matches!(self, Error::Offline(_) | Error::Lost(_))
    }

    /// Whether the call never reached the host, so asking again once the link is back cannot do
    /// twice what it does: a rewrite of the vault, say.
    pub fn unasked(&self) -> bool {
        matches!(self, Error::Offline(_))
    }
}

impl From<accent_lsp::Error> for Error {
    fn from(e: accent_lsp::Error) -> Error {
        Error::Language(e.to_string())
    }
}

/// A language task that was dropped is one nobody waits for; one that panicked failed.
impl From<tokio::task::JoinError> for Error {
    fn from(e: tokio::task::JoinError) -> Error {
        match e.is_cancelled() {
            true => accent_core::Error::Cancelled.into(),
            false => Error::Language(e.to_string()),
        }
    }
}

// ponytail: the two below carry the façade's old shapes, `SaveError` and `io::Result`, until it
// speaks `Error` itself. They go with them.
impl From<accent_core::fs::SaveError> for Error {
    fn from(e: accent_core::fs::SaveError) -> Error {
        use accent_core::fs::SaveError;
        match e {
            SaveError::ChangedOnDisk { current } => {
                accent_core::Error::ChangedOnDisk { current }.into()
            }
            SaveError::Offline => Error::Offline(e.to_string()),
            SaveError::Io(e) => accent_core::Error::io("the file", e).into(),
        }
    }
}

impl From<Error> for std::io::Error {
    fn from(e: Error) -> std::io::Error {
        match e {
            Error::Core(e) => e.into(),
            Error::Offline(_) | Error::Lost(_) => {
                std::io::Error::new(std::io::ErrorKind::NotConnected, e)
            }
            e => std::io::Error::other(e),
        }
    }
}

// ponytail: until the wire carries `Error` itself.
impl From<crate::rpc::RpcError> for Error {
    fn from(e: crate::rpc::RpcError) -> Error {
        use crate::rpc::{CONNECTING, DISCONNECTED, LOST, REFUSED};
        match e.code {
            DISCONNECTED | CONNECTING => Error::Offline(e.message),
            LOST => Error::Lost(e.message),
            REFUSED => Error::Refused(e.message),
            _ => Error::Remote(e.message),
        }
    }
}
