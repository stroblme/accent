use super::*;
use accent_core::bib::Attachment;

pub(super) fn form_dialog(
    app: &App,
    title: &str,
    verb: &str,
    body: &impl IsA<gtk::Widget>,
    apply: impl Fn() -> Result<(), String> + 'static,
) -> adw::Dialog {
    let dialog = adw::Dialog::builder()
        .title(title)
        .content_width(660)
        .content_height(560)
        .build();
    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    let button = gtk::Button::with_label(verb);
    button.add_css_class("suggested-action");
    header.pack_end(&button);
    toolbar.add_top_bar(&header);
    let column = gtk::Box::new(gtk::Orientation::Vertical, 12);
    column.set_margin_start(18);
    column.set_margin_end(18);
    column.set_margin_top(12);
    column.set_margin_bottom(18);
    let error = gtk::Label::builder().wrap(true).xalign(0.0).build();
    error.add_css_class("error");
    column.append(&error);
    column.append(body);
    toolbar.set_content(Some(
        &gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&column)
            .build(),
    ));
    dialog.set_child(Some(&toolbar));
    button.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        move |_| match apply() {
            Ok(()) => {
                dialog.close();
            }
            Err(why) => error.set_text(&why),
        }
    ));
    dialog.present(Some(&app.window));
    dialog
}

pub(super) fn field(form: &gtk::Box, name: &str, value: &str) -> gtk::Entry {
    let label = gtk::Label::builder().label(name).xalign(0.0).build();
    label.add_css_class("dim-label");
    let entry = gtk::Entry::builder().text(value).hexpand(true).build();
    label.set_mnemonic_widget(Some(&entry));
    form.append(&label);
    form.append(&entry);
    entry
}

/// One buffer edit, refused if either the source or its save state changed during review.
pub(super) fn apply(tab: &Rc<Tab>, before: &str, after: &str) -> Result<(), String> {
    if !tab.view.is_editable() {
        return Err("This file is read-only".into());
    }
    if tab.save.disk_changed.get() {
        return Err("Resolve the file’s external change before editing references".into());
    }
    if tab.text() != before {
        return Err(
            "The bibliography changed while this operation was open; review it again".into(),
        );
    }
    Library::parse(after).map_err(|e| e.to_string())?;
    if before == after {
        return Ok(());
    }
    // A common prefix/suffix keeps unchanged cursor marks and minimizes the undo record.
    let mut start = before
        .bytes()
        .zip(after.bytes())
        .take_while(|(a, b)| a == b)
        .count();
    while !before.is_char_boundary(start) || !after.is_char_boundary(start) {
        start -= 1;
    }
    let mut tail = before[start..]
        .bytes()
        .rev()
        .zip(after[start..].bytes().rev())
        .take_while(|(a, b)| a == b)
        .count();
    while !before.is_char_boundary(before.len() - tail)
        || !after.is_char_boundary(after.len() - tail)
    {
        tail -= 1;
    }
    let mut a = tab
        .buffer
        .iter_at_offset(before[..start].chars().count() as i32);
    let mut b = tab
        .buffer
        .iter_at_offset(before[..before.len() - tail].chars().count() as i32);
    tab.buffer.begin_user_action();
    tab.buffer.delete(&mut a, &mut b);
    tab.buffer.insert(&mut a, &after[start..after.len() - tail]);
    tab.buffer.end_user_action();
    Ok(())
}

pub(super) fn review(app: &Rc<App>, tab: &Rc<Tab>, title: &str, before: String, after: String) {
    let form = dialogs::form();
    let label = gtk::Label::builder()
        .label(format!("{} — changes are one undo step", tab.rel()))
        .wrap(true)
        .xalign(0.0)
        .build();
    form.append(&label);
    let diff = similar::TextDiff::from_lines(&before, &after)
        .unified_diff()
        .header("Before", "After")
        .to_string();
    let buffer = gtk::TextBuffer::new(None::<&gtk::TextTagTable>);
    buffer.set_text(&diff);
    let view = gtk::TextView::builder()
        .buffer(&buffer)
        .editable(false)
        .monospace(true)
        .wrap_mode(gtk::WrapMode::WordChar)
        .build();
    form.append(&view);
    let tab = tab.clone();
    form_dialog(app, title, "Apply Changes", &form, move || {
        apply(&tab, &before, &after)
    });
}

pub(super) fn edit(app: &Rc<App>, tab: &Rc<Tab>, index: usize) {
    let before = tab.text();
    let bib = match Library::parse(&before) {
        Ok(b) => b,
        Err(e) => return app.cannot("edit reference", e),
    };
    let Some(e) = bib.entries.get(index) else {
        return;
    };
    let form = dialogs::form();
    form.append(
        &gtk::Label::builder()
            .label(format!("{} · {}", tab.rel(), e.key))
            .xalign(0.0)
            .wrap(true)
            .build(),
    );
    let mut names = vec![
        "title".to_string(),
        "author".into(),
        "year".into(),
        "date".into(),
        "journal".into(),
        "booktitle".into(),
        "doi".into(),
        "url".into(),
        "file".into(),
    ];
    for f in &e.fields {
        if !names.contains(&f.name) {
            names.push(f.name.clone());
        }
    }
    let rows = names
        .into_iter()
        .map(|name| {
            let original = bib.value(e, &name);
            let entry = field(&form, &name, &original);
            (name, original, entry)
        })
        .collect::<Vec<_>>();
    let tab = tab.clone();
    form_dialog(app, "Edit Reference", "Apply Changes", &form, move || {
        let mut changed = before.clone();
        for (name, old, entry) in &rows {
            if entry.text().as_str() != old {
                changed = Library::parse(&changed)
                    .and_then(|b| b.set_field(index, name, &entry.text()))
                    .map_err(|e| e.to_string())?;
            }
        }
        apply(&tab, &before, &changed)
    });
}

pub(super) fn operate(app: &Rc<App>, tab: &Rc<Tab>, name: &str, selected: Vec<usize>) {
    if matches!(name, "undo" | "redo") {
        if tab.view.is_editable() {
            if name == "undo" && tab.buffer.can_undo() {
                tab.buffer.undo();
            } else if name == "redo" && tab.buffer.can_redo() {
                tab.buffer.redo();
            }
        }
        return;
    }
    let bib = match Library::parse(&tab.text()) {
        Ok(b) => b,
        Err(e) => return app.cannot("read bibliography", e),
    };
    if name == "check" {
        return check(app, tab, bib);
    }
    if name == "merge" {
        return merge(app, tab, bib);
    }
    if selected.is_empty() {
        return app.toast("Select one or more references first");
    }
    let i = selected[0];
    if i >= bib.entries.len() {
        return app.toast("The bibliography changed; select the references again");
    }
    match name {
        "edit" => edit(app, tab, i),
        "key" => key(app, tab, bib, i),
        "refresh" => app.refresh_reference(tab, bib, i),
        "copy" | "move" => {
            super::operations::transfer_dialog(app, tab, bib, selected, name == "move")
        }
        "pdf" => open_pdf(app, tab, &bib, i),
        "attach" => app.attach_reference(tab, bib, i),
        "download" => app.download_reference(tab, bib, i),
        "usages" => app.bibliography_usages(&bib.entries[i].key),
        "remove" => {
            let after = match bib.remove(&selected) {
                Ok(s) => s,
                Err(e) => return app.cannot("remove references", e),
            };
            review(
                app,
                tab,
                "Remove References (Keep Attachments)",
                bib.source,
                after,
            );
        }
        _ => {}
    }
}

fn key(app: &Rc<App>, tab: &Rc<Tab>, bib: Library, index: usize) {
    let form = dialogs::form();
    let default_pattern = app.config.borrow().bibliography_key_pattern.clone();
    let pattern = field(
        &form,
        "Pattern: {author}, {title}, {year}",
        &default_pattern,
    );
    let key = field(
        &form,
        "Citation Key",
        &bib.generate_key(index, &default_pattern)
            .unwrap_or_default(),
    );
    let notice=gtk::Label::builder().label("Existing keys stay unchanged until applied. Citation usages are checked before renaming.").wrap(true).xalign(0.0).build();
    form.append(&notice);
    let b = bib.clone();
    pattern.connect_changed(glib::clone!(
        #[weak]
        key,
        move |pattern| {
            if let Ok(value) = b.generate_key(index, &pattern.text()) {
                key.set_text(&value);
            }
        }
    ));
    let app = app.clone();
    let tab = tab.clone();
    let parent = app.clone();
    form_dialog(
        &parent,
        "Generate Citation Key",
        "Review Rename",
        &form,
        move || {
            bib.generate_key(index, &pattern.text())
                .map_err(|e| e.to_string())?;
            let after = bib.rename(index, &key.text()).map_err(|e| e.to_string())?;
            app.config.borrow_mut().bibliography_key_pattern = pattern.text().into();
            if let Some(shell) = app.shell.upgrade() {
                shell.save_config_soon();
            }
            app.rename_reference(
                &tab,
                bib.source.clone(),
                after,
                bib.entries[index].key.clone(),
                key.text().into(),
            );
            Ok(())
        },
    );
}

fn check(app: &Rc<App>, tab: &Rc<Tab>, bib: Library) {
    let Some(vault) = app.vault().cloned() else {
        return;
    };
    let path = tab.rel();
    let before = bib.source.clone();
    let app = app.clone();
    let tab = tab.clone();
    glib::spawn_future_local(async move {
        let checked = work::off_thread("check bibliography", move || {
            let mut issues = bib
                .issues()
                .into_iter()
                .map(|(i, s)| format!("{}: {s}", bib.entries[i].key))
                .collect::<Vec<_>>();
            for e in &bib.entries {
                match Attachment::parse(&bib.value(e, "file")) {
                    Ok(files) => {
                        for file in files {
                            match bib::attachment_path(&path, &file.path)
                                .map_err(|e| e.to_string())
                                .and_then(|p| vault.stat(&p).map_err(|e| e.to_string()))
                            {
                                Ok(Some(_)) => {}
                                Ok(None) => issues
                                    .push(format!("{}: missing attachment {}", e.key, file.path)),
                                Err(why) => issues.push(format!("{}: {why}", e.key)),
                            }
                        }
                    }
                    Err(why) => issues.push(format!("{}: {why}", e.key)),
                }
            }
            let mut cleaned = bib.source.clone();
            for i in 0..bib.entries.len() {
                if let Some(doi) = bib::doi(&bib.value(&bib.entries[i], "doi"))
                    && doi != bib.value(&bib.entries[i], "doi")
                {
                    cleaned = Library::parse(&cleaned)?.set_field(i, "doi", &doi)?;
                }
            }
            Ok::<_, accent_core::Error>((issues, cleaned))
        })
        .await;
        match checked {
            Some(Ok((issues, cleaned))) => {
                let form = dialogs::form();
                let text = if issues.is_empty() {
                    "No problems found".into()
                } else {
                    issues.join("\n")
                };
                form.append(
                    &gtk::Label::builder()
                        .label(text)
                        .selectable(true)
                        .wrap(true)
                        .xalign(0.0)
                        .build(),
                );
                form.append(&gtk::Label::builder().label("Review Cleanup normalizes DOI resolver URLs. Missing fields and attachments can be corrected in Edit Reference.").wrap(true).xalign(0.0).build());
                let target = tab.clone();
                let parent = app.clone();
                form_dialog(
                    &app,
                    "Check Bibliography",
                    "Review Cleanup",
                    &form,
                    move || {
                        review(
                            &parent,
                            &target,
                            "Clean Up Bibliography",
                            before.clone(),
                            cleaned.clone(),
                        );
                        Ok(())
                    },
                );
            }
            Some(Err(e)) => app.cannot("check bibliography", e),
            None => app.toast("Bibliography check stopped"),
        }
    });
}

fn merge(app: &Rc<App>, tab: &Rc<Tab>, bib: Library) {
    let pairs = bib.duplicates();
    if pairs.is_empty() {
        return app.toast("No likely duplicates found");
    }
    if pairs.len() == 1 {
        return merge_pair(app, tab, bib, pairs[0]);
    }
    let form = dialogs::form();
    let labels = pairs
        .iter()
        .map(|(a, b)| format!("{} / {}", bib.entries[*a].key, bib.entries[*b].key))
        .collect::<Vec<_>>();
    let choice =
        gtk::DropDown::from_strings(&labels.iter().map(String::as_str).collect::<Vec<_>>());
    form.append(&choice);
    let parent = app.clone();
    let target = tab.clone();
    form_dialog(
        app,
        "Duplicate References",
        "Compare Fields",
        &form,
        move || {
            merge_pair(
                &parent,
                &target,
                bib.clone(),
                pairs[choice.selected() as usize],
            );
            Ok(())
        },
    );
}

fn merge_pair(app: &Rc<App>, tab: &Rc<Tab>, bib: Library, (keep, remove): (usize, usize)) {
    let form = dialogs::form();
    form.append(&gtk::Label::builder().label(format!("Keep {} and merge {} into it. Choose fields to replace. Attachments from both entries are retained.",bib.entries[keep].key,bib.entries[remove].key)).wrap(true).xalign(0.0).build());
    let mut fields = Vec::new();
    for f in &bib.entries[remove].fields {
        if f.name == "file" {
            continue;
        }
        let old = bib.value(&bib.entries[keep], &f.name);
        let new = bib.value(&bib.entries[remove], &f.name);
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
        fields.push((
            f.name.clone(),
            bib.raw(&bib.entries[remove], &f.name).unwrap().to_string(),
            choice,
        ));
    }
    let parent = app.clone();
    let target = tab.clone();
    form_dialog(
        app,
        "Merge Duplicate References",
        "Review Merge",
        &form,
        move || {
            let mut after = bib.source.clone();
            for (name, raw, choice) in &fields {
                if choice.is_active() {
                    after = Library::parse(&after)
                        .and_then(|b| b.set_raw(keep, name, raw))
                        .map_err(|e| e.to_string())?;
                }
            }
            let mut files = Attachment::parse(&bib.value(&bib.entries[keep], "file"))
                .map_err(|e| e.to_string())?;
            for file in Attachment::parse(&bib.value(&bib.entries[remove], "file"))
                .map_err(|e| e.to_string())?
            {
                if !files.iter().any(|f| f.path == file.path) {
                    files.push(file);
                }
            }
            if !files.is_empty() {
                after = Library::parse(&after)
                    .and_then(|b| b.set_field(keep, "file", &Attachment::format(&files)))
                    .map_err(|e| e.to_string())?;
            }
            after = Library::parse(&after)
                .and_then(|b| b.remove(&[remove]))
                .map_err(|e| e.to_string())?;
            after = Library::parse(&after)
                .and_then(|b| b.redirect(&bib.entries[remove].key, &bib.entries[keep].key))
                .map_err(|e| e.to_string())?;
            parent.rename_reference(
                &target,
                bib.source.clone(),
                after,
                bib.entries[remove].key.clone(),
                bib.entries[keep].key.clone(),
            );
            Ok(())
        },
    );
}

fn open_pdf(app: &Rc<App>, tab: &Tab, bib: &Library, i: usize) {
    let files = match Attachment::parse(&bib.value(&bib.entries[i], "file")) {
        Ok(f) => f,
        Err(e) => return app.cannot("open attachment", e),
    };
    let Some(file) = files.first() else {
        return app.toast("No attachment; use Attach PDF or Download PDF");
    };
    match bib::attachment_path(&tab.rel(), &file.path) {
        Ok(path) => app.open_path(&path),
        Err(e) => app.cannot("open attachment", e),
    }
}
