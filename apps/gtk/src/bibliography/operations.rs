use super::edit::{apply, field, form_dialog, review};
use super::*;
use accent_core::bib::Attachment;

impl App {
    pub(super) fn new_bibliography(self: &Rc<Self>) {
        let Some(vault) = self.vault().cloned() else {
            return self.needs_vault("create a bibliography");
        };
        let form = dialogs::form();
        let path = field(&form, "Vault-relative Path", "references.bib");
        let app = self.clone();
        form_dialog(self, "New Bibliography", "Create", &form, move || {
            let path = path.text().to_string();
            if !is_bib(&path) {
                return Err("Use a .bib filename".into());
            }
            let (app, vault) = (app.clone(), vault.clone());
            glib::spawn_future_local(async move {
                let target = path.clone();
                match work::attempt("create bibliography", move || {
                    vault.write_file(&target, b"\n")
                })
                .await
                {
                    Ok(()) => {
                        app.refresh_corpus(false);
                        app.open_bibliography(&path, None);
                    }
                    Err(why) => app.toast(&why),
                }
            });
            Ok(())
        });
    }

    pub(super) fn cite_references(self: &Rc<Self>, path: &str, selected: Vec<usize>, insert: bool) {
        if selected.is_empty() {
            return self.toast("Select references in Bibliography first");
        }
        let library = self
            .tab_for(path)
            .and_then(|t| Library::parse(&t.text()).ok())
            .or_else(|| {
                self.bibliography
                    .get()
                    .and_then(|p| p.entries.library.borrow().clone())
            });
        let Some(library) = library else {
            return;
        };
        let keys = selected
            .iter()
            .filter_map(|i| library.entries.get(*i).map(|e| e.key.as_str()))
            .collect::<Vec<_>>()
            .join(",");
        if insert {
            let Some(tab) = self.active().filter(|t| t.rel().ends_with(".tex")) else {
                return self.toast("Open a LaTeX document to insert the selected citation");
            };
            if !tab.view.is_editable() {
                return self.toast("This document is read-only");
            }
            tab.buffer.begin_user_action();
            tab.buffer.insert_at_cursor(&format!("\\cite{{{keys}}}"));
            tab.buffer.end_user_action();
            tab.view.grab_focus();
        } else {
            self.window.clipboard().set_text(&keys);
            self.toast("Citation keys copied");
        }
    }

    pub(super) fn bibliography_usages(self: &Rc<Self>, key: &str) {
        let Some(vault) = self.vault().cloned() else {
            return;
        };
        let key = key.to_string();
        let app = self.clone();
        glib::spawn_future_local(async move {
            let query = key.clone();
            let found = work::attempt("find citation usages", move || {
                vault.grep(
                    &query,
                    accent_api::Options {
                        case: true,
                        word: true,
                        regex: false,
                    },
                    2000,
                    false,
                )
            })
            .await;
            match found {
                Ok((hits, _)) => {
                    let form = dialogs::form();
                    for hit in hits {
                        let path = hit.rel_path;
                        let line = hit.line;
                        let range = hit.offset..hit.offset + hit.range.len();
                        let button = gtk::Button::with_label(&format!("{path}:{line}"));
                        form.append(&button);
                        button.connect_clicked(glib::clone!(
                            #[weak]
                            app,
                            move |_| app.open_note_at(
                                &path,
                                Some(crate::sidebar::Target::Range(range.clone()))
                            )
                        ));
                    }
                    form_dialog(&app, &format!("Usages of {key}"), "Close", &form, || Ok(()));
                }
                Err(why) => app.toast(&why),
            }
        });
    }

    /// Keys in manuscripts, macros or other bibliographies must not be broken by a local rename.
    pub(super) fn rename_reference(
        self: &Rc<Self>,
        tab: &Rc<Tab>,
        before: String,
        after: String,
        old: String,
        new: String,
    ) {
        if old == new {
            return review(self, tab, "Review Reference Changes", before, after);
        }
        let Some(vault) = self.vault().cloned() else {
            return self.toast("Open a vault to check citation usages before renaming");
        };
        let app = self.clone();
        let tab = tab.clone();
        let rel = tab.rel();
        let open = self
            .open_tabs()
            .into_iter()
            .filter(|t| t.rel() != rel)
            .map(|t| (t.rel(), t.text()))
            .collect::<Vec<_>>();
        glib::spawn_future_local(async move {
            let needle = old.clone();
            let found = work::attempt("check citation usages", move || {
                let mut used = Vec::new();
                for path in vault.file_paths(false)? {
                    if path == rel
                        || !matches!(
                            path.rsplit('.').next(),
                            Some("tex" | "bib" | "md" | "sty" | "cls")
                        )
                    {
                        continue;
                    }
                    let text = match open.iter().find(|(p, _)| p == &path) {
                        Some((_, s)) => s.clone(),
                        None => vault.read(&path)?.0,
                    };
                    if text.contains(&needle) {
                        used.push(path);
                    }
                }
                Ok::<_, accent_api::Error>(used)
            })
            .await;
            match found {
                Ok(used) if used.is_empty() => {
                    if tab.text() != before {
                        return app.toast("Bibliography changed; review the operation again");
                    }
                    review(&app, &tab, &format!("Change {old} to {new}"), before, after);
                }
                Ok(used) => {
                    let form = dialogs::form();
                    form.append(&gtk::Label::builder().label(format!("{old} occurs in:\n{}\n\nKeep this key until those usages have been reviewed. Use Source to coordinate manuscript and bibliography changes. Files outside the indexed vault are not checked.",used.join("\n"))).wrap(true).selectable(true).xalign(0.0).build());
                    form_dialog(&app, "Citation Key Is In Use", "Close", &form, || Ok(()));
                }
                Err(why) => app.toast(&why),
            }
        });
    }
}

pub(super) fn transfer_dialog(
    app: &Rc<App>,
    tab: &Rc<Tab>,
    bib: Library,
    selected: Vec<usize>,
    moving: bool,
) {
    let form = dialogs::form();
    let paths = app
        .bibliography
        .get()
        .map(|p| p.paths())
        .unwrap_or_default()
        .into_iter()
        .filter(|p| *p != tab.rel())
        .collect::<Vec<_>>();
    let destination = field(
        &form,
        "Destination .bib (existing or new)",
        paths
            .first()
            .map(String::as_str)
            .unwrap_or("project/references.bib"),
    );
    if !paths.is_empty() {
        let chooser =
            gtk::DropDown::from_strings(&paths.iter().map(String::as_str).collect::<Vec<_>>());
        chooser.connect_selected_notify(glib::clone!(
            #[weak]
            destination,
            move |c| {
                if let Some(p) = paths.get(c.selected() as usize) {
                    destination.set_text(p);
                }
            }
        ));
        form.append(&chooser);
    }
    let copies =
        gtk::CheckButton::with_label("Copy attachments beside the destination bibliography");
    form.append(&copies);
    form.append(&gtk::Label::builder().label("Otherwise the entries share the existing PDFs. Copies are independent; later changes are not propagated. Markdown reading notes remain linked to their original PDFs.").wrap(true).xalign(0.0).build());
    let source = tab.clone();
    let app = app.clone();
    let parent = app.clone();
    form_dialog(
        &parent,
        if moving {
            "Move References"
        } else {
            "Copy References"
        },
        if moving { "Move" } else { "Copy" },
        &form,
        move || {
            let to = destination.text().to_string();
            if !is_bib(&to) || to == source.rel() {
                return Err("Choose a different .bib file".into());
            }
            let (app, source, bib, selected) =
                (app.clone(), source.clone(), bib.clone(), selected.clone());
            let copy_files = copies.is_active();
            glib::spawn_future_local(async move {
                let result =
                    transfer(&app, &source, &bib, &selected, &to, moving, copy_files).await;
                match result {
                    Ok(()) => app.toast(if moving {
                        "References moved"
                    } else {
                        "References copied"
                    }),
                    Err(why) => app.toast(&why),
                }
                app.refresh_corpus(false);
            });
            Ok(())
        },
    );
}

async fn saved(app: &Rc<App>, tab: &Rc<Tab>) -> Result<(), String> {
    app.save_tab(tab, false);
    while tab.save.flight.borrow().is_some() {
        glib::timeout_future(Duration::from_millis(25)).await;
    }
    if tab.save.modified.get() || tab.save.disk_changed.get() {
        return Err(format!(
            "Could not save {}; resolve its banner first",
            tab.rel()
        ));
    }
    Ok(())
}

pub(super) async fn transfer(
    app: &Rc<App>,
    source: &Rc<Tab>,
    bib: &Library,
    selected: &[usize],
    to: &str,
    moving: bool,
    copy_files: bool,
) -> Result<(), String> {
    let destination = bib::attachment_path("", to).map_err(|e| e.to_string())?;
    let to = destination.as_str();
    if to == source.rel() {
        return Err("Choose a different bibliography".into());
    }
    if moving && !source.view.is_editable() {
        return Err("The source bibliography is read-only; copy references instead".into());
    }
    let vault = app
        .vault()
        .cloned()
        .ok_or("Open a vault to transfer references")?;
    if source.text() != bib.source || source.save.disk_changed.get() {
        return Err("Source changed; select the references again".into());
    }
    let remainder = moving
        .then(|| bib.move_remainder(selected))
        .transpose()
        .map_err(|e| e.to_string())?;
    let from = source.rel();
    let path = to.to_string();
    let v = vault.clone();
    let existing = work::attempt("read destination", move || match v.stat(&path)? {
        Some(_) => v.read(&path).map(Some),
        None => Ok(None),
    })
    .await?;
    let before = if let Some(tab) = app.tab_for(to) {
        saved(app, &tab).await?;
        tab.text()
    } else {
        existing
            .as_ref()
            .map(|(s, _)| s.clone())
            .unwrap_or_default()
    };
    let mut snippet = Library::parse(&bib.copy_entries(selected, "").map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let mut copy_jobs = Vec::new();
    for i in 0..snippet.entries.len() {
        let files = Attachment::parse(&snippet.value(&snippet.entries[i], "file"))
            .map_err(|e| e.to_string())?;
        if files.is_empty() {
            continue;
        }
        let mut rebased = bib::rebase_files(&from, to, &files).map_err(|e| e.to_string())?;
        if copy_files {
            for (f, new) in files.iter().zip(&mut rebased) {
                let src = bib::attachment_path(&from, &f.path).map_err(|e| e.to_string())?;
                let filename = std::path::Path::new(&src)
                    .file_name()
                    .unwrap()
                    .to_string_lossy();
                new.path = format!("papers/{filename}");
                let dst = bib::attachment_path(to, &new.path).map_err(|e| e.to_string())?;
                if copy_jobs
                    .iter()
                    .any(|(from, to)| to == &dst && from != &src)
                {
                    return Err(format!(
                        "Two attachments would use {dst}; rename one before copying"
                    ));
                }
                if src != dst && !copy_jobs.iter().any(|(_, p)| p == &dst) {
                    copy_jobs.push((src, dst));
                }
            }
        }
        snippet = Library::parse(
            &snippet
                .set_field(i, "file", &Attachment::format(&rebased))
                .map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
    }
    let all = (0..snippet.entries.len()).collect::<Vec<_>>();
    let after = snippet
        .copy_entries(&all, &before)
        .map_err(|e| e.to_string())?;
    let v = vault.clone();
    work::attempt("copy attachments", move || {
        for (src, dst) in copy_jobs {
            if let Some(dir) = std::path::Path::new(&dst).parent() {
                v.create_dir(&dir.to_string_lossy(), true)?;
            }
            v.copy(&src, &dst)?;
        }
        Ok::<_, accent_api::Error>(())
    })
    .await?;
    // New files use create-only writing; an existing file always carries the read's etag.
    if let Some(tab) = app.tab_for(to) {
        apply(&tab, &before, &after)?;
        saved(app, &tab).await?;
    } else {
        let v = vault.clone();
        let path = to.to_string();
        work::attempt("save destination bibliography", move || {
            if let Some((_, etag)) = existing {
                v.save(&path, &after, Some(etag)).map(|_| ())
            } else {
                v.write_file(&path, after.as_bytes())
            }
        })
        .await?;
    }
    if let Some(after) = remainder {
        // The destination is durable before source removal; dependency records stay in the source.
        apply(source, &bib.source, &after)
            .map_err(|why| format!("Copied references; source retained: {why}"))?;
        saved(app, source)
            .await
            .map_err(|why| format!("Destination saved; source removal is still unsaved: {why}"))?;
    }
    if let Some(panel) = app.bibliography.get() {
        panel.refresh();
    }
    Ok(())
}
