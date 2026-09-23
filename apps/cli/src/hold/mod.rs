//! Shells that outlive their window.
//!
//! One holder per user and machine (`accent-cli hold`) owns every shell: its pty, its process, and
//! a model of its screen. A terminal tab runs `accent-cli attach <id>` in its own pty, and that
//! client relays bytes between the tab and the holder over a unix socket, so closing the window
//! only ends the client. The next `attach` with the same id gets the shell back, redrawn with its
//! history, whichever window it lands in — and over `ssh -t` it is the same command on the host.
//!
//! The socket lives in `$TMPDIR/accent-<uid>/`, not `$XDG_RUNTIME_DIR`: the runtime dir is removed
//! at a full logout, which on a remote host happens as soon as the window's ssh master goes, and
//! the socket would vanish under a holder that is still running. tmux keeps its sockets in `/tmp`
//! for the same reason.

pub mod client;
pub mod daemon;
mod protocol;
mod screen;

use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};

/// How `attach` exits when it failed on its own account: the holder could not be started or
/// reached, or the shell could not be started. Never 255, which is ssh's own status and which the
/// window reads as a lost link.
pub const FAILED: i32 = 254;

// Versioned: a holder speaking an older wire keeps running past an upgrade, so a new one must not
// try to talk to it. Bump only when the protocol changes.
const SOCKET: &str = "hold-1.sock";
const LOCK: &str = "hold-1.lock";

/// `$TMPDIR/accent-<uid>`, made if missing, and refused unless it is a directory only we can use:
/// `/tmp` is shared, and whoever owns this directory can put a socket of theirs in our place.
fn dir() -> io::Result<PathBuf> {
    // SAFETY: `getuid` takes nothing and cannot fail.
    let uid = unsafe { libc::getuid() };
    let dir = std::env::temp_dir().join(format!("accent-{uid}"));
    match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
        Err(e) if e.kind() != io::ErrorKind::AlreadyExists => return Err(e),
        _ => {}
    }
    // `symlink_metadata`: a symlink planted under our name is not our directory.
    let meta = std::fs::symlink_metadata(&dir)?;
    if !meta.is_dir() || meta.uid() != uid || meta.mode() & 0o077 != 0 {
        let why = format!("{} is not a private directory of this user", dir.display());
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, why));
    }
    Ok(dir)
}

/// Take a lock, ignoring poison: a panic on one connection must not take every held shell down
/// with it (the `accent_api::locked` pattern).
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}
