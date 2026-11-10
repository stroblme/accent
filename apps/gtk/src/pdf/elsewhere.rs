//! The pages of other PDFs a link preview shows, read on a thread of their own: one for the app,
//! started by the first such preview, which keeps the last document it read open, so hovering the
//! links into one file opens it once.

use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::SystemTime;

use accent_core::pdf::{PdfDoc, Remote, RgbaImage, Theme};
use futures_channel::oneshot;

use super::LOWRES_W;

/// A page of another PDF as a link preview shows it: its number, the y down it the link wants at
/// the top of the view, its height in points, and the page drawn at the preview's width.
pub struct Seen {
    pub page: usize,
    pub top: Option<f32>,
    pub height: f32,
    pub image: RgbaImage,
}

struct Ask {
    path: PathBuf,
    dest: Remote,
    theme: Theme,
    answer: oneshot::Sender<Option<Seen>>,
}

/// The document last read, the file it came from, and that file's modification time and length
/// when it did, which tell a file written since from the one read.
type Open = Option<(PathBuf, Option<(SystemTime, u64)>, PdfDoc)>;

/// Ask for the page `dest` names in the PDF at `path`, drawn in `theme`. It answers `None` for a
/// file that does not open as a PDF or a place it has not, and drops an ask a newer one overtook.
pub fn page(path: PathBuf, dest: Remote, theme: Theme) -> oneshot::Receiver<Option<Seen>> {
    static ASKS: OnceLock<Sender<Ask>> = OnceLock::new();
    let asks = ASKS.get_or_init(|| {
        let (asks, asked) = channel();
        let thread = std::thread::Builder::new().name("accent-pdf-elsewhere".to_string());
        if let Err(e) = thread.spawn(move || serve(asked)) {
            tracing::warn!("cannot start the reader of other PDFs: {e}");
        }
        asks
    });
    let (answer, answered) = oneshot::channel();
    let _ = asks.send(Ask {
        path,
        dest,
        theme,
        answer,
    });
    answered
}

/// Answer the newest ask, the ones before it being for links the pointer has left.
fn serve(asked: Receiver<Ask>) {
    let mut open = None;
    while let Ok(mut ask) = asked.recv() {
        while let Ok(newer) = asked.try_recv() {
            ask = newer;
        }
        let seen = read(&mut open, &ask);
        let _ = ask.answer.send(seen);
    }
}

fn read(open: &mut Open, ask: &Ask) -> Option<Seen> {
    let stamp = std::fs::metadata(&ask.path)
        .ok()
        .and_then(|m| Some((m.modified().ok()?, m.len())));
    if !matches!(open, Some((path, read, _)) if *path == ask.path && *read == stamp) {
        // The last one goes first: one other document at a time is open here.
        *open = None;
        *open = Some((ask.path.clone(), stamp, PdfDoc::open(&ask.path).ok()?));
    }
    let doc = &open.as_ref()?.2;
    let (page, top) = doc.destination(&ask.dest).ok()??;
    let (width, height) = doc.page_size(page).ok()?;
    let image = doc
        .render_page(page, LOWRES_W as f32 / width.max(1.0), ask.theme)
        .ok()?;
    Some(Seen {
        page,
        top,
        height,
        image,
    })
}
