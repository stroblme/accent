//! accent-lsp: a Language Server Protocol client with no UI in it.
//!
//! The protocol types are our own, transcribed for what accent reads (see `types.rs`); the
//! transport is a child process over stdio on the shared [`runtime`].

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

mod client;
mod codec;
mod runtime;
pub mod types;

pub use client::{Client, DEADLINE, Error, Notification, Notifications};
pub use runtime::runtime;

/// A path as the protocol wants it: `file://` and percent-encoding for everything that is not an
/// unreserved character or a separator.
///
/// Hand-rolled rather than pulled in as a dependency because this and [`from_uri`] are the whole
/// of accent's business with URLs — the paths are local, so there is no host, query or fragment
/// to get right.
pub fn to_uri(path: &Path) -> String {
    let mut uri = String::from("file://");
    for byte in path.as_os_str().as_encoded_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                uri.push(*byte as char)
            }
            _ => {
                let _ = write!(uri, "%{byte:02X}");
            }
        }
    }
    uri
}

/// The path a `file://` URI names, or `None` for any other scheme — an `http://` target in a
/// server's answer is a link to open, not a file to jump to.
pub fn from_uri(uri: &str) -> Option<PathBuf> {
    let raw = uri.strip_prefix("file://")?.as_bytes();
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'%'
            && let (Some(hi), Some(lo)) = (
                raw.get(i + 1).copied().and_then(hex),
                raw.get(i + 2).copied().and_then(hex),
            )
        {
            out.push(hi * 16 + lo);
            i += 3;
        } else {
            out.push(raw[i]);
            i += 1;
        }
    }
    Some(PathBuf::from(String::from_utf8(out).ok()?))
}

fn hex(byte: u8) -> Option<u8> {
    (byte as char).to_digit(16).map(|d| d as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_survives_the_trip_through_a_uri() {
        let path = Path::new("/home/me/Notes/Ein Ärger/note ü.md");
        let uri = to_uri(path);
        assert!(uri.starts_with("file:///home/me/Notes/Ein%20"));
        assert_eq!(from_uri(&uri).as_deref(), Some(path));
    }

    #[test]
    fn only_file_uris_are_paths() {
        assert_eq!(from_uri("https://example.org/a"), None);
    }
}
