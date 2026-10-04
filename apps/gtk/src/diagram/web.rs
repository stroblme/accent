//! Pictures a diagram links from the web (`image=https://…`). Nothing is fetched until the reader
//! says Load on the diagram's banner, which the window remembers for that file (the session's
//! `web_images`, never the diagram itself). Then each picture is downloaded once into the cache,
//! where the canvas, an export and a note's embed draw it from, offline too.

use std::cell::RefCell;
use std::collections::HashSet;
use std::path::PathBuf;

use accent_drawio::File;
use gtk::glib;
use soup::prelude::*;

/// The largest picture downloaded: a diagram's picture, not a film.
const MAX_BYTES: usize = 16 << 20;

thread_local! {
    /// The pictures being downloaded now, so two tabs or a tab and an embed ask once.
    static FETCHING: RefCell<HashSet<String>> = RefCell::default();
}

/// Whether `src`, an `image=` value, is a picture on the web.
fn on_web(src: &str) -> bool {
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
    let message = soup::Message::new("GET", url).map_err(|e| e.to_string())?;
    let bytes = session
        .send_and_read_future(&message, glib::Priority::DEFAULT)
        .await
        .map_err(|e| e.to_string())?;
    if message.status_code() != 200 {
        return Err(format!("HTTP {}", message.status_code()));
    }
    if bytes.len() > MAX_BYTES {
        return Err(format!("{} bytes", bytes.len()));
    }
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

#[cfg(test)]
mod tests {
    use accent_drawio::File;

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
}
