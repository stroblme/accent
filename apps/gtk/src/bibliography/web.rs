//! DOI metadata and optional full text are separate, cancellable network operations.
use super::*;
use crate::statusbar::Bytes;
use accent_api::link::Stop;
use soup::prelude::*;

pub(super) fn session() -> soup::Session {
    let session = soup::Session::new();
    session.set_timeout(30);
    session.set_user_agent("Accent/2 bibliography");
    #[cfg(target_os = "macos")]
    if let Ok(anchors) = gio::TlsFileDatabase::new("/etc/ssl/cert.pem") {
        session.set_tls_database(Some(&anchors));
    }
    session
}

/// `url`'s body, at most `limit` bytes, counted into `counted` as they come.
pub(super) async fn get(
    session: &soup::Session,
    url: &str,
    accept: &str,
    limit: usize,
    cancel: &gio::Cancellable,
    counted: Option<&Bytes>,
) -> Result<Vec<u8>, String> {
    if !url.starts_with("https://") && !url.starts_with("http://") {
        return Err("Use an HTTP or HTTPS URL".into());
    }
    let message = soup::Message::new("GET", url).map_err(|e| e.to_string())?;
    if let Some(headers) = message.request_headers() {
        headers.append("Accept", accept);
    }
    let stream = gio::CancellableFuture::new(
        session.send_future(&message, glib::Priority::DEFAULT),
        cancel.clone(),
    )
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())?;
    if cancel.is_cancelled() {
        return Err("Cancelled".into());
    }
    if message.status_code() != 200 {
        let host = message.uri().and_then(|u| u.host()).unwrap_or_default();
        return Err(format!("{host} answered HTTP {}", message.status_code()));
    }
    let total = message.response_headers().map_or(0, |h| h.content_length());
    let mut bytes = Vec::new();
    loop {
        if cancel.is_cancelled() {
            return Err("Cancelled".into());
        }
        let chunk = gio::CancellableFuture::new(
            stream.read_bytes_future(65536, glib::Priority::DEFAULT),
            cancel.clone(),
        )
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
        if chunk.is_empty() {
            break;
        }
        if bytes.len() + chunk.len() > limit {
            return Err("Download exceeds the size limit".into());
        }
        bytes.extend_from_slice(&chunk);
        if let Some(counted) = counted {
            counted.set(bytes.len() as u64, total.max(0) as u64);
        }
    }
    Ok(bytes)
}

pub(super) fn encoded(text: &str) -> String {
    glib::Uri::escape_string(text, None, false).to_string()
}

pub(super) async fn metadata(input: &str, cancel: &gio::Cancellable) -> Result<String, String> {
    let doi = bib::doi(input).ok_or("Enter a DOI or doi.org URL")?;
    let bytes = get(
        &session(),
        &format!("https://doi.org/{}", encoded(&doi)),
        "application/x-bibtex",
        2 << 20,
        cancel,
        None,
    )
    .await?;
    let text = String::from_utf8(bytes).map_err(|e| e.to_string())?;
    let bib = Library::parse(&text).map_err(|e| e.to_string())?;
    if bib.entries.len() != 1 {
        return Err("The DOI service did not return one BibTeX entry".into());
    }
    Ok(text)
}

/// The DOI's open-access copies, Unpaywall's when the reader gave an email, then Crossref's PDF
/// links. An arXiv DOI is DataCite's, which neither knows, so it is not sent.
pub(super) async fn pdf_urls(doi: &str, email: &str, cancel: &gio::Cancellable) -> Vec<String> {
    let mut urls = Vec::new();
    if bib::arxiv_pdf(doi).is_some() {
        return urls;
    }
    let session = session();
    if !email.trim().is_empty() {
        let url = format!(
            "https://api.unpaywall.org/v2/{}?email={}",
            encoded(doi),
            encoded(email.trim())
        );
        if let Ok(bytes) = get(&session, &url, "application/json", 2 << 20, cancel, None).await
            && let Ok(json) = serde_json::from_slice::<serde_json::Value>(&bytes)
        {
            if let Some(url) = json["best_oa_location"]["url_for_pdf"].as_str() {
                urls.push(url.to_string());
            }
            for location in json["oa_locations"].as_array().into_iter().flatten() {
                if let Some(url) = location["url_for_pdf"].as_str()
                    && !urls.iter().any(|u| u == url)
                {
                    urls.push(url.to_string());
                }
            }
        }
    }
    let url = format!("https://api.crossref.org/works/{}", encoded(doi));
    if let Ok(bytes) = get(&session, &url, "application/json", 2 << 20, cancel, None).await
        && let Ok(json) = serde_json::from_slice::<serde_json::Value>(&bytes)
    {
        for link in json["message"]["link"].as_array().into_iter().flatten() {
            if link["content-type"].as_str() == Some("application/pdf")
                && let Some(url) = link["URL"].as_str()
            {
                urls.push(url.to_string());
            }
        }
    }
    urls
}

/// The first of `urls` that answers with a PDF, its bytes counted into `counted`.
pub(super) async fn pdf(
    urls: &[String],
    cancel: &gio::Cancellable,
    counted: &Bytes,
) -> Result<Vec<u8>, String> {
    let session = session();
    let mut error =
        "No PDF found. Enter a direct PDF URL or attach a file from your browser.".to_string();
    for url in urls {
        match get(
            &session,
            url,
            "application/pdf",
            100 << 20,
            cancel,
            Some(counted),
        )
        .await
        {
            Ok(bytes) if bytes.windows(5).take(1024).any(|w| w == b"%PDF-") => return Ok(bytes),
            Ok(_) => {
                error =
                    "The server returned a web page. Download the PDF in your browser and attach it"
                        .into()
            }
            Err(why) => error = why,
        }
        if cancel.is_cancelled() {
            break;
        }
    }
    Err(error)
}

/// Await `work` with `line()` on the status bar and a Stop beside it that cancels `cancel`, as
/// a copy's line has.
pub(super) async fn on_bar<T>(
    app: &Rc<App>,
    line: impl Fn() -> String + 'static,
    cancel: &gio::Cancellable,
    work: impl std::future::Future<Output = T>,
) -> T {
    let stop = Stop::default();
    let (bar, retold, cancel, stopped) = (app.clone(), app.clone(), cancel.clone(), stop.clone());
    crate::statusbar::counting(
        move |text, running| bar.statusbar.set_stoppable(text, running, &stop),
        move |from, to| retold.statusbar.retell_transfer(from, to),
        // A Stop only raises its flag, so the bar's tick is where it reaches the request.
        move || {
            if stopped.stopped() {
                cancel.cancel();
            }
            Some(line())
        },
        work,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downloads_check_status_size_and_pdf_content() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/paper", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            for (status, body) in [
                ("200 OK", "%PDF-1.4\nfixture"),
                ("200 OK", "<html>Login required</html>"),
                ("200 OK", "too large"),
                ("403 Forbidden", "denied"),
            ] {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut request = Vec::new();
                let mut byte = [0];
                while !request.ends_with(b"\r\n\r\n") {
                    stream.read_exact(&mut byte).unwrap();
                    request.push(byte[0]);
                }
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        let context = glib::MainContext::new();
        context
            .with_thread_default(|| {
                context.block_on(async {
                    let cancel = gio::Cancellable::new();
                    let counted = Bytes::default();
                    assert!(
                        pdf(std::slice::from_ref(&url), &cancel, &counted)
                            .await
                            .unwrap()
                            .starts_with(b"%PDF-")
                    );
                    assert_eq!(counted.line("PDF").as_deref(), Some("PDF… 0.0/0.0 MB"));
                    assert!(
                        pdf(std::slice::from_ref(&url), &cancel, &counted)
                            .await
                            .unwrap_err()
                            .contains("web page")
                    );
                    assert!(
                        get(&session(), &url, "*/*", 3, &cancel, None)
                            .await
                            .unwrap_err()
                            .contains("size limit")
                    );
                    assert_eq!(
                        get(&session(), &url, "*/*", 100, &cancel, None)
                            .await
                            .unwrap_err(),
                        "127.0.0.1 answered HTTP 403"
                    );
                    cancel.cancel();
                    assert!(
                        get(&session(), &url, "*/*", 100, &cancel, None)
                            .await
                            .is_err()
                    );
                })
            })
            .unwrap();
        server.join().unwrap();
    }

    #[test]
    #[ignore = "network: cargo test -p accent -- --ignored arxiv"]
    fn downloads_an_arxiv_doi_from_arxiv() {
        let url = bib::arxiv_pdf("https://doi.org/10.48550/arXiv.2611.11858").unwrap();
        let context = glib::MainContext::new();
        context
            .with_thread_default(|| {
                let counted = Bytes::default();
                let bytes = context.block_on(pdf(&[url], &gio::Cancellable::new(), &counted));
                assert!(bytes.unwrap().starts_with(b"%PDF-"));
            })
            .unwrap();
    }
}
