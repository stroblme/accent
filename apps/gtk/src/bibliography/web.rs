//! DOI metadata and optional full text are separate, cancellable network operations.
use super::*;
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

pub(super) async fn get(
    session: &soup::Session,
    url: &str,
    accept: &str,
    limit: usize,
    cancel: &gio::Cancellable,
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
        return Err(format!("HTTP {}", message.status_code()));
    }
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
    )
    .await?;
    let text = String::from_utf8(bytes).map_err(|e| e.to_string())?;
    let bib = Library::parse(&text).map_err(|e| e.to_string())?;
    if bib.entries.len() != 1 {
        return Err("The DOI service did not return one BibTeX entry".into());
    }
    Ok(text)
}

pub(super) async fn pdf_urls(
    doi: &str,
    email: &str,
    cancel: &gio::Cancellable,
) -> Result<Vec<String>, String> {
    let session = session();
    let mut urls = Vec::new();
    if !email.trim().is_empty() {
        let url = format!(
            "https://api.unpaywall.org/v2/{}?email={}",
            encoded(doi),
            encoded(email.trim())
        );
        if let Ok(bytes) = get(&session, &url, "application/json", 2 << 20, cancel).await
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
    if let Ok(bytes) = get(&session, &url, "application/json", 2 << 20, cancel).await
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
    if urls.is_empty() {
        return Err("No downloadable PDF found. Supply a direct PDF URL or attach a file downloaded in your browser.".into());
    }
    Ok(urls)
}

pub(super) async fn pdf(urls: &[String], cancel: &gio::Cancellable) -> Result<Vec<u8>, String> {
    let session = session();
    let mut error = "No PDF URL".to_string();
    for url in urls {
        match get(&session,url,"application/pdf",100<<20,cancel).await {
            Ok(bytes) if bytes.windows(5).take(1024).any(|w|w==b"%PDF-")=>return Ok(bytes),
            Ok(_)=>error="The server returned a webpage rather than a PDF; attach a browser download instead".into(),
            Err(why)=>error=why,
        }
        if cancel.is_cancelled() {
            break;
        }
    }
    Err(error)
}

pub(super) fn progress(app: &App, title: &str) -> (adw::Dialog, gio::Cancellable) {
    let cancel = gio::Cancellable::new();
    let dialog = adw::Dialog::builder()
        .title(title)
        .content_width(380)
        .build();
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    let form = dialogs::form();
    form.set_margin_start(24);
    form.set_margin_end(24);
    form.set_margin_top(24);
    form.set_margin_bottom(24);
    form.append(&gtk::Label::builder().label(title).wrap(true).build());
    let stop = gtk::Button::with_label("Cancel");
    form.append(&stop);
    toolbar.set_content(Some(&form));
    dialog.set_child(Some(&toolbar));
    stop.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        move |_| {
            dialog.close();
        }
    ));
    dialog.connect_closed(glib::clone!(
        #[strong]
        cancel,
        move |_| cancel.cancel()
    ));
    dialog.present(Some(&app.window));
    (dialog, cancel)
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
                    assert!(
                        pdf(std::slice::from_ref(&url), &cancel)
                            .await
                            .unwrap()
                            .starts_with(b"%PDF-")
                    );
                    assert!(
                        pdf(std::slice::from_ref(&url), &cancel)
                            .await
                            .unwrap_err()
                            .contains("webpage")
                    );
                    assert!(
                        get(&session(), &url, "*/*", 3, &cancel)
                            .await
                            .unwrap_err()
                            .contains("size limit")
                    );
                    assert_eq!(
                        get(&session(), &url, "*/*", 100, &cancel)
                            .await
                            .unwrap_err(),
                        "HTTP 403"
                    );
                    cancel.cancel();
                    assert!(get(&session(), &url, "*/*", 100, &cancel).await.is_err());
                })
            })
            .unwrap();
        server.join().unwrap();
    }
}
