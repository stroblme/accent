//! LSP `Content-Length` framing for JSON bodies. Accept extra `Content-Type` headers, LF line
//! endings, and case-insensitive header names. A server's stderr is read here too, as bounded.

use std::io;

/// Maximum announced body length, checked before allocation to bound malformed-message memory
/// use. Four times accent-core's 16 MiB text limit leaves room for a whole JSON-escaped
/// document.
const MAX_FRAME: usize = 64 * 1024 * 1024;

/// The most one message's headers may take together. LSP defines a `Content-Length` and a
/// `Content-Type`, under 100 bytes, and HTTP servers, whose header format LSP borrows, refuse a
/// single header past 8 KiB by default (nginx, Apache).
const MAX_HEADERS: u64 = 8 * 1024;

/// The most of one stderr line kept: journald's default `LineMax`, where it breaks a service's
/// output into records.
const MAX_LINE: u64 = 48 * 1024;

use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, BufReader};

/// Read one message body, or `None` for EOF between messages. EOF inside headers or a body is an
/// error because the message is incomplete.
pub async fn read(r: &mut (impl AsyncBufRead + Unpin)) -> io::Result<Option<Vec<u8>>> {
    let mut len: Option<usize> = None;
    let mut left = MAX_HEADERS;
    let mut line = String::new();
    loop {
        line.clear();
        // Read no further than the headers may run, so a line that never ends stops there.
        left -= (&mut *r).take(left).read_line(&mut line).await? as u64;
        if !line.ends_with('\n') {
            return match left {
                MAX_HEADERS => Ok(None),
                0 => Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "headers past 8 KiB",
                )),
                _ => Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "the headers stopped half way",
                )),
            };
        }
        let header = line.trim_end_matches(['\r', '\n']);
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.trim().eq_ignore_ascii_case("content-length")
        {
            len = value.trim().parse().ok();
        }
    }

    let len = len
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "a message without a length"))?;
    if len > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("a message announcing {len} bytes"),
        ));
    }
    let mut body = vec![0; len];
    r.read_exact(&mut body).await?;
    Ok(Some(body))
}

/// Each line of `r`, its line ending dropped and bytes that are no UTF-8 replaced, to `each`
/// until `r` ends: what a server logs on stderr, read for as long as it writes so that its pipe
/// never fills. A line past [`MAX_LINE`] is cut there and the rest of it skipped, so one that
/// never ends costs no more memory than one that does.
pub async fn lines(r: impl AsyncRead + Unpin, mut each: impl FnMut(String)) {
    let mut r = BufReader::new(r);
    let (mut line, mut cut) = (Vec::new(), false);
    loop {
        line.clear();
        match (&mut r).take(MAX_LINE).read_until(b'\n', &mut line).await {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        if !cut {
            let text = String::from_utf8_lossy(&line);
            each(text.trim_end_matches(['\r', '\n']).to_string());
        }
        cut = !line.ends_with(b"\n");
    }
}

/// Wrap a body in the header the other end expects. The length is in bytes, not characters.
pub fn frame(body: &[u8]) -> Vec<u8> {
    let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(body);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime;

    /// Every message on `input`, ending with the `None` that says the stream is over.
    fn read_all(input: &[u8]) -> Vec<Option<Vec<u8>>> {
        runtime().block_on(async {
            let mut r = tokio::io::BufReader::new(input);
            let mut out = Vec::new();
            loop {
                let message = read(&mut r).await.unwrap();
                let done = message.is_none();
                out.push(message);
                if done {
                    return out;
                }
            }
        })
    }

    #[test]
    fn a_framed_message_survives_the_round_trip() {
        let body = r#"{"text":"héllo 😀"}"#.as_bytes();
        let read = read_all(&frame(body));
        assert_eq!(read[0].as_deref(), Some(body));
        assert_eq!(read[1], None);
    }

    #[test]
    fn other_headers_and_bare_newlines_are_tolerated() {
        let mut input =
            b"content-length: 2\nContent-Type: application/vscode-jsonrpc\n\n{}".to_vec();
        input.extend_from_slice(&frame(b"[]"));
        let read = read_all(&input);
        assert_eq!(read[0].as_deref(), Some(&b"{}"[..]));
        assert_eq!(read[1].as_deref(), Some(&b"[]"[..]));
        assert_eq!(read[2], None);
    }

    #[test]
    fn an_empty_stream_is_not_an_error() {
        assert_eq!(read_all(b""), vec![None]);
    }

    /// The length is read before the body, so an absurd one must be refused rather than allocated.
    #[test]
    fn a_length_nobody_could_mean_is_refused() {
        let input = format!("Content-Length: {}\r\n\r\n", MAX_FRAME + 1).into_bytes();
        let e = runtime()
            .block_on(async { read(&mut tokio::io::BufReader::new(&input[..])).await })
            .expect_err("an unreadable length is an error, not an allocation");
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
    }

    /// Headers are read no further than they may run: one absurd header before a sound message,
    /// or one that never ends, is refused without being held.
    #[test]
    fn headers_past_their_limit_are_refused_unread() {
        let long = format!("X-Pad: {}\r\n", "a".repeat(8 << 20)).into_bytes();
        for input in [[long, frame(b"{}")].concat(), vec![b'a'; 8 << 20]] {
            let mut input = std::io::Cursor::new(input);
            let e = runtime()
                .block_on(read(&mut input))
                .expect_err("headers past the limit");
            assert_eq!(e.kind(), io::ErrorKind::InvalidData);
            assert!(input.position() <= MAX_HEADERS);
        }
    }

    /// A line past the limit is cut there, and reading goes on after it, through bytes that are
    /// no UTF-8 too.
    #[test]
    fn stderr_lines_are_cut_and_read_on() {
        let mut input = vec![b'a'; 3 * MAX_LINE as usize];
        input.extend(b"\n\xffafter\r\nlast");
        let mut got = Vec::new();
        runtime().block_on(lines(&input[..], |line| got.push(line)));
        let head = "a".repeat(MAX_LINE as usize);
        assert_eq!(got, [head.as_str(), "\u{fffd}after", "last"]);
    }
}
