//! A note as one self-contained HTML page (DESIGN.md, Preview): core writes the body, this puts in
//! its pictures and the paper stylesheet. Export as HTML… writes it where the chooser says;
//! Print… and Open in Browser hand it to the system browser, whose print dialog prints it.

use super::*;
use crate::assets;
use crate::look::{self, Look, Served};

/// What a page is for, which sets the policy in its head.
#[derive(Clone, Copy)]
pub(crate) enum Csp {
    /// A file to share: no script runs, and a web image still loads where it is read.
    File,
    /// The browser's view of the note: nothing loads but what is in the page, so a web image stays
    /// blocked as in the preview.
    Browser,
}

impl Csp {
    fn policy(self) -> &'static str {
        match self {
            Csp::File => "script-src 'none'",
            Csp::Browser => "default-src 'none'; img-src data:; style-src 'unsafe-inline'",
        }
    }
}

impl App {
    /// Export as HTML… for a note, once the chooser has answered: its page written to `dest`.
    pub(crate) fn html_to(self: &Rc<Self>, tab: Rc<Tab>, dest: PathBuf) {
        let name = dest_name(&dest);
        let app = self.clone();
        self.busy_with(
            format!("Exporting {name}…"),
            format!("export {name}"),
            async move {
                let html = page(&app, &tab.rel(), tab.text(), Csp::File).await?;
                written(dest, html).await?;
                Ok(Some(format!("Exported {name}")))
            },
        );
    }

    /// Print… and Open in Browser for a note: its page written to the cache and opened in the
    /// system browser, then `said` in a toast.
    pub(crate) fn open_in_browser(self: &Rc<Self>, tab: Rc<Tab>, said: Option<&'static str>) {
        let rel = tab.rel();
        let name = doc::file_name(&rel).to_string();
        let app = self.clone();
        self.busy_with(
            format!("Opening {name} in your browser…"),
            format!("open {name} in your browser"),
            async move {
                let html = page(&app, &rel, tab.text(), Csp::Browser).await?;
                let file = browser_file(&rel);
                written(file.clone(), html).await?;
                launch(&app.window, &file).await?;
                Ok(said.map(str::to_string))
            },
        );
    }
}

/// `text`, the note at `rel`, as one page: core writes the body on a worker, its diagrams are drawn
/// here on the main thread, and a worker fetches its pictures into it.
pub(crate) async fn page(
    app: &Rc<App>,
    rel: &str,
    text: String,
    csp: Csp,
) -> Result<String, String> {
    let stopped = || "the worker stopped".to_string();
    let note = rel.to_string();
    let body = crate::work::off_thread("export", move || {
        accent_core::markdown::to_html_file(&text, &note)
    })
    .await
    .ok_or_else(stopped)?;
    let (resolve, css) = (app.asset_resolver(), css::paper_css(&app.config.borrow()));
    let web = app.web_images.borrow().clone();
    let drawn = drawn_diagrams(&body, &resolve, rel, &web).await;
    let title = Path::new(doc::file_name(rel))
        .file_stem()
        .map_or_else(String::new, |s| s.to_string_lossy().into_owned());
    let note = rel.to_string();
    // Fetching a picture is a round trip on a remote vault, and the page may hold many.
    crate::work::off_thread("export", move || {
        let body = inline_images(&body, |uri| match drawn.get(uri) {
            Some(diagram) => diagram.clone(),
            None => data_uri(&assets::asset(&*resolve, &note, uri)?),
        });
        document(&title, &css, &body, csp)
    })
    .await
    .ok_or_else(stopped)
}

/// Write `html` to `dest` on a worker.
async fn written(dest: PathBuf, html: String) -> Result<(), String> {
    let write = move || {
        if let Some(dir) = dest.parent() {
            std::fs::create_dir_all(dir)?;
        }
        accent_core::fs::write_bytes(&dest, html.as_bytes(), None).map_err(std::io::Error::other)
    };
    match crate::work::off_thread("export", write).await {
        Some(Ok(_)) => Ok(()),
        Some(Err(e)) => Err(e.to_string()),
        None => Err("the worker stopped".to_string()),
    }
}

/// Where the browser's copy of the note at `rel` goes: the cache, named after the note and a digest
/// of its path, so two notes of one name keep a page each and opening one again replaces its own.
fn browser_file(rel: &str) -> PathBuf {
    let stem = Path::new(doc::file_name(rel))
        .file_stem()
        .map_or_else(String::new, |s| s.to_string_lossy().into_owned());
    let digest = glib::compute_checksum_for_string(glib::ChecksumType::Sha256, rel)
        .map_or_else(String::new, |d| d[..8].to_string());
    glib::user_cache_dir()
        .join("accent")
        .join("browser")
        .join(format!("{stem}-{digest}.html"))
}

/// Open `file` in the system browser through `open`, as `fileops::reveal` shows a file in Finder:
/// GTK's launcher asks a portal over D-Bus, which macOS has none of.
#[cfg(target_os = "macos")]
async fn launch(_window: &adw::ApplicationWindow, file: &Path) -> Result<(), String> {
    let mut open = std::process::Command::new("open");
    open.arg(file);
    crate::work::off_thread("open", move || open.output())
        .await
        .ok_or("the worker stopped")?
        .map_err(|e| e.to_string())
        .and_then(|out| match out.status.success() {
            true => Ok(()),
            false => Err(String::from_utf8_lossy(&out.stderr).trim().to_string()),
        })
}

/// Open `file` in the system browser through GTK's launcher: the OpenURI portal where there is one,
/// which a Flatpak needs.
#[cfg(not(target_os = "macos"))]
async fn launch(window: &adw::ApplicationWindow, file: &Path) -> Result<(), String> {
    gtk::FileLauncher::new(Some(&gio::File::for_path(file)))
        .launch_future(Some(window))
        .await
        .map_err(|e| e.to_string())
}

/// The whole file: `body` under `title`, the stylesheet and the policy `csp` sets.
fn document(title: &str, css: &str, body: &str, csp: Csp) -> String {
    format!(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\">\
         <meta http-equiv=\"Content-Security-Policy\" content=\"{}\">\
         <title>{}</title><style>{css}</style></head><body>{body}</body></html>",
        csp.policy(),
        glib::markup_escape_text(title)
    )
}

/// Serve `path` for paper export as a data URI through look::serve.
/// Unsupported TIFF and EXIF-corrected images become PNG; others retain detected file types.
fn data_uri(path: &Path) -> Option<String> {
    let (bytes, mime) = match look::serve(path, Look::paper(), false) {
        Served::Bytes(bytes, mime) => (bytes.to_vec(), mime.to_string()),
        Served::File => {
            let bytes = std::fs::read(path).ok()?;
            let (kind, _) = gio::content_type_guess(Some(path), Some(bytes.as_slice()));
            (bytes, gio::content_type_get_mime_type(&kind)?.to_string())
        }
    };
    Some(format!(
        "data:{mime};base64,{}",
        glib::base64_encode(&bytes)
    ))
}

/// Every diagram `html` embeds drawn as the page shows it, as a `data:` URI by its address, or
/// `None` where it cannot be: drawn here, on the main thread, the file fetched on a worker.
async fn drawn_diagrams(
    html: &str,
    resolve: &Arc<assets::Resolve>,
    note: &str,
    web: &HashSet<String>,
) -> HashMap<String, Option<String>> {
    let mut drawn = HashMap::new();
    // Each address as `inline_images` hands it over: as the writer escapes it, unescaped.
    let sources = html
        .split(" src=\"")
        .skip(1)
        .filter_map(|rest| rest.split('"').next())
        .map(|uri| uri.replace("&amp;", "&"))
        .filter(|uri| assets::is_diagram(uri));
    for uri in sources {
        let (resolve, note, at) = (resolve.clone(), note.to_string(), uri.clone());
        let found =
            crate::work::off_thread("asset", move || assets::asset_of(&*resolve, &note, &at));
        let svg = match found.await.flatten() {
            Some((key, path)) => {
                let page = assets::diagram_page(&uri);
                diagram::embed::svg(&path, page.as_deref(), web.contains(&key)).await
            }
            None => None,
        };
        let data = svg.map(|svg| {
            format!(
                "data:image/svg+xml;base64,{}",
                glib::base64_encode(svg.as_bytes())
            )
        });
        drawn.insert(uri, data);
    }
    drawn
}

/// Embed accent-scheme images as data URIs through `fetch`; remove unavailable src attributes.
/// External image sources remain unchanged.
fn inline_images(html: &str, fetch: impl Fn(&str) -> Option<String>) -> String {
    const SRC: &str = " src=\"";
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(at) = rest.find(SRC) {
        let from = at + SRC.len();
        let Some(len) = rest[from..].find('"') else {
            break;
        };
        let (value, after) = (&rest[from..from + len], from + len + 1);
        out.push_str(&rest[..at]);
        match value.starts_with("accent:") {
            // The attribute is as the writer escapes it.
            true => {
                if let Some(data) = fetch(&value.replace("&amp;", "&")) {
                    out.push_str(&format!("{SRC}{data}\""));
                }
            }
            false => out.push_str(&rest[at..after]),
        }
        rest = &rest[after..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A TIFF goes into the page as the PNG the preview is served: no browser draws one.
    #[test]
    fn a_tiff_is_exported_as_a_png() {
        let path = std::env::temp_dir().join(format!("accent-export-{}.tif", std::process::id()));
        std::fs::write(&path, look::tests::grey_tiff(1)).unwrap();
        let uri = data_uri(&path);
        std::fs::remove_file(&path).unwrap();
        assert!(uri.is_some_and(|u| u.starts_with("data:image/png;base64,")));
    }

    #[test]
    fn an_exported_page_carries_its_images_and_nothing_of_the_app() {
        let page = "<p><img src=\"accent://file/a%20b.png?x=1&amp;y=2\" alt=\"a\" />\
            <img src=\"https://example.org/x.png\">\
            <img alt=\"gone\" src=\"accent://file/gone.png\"></p>";
        let body = inline_images(page, |uri| {
            (uri == "accent://file/a%20b.png?x=1&y=2").then(|| "data:image/png;base64,AA==".into())
        });
        let file = document("Note & Co", "p { margin: 0; }", &body, Csp::File);
        assert!(!file.contains("accent:"), "{file}");
        assert!(
            file.contains("<img src=\"data:image/png;base64,AA==\" alt=\"a\" />"),
            "{file}"
        );
        assert!(
            file.contains("<img src=\"https://example.org/x.png\">"),
            "{file}"
        );
        assert!(file.contains("<img alt=\"gone\">"), "{file}");
        assert!(file.contains("content=\"script-src 'none'\""), "{file}");
        assert!(file.contains("<title>Note &amp; Co</title>"), "{file}");
        assert!(
            file.contains("<style>p { margin: 0; }</style></head><body><p>"),
            "{file}"
        );
        // In the browser nothing loads from outside the page.
        let file = document("Note", "", &body, Csp::Browser);
        assert!(
            file.contains("content=\"default-src 'none'; img-src data:;"),
            "{file}"
        );
    }
}
