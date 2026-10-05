//! Replace All across the vault, from the sidebar's Search pane, and the Undo on its toast.

use super::*;

impl App {
    /// Rewrite every match of `re` in the vault, from the sidebar's Replace All.
    ///
    /// Open tabs are saved first: the vault writes through the etag gate, so an unsaved buffer
    /// would come back as a changed-on-disk banner instead of a replacement. That part is the
    /// main loop's, and so is the reload afterwards; the rewrite between them is not. It is a
    /// read, a substitution and an fsync per file — 1.9 s across 245 notes and 35 s across 3.3k
    /// of them, measured on the generated vault — so it goes to a worker thread and `done` hands
    /// the sidebar back its pane when it lands.
    pub fn replace_in_files(
        self: &Rc<Self>,
        query: String,
        options: accent_api::Options,
        replacement: String,
        literal: bool,
        include_ignored: bool,
        done: Box<dyn FnOnce()>,
    ) {
        let Some(vault) = self.vault().cloned() else {
            done();
            return self.needs_vault("replace across files");
        };
        let Some(ops) = self.ops().cloned() else {
            done();
            return;
        };
        let open: Vec<String> = self.open_tabs().iter().map(|tab| tab.rel()).collect();
        (ops.flush)(&open);
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let outcome = crate::work::attempt("replace", move || {
                vault.replace_all(&query, options, &replacement, literal, include_ignored)
            })
            .await;
            if let Some(app) = weak.upgrade() {
                match outcome {
                    Ok(report) => {
                        let unsaved = (ops.reload)(&report.rewritten);
                        let message = replace_message(
                            report.matches,
                            report.rewritten.len(),
                            report.failed.len(),
                            unsaved,
                            report.undoable,
                        );
                        // Under one key, so a newer rewrite takes away the Undo of the one
                        // before it, which would now undo this one.
                        let toast = Toast::new(&message).key("replace");
                        app.add_toast(match report.undoable {
                            true => toast.button("Undo", {
                                let weak = Rc::downgrade(&app);
                                move || {
                                    if let Some(app) = weak.upgrade() {
                                        app.undo_replace();
                                    }
                                }
                            }),
                            false => toast,
                        });
                    }
                    // Including a worker that stopped: a Replace the user asked for and watched a
                    // progress state run through must never end in silence.
                    Err(why) => app.toast(&why),
                }
            }
            done();
        });
    }

    /// Put back what the last Replace All rewrote: the Undo on the toast it left.
    ///
    /// Open tabs are saved first, as before the rewrite, so a note edited in one since counts as
    /// changed and is left alone rather than written over. The files it did put back are then
    /// reloaded, and the Search pane asks its question again, since its rows are the rewrite's.
    pub fn undo_replace(self: &Rc<Self>) {
        let (Some(vault), Some(ops)) = (self.vault().cloned(), self.ops().cloned()) else {
            return;
        };
        let open: Vec<String> = self.open_tabs().iter().map(|tab| tab.rel()).collect();
        (ops.flush)(&open);
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let outcome = crate::work::attempt("undo replace", move || vault.undo_replace()).await;
            let Some(app) = weak.upgrade() else { return };
            match outcome {
                Ok(report) => {
                    (ops.reload)(&report.restored);
                    let message = undo_message(&report);
                    tracing::debug!(toast = message, "replace undone");
                    app.toast(&message);
                    if let Some(sidebar) = app.sidebar.get() {
                        sidebar.requery_search();
                    }
                }
                Err(why) => app.toast(&why),
            }
        });
    }
}

/// What the toast says after a Replace All: what it wrote, what it could not, and what is still
/// showing the old text because its tab has unsaved edits. Same shape as `fileops::rename_message`.
fn replace_message(
    matches: usize,
    files: usize,
    failed: usize,
    unsaved: usize,
    undoable: bool,
) -> String {
    let plural = |n: usize, one: &str, many: &str| match n {
        1 => format!("1 {one}"),
        n => format!("{n} {many}"),
    };
    let mut message = match matches {
        0 => "Nothing to replace".to_string(),
        _ => format!(
            "Replaced {} in {}",
            plural(matches, "match", "matches"),
            plural(files, "file", "files")
        ),
    };
    if failed > 0 {
        message.push_str(&format!("; {failed} could not be written"));
    }
    if unsaved > 0 {
        message.push_str(&format!(
            "; {unsaved} have unsaved changes and were not reloaded"
        ));
    }
    // Past the size an undo keeps, which the confirmation already warned of.
    if files > 0 && !undoable {
        message.push_str("; it cannot be undone");
    }
    message
}

/// What an Undo on the Replace All toast did. A file changed since the rewrite was left alone,
/// and one such file is named, since the reader may want to go and look at it.
fn undo_message(report: &accent_api::UndoReport) -> String {
    let mut parts = Vec::new();
    match report.restored.len() {
        0 => {}
        1 => parts.push("Restored 1 file".to_string()),
        n => parts.push(format!("Restored {n} files")),
    }
    match &report.skipped[..] {
        [] => {}
        [one] => parts.push(format!("{one} changed since and was left as it is")),
        many => parts.push(format!(
            "{} files changed since and were left as they are",
            many.len()
        )),
    }
    if !report.failed.is_empty() {
        parts.push(format!("{} could not be written", report.failed.len()));
    }
    match parts.is_empty() {
        true => "Nothing to undo".to_string(),
        false => parts.join("; "),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replace_toast_counts_matches_files_and_what_went_wrong() {
        assert_eq!(replace_message(0, 0, 0, 0, false), "Nothing to replace");
        assert_eq!(
            replace_message(1, 1, 0, 0, true),
            "Replaced 1 match in 1 file"
        );
        assert_eq!(
            replace_message(7, 3, 0, 0, true),
            "Replaced 7 matches in 3 files"
        );
        assert_eq!(
            replace_message(7, 3, 1, 2, true),
            "Replaced 7 matches in 3 files; 1 could not be written; 2 have unsaved changes and were not reloaded"
        );
        assert_eq!(
            replace_message(7, 3, 0, 0, false),
            "Replaced 7 matches in 3 files; it cannot be undone"
        );
    }

    #[test]
    fn undo_toast_names_one_skipped_file_and_counts_several() {
        let report = |restored: &[&str], skipped: &[&str]| accent_api::UndoReport {
            restored: restored.iter().map(|s| s.to_string()).collect(),
            skipped: skipped.iter().map(|s| s.to_string()).collect(),
            failed: Vec::new(),
        };
        assert_eq!(undo_message(&report(&[], &[])), "Nothing to undo");
        assert_eq!(undo_message(&report(&["a.md"], &[])), "Restored 1 file");
        assert_eq!(
            undo_message(&report(&["a.md", "b.md"], &["notes/c.md"])),
            "Restored 2 files; notes/c.md changed since and was left as it is"
        );
        assert_eq!(
            undo_message(&report(&[], &["c.md", "d.md"])),
            "2 files changed since and were left as they are"
        );
    }
}
