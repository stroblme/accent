//! Keep terminal shells alive across windows. A holder owns each pty and screen model;
//! `attach` relays a tab or `ssh -t`, and closing the tab only detaches. Sockets live under
//! `$TMPDIR/accent-<uid>/` so remote holders survive logout.

pub mod client;
pub mod clip;
pub mod daemon;
mod discovery;
pub mod mcp;
mod protocol;
mod screen;

use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

/// Failure status for `attach`. Avoid 255, which SSH uses for lost connections.
pub const FAILED: i32 = 254;

// Bump only for incompatible wire changes; discovery keeps existing shells on supported older
// holders. Additive operations should preserve the terminal wire and negotiate their support.
const SOCKET: &str = "hold-3.sock";
const LOCK: &str = "hold-3.lock";

/// Create a private `$TMPDIR/accent-<uid>` directory, rejecting one accessible to others.
pub(crate) fn dir() -> io::Result<PathBuf> {
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

/// The same vault token is used by its MCP server and a terminal explicitly shared with it.
pub(crate) fn mcp_owner(vault: &Path) -> io::Result<String> {
    let root = vault.canonicalize()?;
    Ok(format!(
        "mcp:{}",
        accent_api::ssh::hash_of(root.as_os_str().as_bytes())
    ))
}

/// Ignore lock poison so one connection panic cannot terminate every held shell.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}
