//! ZMTP 3.0 (rfc.zeromq.org/spec/23) as far as a Jupyter client needs it: the NULL mechanism,
//! a DEALER or a SUB, frames in and out. A 3.1 peer such as libzmq 4.3 speaks 3.0 to us.

use std::io;
use std::path::Path;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader, BufWriter};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

const MORE: u8 = 1;
const LONG: u8 = 2;
const COMMAND: u8 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Dealer,
    Sub,
}

/// Ours: the signature, version 3.0, the NULL mechanism, as a client.
fn greeting() -> [u8; 64] {
    let mut greeting = [0; 64];
    (greeting[0], greeting[9], greeting[10]) = (0xff, 0x7f, 3);
    greeting[12..16].copy_from_slice(b"NULL");
    greeting
}

/// Greet the peer on `s` and say READY as a socket of `kind`, a DEALER under `identity`; a SUB
/// then subscribes to everything.
pub(crate) async fn handshake<S>(s: &mut S, kind: Kind, identity: &[u8]) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let ours = greeting();
    s.write_all(&ours).await?;
    let mut theirs = [0; 64];
    s.read_exact(&mut theirs).await?;
    if theirs[0] != 0xff || theirs[9] != 0x7f || theirs[10] < 3 || theirs[12..32] != ours[12..32] {
        return Err(refused(
            "its peer does not speak ZMTP 3 with the NULL mechanism",
        ));
    }
    let mut ready = b"\x05READY".to_vec();
    let mut property = |name: &str, value: &[u8]| {
        ready.push(name.len() as u8);
        ready.extend(name.as_bytes());
        ready.extend((value.len() as u32).to_be_bytes());
        ready.extend(value);
    };
    match kind {
        Kind::Dealer => {
            property("Socket-Type", b"DEALER");
            property("Identity", identity);
        }
        Kind::Sub => property("Socket-Type", b"SUB"),
    }
    write_frame(s, COMMAND, &ready).await?;
    let (flags, body) = read_frame(s).await?;
    if flags & COMMAND == 0 || !body.starts_with(b"\x05READY") {
        return Err(refused("its peer did not say READY"));
    }
    if kind == Kind::Sub {
        write_frame(s, 0, &[1]).await?;
    }
    s.flush().await
}

fn refused(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why)
}

/// Connect to the socket at `path` as a socket of `kind`.
pub(crate) async fn connect(
    path: &Path,
    kind: Kind,
    identity: &[u8],
) -> io::Result<(
    Reader<BufReader<OwnedReadHalf>>,
    Writer<BufWriter<OwnedWriteHalf>>,
)> {
    let mut stream = UnixStream::connect(path).await?;
    handshake(&mut stream, kind, identity).await?;
    let (read, write) = stream.into_split();
    Ok((Reader(BufReader::new(read)), Writer(BufWriter::new(write))))
}

async fn write_frame<W: AsyncWrite + Unpin>(w: &mut W, flags: u8, body: &[u8]) -> io::Result<()> {
    match u8::try_from(body.len()) {
        Ok(size) => w.write_all(&[flags, size]).await?,
        Err(_) => {
            w.write_u8(flags | LONG).await?;
            w.write_u64(body.len() as u64).await?;
        }
    }
    w.write_all(body).await
}

async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> io::Result<(u8, Vec<u8>)> {
    let flags = r.read_u8().await?;
    let size = match flags & LONG {
        0 => u64::from(r.read_u8().await?),
        _ => r.read_u64().await?,
    };
    let size = usize::try_from(size).map_err(|_| refused("a frame is too long"))?;
    let mut body = vec![0; size];
    r.read_exact(&mut body).await?;
    Ok((flags, body))
}

pub(crate) struct Reader<R>(R);

impl<R: AsyncRead + Unpin> Reader<R> {
    /// The next message's frames; a command between messages is skipped.
    pub(crate) async fn recv(&mut self) -> io::Result<Vec<Vec<u8>>> {
        let mut frames = Vec::new();
        loop {
            let (flags, body) = read_frame(&mut self.0).await?;
            if flags & COMMAND != 0 {
                continue;
            }
            frames.push(body);
            if flags & MORE == 0 {
                return Ok(frames);
            }
        }
    }
}

pub(crate) struct Writer<W>(W);

impl<W: AsyncWrite + Unpin> Writer<W> {
    pub(crate) async fn send(&mut self, frames: &[Vec<u8>]) -> io::Result<()> {
        for (i, frame) in frames.iter().enumerate() {
            let more = if i + 1 < frames.len() { MORE } else { 0 };
            write_frame(&mut self.0, more, frame).await?;
        }
        self.0.flush().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The properties of a READY's `body`, after its name.
    fn properties(body: &[u8]) -> Vec<(String, Vec<u8>)> {
        let mut rest = &body[6..];
        let mut out = Vec::new();
        while let [len, tail @ ..] = rest {
            let (name, tail) = tail.split_at(*len as usize);
            let (size, tail) = tail.split_at(4);
            let size = u32::from_be_bytes(size.try_into().unwrap()) as usize;
            out.push((
                String::from_utf8(name.to_vec()).unwrap(),
                tail[..size].to_vec(),
            ));
            rest = &tail[size..];
        }
        out
    }

    /// A ROUTER or PUB peer's half of the handshake, answering what our side said.
    async fn peer(mut s: UnixStream, kind: &[u8]) -> (UnixStream, Vec<(String, Vec<u8>)>) {
        let mut greeting = [0; 64];
        s.read_exact(&mut greeting).await.unwrap();
        assert_eq!((greeting[0], greeting[9], greeting[10]), (0xff, 0x7f, 3));
        assert_eq!(&greeting[12..17], b"NULL\0");
        s.write_all(&super::greeting()).await.unwrap();
        let (flags, ready) = read_frame(&mut s).await.unwrap();
        assert_eq!(flags, COMMAND);
        let mut answer = b"\x05READY\x0bSocket-Type".to_vec();
        answer.extend((kind.len() as u32).to_be_bytes());
        answer.extend(kind);
        write_frame(&mut s, COMMAND, &answer).await.unwrap();
        (s, properties(&ready))
    }

    #[test]
    fn a_handshake_and_frames_cross_a_socket_pair() {
        accent_lsp::runtime().block_on(async {
            let (mut ours, theirs) = UnixStream::pair().unwrap();
            let answered = tokio::spawn(peer(theirs, b"ROUTER"));
            handshake(&mut ours, Kind::Dealer, b"me").await.unwrap();
            let (theirs, said) = answered.await.unwrap();
            let expected = [("Socket-Type", &b"DEALER"[..]), ("Identity", b"me")];
            let expected: Vec<_> = expected.map(|(n, v)| (n.to_string(), v.to_vec())).into();
            assert_eq!(said, expected);

            // Both ways, a long frame and an empty one among them.
            let message = vec![b"a".to_vec(), vec![7; 300], Vec::new()];
            let (our_read, our_write) = ours.into_split();
            let (their_read, their_write) = theirs.into_split();
            let (mut our_read, mut our_write) = (Reader(our_read), Writer(our_write));
            let (mut their_read, mut their_write) = (Reader(their_read), Writer(their_write));
            our_write.send(&message).await.unwrap();
            assert_eq!(their_read.recv().await.unwrap(), message);
            their_write.send(&message).await.unwrap();
            assert_eq!(our_read.recv().await.unwrap(), message);

            // A SUB subscribes to everything once it is READY.
            let (mut ours, theirs) = UnixStream::pair().unwrap();
            let answered = tokio::spawn(peer(theirs, b"PUB"));
            handshake(&mut ours, Kind::Sub, b"").await.unwrap();
            let (mut theirs, said) = answered.await.unwrap();
            assert_eq!(said, [("Socket-Type".to_string(), b"SUB".to_vec())]);
            assert_eq!(read_frame(&mut theirs).await.unwrap(), (0, vec![1]));
        });
    }
}
