//! UI-independent Jupyter kernel client: our own, on the language client's one runtime
//! ([`accent_lsp::runtime()`]), as the language and debug clients are.
//!
//! It speaks Jupyter's messaging protocol to a kernel it starts, over ZMTP 3.0's NULL mechanism
//! on unix sockets: DEALER for shell, stdin and control, SUB for IOPub, and no heartbeat, the
//! kernel being our child, whose exit is how its death is heard. That much of ZeroMQ is a few
//! hundred lines; zmq.rs would bring a second async stack and a regex engine into the server a
//! remote vault uploads, and libzmq is C++ to cross-compile for musl.

mod kernel;
mod wire;
mod zmtp;

use std::time::Duration;

pub use kernel::{Channel, Incoming, Incomings, Kernel};
pub use wire::{Header, Message};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("a message is not signed with the kernel's key")]
    BadSignature,
    #[error("a message is not Jupyter's: {0}")]
    Malformed(String),
    /// Before it answered: its last words, or its exit status.
    #[error("the kernel exited: {0}")]
    Exited(String),
    #[error("the kernel did not answer within {0:?}")]
    Timeout(Duration),
    #[error("the kernel has ended")]
    Closed,
}
