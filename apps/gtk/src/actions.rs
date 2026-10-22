//! Shared action definitions, dispatch, effective accelerators and menus.

use super::*;
use crate::export::Export;
use accent_api::debug::Step;
use accent_core::conflict::Take;

mod catalog;
pub use catalog::ACTIONS;

impl App {
    fn run_action(self: &Rc<Self>, name: &str) {
        if name.starts_with("diagram-") {
            self.diagram_action(name);
            return;
        }
        if name.starts_with("notebook-") {
            self.notebook_action(name);
            return;
        }
        match name {
            "save" => self.save(),
            "save-as" => self.save_as(),
            "print" => self.print(),
            "export-pdf" => self.export(Export::Pdf),
            "export-html" => self.export(Export::Html),
            "export-png" => self.export(Export::Png),
            "export-svg" => self.export(Export::Svg),
            "save-session" => self.save_shells(),
            "close-session" => self.close_session(),
            "reload-window" => self.reload(),
            "open-file" => self.open_file_dialog(),
            "new-file" => {
                let Some(vault) = self.vault() else {
                    return self.needs_vault("create a file");
                };
                let dir = self
                    .selected_dir()
                    .unwrap_or_else(|| vault.config().new_file_dir);
                if let Some(ops) = self.ops() {
                    fileops::new_file(ops, &dir);
                }
            }
            "new-folder" => {
                if let Some(ops) = self.need_ops("create a folder") {
                    fileops::new_folder(ops, &self.selected_dir().unwrap_or_default())
                }
            }
            // Where New File reads the selection, so the palette's way in lands in the folder the
            // tree is showing — the same target the row's own menu item is given.
            "new-drawing" => {
                if let Some(ops) = self.need_ops("create a drawing") {
                    fileops::new_drawing(ops, &self.selected_dir().unwrap_or_default())
                }
            }
            // The one way to upload into the vault root: the tree has no row for it, so the
            // folder's own context menu cannot offer it and this reads the selection the way
            // New Folder does.
            "upload" => {
                if let Some(ops) = self.need_ops("upload files") {
                    match ops.vault.is_remote() {
                        true => fileops::upload(ops, &self.selected_dir().unwrap_or_default()),
                        // Listed for every vault, because the palette shows all of ACTIONS, so
                        // the local one says why nothing opened rather than doing nothing.
                        false => self.toast("This vault is already on this machine"),
                    }
                }
            }
            "terminal" => self.open_terminal(),
            "terminal-remote" => self.choose_remote_terminal(),
            "terminal-local" => self.open_terminal_at(Some(glib::home_dir())),
            // Nothing to do over any other tab: the editor and the PDF have their own copy.
            "terminal-copy" => {
                if let Some(Doc::Terminal(term)) = self.active_doc() {
                    term.copy();
                }
            }
            // `Ctrl+Shift+V` over a text tab is Paste as Plain Text: an application accelerator
            // is dispatched at the window, so the editor cannot claim the chord for itself.
            "terminal-paste" => match self.active_doc() {
                Some(Doc::Terminal(term)) => term.paste(),
                Some(Doc::Text(tab)) => tab.paste_verbatim(),
                Some(Doc::Notebook(notebook)) => notebook.paste_verbatim(),
                _ => {}
            },
            "terminal-share" | "terminal-revoke" => {
                let Some(Doc::Terminal(term)) = self.active_doc() else {
                    return self.toast("Select a held terminal in a vault to share it with MCP");
                };
                let share = name == "terminal-share";
                let app = Rc::downgrade(self);
                term.change_share(share, move |term, result| {
                    let Some(app) = app.upgrade() else { return };
                    match result {
                        Ok(()) => {
                            app.toast(if share {
                                "Terminal shared with MCP"
                            } else {
                                "MCP access revoked"
                            });
                            let app = Rc::downgrade(&app);
                            term.ask(move |_| {
                                if let Some(app) = app.upgrade() {
                                    app.sync_details();
                                }
                            });
                        }
                        Err(why) => app.toast(&why),
                    }
                });
            }
            "paste-plain" => match self.active_notebook() {
                Some(notebook) => notebook.paste_verbatim(),
                None => self.with_active(Tab::paste_verbatim),
            },
            "close-tab" => {
                if let Some(page) = self.tabs().selected_page() {
                    self.tabs().close_page(&page);
                }
            }
            "close-pane-tabs" => self.close_pane_tabs(),
            "next-tab" => self.cycle_tab(true),
            "previous-tab" => self.cycle_tab(false),
            "split-left" => self.split_active(Side::Left),
            "split-right" => self.split_active(Side::Right),
            "split-up" => self.split_active(Side::Up),
            "split-down" => self.split_active(Side::Down),
            "move-tab-left" => self.move_tab(Side::Left),
            "move-tab-right" => self.move_tab(Side::Right),
            "move-tab-up" => self.move_tab(Side::Up),
            "move-tab-down" => self.move_tab(Side::Down),
            "move-to-new-window" => {
                let page = self.menu_page.borrow().clone();
                let gtk_app = self.window.application().and_downcast::<adw::Application>();
                if let (Some(page), Some(shell), Some(gtk_app)) = (
                    page.or_else(|| self.tabs().selected_page()),
                    self.shell.upgrade(),
                    gtk_app,
                ) {
                    shell.move_apart(&gtk_app, self, &page);
                }
            }
            "pin-tab" => self.pin_tab(true),
            "unpin-tab" => self.pin_tab(false),
            "divider-left" => self.move_divider(Side::Left),
            "divider-right" => self.move_divider(Side::Right),
            "divider-up" => self.move_divider(Side::Up),
            "divider-down" => self.move_divider(Side::Down),
            "palette-files" => self.palette(palette::Mode::Files),
            "palette-commands" => self.palette(palette::Mode::Commands),
            "palette-symbols" => self.palette(palette::Mode::Symbols),
            "palette-tags" => self.palette(palette::Mode::Tags),
            "open-recent" => self.palette(palette::Mode::Vaults),
            "find" => self.open_find(find::Mode::Find),
            "replace" => self.open_find(find::Mode::Replace),
            // `Ctrl+G` over a diagram groups the selection, as `Ctrl+D` duplicates it.
            "goto-line" => match self.active_diagram() {
                Some(d) => d.group(),
                None => self.open_find(find::Mode::Goto),
            },
            "find-next" => self.pane().find.step(true),
            "find-previous" => self.pane().find.step(false),
            // `Ctrl+D` over a diagram duplicates the selection: an application accelerator is
            // dispatched at the window, so the canvas cannot claim the chord for itself.
            "duplicate-line" => match self.active_diagram() {
                Some(d) => d.duplicate(),
                None => self.with_view(editor::duplicate_line),
            },
            "toggle-comment" => self.with_view(editor::toggle_comment),
            // Not over a notebook, whose column never scrolls sideways.
            "toggle-wrap" => self.with_active(Tab::toggle_wrap),
            "delete-line" => self.with_view(editor::delete_line),
            "newline-below" => {
                // A notebook in front runs the selected cell, as Jupyter's `Ctrl+Enter` does, and
                // a diagram's label being edited takes `Ctrl+Return` as finishing it.
                if let Some(notebook) = self.active_notebook() {
                    return self.run_selected(&notebook, false);
                }
                let committed = self.active_diagram().is_some_and(|d| d.commit_label());
                if !committed && let Some(tab) = self.active() {
                    tab.newline_below();
                }
            }
            "scroll-up" | "scroll-down" => {
                let n = if name == "scroll-up" { -1 } else { 1 };
                match self.active_notebook() {
                    Some(notebook) => notebook.scroll_lines(n),
                    None => self.with_active(|tab| tab.scroll_lines(n)),
                }
            }
            "caret-above" | "caret-below" => {
                let below = name == "caret-below";
                match self.active() {
                    Some(tab) => tab.add_caret(below),
                    // A cell's column stays in the cell.
                    None => self.with_view(|view| {
                        if let Some(column) = view.downcast_ref::<multicaret::View>() {
                            column.add_caret(below);
                        }
                    }),
                }
            }
            "next-occurrence" => self.with_view(editor::add_next_occurrence),
            "all-occurrences" => self.with_view(editor::select_all_occurrences),
            "zoom-in" | "zoom-out" | "zoom-reset" => self.zoom_action(name),
            "back" => self.navigate(false),
            "forward" => self.navigate(true),
            "pdf-invert" => self.with_pdf(|pdf| pdf.toggle_invert()),
            "image-invert" => self.with_image(|image| self.invert_image(&image.key())),
            "pdf-copy" => self.with_pdf(|pdf| pdf.copy_selection()),
            // The header's Undo and Redo fire these over a diagram too.
            "pdf-undo" => match self.active_diagram() {
                Some(d) => d.undo(),
                None => self.with_pdf(|pdf| pdf.undo()),
            },
            "pdf-redo" => match self.active_diagram() {
                Some(d) => d.redo(),
                None => self.with_pdf(|pdf| pdf.redo()),
            },
            "pdf-copy-link" => self.with_pdf(|pdf| pdf.copy_link()),
            "pdf-next-page" => self.with_pdf(|pdf| pdf.next_page()),
            "pdf-previous-page" => self.with_pdf(|pdf| pdf.previous_page()),
            "pdf-export-highlights" => self.export_highlights(),
            "pdf-go-to-source" => self.go_to_source(),
            "show-in-pdf" => self.show_in_pdf(),
            // A diagram's ring is its own, not the window's drawing state.
            "pdf-draw" => {
                let out = match self.active_diagram() {
                    Some(d) => d.ring_shown(),
                    None => self.drawing.get(),
                };
                self.set_drawing(!out);
            }
            "pdf-pen" => self.pdf_mode(pdfview::Mode::Pen),
            "pdf-highlighter" => self.pdf_mode(pdfview::Mode::Highlighter),
            "pdf-eraser" => self.pdf_mode(pdfview::Mode::Eraser),
            "pdf-line" => self.pdf_mode(pdfview::Mode::Line),
            "pdf-rect" => self.pdf_mode(pdfview::Mode::Rect),
            "pdf-circle" => self.pdf_mode(pdfview::Mode::Circle),
            "pdf-adjust" => self.pdf_mode(pdfview::Mode::Adjust),
            "insert-sketch" => self.insert_sketch(),
            "pdf-add-page-before" => self.pdf_add_page(false),
            "pdf-add-page-after" => self.pdf_add_page(true),
            "pdf-delete-page" => self.pdf_delete_page(),
            "pdf-move-page-up" => self.pdf_move_page(false),
            "pdf-move-page-down" => self.pdf_move_page(true),
            "pdf-fit-width" => self.with_pdf(|pdf| pdf.set_zoom(PdfZoom::FitWidth)),
            "pdf-fit-page" => self.with_pdf(|pdf| pdf.set_zoom(PdfZoom::FitPage)),
            "minimap" => self.toggle_minimap(),
            // A preference, not this window's state: written, then put into effect in every
            // window the way a switch in Preferences is, which also moves each window's check mark.
            "show-hidden-files" => {
                {
                    let mut config = self.config.borrow_mut();
                    config.show_hidden = !config.show_hidden;
                }
                self.config_changed();
            }
            // No vault needed: the name is the last segment of a loose file's key as well.
            "copy-name" => {
                if let Some(rel) = self.menu_rel() {
                    self.window.clipboard().set_text(doc::file_name(&rel));
                }
            }
            "copy-relative-path" => {
                if let (Some(rel), Some(ops)) =
                    (self.menu_rel(), self.need_ops("copy a vault path"))
                {
                    fileops::copy_relative_path(ops, &rel);
                }
            }
            "copy-absolute-path" => {
                if let Some(rel) = self.menu_rel() {
                    match self.ops() {
                        Some(ops) => fileops::copy_absolute_path(ops, &rel),
                        // No vault, so the key already is the absolute path.
                        None => self.window.clipboard().set_text(&rel),
                    }
                }
            }
            "show-in-files" => {
                if let Some(rel) = self.menu_rel() {
                    if self.on_host(&rel) {
                        let name = doc::file_name(&rel);
                        return self
                            .cannot(&format!("show {name}"), format!("it is on {}", self.host()));
                    }
                    let path = self.root().join(&rel);
                    let toast = self.clone();
                    fileops::reveal(&self.window, &path, move |m| toast.toast(m));
                }
            }
            "reveal-in-sidebar" => self.reveal_in_sidebar(),
            "sidebar" => self
                .sidebar_column
                .set_visible(!self.sidebar_column.is_visible()),
            "pane-files" => self.show_pane("files"),
            "pane-search" => {
                self.seed_search();
                self.show_pane("search");
            }
            "replace-in-files" => {
                self.sidebar_column.set_visible(true);
                // Before the pane opens: it focuses the replace box when there is a query to
                // replace, and the selection is what makes one.
                self.seed_search();
                if let Some(sidebar) = self.sidebar.get() {
                    sidebar.show_replace();
                }
            }
            "search-all" => {
                self.sidebar_column.set_visible(true);
                if let Some(sidebar) = self.sidebar.get() {
                    sidebar.toggle_search_all();
                }
            }
            // The References section follows a code tab's caret only while it is on screen, so
            // the pane comes up with what is under the caret now.
            "pane-info" => {
                self.show_pane("info");
                self.refresh_references();
            }
            "file-details" => self.show_section("details"),
            "pane-git" => {
                self.show_pane("git");
                // The chord is how the keyboard reaches the commit box; the pane on its own
                // leaves the caret in the note.
                if let Some(git) = self.git.get() {
                    git.focus_commit();
                }
            }
            // The pane's own button and the status bar's branch are this one action, so whichever
            // is pressed, the repository synced is the one the active document sits in and the
            // pane's selection ends up on it.
            "git-sync" => {
                if let Some(git) = self.git.get() {
                    let key = self
                        .active_doc()
                        .filter(|d| !d.is_transient())
                        .map(|d| d.key());
                    git.sync(key.as_deref());
                }
            }
            "git-sync-all" => {
                if let Some(git) = self.git.get() {
                    git.sync_all();
                }
            }
            // The pane's selected repository, like the branch popover's own button: the pane is
            // put on screen so the conflicts a merge may leave are in view.
            "git-merge" => {
                self.show_pane("git");
                if let Some(git) = self.git.get() {
                    git.merge_branch();
                }
            }
            "git-merge-abort" => {
                if let Some(git) = self.git.get() {
                    git.abort_merge();
                }
            }
            // On screen for the reason Merge Branch… gives: the branch list is the pane's.
            "git-delete-branch" => {
                self.show_pane("git");
                if let Some(git) = self.git.get() {
                    git.delete_other_branch();
                }
            }
            "pane-outline" => self.show_pane("outline"),
            "pane-properties" => self.show_pane("properties"),
            "view-mode" => self.set_mode(self.mode.get().next()),
            "follow-link" => self.go_to_definition(),
            "debug-toggle-breakpoint" => self.toggle_breakpoint_at_caret(),
            "debug-remove-breakpoints" => self.remove_all_breakpoints(),
            "debug-watch" => self.watch_at_caret(),
            "debug-start" => self.start_debugging(),
            "debug-continue" => self.debug_resume(),
            "debug-pause" => self.debug_pause(),
            "debug-step-over" => self.debug_step(Step::Over),
            "debug-step-into" => self.debug_step(Step::Into),
            "debug-step-out" => self.debug_step(Step::Out),
            "debug-restart" => self.restart_debugging(),
            "debug-stop" => self.stop_debugging(),
            "debug-console" => self.show_debug_console(),
            "conflict-current" | "conflict-incoming" | "conflict-both" => {
                let take = match name {
                    "conflict-current" => Take::Current,
                    "conflict-incoming" => Take::Incoming,
                    _ => Take::Both,
                };
                if !self
                    .active()
                    .is_some_and(|tab| tab.conflicts().accept_at_caret(take))
                {
                    self.toast("No conflict at the caret");
                }
            }
            "conflict-all-current" | "conflict-all-incoming" => {
                let take = match name {
                    "conflict-all-current" => Take::Current,
                    _ => Take::Incoming,
                };
                if !self
                    .active()
                    .is_some_and(|tab| tab.conflicts().accept_all(take))
                {
                    self.toast("No conflicts in this file");
                }
            }
            "hunk-stage" | "hunk-revert" | "hunk-take" | "hunk-both" => {
                let label = match name {
                    "hunk-stage" => "Stage",
                    "hunk-revert" => "Revert",
                    "hunk-take" => "Take",
                    _ => "Keep Both",
                };
                let compare = self.active().and_then(|tab| tab.comparison());
                if !compare.is_some_and(|c| c.act_at_caret(label)) {
                    self.toast("No hunk at the caret");
                }
            }
            "merge-resolved" => match self.active().filter(|tab| tab.merging().is_some()) {
                Some(tab) => self.mark_resolved(&tab),
                None => self.toast("No merge to mark resolved"),
            },
            "conflict-next" | "conflict-previous" => {
                let forward = name == "conflict-next";
                if !self
                    .active()
                    .is_some_and(|tab| tab.conflicts().step(forward))
                {
                    self.toast("No conflicts in this file");
                }
            }
            "quick-fix" => self.quick_fix(),
            "fold" => self.with_active(Tab::fold_at_caret),
            "unfold" => self.with_active(Tab::unfold_at_caret),
            "fold-all" => self.with_active(Tab::fold_all),
            "unfold-all" => self.with_active(Tab::unfold_all),
            "lsp-restart-file" => {
                if let Some(tab) = self.active() {
                    lang::reopen(&tab);
                } else if let Some(notebook) = self.active_notebook() {
                    notebook.reopen_language();
                }
            }
            "python-environment" => self.choose_python_environment(),
            // The tab menu names its own page; F2 and the palette mean the tree's selection, and
            // the tab in front where the tree has none.
            "rename" => {
                let target = self
                    .menu_file()
                    .map(|rel| (false, rel))
                    .or_else(|| self.selected_row().map(|row| (row.is_dir(), row.rel)))
                    .or_else(|| self.active().map(|tab| (false, tab.rel())));
                if let (Some((is_dir, rel)), Some(ops)) = (target, self.need_ops("rename a file")) {
                    fileops::rename(ops, &rel, is_dir);
                }
            }
            // The same three targets, in the same order. A trash goes to the system's, so it is
            // the one delete that can be taken back and needs no question of its own.
            "trash" => {
                let target = self
                    .menu_file()
                    .or_else(|| self.selected_row().map(|row| row.rel))
                    .or_else(|| self.active().map(|tab| tab.rel()));
                if let (Some(rel), Some(ops)) = (target, self.need_ops("move a file to the trash"))
                {
                    fileops::trash(ops, &rel);
                }
            }
            "new-from-template" => {
                if let Some(ops) = self.need_ops("create a note from a template") {
                    fileops::new_from_template(ops);
                }
            }
            "insert-template" => self.insert_template(),
            "present" => self.set_presenting(self.presenting.get().is_none()),
            "fullscreen" => self.window.set_fullscreened(!self.window.is_fullscreen()),
            "preferences" => self.preferences(),
            "shortcuts" => shortcuts::present(&self.window, &self.config.borrow()),
            "menu" => self.menu.popup(),
            "about" => self.about(),
            _ => tracing::warn!("no handler for action {name}"),
        }
    }

    /// Save the merge result before staging (`git::Panel::mark_resolved`); failed saves never stage.
    pub(crate) fn mark_resolved(self: &Rc<Self>, tab: &Rc<Tab>) {
        let Some(git) = self.git.get() else {
            return;
        };
        self.save_tab_now(tab);
        let left = accent_core::conflict::blocks(&tab.text()).len();
        git.mark_resolved(&tab.rel(), !tab.save.modified.get(), left);
    }

    /// Run `f` on the active text tab. Over anything else the action is a no-op, which is what
    /// every editing chord does over an image or a PDF.
    fn with_active(&self, f: impl FnOnce(&Tab)) {
        if let Some(tab) = self.active() {
            f(&tab);
        }
    }

    /// Run `f` on the view the editing chords act on: the active text tab's, or the selected
    /// cell's of the notebook in front.
    fn with_view(&self, f: impl FnOnce(&sourceview5::View)) {
        if let Some(tab) = self.active() {
            return f(&tab.view);
        }
        if let Some(view) = self.active_notebook().and_then(|n| n.editing_view()) {
            f(&view);
        }
    }

    /// The same for the actions that only mean something over a PDF.
    fn with_pdf(&self, f: impl FnOnce(&Rc<pdftab::PdfTab>)) {
        if let Some(pdf) = self.active_pdf() {
            f(&pdf);
        }
    }

    /// The same for the actions that only mean something over an image.
    fn with_image(&self, f: impl FnOnce(&Rc<doc::Viewer>)) {
        if let Some(Doc::Image(image)) = self.active_doc() {
            f(&image);
        }
    }

    /// Dispatch zoom chords according to the active document kind.
    ///
    /// PDFs fit pages, images resize, shells scale their font and text uses document zoom.
    /// Status pages ignore zoom. Keep dispatch aligned with [`App::sync_status`] and
    /// [`App::refresh_zoom`] so readouts describe the rendered document.
    fn zoom_action(self: &Rc<Self>, name: &str) {
        // Reset is 100 % for anything counted in percentages, and Fit Height for a PDF: one whole
        // page, which is what a reader resetting a zoom wants back, though a PDF opens at Fit
        // Width.
        let stepped = |from: f64| match name {
            "zoom-in" => stepped_zoom(from, false),
            "zoom-out" => stepped_zoom(from, true),
            _ => 1.0,
        };
        match self.active_doc() {
            Some(Doc::Pdf(pdf)) => match name {
                "zoom-in" => pdf.zoom_step(false),
                "zoom-out" => pdf.zoom_step(true),
                _ => pdf.set_zoom(PdfZoom::FitPage),
            },
            Some(Doc::Terminal(term)) => {
                term.set_zoom(stepped(term.zoom()));
                self.refresh_zoom();
            }
            Some(Doc::Text(_)) | Some(Doc::Diff(_)) | Some(Doc::Notebook(_)) => {
                self.set_zoom(stepped(self.zoom.get()))
            }
            Some(Doc::Image(image)) => self.zoom_image(
                &image,
                match name {
                    "zoom-in" => Some(false),
                    "zoom-out" => Some(true),
                    _ => None,
                },
                None,
            ),
            Some(Doc::Diagram(d)) => match name {
                "zoom-in" => d.zoom_step(false),
                "zoom-out" => d.zoom_step(true),
                _ => d.fit_page(),
            },
            Some(Doc::Status(_)) | None => {}
        }
    }

    /// Apply application-wide accelerators and rebuild this window's captured chords.
    pub fn refresh_accels(&self) {
        let Some((shell, gtk_app)) = self.shell.upgrade().zip(self.window.application()) else {
            return;
        };
        shell.apply_accels(&gtk_app);
        let config = self.config.borrow();
        let captured: Vec<(&str, String)> = CAPTURED
            .iter()
            .flat_map(|action| {
                accels_for(&config, action)
                    .into_iter()
                    .map(move |accel| (*action, accel))
            })
            .collect();
        fill_captured(&self.captured, &captured);
    }

    /// Apply a shortcut override in every window; `None` restores [`ACTIONS`] defaults. Return effective bindings.
    pub fn rebind(&self, action: &str, accels: Option<Vec<String>>) -> Vec<String> {
        {
            let mut config = self.config.borrow_mut();
            match accels {
                Some(accels) => config.shortcuts.insert(action.to_string(), accels),
                None => config.shortcuts.remove(action),
            };
        }
        self.config_changed();
        accels_for(&self.config.borrow(), action)
    }
}

/// Effective bindings: config override or defaults. An empty override leaves the action unbound but in the palette.
pub fn accels_for(config: &Config, action: &str) -> Vec<String> {
    if let Some(accels) = config.shortcuts.get(action) {
        return accels.clone();
    }
    ACTIONS
        .iter()
        .find(|(name, _, _)| *name == action)
        .map(|(_, _, accels)| accels.iter().map(|a| a.to_string()).collect())
        .unwrap_or_default()
}

/// Whether `event` is one of `action`'s chords, as the table has it now.
pub fn is_chord(config: &Config, action: &str, event: &gdk::Event) -> bool {
    accels_for(config, action)
        .iter()
        .filter_map(|accel| gtk::ShortcutTrigger::parse_string(accel))
        .any(|trigger| trigger.trigger(event, false) == gdk::KeyMatch::Exact)
}

/// A debug session's controls, on VS Code's keys (user decision 2026-10-17). They are never
/// installed: the window hears them itself while it debugs (`wire::wire_window`), ahead of whatever
/// has the keyboard, GtkPaned's `F6` and a focused shell included.
const SESSION: &[&str] = &[
    "win.debug-continue",
    "win.debug-pause",
    "win.debug-step-over",
    "win.debug-step-into",
    "win.debug-step-out",
    "win.debug-restart",
    "win.debug-stop",
];

/// What gives up a chord it shares with [`SESSION`] while a session runs: `F5`, `F10` and `F11`.
const SHADOWED: &[&str] = &["win.present", "win.menu", "win.fullscreen"];

/// Whether `a` and `b` may hold one chord: a session control and a command it shadows.
pub fn may_share(a: &str, b: &str) -> bool {
    let shadows = |a: &str, b: &str| SESSION.contains(&a) && SHADOWED.contains(&b);
    shadows(a, b) || shadows(b, a)
}

/// The session control `event` is the chord of.
pub fn session_chord(config: &Config, event: &gdk::Event) -> Option<&'static str> {
    SESSION
        .iter()
        .copied()
        .find(|action| is_chord(config, action, event))
}

// GtkTextView binds Ctrl+Up/Down to paragraph movement and GtkSourceView binds Shift+Alt+Up/Down
// to move-viewport. Both are class shortcuts, which run in the bubble phase at the focused view
// and so get the key before the window's application accelerators ever see it. Claiming these four
// actions in the capture phase at the window is the way past that; whichever chords they carry.
const CAPTURED: &[&str] = &[
    "win.scroll-up",
    "win.scroll-down",
    "win.caret-above",
    "win.caret-below",
    // GtkTextView binds Ctrl+K to deleting to the end of the line, which is not something anyone
    // reaches for in an editor that has Ctrl+L for the whole line.
    "win.toggle-comment",
    // GtkTextView takes Return with any modifier for a new line.
    "win.quick-fix",
];

/// The chords that edit the tab in front or move about in it. They are bound only while the
/// keyboard is in a tab or on its pane's tab bar (user decisions 2026-10-13 and 2026-10-16), so
/// pressed in the sidebar, a find bar or a dialog the key is that widget's, and nothing edits the
/// note behind it; the palette still runs them. The find family goes by the active pane instead.
const EDITING: &[&str] = &[
    "win.duplicate-line",
    "win.delete-line",
    "win.newline-below",
    "win.toggle-comment",
    "win.toggle-wrap",
    "win.scroll-up",
    "win.scroll-down",
    "win.caret-above",
    "win.caret-below",
    "win.next-occurrence",
    "win.all-occurrences",
    "win.paste-plain",
    // Paste as Plain Text over a text tab.
    "win.terminal-paste",
    "win.fold",
    "win.unfold",
    "win.quick-fix",
    "win.debug-toggle-breakpoint",
];

/// Where the active window's keyboard is, which decides what the shared accelerator table holds.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Keys {
    /// In a shell, which keeps every chord but [`kept`] ones.
    Shell,
    /// In a tab, or on a pane's tab bar: the whole table.
    Tab,
    /// Anywhere else, which leaves the [`EDITING`] chords to the focused widget.
    Elsewhere,
}

impl Keys {
    /// The table is the application's, so it goes by the active window's focus.
    fn of(gtk_app: &gtk::Application) -> Keys {
        Keys::at(
            gtk_app
                .active_window()
                .and_then(|window| gtk::prelude::GtkWindowExt::focus(&window)),
        )
    }

    /// Where the keyboard is when `focus` has it. A pane's tab bar counts as its tab: a second
    /// click on the tab in front leaves the keyboard on its label.
    pub(crate) fn at(focus: Option<gtk::Widget>) -> Keys {
        let in_tab = |w: &gtk::Widget| {
            [adw::TabView::static_type(), adw::TabBar::static_type()]
                .into_iter()
                .any(|tabs| w.ancestor(tabs).is_some())
        };
        match focus {
            Some(w) if w.is::<vte4::Terminal>() => Keys::Shell,
            Some(w) if in_tab(&w) => Keys::Tab,
            _ => Keys::Elsewhere,
        }
    }
}

/// Select application shortcuts retained while a terminal owns focus.
///
/// Application accelerators precede VTE, so `Shell::apply_accels` must unbind all others.
/// Keep Close Tab, MRU tab cycling, pane and divider moves, New Terminal, zoom, Fullscreen,
/// Go to File and Primary+Shift or Control+Shift chords. These preserve navigation and window control;
/// readline alternatives remain for Ctrl+W's delete-word and Ctrl+E's end-of-line.
///
/// Matching uses accelerator spelling. Hand-written `<Ctrl>` aliases are not recognised;
/// parsing would require GTK initialisation unavailable to these tests.
fn reserved(action: &str, accel: &str) -> bool {
    matches!(
        action,
        "win.close-tab"
            | "win.next-tab"
            | "win.previous-tab"
            | "win.move-tab-left"
            | "win.move-tab-right"
            | "win.move-tab-up"
            | "win.move-tab-down"
            | "win.divider-left"
            | "win.divider-right"
            | "win.divider-up"
            | "win.divider-down"
            | "win.terminal"
            | "win.zoom-in"
            | "win.zoom-out"
            | "win.zoom-reset"
            | "win.fullscreen"
            | "win.palette-files"
            | "win.fold"
            | "win.unfold"
    ) || ((accel.contains("<Primary>") || accel.contains("<Control>")) && accel.contains("<Shift>"))
}

/// Keep [`reserved`] shortcuts, or only terminal Copy and Paste when forwarding all keys.
///
/// VTE turns Ctrl+Shift+C into Ctrl+C for programs, so forwarding Copy would interrupt them.
/// AdwTabView's separate PageUp/PageDown shortcuts still allow leaving the terminal.
fn kept(action: &str, accel: &str, forward: bool) -> bool {
    match forward {
        true => matches!(action, "win.terminal-copy" | "win.terminal-paste"),
        false => reserved(action, accel),
    }
}

/// Whether the table holds `accel` for `action` with the keyboard at `keys`. [`SESSION`] is never
/// installed, and a [`SHADOWED`] command loses a chord in `shadowed`, the session's while the
/// active window debugs.
fn installed(action: &str, accel: &str, keys: Keys, forward: bool, shadowed: &[String]) -> bool {
    let shadows = shadowed.iter().any(|s| s == accel);
    if SESSION.contains(&action) || (SHADOWED.contains(&action) && shadows) {
        return false;
    }
    match keys {
        Keys::Shell => kept(action, accel, forward),
        Keys::Tab => true,
        Keys::Elsewhere => !EDITING.contains(&action),
    }
}

fn clear(controller: &gtk::ShortcutController) {
    let old: Vec<gtk::Shortcut> = (0..controller.n_items())
        .filter_map(|i| controller.item(i).and_downcast::<gtk::Shortcut>())
        .collect();
    for shortcut in old {
        controller.remove_shortcut(&shortcut);
    }
}

/// Capture editor chords before text widgets, checking focus first so terminal
/// shortcuts such as Ctrl+K retain shell behavior.
fn fill_captured(controller: &gtk::ShortcutController, bindings: &[(&str, String)]) {
    clear(controller);
    for (action, accel) in bindings {
        let Some(trigger) = gtk::ShortcutTrigger::parse_string(accel) else {
            continue;
        };
        let action = action.to_string();
        controller.add_shortcut(gtk::Shortcut::new(
            Some(trigger),
            Some(gtk::CallbackAction::new(move |widget, _| {
                let focus = widget
                    .root()
                    .and_downcast::<gtk::Window>()
                    .and_then(|w| gtk::prelude::GtkWindowExt::focus(&w));
                // A notebook cell keeps `Alt+Return` as Insert Cell Below (user decision
                // 2026-10-20) until it is decided what Quick Fix's chord means there.
                let cell = |f: &gtk::Widget| f.has_css_class("notebook-source");
                let editing = focus.is_some_and(|f| {
                    f.is::<sourceview5::View>() && !(action == "win.quick-fix" && cell(&f))
                });
                if !editing {
                    return glib::Propagation::Proceed;
                }
                widget.activate_action(&action, None).is_ok().into()
            })),
        ));
    }
}

pub fn install_actions(app: &Rc<App>) {
    for (full, _, _) in ACTIONS {
        if let Some(name) = full.strip_prefix("win.") {
            // Show Hidden Files and Show Grid carry their values, so a menu draws them as check
            // items. Activating one runs the handler below like any other; the value follows the
            // preference in `App::apply_config`.
            let action = match name {
                "show-hidden-files" => gio::SimpleAction::new_stateful(
                    name,
                    None,
                    &app.config.borrow().show_hidden.to_variant(),
                ),
                "diagram-grid" => gio::SimpleAction::new_stateful(
                    name,
                    None,
                    &app.config.borrow().diagram.grid.to_variant(),
                ),
                _ => gio::SimpleAction::new(name, None),
            };
            action.connect_activate(glib::clone!(
                #[weak]
                app,
                move |_, _| {
                    app.command_used(full);
                    app.run_action(name);
                }
            ));
            app.window.add_action(&action);
        }
    }
    // The session's controls wait for a session, and Print and the exports for a document.
    app.sync_debug();
    app.sync_prints();

    app.captured
        .set_propagation_phase(gtk::PropagationPhase::Capture);
    app.window.add_controller(app.captured.clone());
    app.refresh_accels();

    // A focused shell keeps the keyboard, which means the table has to be rebuilt whenever it
    // crosses into or out of a terminal. `focus-widget` hears every way that happens inside a
    // window: a click, a tab switch, a dialog, `Ctrl+J` itself. Between windows it is the
    // application's `active-window`, hooked in `main`.
    app.window.connect_focus_widget_notify(glib::clone!(
        #[weak]
        app,
        move |window| {
            if let Some((shell, gtk_app)) = app.shell.upgrade().zip(window.application()) {
                shell.sync_accels(&gtk_app);
            }
        }
    ));
}

/// Map mouse side buttons 8 and 9 to Back/Forward; GDK names only the first three buttons.
pub fn nav_action(button: u32) -> Option<&'static str> {
    match button {
        8 => Some("win.back"),
        9 => Some("win.forward"),
        _ => None,
    }
}

/// The label [`ACTIONS`] gives `action`, or the name itself for one it does not list.
pub fn label_of(action: &str) -> &str {
    ACTIONS
        .iter()
        .find(|(name, _, _)| *name == action)
        .map_or(action, |(_, label, _)| *label)
}

/// Build the primary menu's file, vault, view and application sections for `key`.
///
/// Omit unavailable vault operations. Shell windows offer Save Session and named sessions
/// Close Session; local shell creation is separate when New Terminal targets a host.
/// Document commands stay in shortcuts and the palette rather than this window-level menu, but
/// for Print… and Export As, which [`primary_menu`] adds after the files.
fn primary_actions(key: &crate::shell::WindowKey) -> [Vec<&'static str>; 4] {
    let vault = key.vault().is_some();
    let mut shows = vec!["win.terminal", "win.terminal-remote"];
    if key.vault().is_some_and(ssh::is_remote_path) {
        shows.push("win.terminal-local");
    }
    shows.push("win.present");
    let mut files = match vault {
        true => vec!["win.new-file", "win.new-folder"],
        false => Vec::new(),
    };
    files.push("win.open-file");
    // What changes which vault this window is on: the three ways in, then this one again, then
    // the way out.
    let mut vaults = vec![
        "app.open-local",
        "app.open-remote",
        "win.open-recent",
        "win.reload-window",
    ];
    if vault {
        vaults.push("app.close-vault");
    }
    if key.is_terminal() {
        vaults.push("win.save-session");
    }
    if matches!(key, crate::shell::WindowKey::Terminal(_)) {
        vaults.push("win.close-session");
    }
    [
        files,
        vaults,
        shows,
        vec!["win.preferences", "win.shortcuts", "win.about", "app.quit"],
    ]
}

pub fn menu_button(key: &crate::shell::WindowKey) -> gtk::MenuButton {
    gtk::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .tooltip_text("Main Menu")
        .menu_model(&primary_menu(key))
        .valign(gtk::Align::Center)
        .build()
}

/// The primary menu's model for a window opened on `key`, and again whenever the window takes
/// another key: a window of shells saved under a name gains Close Session there and then.
///
/// Print… and Export As follow the files, every format listed and greyed where the tab in front
/// cannot do it (`App::sync_prints`); not in a window of shells, where a document is the exception.
pub fn primary_menu(key: &crate::shell::WindowKey) -> gio::Menu {
    let menu = gio::Menu::new();
    let section = |actions: Vec<&str>| {
        let section = gio::Menu::new();
        for action in actions {
            section.append(Some(label_of(action)), Some(action));
        }
        section
    };
    let [files, vaults, shows, application] = primary_actions(key);
    menu.append_section(None, &section(files));
    if !key.is_terminal() {
        let exports = gio::Menu::new();
        // The submenu says "Export as" already, so an item names only its format.
        for action in &PRINTS[1..] {
            let label = label_of(action);
            exports.append(
                Some(label.strip_prefix("Export as ").unwrap_or(label)),
                Some(action),
            );
        }
        let prints = section(vec![PRINTS[0]]);
        prints.append_submenu(Some("Export As"), &exports);
        menu.append_section(None, &prints);
    }
    for group in [vaults, shows, application] {
        menu.append_section(None, &section(group));
    }
    menu
}

/// Print… and every Export as…, in menu order.
pub const PRINTS: [&str; 5] = [
    "win.print",
    "win.export-pdf",
    "win.export-html",
    "win.export-png",
    "win.export-svg",
];

/// Which of [`PRINTS`] a document of `kind` can do: what the tab menu lists, and what the primary
/// menu leaves sensitive.
pub fn print_actions(kind: Option<Kind>) -> &'static [&'static str] {
    match kind {
        Some(Kind::Note) => &["win.print", "win.export-pdf", "win.export-html"],
        Some(Kind::Pdf) => &["win.print", "win.export-pdf"],
        Some(Kind::Diagram) => &[
            "win.print",
            "win.export-pdf",
            "win.export-png",
            "win.export-svg",
        ],
        _ => &[],
    }
}

/// Initial tab menu with Pin and no page-specific file, transfer or print commands.
pub fn tab_menu() -> gio::Menu {
    let menu = gio::Menu::new();
    fill_tab_menu(&menu, false, false, false, None);
    menu
}

/// Fill the shared tab context menu for the page about to show it.
///
/// `file` enables vault-only Rename and Trash; `movable` enables opening in another window.
/// `pinned` selects Pin or Unpin, and `prints`, the page's kind, supplies supported
/// print/export formats and a diagram's Show Grid.
/// Split leads; Reveal and exports have separate sections because their effects differ.
///
/// Update the existing model because AdwTabView and its popover retain it across pages
/// (`wire::wire_pane`, `setup-menu`).
pub fn fill_tab_menu(
    menu: &gio::Menu,
    file: bool,
    movable: bool,
    pinned: bool,
    prints: Option<Kind>,
) {
    menu.remove_all();
    let split = gio::Menu::new();
    for side in [Side::Left, Side::Right, Side::Up, Side::Down] {
        let action = format!("win.split-{}", side.action());
        split.append(Some(label_of(&action)), Some(&action));
    }
    menu.append_section(None, &split);
    let move_tab = gio::Menu::new();
    for side in [Side::Left, Side::Right, Side::Up, Side::Down] {
        let action = format!("win.move-tab-{}", side.action());
        move_tab.append(Some(label_of(&action)), Some(&action));
    }
    if movable {
        let action = "win.move-to-new-window";
        move_tab.append(Some(label_of(action)), Some(action));
    }
    // After the moves, as GNOME Web has it: pinning is where in the bar the tab stays.
    let pin = match pinned {
        true => "win.unpin-tab",
        false => "win.pin-tab",
    };
    move_tab.append(Some(label_of(pin)), Some(pin));
    menu.append_section(None, &move_tab);
    for action in [
        "win.copy-name",
        "win.copy-relative-path",
        "win.copy-absolute-path",
        "win.show-in-files",
    ] {
        menu.append(Some(label_of(action)), Some(action));
    }
    let reveal = gio::Menu::new();
    reveal.append(
        Some(label_of("win.reveal-in-sidebar")),
        Some("win.reveal-in-sidebar"),
    );
    menu.append_section(None, &reveal);
    let diagram = prints == Some(Kind::Diagram);
    let notebook = prints == Some(Kind::Notebook);
    let prints = print_actions(prints);
    if !prints.is_empty() {
        let section = gio::Menu::new();
        for action in prints {
            section.append(Some(label_of(action)), Some(action));
        }
        menu.append_section(None, &section);
    }
    if diagram {
        let view = gio::Menu::new();
        view.append(Some(label_of("win.diagram-grid")), Some("win.diagram-grid"));
        menu.append_section(None, &view);
    }
    if notebook {
        let view = gio::Menu::new();
        let action = "win.notebook-open-text";
        view.append(Some(label_of(action)), Some(action));
        menu.append_section(None, &view);
    }
    if file {
        // The tree's own arrangement, which is what "tree rows and tabs share the shape" means:
        // the name first, and the one destructive item alone at the end so it is never next to
        // Rename by accident (DESIGN.md, Context menus).
        for action in ["win.rename", "win.trash"] {
            let section = gio::Menu::new();
            section.append(Some(label_of(action)), Some(action));
            menu.append_section(None, &section);
        }
    }
}

/// Pane bounds for [`panes::neighbour`]; zero rectangle before allocation, including newly split panes.
pub fn pane_rect(pane: &Pane, root: &gtk::Widget) -> graphene::Rect {
    pane.widget()
        .compute_bounds(root)
        .unwrap_or_else(graphene::Rect::zero)
}

/// Use one toggle: pressed state and current-state icon distinguish the two states.
pub fn mode_switcher() -> gtk::ToggleButton {
    let button = gtk::ToggleButton::builder()
        .icon_name(Mode::Editor.icon())
        .tooltip_text(label_of("win.view-mode"))
        .valign(gtk::Align::Center)
        .build();
    button.add_css_class("flat");
    button
}

impl Shell {
    /// Replace application accelerators with the current config's full table.
    ///
    /// Filter through [`kept`] while the active window focuses a shell, allowing other keys to
    /// reach VTE, and leave out the [`EDITING`] chords while its keyboard is in no tab. Filter
    /// effective bindings so rebindings obey the same rule; the table belongs to the application
    /// even when another window rebuilds it. While the active window debugs, its session's chords
    /// are taken from the commands they shadow ([`installed`]).
    pub fn apply_accels(&self, gtk_app: &gtk::Application) {
        let config = self.config.borrow();
        let (keys, debugging) = self.keys_of(gtk_app);
        self.keys.set((keys, debugging));
        let shadowed: Vec<String> = match debugging {
            true => SESSION
                .iter()
                .flat_map(|action| accels_for(&config, action))
                .collect(),
            false => Vec::new(),
        };
        let forward = config.forward_keys_to_terminal;
        for (action, _, _) in ACTIONS {
            let accels = accels_for(&config, action);
            let accels: Vec<&str> = accels
                .iter()
                .map(String::as_str)
                .filter(|accel| installed(action, accel, keys, forward, &shadowed))
                .collect();
            gtk_app.set_accels_for_action(action, &accels);
        }
    }

    /// Rebuild shortcuts only when the keyboard crosses into or out of a shell or a tab, or a
    /// session starts or ends in the active window, avoiding work on every click.
    pub fn sync_accels(&self, gtk_app: &gtk::Application) {
        if self.keys_of(gtk_app) != self.keys.get() {
            self.apply_accels(gtk_app);
        }
    }

    /// Where the active window's keyboard is, and whether that window debugs.
    fn keys_of(&self, gtk_app: &gtk::Application) -> (Keys, bool) {
        let debugging = gtk_app
            .active_window()
            .and_then(|window| self.app_at(&window))
            .is_some_and(|app| app.debugger.running());
        (Keys::of(gtk_app), debugging)
    }
}
