use super::edit::{apply, field, form_dialog, review};
use super::*;
use accent_core::bib::Attachment;

impl App {
    pub(super) fn add_reference(self: &Rc<Self>, path: &str) {
        let form = dialogs::form();
        let input = field(&form, "DOI or DOI URL", "");
        let raw = gtk::TextBuffer::new(None::<&gtk::TextTagTable>);
        let view = gtk::TextView::builder()
            .buffer(&raw)
            .monospace(true)
            .wrap_mode(gtk::WrapMode::WordChar)
            .height_request(120)
            .build();
        form.append(
            &gtk::Label::builder()
                .label("Or paste BibTeX / write a new entry")
                .xalign(0.0)
                .build(),
        );
        form.append(&view);
        raw.set_text("@article{new_reference,\n  title = {},\n  author = {},\n  year = {},\n}\n");
        let download = gtk::CheckButton::with_label("Download PDF after adding the reference");
        form.append(&download);
        let direct = field(&form, "PDF URL (optional)", "");
        let note = gtk::Label::builder()
            .label(format!("Destination: {path}"))
            .wrap(true)
            .xalign(0.0)
            .build();
        form.append(&note);
        let app = self.clone();
        let path = path.to_string();
        let parent = app.clone();
        form_dialog(
            &parent,
            "Add Reference",
            "Review Reference",
            &form,
            move || {
                let input = input.text().to_string();
                let raw = raw
                    .text(&raw.start_iter(), &raw.end_iter(), false)
                    .to_string();
                if input.trim().is_empty() {
                    Library::parse(&raw).map_err(|e| e.to_string())?;
                } else if bib::doi(&input).is_none() {
                    return Err("Enter a DOI or doi.org URL".into());
                }
                let (app, path) = (app.clone(), path.clone());
                let download = download.is_active();
                let direct = direct.text().to_string();
                glib::spawn_future_local(async move {
                    let fetched = if input.trim().is_empty() {
                        Ok(raw)
                    } else {
                        let (progress, cancel) =
                            web::progress(&app, "Retrieving Reference Metadata…");
                        let result = web::metadata(&input, &cancel).await;
                        let cancelled = cancel.is_cancelled();
                        progress.close();
                        if cancelled {
                            return;
                        }
                        result
                    };
                    match fetched {
                        Ok(raw) => app.review_import(&path, raw, download, direct),
                        Err(why) => app.cannot("retrieve reference", why),
                    }
                });
                Ok(())
            },
        );
    }

    fn review_import(self: &Rc<Self>, path: &str, raw: String, download: bool, direct: String) {
        self.with_tab(path, Opened::Kept, "add reference", move |app, tab| {
            let imported = match Library::parse(&raw) {
                Ok(b) => b,
                Err(e) => return app.cannot("import reference", e),
            };
            if imported.entries.len() != 1 {
                return app.toast("Import one reference at a time");
            }
            let before = tab.text();
            let destination = match Library::parse(&before) {
                Ok(b) => b,
                Err(e) => return app.cannot("read bibliography", e),
            };
            let doi = imported.value(&imported.entries[0], "doi");
            if let Some(doi) = bib::doi(&doi)
                && let Some(existing) = destination
                    .entries
                    .iter()
                    .find(|e| bib::doi(&destination.value(e, "doi")).as_ref() == Some(&doi))
            {
                app.toast(&format!(
                    "Already in this bibliography: {}. Use Refresh Metadata or Download PDF.",
                    existing.key
                ));
                return;
            }
            let form = dialogs::form();
            let mut proposed = imported
                .generate_key(0, &app.config.borrow().bibliography_key_pattern)
                .unwrap_or_else(|_| imported.entries[0].key.clone());
            let base = proposed.clone();
            let mut n = 2;
            while destination.entries.iter().any(|e| e.key == proposed) {
                proposed = format!("{base}_{n}");
                n += 1;
            }
            let key = field(&form, "Citation Key", &proposed);
            let mut rows = Vec::new();
            for f in &imported.entries[0].fields {
                let old = imported.value(&imported.entries[0], &f.name);
                let entry = field(&form, &f.name, &old);
                rows.push((f.name.clone(), old, entry));
            }
            form.append(
                &gtk::Label::builder()
                    .label(format!("Destination: {}", tab.rel()))
                    .wrap(true)
                    .xalign(0.0)
                    .build(),
            );
            let target = tab.clone();
            let parent = app.clone();
            form_dialog(app, "Review Reference", "Add Reference", &form, move || {
                let mut text = imported.rename(0, &key.text()).map_err(|e| e.to_string())?;
                for (name, old, entry) in &rows {
                    if entry.text().as_str() != old {
                        text = Library::parse(&text)
                            .and_then(|b| b.set_field(0, name, &entry.text()))
                            .map_err(|e| e.to_string())?;
                    }
                }
                let incoming = Library::parse(&text).map_err(|e| e.to_string())?;
                let after = incoming
                    .copy_entries(&[0], &before)
                    .map_err(|e| e.to_string())?;
                apply(&target, &before, &after)?;
                if download {
                    let bib = Library::parse(&after).map_err(|e| e.to_string())?;
                    let index = bib
                        .entries
                        .iter()
                        .position(|e| e.key == key.text())
                        .unwrap();
                    parent.pdf_dialog(&target, bib, index, &direct);
                }
                Ok(())
            });
        });
    }

    pub(super) fn refresh_reference(self: &Rc<Self>, tab: &Rc<Tab>, bib: Library, index: usize) {
        let doi = bib.value(&bib.entries[index], "doi");
        let Some(doi) = bib::doi(&doi) else {
            return self.toast("Add a valid DOI to this reference first");
        };
        let app = self.clone();
        let tab = tab.clone();
        let (progress, cancel) = web::progress(self, "Retrieving Reference Metadata…");
        glib::spawn_future_local(async move {
            let result = web::metadata(&doi, &cancel).await;
            let cancelled = cancel.is_cancelled();
            progress.close();
            if cancelled {
                return;
            }
            let raw = match result {
                Ok(s) => s,
                Err(why) => return app.cannot("refresh metadata", why),
            };
            let incoming = match Library::parse(&raw) {
                Ok(b) => b,
                Err(e) => return app.cannot("read metadata", e),
            };
            let form = dialogs::form();
            let mut rows = Vec::new();
            for f in &incoming.entries[0].fields {
                if f.name == "file" {
                    continue;
                }
                let old = bib.value(&bib.entries[index], &f.name);
                let new = incoming.value(&incoming.entries[0], &f.name);
                if old == new {
                    continue;
                }
                let choice = gtk::CheckButton::with_label(&format!(
                    "{}: {} → {}",
                    f.name,
                    if old.is_empty() { "(empty)" } else { &old },
                    new
                ));
                choice.set_active(old.is_empty());
                form.append(&choice);
                // Resolve provider macros rather than introducing undefined strings locally.
                rows.push((f.name.clone(), new, choice));
            }
            if rows.is_empty() {
                return app.toast("Reference metadata is up to date");
            }
            let target = tab.clone();
            let parent = app.clone();
            form_dialog(
                &app,
                "Review Metadata Changes",
                "Review Changes",
                &form,
                move || {
                    let mut after = bib.source.clone();
                    for (name, value, choice) in &rows {
                        if choice.is_active() {
                            after = Library::parse(&after)
                                .and_then(|b| b.set_field(index, name, value))
                                .map_err(|e| e.to_string())?;
                        }
                    }
                    if target.text() != bib.source {
                        return Err("Bibliography changed; refresh again".into());
                    }
                    review(
                        &parent,
                        &target,
                        "Update Reference Metadata",
                        bib.source.clone(),
                        after,
                    );
                    Ok(())
                },
            );
        });
    }

    pub(super) fn download_reference(self: &Rc<Self>, tab: &Rc<Tab>, bib: Library, index: usize) {
        self.pdf_dialog(tab, bib, index, "");
    }
    fn pdf_dialog(self: &Rc<Self>, tab: &Rc<Tab>, bib: Library, index: usize, direct: &str) {
        let form = dialogs::form();
        let url = field(
            &form,
            "PDF URL (optional; otherwise look up the DOI)",
            direct,
        );
        let email = field(
            &form,
            "Unpaywall Email (optional, sent to Unpaywall for open-access lookup)",
            "",
        );
        let filename = bib.entries[index]
            .key
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || "_-".contains(c) {
                    c
                } else {
                    '_'
                }
            })
            .collect::<String>();
        let path = field(
            &form,
            "PDF Path Relative to This Bibliography",
            &format!("papers/{filename}.pdf"),
        );
        form.append(&gtk::Label::builder().label("The reference is kept if downloading fails. A login page is not saved as a PDF. Existing attachments are never overwritten.").wrap(true).xalign(0.0).build());
        let parent = self.clone();
        let target = tab.clone();
        form_dialog(self, "Download PDF", "Download", &form, move || {
            let rel = path.text().to_string();
            let dest = bib::attachment_path(&target.rel(), &rel).map_err(|e| e.to_string())?;
            let doi = bib::doi(&bib.value(&bib.entries[index], "doi"));
            let url = url.text().to_string();
            if url.is_empty() && doi.is_none() {
                return Err("Supply a PDF URL or add a DOI to the reference".into());
            }
            let (app, tab, bib, email) = (
                parent.clone(),
                target.clone(),
                bib.clone(),
                email.text().to_string(),
            );
            let Some(vault) = app.vault().cloned() else {
                return Err("Open a vault to download attachments".into());
            };
            let key = bib.entries[index].key.clone();
            glib::spawn_future_local(async move {
                let (progress, cancel) = web::progress(&app, "Downloading PDF…");
                let urls = if url.is_empty() {
                    web::pdf_urls(&doi.unwrap(), &email, &cancel).await
                } else {
                    Ok(vec![url])
                };
                let bytes = match urls {
                    Ok(urls) => web::pdf(&urls, &cancel).await,
                    Err(e) => Err(e),
                };
                let cancelled = cancel.is_cancelled();
                progress.close();
                if cancelled {
                    return;
                }
                let bytes = match bytes {
                    Ok(bytes) => bytes,
                    Err(why) => return app.cannot("download PDF (reference kept)", why),
                };
                let saved = dest.clone();
                let done = work::attempt("save PDF", move || {
                    if let Some(dir) = std::path::Path::new(&saved).parent() {
                        vault.create_dir(&dir.to_string_lossy(), true)?;
                    }
                    vault.write_file(&saved, &bytes)
                })
                .await;
                match done {
                    Ok(()) => match attach(&tab, &key, &rel) {
                        Ok(()) => {
                            app.toast("PDF downloaded and attached");
                            app.open_path(&dest);
                        }
                        Err(why) => {
                            app.cannot(&format!("attach downloaded {dest} (file kept)"), why)
                        }
                    },
                    Err(why) => app.toast(&why),
                }
            });
            Ok(())
        });
    }

    pub(super) fn attach_reference(self: &Rc<Self>, tab: &Rc<Tab>, bib: Library, index: usize) {
        let form = dialogs::form();
        let path = field(&form, "PDF Path Relative to This Bibliography", "");
        let local = Rc::new(RefCell::new(None::<std::path::PathBuf>));
        let choose = gtk::Button::with_label("Choose PDF from This Computer…");
        form.append(&choose);
        let selected = gtk::Label::builder().wrap(true).xalign(0.0).build();
        form.append(&selected);
        let parent = self.window.clone();
        choose.connect_clicked(glib::clone!(
            #[weak]
            path,
            #[strong]
            local,
            #[weak]
            selected,
            move |_| {
                let dialog = gtk::FileDialog::builder().title("Attach PDF").build();
                let filter = gtk::FileFilter::new();
                filter.add_mime_type("application/pdf");
                let filters = gio::ListStore::new::<gtk::FileFilter>();
                filters.append(&filter);
                dialog.set_filters(Some(&filters));
                let parent = parent.clone();
                let path = path.clone();
                let local = local.clone();
                let selected = selected.clone();
                glib::spawn_future_local(async move {
                    if let Ok(file) = dialog.open_future(Some(&parent)).await
                        && let Some(file) = file.path()
                    {
                        let Some(name) = file.file_name() else {
                            return;
                        };
                        path.set_text(&format!("papers/{}", name.to_string_lossy()));
                        selected.set_text(&format!("Copy {} into the vault", file.display()));
                        *local.borrow_mut() = Some(file);
                    }
                });
            }
        ));
        let key = bib.entries[index].key.clone();
        let tab = tab.clone();
        let app = self.clone();
        form_dialog(self, "Attach PDF", "Attach", &form, move || {
            let relative = path.text().to_string();
            let dest = bib::attachment_path(&tab.rel(), &relative).map_err(|e| e.to_string())?;
            let vault = app.vault().cloned().ok_or("Open a vault to attach a PDF")?;
            let local = local.borrow().clone();
            let app = app.clone();
            let tab = tab.clone();
            let key = key.clone();
            glib::spawn_future_local(async move {
                let result = work::attempt("attach PDF", move || {
                    if let Some(local) = local {
                        use std::io::Read;
                        let mut signature = [0; 1024];
                        let count = std::fs::File::open(&local)
                            .and_then(|mut f| f.read(&mut signature))
                            .map_err(|e| e.to_string())?;
                        if !signature[..count].windows(5).any(|s| s == b"%PDF-") {
                            return Err("The selected file is not a PDF".to_string());
                        }
                        if let Some(dir) = std::path::Path::new(&dest).parent() {
                            vault
                                .create_dir(&dir.to_string_lossy(), true)
                                .map_err(|e| e.to_string())?;
                        }
                        vault
                            .upload(&local, &dest, false)
                            .map_err(|e| e.to_string())?;
                    } else if vault.stat(&dest).map_err(|e| e.to_string())?.is_none() {
                        return Err("That attachment does not exist".into());
                    }
                    Ok::<_, String>(())
                })
                .await;
                match result {
                    Ok(()) => {
                        if let Err(why) = attach(&tab, &key, &relative) {
                            app.cannot("link PDF (file kept)", why);
                        }
                    }
                    Err(why) => app.toast(&why),
                }
            });
            Ok(())
        });
    }
}

fn attach(tab: &Rc<Tab>, key: &str, path: &str) -> Result<(), String> {
    let before = tab.text();
    let bib = Library::parse(&before).map_err(|e| e.to_string())?;
    let index = bib
        .entries
        .iter()
        .position(|e| e.key == key)
        .ok_or("The reference was removed or renamed")?;
    let mut files =
        Attachment::parse(&bib.value(&bib.entries[index], "file")).map_err(|e| e.to_string())?;
    if !files.iter().any(|f| f.path == path) {
        files.push(Attachment {
            description: String::new(),
            path: path.into(),
            kind: "PDF".into(),
        });
    }
    let after = bib
        .set_field(index, "file", &Attachment::format(&files))
        .map_err(|e| e.to_string())?;
    apply(tab, &before, &after)
}
