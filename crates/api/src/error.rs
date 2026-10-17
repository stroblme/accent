//! Backend-independent façade errors, serialized unchanged across the wire.
//! [`Error::Core`] preserves core failures; other variants describe remote
//! connections and language servers.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize, Deserialize)]
pub enum Error {
    #[error(transparent)]
    Core(#[from] accent_core::Error),
    /// No host connection; the request was not sent.
    #[error("{0}")]
    Offline(String),
    /// The connection failed during a call. The host may have completed it
    /// without returning an answer.
    #[error("{0}")]
    Lost(String),
    /// The host refused the vault because its folder or index could not be opened.
    /// Retrying requires a change on the host.
    #[error("{0}")]
    Refused(String),
    /// SSH, forwarding, transfer, or deadline failure not covered above.
    #[error("{0}")]
    Remote(String),
    /// The host's `accent-cli` has no such method: it is older than this window.
    #[error("the host's accent-cli has no method {0}")]
    UnknownMethod(String),
    /// An argument or response could not be decoded by the other end.
    #[error("{0}")]
    Protocol(String),
    /// A language server failed, or there is none to ask.
    #[error("{0}")]
    Language(String),
    /// A language server cannot answer while still loading the project.
    #[error("{0}")]
    NotYet(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

impl Error {
    /// Whether the connection failed.
    pub fn is_offline(&self) -> bool {
        matches!(self, Error::Offline(_) | Error::Lost(_))
    }

    /// Whether the request never reached the host, making a retry safe
    /// from duplicate side effects.
    pub fn unasked(&self) -> bool {
        matches!(self, Error::Offline(_))
    }
}

impl From<accent_lsp::Error> for Error {
    fn from(e: accent_lsp::Error) -> Error {
        Error::Language(e.to_string())
    }
}

impl From<accent_dap::Error> for Error {
    fn from(e: accent_dap::Error) -> Error {
        Error::Language(e.to_string())
    }
}

/// Treat cancelled language tasks as cancelled requests and panics as failures.
impl From<tokio::task::JoinError> for Error {
    fn from(e: tokio::task::JoinError) -> Error {
        match e.is_cancelled() {
            true => accent_core::Error::Cancelled.into(),
            false => Error::Language(e.to_string()),
        }
    }
}
