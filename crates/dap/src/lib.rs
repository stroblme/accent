//! UI-independent Debug Adapter Protocol client with the response and event types accent reads,
//! over the language client's framing ([`accent_lsp::codec`]) on its one runtime
//! ([`accent_lsp::runtime()`]).

mod client;
pub mod types;

pub use client::{Client, DEADLINE, Error, Incoming, Incomings};
