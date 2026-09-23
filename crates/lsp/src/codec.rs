//! The wire framing: `Content-Length` headers and a JSON body, as LSP defines it.
//!
//! Headers are read one line at a time until the blank line, then exactly as many bytes as the
//! length announced. Servers vary in the small things — an extra `Content-Type`, a lone `\n`
//! instead of `\r\n`, a differently cased header name — so all three are tolerated here rather
//! than in every caller.

use std::io;

/// The most a single message may announce. A server that says it is about to send a gigabyte has
/// either lost framing or is not a language server, and either way the allocation is the damage:
/// the length is read from the wire before a byte of the body is.
///
/// Four times `accent-core`'s `fs::MAX_TEXT` (16 MiB), the largest document accent will open at
/// all, so a `didChange` carrying one whole and JSON-escaped still fits. Spelled out rather than
/// linked: this crate does not depend on that one, and an intra-doc link to it resolves to
/// nothing.
const MAX_FRAME: usize = 64 * 1024 * 1024;

use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt};

/// Read one message body, or `None` when the stream ends between messages.
///
/// An end *inside* the headers or the body is an error: the message was cut in half and the
/// caller must not mistake that for an orderly goodbye.
pub async fn read(r: &mut (impl AsyncBufRead + Unpin)) -> io::Result<Option<Vec<u8>>> {
    let mut len: Option<usize> = None;
    let mut started = false;
    let mut line = String::new();
    loop {
        line.clear();
        if r.read_line(&mut line).await? == 0 {
            return match started {
                false => Ok(None),
                true => Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "the headers stopped half way",
                )),
            };
        }
        started = true;
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
}
