//! The other end of the holder's socket: the one-shot `kill` and `held`.

use super::SOCKET;
use super::protocol::{self, KILL, LIST, OUTPUT};
use std::io::{self, Write};
use std::os::unix::net::UnixStream;

/// End shell `id`. Without a holder there is no shell to end, and none is started to find out.
pub fn kill(id: &str) -> io::Result<()> {
    let Some(mut conn) = holder()? else {
        return Ok(());
    };
    protocol::write_frame(&mut conn, KILL, id.as_bytes())
}

/// Print the held shells, one JSON object per line: nothing when no holder is running.
pub fn held() -> io::Result<()> {
    let Some(mut conn) = holder()? else {
        return Ok(());
    };
    protocol::write_frame(&mut conn, LIST, &[])?;
    if let Some((OUTPUT, lines)) = protocol::read_frame(&mut conn)? {
        io::stdout().write_all(&lines)?;
    }
    Ok(())
}

/// The running holder, if there is one.
fn holder() -> io::Result<Option<UnixStream>> {
    Ok(UnixStream::connect(super::dir()?.join(SOCKET)).ok())
}
