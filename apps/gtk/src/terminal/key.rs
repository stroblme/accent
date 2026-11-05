//! The keys a shell's tab and a saved terminal session are known by.

use std::path::{Path, PathBuf};

use gtk::glib;

/// What a terminal's key starts with. Not a path, so nothing that walks the open documents by path
/// can collide with one.
pub(super) const KEY: &str = "terminal:";

/// Generate a sixteen-hex-digit shell key, unique across windows, saved sessions and machines.
pub fn new_key() -> String {
    format!("{KEY}{:08x}{:08x}", glib::random_int(), glib::random_int())
}

/// A debugged program's tab key: never a held shell's, so neither the holder nor a session
/// restore takes it for one.
pub fn program_key() -> String {
    format!(
        "program:{:08x}{:08x}",
        glib::random_int(),
        glib::random_int()
    )
}

/// The Debug Console's tab key: one per window, and never a held shell's.
pub fn console_key() -> String {
    "debug:console".to_string()
}

/// Whether a session's key names a shell rather than a file.
pub fn is_key(key: &str) -> bool {
    key.strip_prefix(KEY)
        .is_some_and(|id| id.len() == 16 && id.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// What a saved terminal session's key starts with: `terminal://<name>`, the way a remote vault's
/// is `ssh://…`, so the recent list, the state file and the command line all take it as they are.
pub const SESSION: &str = "terminal://";

/// The key the terminal session `name` is remembered by.
pub fn session_key(name: &str) -> PathBuf {
    PathBuf::from(format!("{SESSION}{name}"))
}

/// The name in a terminal session's key: `None` for any other key, and for a name that is empty
/// or holds a `/`.
pub fn session_name(key: &Path) -> Option<&str> {
    key.to_str()?
        .strip_prefix(SESSION)
        .filter(|name| !name.is_empty() && !name.contains('/'))
}

/// The name the holder knows the shell by: its key without the prefix.
pub(super) fn id(key: &str) -> &str {
    key.strip_prefix(KEY).unwrap_or(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_terminal_key_is_never_a_path() {
        let key = new_key();
        assert!(is_key(&key), "{key}");
        // Sixteen hex digits after the prefix, and a fresh one each time.
        assert_eq!(key.len(), "terminal:".len() + 16);
        assert_ne!(key, new_key());
        // Not loose, which is what would send it through the file machinery.
        assert!(!crate::doc::is_loose_key(&key));
        // A note that merely starts the same way is still a note.
        assert!(!is_key("terminal:notes.md"));
    }

    #[test]
    fn a_session_key_reads_back_as_its_name() {
        assert_eq!(session_key("dev"), PathBuf::from("terminal://dev"));
        assert_eq!(session_name(&session_key("dev")), Some("dev"));
        assert_eq!(session_name(&session_key("my work")), Some("my work"));
        // No name, a name that would be a path, and every other kind of key.
        assert_eq!(session_name(Path::new("terminal://")), None);
        assert_eq!(session_name(Path::new("terminal://a/b")), None);
        assert_eq!(session_name(Path::new("ssh://box/srv/vault")), None);
        assert_eq!(session_name(Path::new("/home/me/Notes")), None);
    }
}
