//! What the clipboard holds beyond the text GTK's own paste reads: one MIME type's bytes, and
//! the HTML another application copies formatted text as, which a drop may carry too; and an
//! image put on it.

use gtk::prelude::*;
use gtk::{gdk, gio, glib};

/// What a browser, an office suite or an editor copies formatted text as.
pub const HTML: &str = "text/html";

/// What `clipboard` holds as `mime`, read whole, or `None` when it holds none.
pub async fn read(clipboard: &gdk::Clipboard, mime: &str) -> Option<Vec<u8>> {
    let (stream, _) = (clipboard
        .read_future(&[mime], glib::Priority::DEFAULT)
        .await)
        .inspect_err(|e| tracing::debug!("no {mime} on the clipboard: {e}"))
        .ok()?;
    whole(stream, mime).await
}

/// All of `stream`, which carries `mime`, however large: a page of strokes is easily more than a
/// buffer's guess.
async fn whole(stream: gio::InputStream, mime: &str) -> Option<Vec<u8>> {
    let out = gio::MemoryOutputStream::new_resizable();
    let flags =
        gio::OutputStreamSpliceFlags::CLOSE_SOURCE | gio::OutputStreamSpliceFlags::CLOSE_TARGET;
    (out.splice_future(&stream, flags, glib::Priority::DEFAULT)
        .await)
        .inspect_err(|e| tracing::warn!("reading {mime}: {e}"))
        .ok()?;
    Some(out.steal_as_bytes().to_vec())
}

/// Put the picture `draw` makes on `clipboard`, which GDK offers as PNG among the image types it
/// writes. `draw` runs on a worker: decoding a large file or drawing an SVG would hold the window.
pub fn set_texture(
    clipboard: &gdk::Clipboard,
    draw: impl FnOnce() -> Result<gdk::Texture, glib::Error> + Send + 'static,
) {
    let clipboard = clipboard.clone();
    glib::spawn_future_local(async move {
        match crate::work::off_thread("copied image", draw).await {
            Some(Ok(texture)) => clipboard.set_texture(&texture),
            Some(Err(e)) => tracing::warn!("cannot copy the image: {e}"),
            None => {}
        }
    });
}

/// The clipboard's HTML as text, or `None` when it holds none.
pub async fn html(clipboard: &gdk::Clipboard) -> Option<String> {
    read(clipboard, HTML).await.map(|bytes| decode(&bytes))
}

/// The HTML `drop` carries as text, or `None` when it carries none.
pub async fn dropped_html(drop: &gdk::Drop) -> Option<String> {
    let (stream, _) = (drop.read_future(&[HTML], glib::Priority::DEFAULT).await)
        .inspect_err(|e| tracing::debug!("no {HTML} in the drop: {e}"))
        .ok()?;
    whole(stream, HTML).await.map(|bytes| decode(&bytes))
}

/// Clipboard HTML's bytes as text: UTF-8, its byte-order mark dropped, or UTF-16 where a mark or
/// a NUL beside the first character says so, as Firefox wrote it under X11 for years.
fn decode(bytes: &[u8]) -> String {
    let utf16 = |bytes: &[u8], big: bool| {
        let units: Vec<u16> = (bytes.as_chunks::<2>().0.iter())
            .map(|&pair| match big {
                true => u16::from_be_bytes(pair),
                false => u16::from_le_bytes(pair),
            })
            .collect();
        String::from_utf16_lossy(&units)
    };
    let text = match bytes {
        [0xFF, 0xFE, rest @ ..] => utf16(rest, false),
        [0xFE, 0xFF, rest @ ..] => utf16(rest, true),
        [0xEF, 0xBB, 0xBF, rest @ ..] => String::from_utf8_lossy(rest).into_owned(),
        [_, 0, ..] => utf16(bytes, false),
        [0, _, ..] => utf16(bytes, true),
        _ => String::from_utf8_lossy(bytes).into_owned(),
    };
    // Some applications end it as a C string.
    text.trim_end_matches('\0').to_string()
}

#[cfg(test)]
mod tests {
    use super::decode;

    #[test]
    fn html_reads_in_each_encoding_it_comes_in() {
        let html = "<b>ä</b>";
        let le: Vec<u8> = html.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let be: Vec<u8> = html.encode_utf16().flat_map(u16::to_be_bytes).collect();
        for bytes in [
            html.as_bytes().to_vec(),
            [&[0xEF, 0xBB, 0xBF], html.as_bytes()].concat(),
            [&[0xFF, 0xFE], le.as_slice()].concat(),
            [&[0xFE, 0xFF], be.as_slice()].concat(),
            le.clone(),
            be,
            [html.as_bytes(), b"\0"].concat(),
        ] {
            assert_eq!(decode(&bytes), html, "{bytes:?}");
        }
    }
}
