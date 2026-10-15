//! Keep terminal shells alive across windows. A holder owns each pty and screen model;
//! `attach` relays a tab or `ssh -t`, and closing the tab only detaches. Sockets live under
//! `$TMPDIR/accent-<uid>/` so remote holders survive logout.

pub mod client;
pub mod clip;
pub mod daemon;
mod protocol;
mod screen;

use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};

/// Failure status for `attach`. Avoid 255, which SSH uses for lost connections.
pub const FAILED: i32 = 254;

// Versioned: a holder speaking an older wire keeps running past an upgrade, so a new one must not
// try to talk to it. Bump only when the protocol changes.
const SOCKET: &str = "hold-2.sock";
const LOCK: &str = "hold-2.lock";

/// Create a private `$TMPDIR/accent-<uid>` directory, rejecting one accessible to others.
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

/// Ignore lock poison so one connection panic cannot terminate every held shell.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}
