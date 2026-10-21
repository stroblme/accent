//! Tagged, length-prefixed frames over a Unix socket: `tag u8 | len u32 BE | payload`.
//! `HELLO` attaches; `KILL` and `LIST` are one-shot requests. After `HELLO`, the client
//! sends `INPUT` and `RESIZE`; the holder sends `OUTPUT`, then `EXIT` or `DETACHED`.
//! Encoding stays manual because every terminal byte passes through here.

use std::ffi::{OsStr, OsString};
use std::io::{self, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

/// Client: attach to a shell, starting it if none has this id. Payload: [`Hello`].
pub const HELLO: u8 = b'h';
/// Client: attach only to an existing shell scoped to the supplied owner. Payload: [`Hello`].
pub const ATTACH_EXISTING: u8 = b'A';
/// Client: keystrokes, as the terminal sent them.
pub const INPUT: u8 = b'i';
/// Client: the terminal's new size, as [`size`] encodes it.
pub const RESIZE: u8 = b'r';
/// Client: end the shell with this id.
pub const KILL: u8 = b'k';
/// Client: which shells are held. Answered with one `OUTPUT` of JSON lines.
pub const LIST: u8 = b'l';
/// Client: create a held shell without taking a terminal attachment. Payload: [`Hello`].
pub const CREATE: u8 = b'c';
/// Client: list only shells with this owner. Payload: owner bytes.
pub const LIST_OWNED: u8 = b'L';
/// Client: read a shell's bounded output and screen. Payload: JSON scoped request.
pub const READ: u8 = b'q';
/// Client: send input to an owned shell. Payload: JSON scoped request.
pub const SEND: u8 = b's';
/// Client: stop an owned shell. Payload: JSON scoped request.
pub const STOP: u8 = b't';
/// Client: share a live terminal with one MCP vault. Payload: JSON id and owner.
pub const SHARE: u8 = b'g';
/// Client: revoke MCP sharing for a live terminal. Payload: terminal ID.
pub const REVOKE: u8 = b'v';
/// Holder: what the shell wrote, or the replay of its screen.
pub const OUTPUT: u8 = b'o';
/// Holder: one-shot request succeeded.
pub const ACK: u8 = b'a';
/// Holder: one-shot request failed. Payload: UTF-8 reason.
pub const ERROR: u8 = b'e';
/// Holder: the shell ended with this status, an `i32` BE.
pub const EXIT: u8 = b'x';
/// Holder: another terminal took this shell over.
pub const DETACHED: u8 = b'd';

/// Maximum accepted payload, limiting allocation from a corrupt length. Output and replay use
/// 64 KiB chunks.
pub const MAX: usize = 1 << 20;

/// Write one frame in a single `write_all`, so two threads that share a stream under a lock
/// never interleave their frames.
pub fn write_frame(w: &mut impl Write, tag: u8, payload: &[u8]) -> io::Result<()> {
    let mut frame = Vec::with_capacity(5 + payload.len());
    frame.push(tag);
    frame.extend((payload.len() as u32).to_be_bytes());
    frame.extend(payload);
    w.write_all(&frame)
}

/// Read one frame. Return `None` on a clean end; reject truncated frames.
pub fn read_frame(r: &mut impl Read) -> io::Result<Option<(u8, Vec<u8>)>> {
    let mut tag = [0];
    if r.read(&mut tag)? == 0 {
        return Ok(None);
    }
    let mut len = [0; 4];
    r.read_exact(&mut len)?;
    let len = u32::from_be_bytes(len) as usize;
    if len > MAX {
        let why = format!("a frame of {len} bytes");
        return Err(io::Error::new(io::ErrorKind::InvalidData, why));
    }
    let mut payload = vec![0; len];
    r.read_exact(&mut payload)?;
    Ok(Some((tag[0], payload)))
}

/// A terminal size as `rows u16 | cols u16`, big-endian.
pub fn size(rows: u16, cols: u16) -> [u8; 4] {
    let ([r0, r1], [c0, c1]) = (rows.to_be_bytes(), cols.to_be_bytes());
    [r0, r1, c0, c1]
}

/// Decode [`size`], rejecting zero dimensions that would panic the screen model.
pub fn parse_size(bytes: &[u8]) -> Option<(u16, u16)> {
    let [r0, r1, c0, c1] = bytes.try_into().ok()?;
    let (rows, cols) = (u16::from_be_bytes([r0, r1]), u16::from_be_bytes([c0, c1]));
    (rows > 0 && cols > 0).then_some((rows, cols))
}

/// What a terminal says when it attaches: which shell, and for one that has to be started, where,
/// for whom and with which environment. `rows u16 | cols u16 | id \0 cwd \0 owner \0
/// (KEY=VALUE \0)*`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hello {
    pub id: String,
    pub cwd: PathBuf,
    /// Who starts the shell, as `held` lists it: the app's state directory, empty for none.
    pub owner: String,
    pub rows: u16,
    pub cols: u16,
    pub env: Vec<(OsString, OsString)>,
}

impl Hello {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = size(self.rows, self.cols).to_vec();
        for part in [
            self.id.as_bytes(),
            self.cwd.as_os_str().as_bytes(),
            self.owner.as_bytes(),
        ] {
            out.extend(part);
            out.push(0);
        }
        for (key, value) in &self.env {
            out.extend(key.as_bytes());
            out.push(b'=');
            out.extend(value.as_bytes());
            out.push(0);
        }
        out
    }

    /// Decode a [`Hello`] or return `None`. Skip environment entries without `=`.
    pub fn decode(bytes: &[u8]) -> Option<Hello> {
        let (rows, cols) = parse_size(bytes.get(..4)?)?;
        let mut parts = bytes[4..].split(|&b| b == 0);
        let id = String::from_utf8(parts.next()?.to_vec()).ok()?;
        let cwd = PathBuf::from(OsStr::from_bytes(parts.next()?));
        let owner = String::from_utf8(parts.next()?.to_vec()).ok()?;
        let env = parts
            .filter_map(|var| {
                let eq = var.iter().position(|&b| b == b'=')?;
                let (key, value) = (
                    OsStr::from_bytes(&var[..eq]),
                    OsStr::from_bytes(&var[eq + 1..]),
                );
                Some((key.to_owned(), value.to_owned()))
            })
            .collect();
        Some(Hello {
            id,
            cwd,
            owner,
            rows,
            cols,
            env,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip() {
        let mut wire = Vec::new();
        write_frame(&mut wire, INPUT, b"ls\r").unwrap();
        write_frame(&mut wire, EXIT, &3i32.to_be_bytes()).unwrap();
        let mut r = wire.as_slice();
        assert_eq!(read_frame(&mut r).unwrap(), Some((INPUT, b"ls\r".to_vec())));
        assert_eq!(
            read_frame(&mut r).unwrap(),
            Some((EXIT, 3i32.to_be_bytes().to_vec()))
        );
        assert_eq!(read_frame(&mut r).unwrap(), None);

        // Cut inside a frame: the peer went away mid-sentence, which is not a clean end.
        assert!(read_frame(&mut &wire[..6]).is_err());

        let mut huge = vec![OUTPUT];
        huge.extend((MAX as u32 + 1).to_be_bytes());
        assert!(read_frame(&mut huge.as_slice()).is_err());
    }

    #[test]
    fn a_hello_round_trips() {
        let hello = Hello {
            id: "0123456789abcdef".into(),
            cwd: "/home/me/a dir".into(),
            owner: "/home/me/.local/state".into(),
            rows: 24,
            cols: 80,
            env: vec![
                ("SHELL".into(), "/bin/bash".into()),
                ("LS_COLORS".into(), "di=01;34:ln=01;36".into()),
            ],
        };
        assert_eq!(Hello::decode(&hello.encode()), Some(hello));
        assert_eq!(parse_size(&size(0, 80)), None);
    }
}
