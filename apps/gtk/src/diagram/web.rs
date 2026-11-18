//! Download diagram web images only after Load permission, stored per file in the
//! window session rather than the diagram. Cache each URL once for canvas, export
//! and note embeds, including offline use.

use std::cell::RefCell;
use std::collections::HashSet;
use std::path::PathBuf;

use accent_drawio::File;
use gtk::glib;
use gtk::prelude::*;
use soup::prelude::*;

/// The largest picture downloaded: a diagram's picture, not a film.
const MAX_BYTES: usize = 16 << 20;

thread_local! {
    /// The pictures being downloaded now, so two tabs or a tab and an embed ask once.
    static FETCHING: RefCell<HashSet<String>> = RefCell::default();
}

/// Whether `src`, an `image=` value, is a picture on the web.
pub(super) fn on_web(src: &str) -> bool {
    src.starts_with("https://") || src.starts_with("http://")
}

/// Every picture on the web `file` links, once each, in the order of its pages.
pub fn urls(file: &File) -> Vec<String> {
    let mut seen = HashSet::new();
    let cells = file.pages.iter().flat_map(|p| &p.cells);
    cells
        .filter_map(|c| c.style.get("image").filter(|src| on_web(src)))
        .filter(|src| seen.insert(src.to_string()))
        .map(str::to_string)
        .collect()
}

/// Where the picture at `url` is kept once downloaded: named by a digest of the address.
pub fn path(url: &str) -> PathBuf {
    let digest = glib::compute_checksum_for_string(glib::ChecksumType::Sha256, url);
    let name = digest.map_or_else(String::new, |d| d.to_string());
    glib::user_cache_dir()
        .join("accent")
        .join("web-images")
        .join(name)
}

/// The downloaded bytes of the picture at `url`, `None` until it has been.
pub fn cached(url: &str) -> Option<Vec<u8>> {
    std::fs::read(path(url)).ok()
}

/// How long a download may stay silent before it is given up on.
const TIMEOUT_S: u32 = 20;

/// Download each of `urls` not downloaded or downloading yet, one after another: how many came
/// in, and how many this machine could not fetch (offline, gone, refused, too big).
pub async fn fetch(urls: &[String]) -> (usize, usize) {
    let (mut fetched, mut failed) = (0, 0);
    let session = soup::Session::new();
    session.set_timeout(TIMEOUT_S);
    // GnuTLS as MacPorts and Homebrew build it trusts what is under their prefix, which Accent.app
    // does not carry: Apple's own CA bundle stands in.
    #[cfg(target_os = "macos")]
    match gtk::gio::TlsFileDatabase::new("/etc/ssl/cert.pem") {
        Ok(anchors) => session.set_tls_database(Some(&anchors)),
        Err(e) => tracing::info!("no CA bundle for diagram pictures: {e}"),
    }
    for url in urls.iter().filter(|url| !path(url).is_file()) {
        if !FETCHING.with_borrow_mut(|f| f.insert(url.clone())) {
            continue;
        }
        match download(&session, url).await {
            Ok(()) => fetched += 1,
            Err(e) => {
                tracing::info!("diagram picture {url} not loaded: {e}");
                failed += 1;
            }
        }
        FETCHING.with_borrow_mut(|f| f.remove(url));
    }
    (fetched, failed)
}

async fn download(session: &soup::Session, url: &str) -> Result<(), String> {
    let bytes = body(session, url, MAX_BYTES).await?;
    let to = path(url);
    crate::work::off_thread("diagram picture", move || {
        // Whole or not at all: written beside and moved into place.
        let dir = to.parent().ok_or("no cache directory")?;
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let part = to.with_extension("part");
        std::fs::write(&part, &bytes).map_err(|e| e.to_string())?;
        std::fs::rename(&part, &to).map_err(|e| e.to_string())
    })
    .await
    .unwrap_or_else(|| Err("the worker stopped".to_string()))
}

/// The body of the answer to `url`, decoded, read as it comes and given up on once it passes
/// `limit` bytes, so a picture too big, packed or never ending is not kept whole first. libsoup
/// hangs up on a body let go of before its end rather than reading the rest.
async fn body(session: &soup::Session, url: &str, limit: usize) -> Result<Vec<u8>, String> {
    let message = soup::Message::new("GET", url).map_err(|e| e.to_string())?;
    let stream = session
        .send_future(&message, glib::Priority::DEFAULT)
        .await
        .map_err(|e| e.to_string())?;
    if message.status_code() != 200 {
        return Err(format!("HTTP {}", message.status_code()));
    }
    let mut bytes = Vec::new();
    loop {
        let chunk = stream
            .read_bytes_future(1 << 16, glib::Priority::DEFAULT)
            .await
            .map_err(|e| e.to_string())?;
        if chunk.is_empty() {
            return Ok(bytes);
        }
        if bytes.len() + chunk.len() > limit {
            return Err(format!("more than {limit} bytes"));
        }
        bytes.extend_from_slice(&chunk);
    }
}

#[cfg(test)]
mod tests {
    use accent_drawio::File;
    use gtk::prelude::*;
    use gtk::{gio, glib};
    use soup::prelude::*;
    use std::io::{ErrorKind, Read, Write};

    #[test]
    fn a_diagram_s_web_pictures_are_listed_once() {
        let file = File::from_bytes(
            br#"<mxfile><diagram><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
            <mxCell id="a" style="shape=image;image=https://example.org/a.png;" vertex="1" parent="1"/>
            <mxCell id="b" style="shape=image;image=https://example.org/a.png;" vertex="1" parent="1"/>
            <mxCell id="c" style="shape=image;image=data:image/png,AAAA;" vertex="1" parent="1"/>
            </root></mxGraphModel></diagram></mxfile>"#,
        )
        .unwrap();
        assert_eq!(super::urls(&file), ["https://example.org/a.png"]);
        assert_ne!(super::path("https://a/1"), super::path("https://a/2"));
    }

    /// A body read on past the limit, by the size it says, in chunks or packed, is given up on
    /// and its connection hung up, as an error's is, while a picture within it comes in whole.
    #[test]
    fn a_body_past_the_limit_is_hung_up_on() {
        const LIMIT: usize = 1 << 20;
        // How far the server writes a body without end: a client still reading gets this far.
        const CEILING: usize = 64 << 20;
        let packed = {
            let out = gio::MemoryOutputStream::new_resizable();
            let gzip = gio::ZlibCompressor::new(gio::ZlibCompressorFormat::Gzip, -1);
            let packing = gio::ConverterOutputStream::new(&out, &gzip);
            packing
                .clone()
                .into_write()
                .write_all(&vec![0; 32 << 20])
                .unwrap();
            packing.close(gio::Cancellable::NONE).unwrap();
            out.steal_as_bytes().to_vec()
        };
        let picture = b"\x89PNG\r\n\x1a\n, a small picture".to_vec();
        // Each answer's head and body, `None` for zeros the server writes until it is hung up on.
        let answers = [
            ("200 OK\r\nContent-Length: 1073741824".to_string(), None),
            ("200 OK\r\nTransfer-Encoding: chunked".to_string(), None),
            (
                "404 Not Found\r\nTransfer-Encoding: chunked".to_string(),
                None,
            ),
            (
                format!(
                    "200 OK\r\nContent-Encoding: gzip\r\nContent-Length: {}",
                    packed.len()
                ),
                Some(packed),
            ),
            (
                format!("200 OK\r\nContent-Length: {}", picture.len()),
                Some(picture.clone()),
            ),
        ];
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/a.png", listener.local_addr().unwrap());
        // What the server wrote of each body, and the error that stopped it.
        let server = std::thread::spawn(move || {
            let mut ends = Vec::new();
            for (head, body) in answers {
                let (mut stream, _) = listener.accept().unwrap();
                let timeout = Some(std::time::Duration::from_secs(10));
                stream.set_read_timeout(timeout).unwrap();
                stream.set_write_timeout(timeout).unwrap();
                let mut request = Vec::new();
                let mut byte = [0];
                while !request.ends_with(b"\r\n\r\n") {
                    stream.read_exact(&mut byte).unwrap();
                    request.push(byte[0]);
                }
                let chunked = head.contains("chunked");
                write!(stream, "HTTP/1.1 {head}\r\nConnection: close\r\n\r\n").unwrap();
                let Some(body) = body else {
                    let zeros = [0; 1 << 16];
                    let mut written = 0;
                    let stopped = loop {
                        if written >= CEILING {
                            break None;
                        }
                        let sent = match chunked {
                            true => write!(stream, "{:x}\r\n", zeros.len())
                                .and_then(|_| stream.write_all(&zeros))
                                .and_then(|_| stream.write_all(b"\r\n")),
                            false => stream.write_all(&zeros),
                        };
                        match sent {
                            Ok(()) => written += zeros.len(),
                            Err(e) => break Some(e.kind()),
                        }
                    };
                    ends.push((written, stopped));
                    continue;
                };
                stream.write_all(&body).unwrap();
            }
            ends
        });
        let context = glib::MainContext::new();
        let read = context
            .with_thread_default(|| {
                context.block_on(async {
                    let session = soup::Session::new();
                    session.set_timeout(5);
                    let mut read = Vec::new();
                    for _ in 0..5 {
                        read.push(super::body(&session, &url, LIMIT).await);
                    }
                    read
                })
            })
            .unwrap();
        let over = Err(format!("more than {LIMIT} bytes"));
        assert_eq!(
            read,
            [
                over.clone(),
                over.clone(),
                Err("HTTP 404".to_string()),
                over,
                Ok(picture)
            ]
        );
        for (written, stopped) in server.join().unwrap() {
            assert!(written < CEILING, "read on for {written} bytes");
            let hung_up = [ErrorKind::BrokenPipe, ErrorKind::ConnectionReset];
            assert!(stopped.is_some_and(|e| hung_up.contains(&e)), "{stopped:?}");
        }
    }
}
