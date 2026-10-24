//! Locate live shells across compatible holder versions before creating a replacement.

use super::SOCKET;
use super::protocol::{self, LIST, OUTPUT};
use serde_json::Value;
use std::io;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub(super) struct Held {
    pub socket: PathBuf,
    pub shells: Vec<Value>,
}

/// Only a missing socket or a dead listener means absent; other failures must stop creation.
pub(super) fn open(socket: &Path) -> io::Result<Option<UnixStream>> {
    match UnixStream::connect(socket) {
        Ok(conn) => Ok(Some(conn)),
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            ) =>
        {
            Ok(None)
        }
        Err(e) => Err(e),
    }
}

/// v2 has the same HELLO, LIST and KILL wire as v3, but none of its scoped MCP operations.
pub(super) fn running() -> io::Result<Vec<Held>> {
    let mut holders = Vec::new();
    for entry in std::fs::read_dir(super::dir()?)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("hold-") || !name.ends_with(".sock") {
            continue;
        }
        let socket = entry.path();
        let Some(mut conn) = open(&socket)? else {
            continue;
        };
        if name != SOCKET && name != "hold-2.sock" {
            return Err(io::Error::other(format!(
                "unsupported terminal holder {name}; use its matching accent-cli to resume existing shells"
            )));
        }
        conn.set_read_timeout(Some(Duration::from_secs(5)))?;
        conn.set_write_timeout(Some(Duration::from_secs(5)))?;
        protocol::write_frame(&mut conn, LIST, &[])?;
        let Some((OUTPUT, bytes)) = protocol::read_frame(&mut conn)? else {
            return Err(io::Error::other(format!(
                "terminal holder {name} did not list its shells"
            )));
        };
        let shells = bytes
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| {
                let shell: Value = serde_json::from_slice(line).map_err(io::Error::other)?;
                if shell["id"].as_str().is_none() {
                    return Err(io::Error::other(
                        "terminal holder listed a shell without an ID",
                    ));
                }
                Ok(shell)
            })
            .collect::<io::Result<Vec<_>>>()?;
        holders.push(Held { socket, shells });
    }
    holders.sort_by(|a, b| a.socket.cmp(&b.socket));
    Ok(holders)
}

pub(super) fn find(id: &str) -> io::Result<Option<PathBuf>> {
    let mut found = None;
    for holder in running()? {
        if holder.shells.iter().any(|shell| shell["id"] == id) {
            if found.is_some() {
                return Err(io::Error::other(format!(
                    "terminal {id} exists in more than one holder; use the matching accent-cli to select it"
                )));
            }
            found = Some(holder.socket);
        }
    }
    Ok(found)
}
