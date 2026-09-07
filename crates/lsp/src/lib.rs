//! accent-lsp: a Language Server Protocol client with no UI in it.
//!
//! The protocol types are our own, transcribed for what accent reads (see `types.rs`); the
//! transport is a child process over stdio on the shared [`runtime`].

mod runtime;

pub use runtime::runtime;
