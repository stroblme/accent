//! The action table and everything that dispatches from it: `run_action`, the accelerators in
//! force and the menus that name the actions.

use super::*;

/// Every user-facing action: the name it answers to, the label the menu and the palette show, and
/// its accelerators. One table, so an action cannot exist without being reachable and findable
/// (DESIGN.md, Keyboard). Stepping along the bar and picking a tab by number stay `AdwTabView`'s
/// own shortcuts; the two that walk the tabs in the order they were last used are ours, because
/// libadwaita has no notion of that order.
pub const ACTIONS: &[(&str, &str, &[&str])] = &[
    ("win.save", "Save", &["<Control>s"]),
    ("win.open-file", "Open File…", &["<Control>o"]),
    ("win.new-file", "New File", &["<Control>n"]),
    ("win.new-folder", "New Folder", &["<Control><Shift>n"]),
    ("win.upload", "Upload Files…", &[]),
    ("win.close-tab", "Close Tab", &["<Control>w"]),
    // Most-recently-used order, so one press is the note before this one. Both spellings of the
    // backwards chord, because X11 delivers Shift+Tab as `ISO_Left_Tab` and which of the two a
    // GTK trigger matches is a question of the keymap rather than of the table.
    ("win.next-tab", "Next Tab", &["<Control>Tab"]),
    (
        "win.previous-tab",
        "Previous Tab",
        &["<Control><Shift>Tab", "<Control><Shift>ISO_Left_Tab"],
    ),
    ("win.terminal", "New Terminal", &["<Control>j"]),
    // Actions rather than callbacks on the shell itself, so they rebind, list in the palette and
    // can be named by the terminal's own context menu. Both spellings carry Control and Shift, so
    // `forwarded` hands them back from a focused shell without being told to.
    (
        "win.terminal-copy",
        "Copy in Terminal",
        &["<Control><Shift>c"],
    ),
    (
        "win.terminal-paste",
        "Paste in Terminal",
        &["<Control><Shift>v"],
    ),
    // Split Right takes VS Code's chord; the other three are menu and palette only, because
    // three more accelerators for the same idea is three more chords nobody has to spare.
    ("win.split-right", "Split Right", &["<Control>backslash"]),
    ("win.split-left", "Split Left", &[]),
    ("win.split-up", "Split Up", &[]),
    ("win.split-down", "Split Down", &[]),
    // Move the tab into the pane that way, splitting one off only when there is none. Left and
    // right alone carry chords: `Shift+Alt+Up` / `Shift+Alt+Down` are the multi-caret pair, and
    // every plainer arrow chord is spoken for — `Alt+Left` / `Alt+Right` are Back and Forward,
    // `Alt+Up` / `Alt+Down` and `Ctrl+Alt`+arrow are on DESIGN.md's never-bind list.
    ("win.move-tab-left", "Move Tab Left", &["<Shift><Alt>Left"]),
    (
        "win.move-tab-right",
        "Move Tab Right",
        &["<Shift><Alt>Right"],
    ),
    ("win.move-tab-up", "Move Tab Up", &[]),
    ("win.move-tab-down", "Move Tab Down", &[]),
    ("app.new-window", "New Window", &[]),
    ("app.open-vault", "Open Folder…", &["<Control><Shift>o"]),
    ("app.open-remote", "Open Remote…", &[]),
    ("win.open-recent", "Open Recent…", &["<Control>r"]),
    ("app.close-vault", "Close Vault", &[]),
    ("app.quit", "Quit", &["<Control>q"]),
    ("win.palette-files", "Go to File…", &["<Control>e"]),
    (
        "win.palette-commands",
        "Run a Command…",
        &["<Control>p", "<Control><Shift>p"],
    ),
    ("win.find", "Find", &["<Control>f"]),
    ("win.replace", "Replace", &["<Control>h"]),
    (
        "win.replace-in-files",
        "Replace in Notes",
        &["<Control><Shift>h"],
    ),
    ("win.find-next", "Find Next", &["F3"]),
    ("win.find-previous", "Find Previous", &["<Shift>F3"]),
    ("win.goto-line", "Go to Line", &["<Control>g"]),
    ("win.duplicate-line", "Duplicate Line", &["<Control>d"]),
    ("win.delete-line", "Delete Line", &["<Control>l"]),
    (
        "win.newline-below",
        "Insert Line Below",
        &["<Control>Return"],
    ),
    ("win.toggle-comment", "Toggle Comment", &["<Control>k"]),
    ("win.toggle-wrap", "Toggle Word Wrap", &["<Alt>z"]),
    ("win.scroll-up", "Scroll Up", &["<Control>Up"]),
    ("win.scroll-down", "Scroll Down", &["<Control>Down"]),
    ("win.caret-above", "Add Caret Above", &["<Shift><Alt>Up"]),
    ("win.caret-below", "Add Caret Below", &["<Shift><Alt>Down"]),
    (
        "win.zoom-in",
        "Zoom In",
        &["<Control>plus", "<Control>equal", "<Control>KP_Add"],
    ),
    (
        "win.zoom-out",
        "Zoom Out",
        &["<Control>minus", "<Control>KP_Subtract"],
    ),
    (
        "win.zoom-reset",
        "Reset Zoom",
        &["<Control>0", "<Control>KP_0"],
    ),
    ("win.sidebar", "Toggle Sidebar", &["F9"]),
    ("win.pane-files", "Files Pane", &["<Control><Shift>e"]),
    ("win.pane-search", "Search Pane", &["<Control><Shift>f"]),
    // No chord: it is the Search pane's own All button, and the palette is how a command with
    // no chord is found.
    ("win.search-all", "Search Ignored Files", &[]),
    ("win.pane-tags", "Tags Pane", &["<Control><Shift>t"]),
    ("win.pane-git", "Git Pane", &["<Control><Shift>g"]),
    ("win.git-sync", "Sync", &[]),
    ("win.git-merge", "Merge Branch…", &[]),
    ("win.git-merge-abort", "Abort Merge", &[]),
    ("win.git-delete-branch", "Delete Branch…", &[]),
    ("win.pane-outline", "Outline Pane", &["<Control><Shift>l"]),
    (
        "win.pane-properties",
        "Properties Pane",
        &["<Control><Shift>a"],
    ),
    // Back and forward walk the active pane's history, over every kind of document. They take
    // the chords a browser uses for the same idea, and the mouse's side buttons with them.
    ("win.back", "Back", &["<Alt>Left"]),
    ("win.forward", "Forward", &["<Alt>Right"]),
    // The PDF reader. The rest live in the palette, found by name rather than by chord.
    //
    // Paging carries no chord here, and cannot: a reader pages with `Space`, `Shift+Space`, `n`,
    // `p` and the arrows, and an application accelerator is dispatched at the window ahead of
    // whatever has the keyboard — a bare `space` in this table would stop the editor, the
    // terminal and every entry in the app from taking one. Those keys stay on the PDF tab's own
    // controller (`PdfTab::wire_keys`), which fires these two actions rather than paging itself,
    // so the palette lists the commands and anything that can activate an action can page.
    ("win.pdf-next-page", "Next Page", &[]),
    ("win.pdf-previous-page", "Previous Page", &[]),
    ("win.pdf-fit-width", "Fit Width", &[]),
    ("win.pdf-fit-page", "Fit Height", &[]),
    ("win.pdf-invert", "Invert PDF Colours", &[]),
    ("win.pdf-copy", "Copy Selection", &[]),
    // No accelerator here for the same reason the paging commands have none: `Ctrl+Z` over a
    // note is GtkSourceView's own undo, and an application accelerator would take it from every
    // text view in the window. The PDF tab's key controller fires these two.
    ("win.pdf-undo", "Undo Drawing", &[]),
    ("win.pdf-redo", "Redo Drawing", &[]),
    ("win.pdf-copy-link", "Copy Link to Selection", &[]),
    ("win.pdf-export-highlights", "Export Highlights to PDF", &[]),
    ("win.pdf-draw", "Drawing", &["<Control><Shift>i"]),
    ("win.pdf-pen", "Pen", &[]),
    ("win.pdf-highlighter", "Highlighter", &[]),
    ("win.pdf-eraser", "Eraser", &[]),
    ("win.pdf-line", "Line", &[]),
    ("win.pdf-rect", "Rectangle", &[]),
    ("win.pdf-circle", "Circle", &[]),
    ("win.pdf-adjust", "Adjust", &[]),
    ("win.insert-sketch", "Insert Sketch", &[]),
    // The diagram tab. Undo, Redo, Delete Selection, Select All Shapes, Edit Label and the
    // paging commands are the canvas's own keys, which fire these, and carry no chord for the
    // PDF's reason: `Ctrl+Z`, `Ctrl+A`, `Delete` and `Return` belong to whatever has the
    // keyboard. Duplicate Selection is `Ctrl+D`, which is Duplicate Line's chord and which
    // `run_action` hands to a diagram in front.
    ("win.diagram-undo", "Undo Diagram Edit", &[]),
    ("win.diagram-redo", "Redo Diagram Edit", &[]),
    ("win.diagram-delete", "Delete Selection", &[]),
    ("win.diagram-duplicate", "Duplicate Selection", &[]),
    ("win.diagram-select-all", "Select All Shapes", &[]),
    ("win.diagram-edit-label", "Edit Label", &[]),
    ("win.diagram-next-page", "Next Diagram Page", &[]),
    ("win.diagram-previous-page", "Previous Diagram Page", &[]),
    ("win.diagram-add-page", "Add Diagram Page", &[]),
    ("win.diagram-rename-page", "Rename Diagram Page…", &[]),
    ("win.diagram-delete-page", "Delete Diagram Page", &[]),
    ("win.diagram-to-front", "Bring to Front", &[]),
    ("win.diagram-to-back", "Send to Back", &[]),
    ("win.diagram-select", "Select and Move", &[]),
    ("win.diagram-rect", "Add Rectangle", &[]),
    ("win.diagram-ellipse", "Add Ellipse", &[]),
    ("win.diagram-text", "Add Text", &[]),
    ("win.diagram-connector", "Add Connector", &[]),
    ("win.diagram-image", "Add Image…", &[]),
    (
        "win.pane-references",
        "References Pane",
        &["<Control><Shift>b"],
    ),
    ("win.view-mode", "Toggle Preview", &["<Control>m"]),
    ("win.minimap", "Toggle Minimap", &[]),
    // No chord: `Ctrl+H`, the file managers' own, is Replace here.
    ("win.show-hidden-files", "Show Hidden Files", &[]),
    ("win.copy-name", "Copy Name", &[]),
    ("win.copy-relative-path", "Copy Relative Path", &[]),
    ("win.copy-absolute-path", "Copy Absolute Path", &[]),
    ("win.show-in-files", "Show in Files", &[]),
    ("win.reveal-in-sidebar", "Reveal in Sidebar", &[]),
    (
        "win.follow-link",
        "Go to Definition",
        &["<Control><Shift>Return", "F12"],
    ),
    // Control+Shift chords, which the terminal's reserved set already lets through, and which no
    // GtkSourceView built-in claims.
    ("win.fold", "Fold", &["<Control><Shift>bracketleft"]),
    ("win.unfold", "Unfold", &["<Control><Shift>bracketright"]),
    ("win.fold-all", "Fold All", &[]),
    ("win.unfold-all", "Unfold All", &[]),
    ("win.rename", "Rename", &["F2"]),
    // No chord: Delete over a tree row already trashes, from the tree's own key controller, and
    // a chord that reaches a focused editor is the last thing a delete should have.
    ("win.trash", "Move to Trash", &[]),
    (
        "win.new-from-template",
        "New from Template…",
        &["<Control><Shift>d"],
    ),
    ("win.insert-template", "Insert Template…", &[]),
    ("win.present", "Presentation Mode", &["F5"]),
    ("win.fullscreen", "Fullscreen", &["F11"]),
    ("win.preferences", "Preferences", &["<Control>comma"]),
    ("win.menu", "Primary Menu", &["F10"]),
    ("win.about", "About accent", &[]),
];

impl App {
    fn run_action(self: &Rc<Self>, name: &str) {
        if name.starts_with("diagram-") {
            self.diagram_action(name);
            return;
        }
        match name {
            "save" => self.save_active(),
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
            // Nothing to do over any other tab: the editor and the PDF have their own copy, and a
            // paste into a document is GtkTextView's.
            "terminal-copy" => {
                if let Some(Doc::Terminal(term)) = self.active_doc() {
                    term.copy();
                }
            }
            "terminal-paste" => {
                if let Some(Doc::Terminal(term)) = self.active_doc() {
                    term.paste();
                }
            }
            "close-tab" => {
                if let Some(page) = self.tabs().selected_page() {
                    self.tabs().close_page(&page);
                }
            }
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
            "palette-files" => self.palette(palette::Mode::Files),
            "palette-commands" => self.palette(palette::Mode::Commands),
            "open-recent" => self.palette(palette::Mode::Vaults),
            "find" => self.open_find(find::Mode::Find),
            "replace" => self.open_find(find::Mode::Replace),
            "goto-line" => self.open_find(find::Mode::Goto),
            "find-next" => self.pane().find.step(true),
            "find-previous" => self.pane().find.step(false),
            // `Ctrl+D` over a diagram duplicates the selection: an application accelerator is
            // dispatched at the window, so the canvas cannot claim the chord for itself.
            "duplicate-line" => match self.active_diagram() {
                Some(d) => d.duplicate(),
                None => self.with_active(Tab::duplicate_line),
            },
            "toggle-comment" => self.with_active(Tab::toggle_comment),
            "toggle-wrap" => self.with_active(Tab::toggle_wrap),
            "delete-line" => self.with_active(Tab::delete_line),
            "newline-below" => {
                // `Ctrl+Return` belongs to the git commit box while the keyboard is in it
                // (DESIGN.md, Git pane). A window accelerator is dispatched ahead of any
                // controller on the focused widget, so the box cannot claim the chord itself.
                let committed = self.git.get().is_some_and(|git| git.commit_if_focused())
                    || self.active_diagram().is_some_and(|d| d.commit_label());
                if !committed && let Some(tab) = self.active() {
                    tab.newline_below();
                }
            }
            "scroll-up" => self.with_active(|tab| tab.scroll_lines(-1)),
            "scroll-down" => self.with_active(|tab| tab.scroll_lines(1)),
            "caret-above" => self.with_active(|tab| tab.add_caret(false)),
            "caret-below" => self.with_active(|tab| tab.add_caret(true)),
            "zoom-in" | "zoom-out" | "zoom-reset" => self.zoom_action(name),
            "back" => self.navigate(false),
            "forward" => self.navigate(true),
            "pdf-invert" => self.with_pdf(|pdf| pdf.toggle_invert()),
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
            "pane-tags" => self.show_pane("tags"),
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
            "pane-references" => {
                self.show_pane("references");
                self.refresh_references();
            }
            "view-mode" => self.set_mode(self.mode.get().next()),
            "follow-link" => self.go_to_definition(),
            "fold" => self.with_active(Tab::fold_at_caret),
            "unfold" => self.with_active(Tab::unfold_at_caret),
            "fold-all" => self.with_active(Tab::fold_all),
            "unfold-all" => self.with_active(Tab::unfold_all),
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
            "menu" => self.menu.popup(),
            "about" => self.about(),
            _ => tracing::warn!("no handler for action {name}"),
        }
    }

    /// Run `f` on the active text tab. Over anything else the action is a no-op, which is what
    /// every editing chord does over an image or a PDF.
    fn with_active(&self, f: impl FnOnce(&Tab)) {
        if let Some(tab) = self.active() {
            f(&tab);
        }
    }

    /// The same for the actions that only mean something over a PDF.
    fn with_pdf(&self, f: impl FnOnce(&Rc<pdftab::PdfTab>)) {
        if let Some(pdf) = self.active_pdf() {
            f(&pdf);
        }
    }

    /// One of the three zoom chords, dispatched to whatever the active tab is.
    ///
    /// A PDF fits its pages, an image is given a size of its own, a shell scales its own font and
    /// a document or a comparison scales the display-wide one; a status page draws at a size
    /// nobody chose, so the chords do nothing there. It is matched in the same shape as
    /// [`App::sync_status`] and [`App::refresh_zoom`] on purpose: what the chords reach and what
    /// the readout says have to be the same list, or the bar says 120 % over something drawn at
    /// its own size.
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
            Some(Doc::Text(_)) | Some(Doc::Diff(_)) => self.set_zoom(stepped(self.zoom.get())),
            Some(Doc::Image(image)) => self.zoom_image(
                &image,
                match name {
                    "zoom-in" => Some(false),
                    "zoom-out" => Some(true),
                    _ => None,
                },
            ),
            Some(Doc::Diagram(d)) => match name {
                "zoom-in" => d.zoom_step(false),
                "zoom-out" => d.zoom_step(true),
                _ => d.fit_page(),
            },
            Some(Doc::Status(_)) | None => {}
        }
    }

    /// Push the accelerators in force into the application and rebuild this window's captured
    /// chords. The table is the application's and lives with the [`Shell`]; the captured
    /// controller is the window's own.
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

    /// Store an accelerator override for `action` and put it into effect at once, in every
    /// window. `None` drops the override, so the action goes back to what [`ACTIONS`] says.
    /// Returns what is in force after.
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

/// What `action` is bound to right now: the user's override from the config if there is one, the
/// built-in table otherwise. An override that is an empty list leaves the action unbound, which is
/// a binding too — it still lists in the palette, just without a chord.
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
];

/// The chords the window keeps while a shell has the keyboard. Everything else in [`ACTIONS`]
/// goes to the shell: a terminal that answers only half of readline is not a terminal.
///
/// GTK dispatches a window's application accelerators at the window in the capture phase, ahead
/// of the focused VTE, so a chord in the table is eaten whatever the terminal does with it —
/// unbinding it in `Shell::apply_accels` is what lets the key through. The reserved set is small
/// and each entry earns its place:
///
/// * `win.close-tab` (`Ctrl+W`) — Close Tab has to mean the same thing over every tab. This is
///   the one budgeted cost: readline loses delete-word, and `Alt+Backspace` still does it.
/// * `win.next-tab` / `win.previous-tab` (`Ctrl+Tab`) — the same rule as Close Tab, and these
///   were `AdwTabView`'s own capture-phase chords before they were actions, so a shell never had
///   them to lose. No readline meaning either: `Ctrl+I` is the completion key, not `Ctrl+Tab`.
/// * `win.terminal` (`Ctrl+J`) and the three zoom actions — the chords that open a shell and
///   scale one have to be reachable from inside one.
/// * `win.fullscreen` (`F11`) — no readline or curses meaning, and GNOME Terminal keeps the same
///   key for the same reason: a fullscreen window has to be leavable from a focused shell.
/// * every chord whose spelling carries both `<Control>` and `<Shift>` — the existing convention,
///   which no shell claims, and which already covers Copy and Paste in Terminal, the pane chords,
///   the palette's second spelling and Replace in Notes.
///
/// ponytail: matched on the accelerator's spelling. A `<Primary>` or `<Ctrl>` written by hand into
/// the config is not recognised; `gtk::accelerator_parse` would settle it but needs an initialised
/// GTK, which the tests do not have.
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
            | "win.terminal"
            | "win.zoom-in"
            | "win.zoom-out"
            | "win.zoom-reset"
            | "win.fullscreen"
    ) || (accel.contains("<Control>") && accel.contains("<Shift>"))
}

fn clear(controller: &gtk::ShortcutController) {
    let old: Vec<gtk::Shortcut> = (0..controller.n_items())
        .filter_map(|i| controller.item(i).and_downcast::<gtk::Shortcut>())
        .collect();
    for shortcut in old {
        controller.remove_shortcut(&shortcut);
    }
}

/// The window's capture controller, which takes chords the text widgets would otherwise claim.
///
/// It runs before everything, so it has to ask who has the keyboard first: these are editor
/// chords, and a shell wants `Ctrl+K` to kill to the end of the line rather than to toggle a
/// comment in a note nobody is looking at.
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
                let editing = widget
                    .root()
                    .and_downcast::<gtk::Window>()
                    .and_then(|w| gtk::prelude::GtkWindowExt::focus(&w))
                    .is_some_and(|f| f.is::<sourceview5::View>());
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
            // Show Hidden Files carries its value, so a menu draws it as a check item. Activating
            // it runs the handler below like any other; the value follows the preference in
            // `App::apply_config`.
            let action = match name {
                "show-hidden-files" => gio::SimpleAction::new_stateful(
                    name,
                    None,
                    &app.config.borrow().show_hidden.to_variant(),
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

/// Which action a mouse button asks for, for the two GTK has no name for. GDK names only the
/// first three buttons; 8 and 9 are the side pair every mouse that has one ships, and browsers
/// have meant back and forward by them for twenty years.
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

pub fn menu_button() -> gtk::MenuButton {
    let menu = gio::Menu::new();
    for group in [
        [
            "win.new-file",
            "win.new-folder",
            "win.open-file",
            "win.save",
        ]
        .as_slice(),
        // What changes which vault this window is on: the three ways in, then the way out.
        [
            "app.open-vault",
            "app.open-remote",
            "win.open-recent",
            "app.close-vault",
        ]
        .as_slice(),
        ["win.find", "win.view-mode", "win.terminal", "win.present"].as_slice(),
        ["win.preferences", "win.about", "app.quit"].as_slice(),
    ] {
        let section = gio::Menu::new();
        for action in group {
            section.append(Some(label_of(action)), Some(action));
        }
        menu.append_section(None, &section);
    }
    gtk::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .tooltip_text("Main Menu")
        .menu_model(&menu)
        .valign(gtk::Align::Center)
        .build()
}

/// The tab's own context menu, as a pane is built with it: nothing yet known about which page
/// will show it, so without the two items that name a file.
pub fn tab_menu() -> gio::Menu {
    let menu = gio::Menu::new();
    fill_tab_menu(&menu, false);
    menu
}

/// The tab's own context menu: what can be done with the file behind a tab without touching it.
/// Splitting leads, because it opens rather than copies; Reveal sits in a section of its own
/// because it moves the sidebar rather than the clipboard.
///
/// `file` says whether the tab about to show this holds a file of this vault, which is what
/// Rename and Move to Trash need and nothing else here does: a shell and a comparison are no
/// file, and a loose one is outside the vault those two act in. On such a tab the two are not on
/// the menu at all rather than on it and refusing (DESIGN.md, Principle 1).
///
/// Filled in place rather than built afresh, because `AdwTabView` holds one model per pane and a
/// `GtkPopoverMenu` follows the model it was made from: the page about to show the menu is what
/// decides what it says (`wire::wire_pane`, `setup-menu`).
pub fn fill_tab_menu(menu: &gio::Menu, file: bool) {
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

/// Where `pane` sits in the window, for [`panes::neighbour`]. A pane that has not been allocated
/// yet — a split in the same main-loop turn — has no bounds, and a zero rect is what says so.
pub fn pane_rect(pane: &Pane, root: &gtk::Widget) -> graphene::Rect {
    pane.widget()
        .compute_bounds(root)
        .unwrap_or_else(graphene::Rect::zero)
}

/// One button rather than a two-item group: there are only two states, so the pressed look plus
/// an icon that names the current one says everything a second toggle would have.
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
    /// Push the accelerators in force into the application. Done wholesale: forty
    /// `set_accels_for_action` calls are cheaper than working out which of them a config change
    /// touched.
    ///
    /// A focused shell narrows the table to [`reserved`], because an application accelerator is
    /// dispatched at the window ahead of the VTE and unbinding it is the only thing that lets the
    /// key reach the shell. The filter reads the accelerators in force, so a rebound chord follows
    /// the same rule as the default it replaced. The shell asked about is the active window's,
    /// whichever window is rebuilding: the table is one for all of them.
    pub fn apply_accels(&self, gtk_app: &gtk::Application) {
        let config = self.config.borrow();
        let shell = terminal::has_focus(gtk_app);
        self.shell_keys.set(shell);
        for (action, _, _) in ACTIONS {
            let accels = accels_for(&config, action);
            let accels: Vec<&str> = accels
                .iter()
                .map(String::as_str)
                .filter(|accel| !shell || reserved(action, accel))
                .collect();
            gtk_app.set_accels_for_action(action, &accels);
        }
    }

    /// The keyboard moved: rebuild the table if it crossed into or out of a shell. Only a change
    /// is worth acting on — focus moves on every click, and the rebuild is sixty calls.
    pub fn sync_accels(&self, gtk_app: &gtk::Application) {
        if terminal::has_focus(gtk_app) != self.shell_keys.get() {
            self.apply_accels(gtk_app);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accels_for_prefers_the_override() {
        let mut config = Config::default();
        assert_eq!(accels_for(&config, "win.save"), ["<Control>s"]);
        assert!(accels_for(&config, "win.about").is_empty());
        assert!(accels_for(&config, "win.nonexistent").is_empty());

        config.shortcuts.insert(
            "win.save".to_string(),
            vec!["<Control><Shift>s".to_string()],
        );
        // An override replaces the whole list rather than adding to it.
        assert_eq!(accels_for(&config, "win.save"), ["<Control><Shift>s"]);
        // An empty override is "unbound", not "fall back to the default".
        config.shortcuts.insert("win.find".to_string(), Vec::new());
        assert!(accels_for(&config, "win.find").is_empty());
    }

    /// Every action the capture controller claims has to be in the table it reads its chords from,
    /// or a rebind would silently drop it.
    #[test]
    fn captured_actions_are_in_the_action_table() {
        for action in CAPTURED {
            assert!(
                ACTIONS.iter().any(|(name, _, _)| name == action),
                "{action} is captured but not in ACTIONS"
            );
        }
    }

    /// The tab menu builds its action names with `format!`, so nothing but this says that what it
    /// puts on a menu is a command the window has and the palette lists.
    #[test]
    fn the_tab_menu_names_actions_that_exist() {
        for side in [Side::Left, Side::Right, Side::Up, Side::Down] {
            for prefix in ["win.split-", "win.move-tab-"] {
                let action = format!("{prefix}{}", side.action());
                assert!(
                    ACTIONS.iter().any(|(name, _, _)| *name == action),
                    "{action} is on the tab menu but not in ACTIONS"
                );
            }
        }
        for action in ["win.rename", "win.trash"] {
            assert!(
                ACTIONS.iter().any(|(name, _, _)| *name == action),
                "{action} is on the tab menu but not in ACTIONS"
            );
        }
    }

    /// The two items a shell or a comparison must not be offered, and that a pane built before
    /// any page exists does not carry either.
    #[test]
    fn the_tab_menu_names_a_file_only_where_there_is_one() {
        let sections = |file| {
            let menu = gio::Menu::new();
            fill_tab_menu(&menu, file);
            menu.n_items()
        };
        assert_eq!(sections(true), sections(false) + 2);
        assert_eq!(tab_menu().n_items(), sections(false));
    }

    /// Same guard for the mouse: a side button fires an action by name, so the name has to be one
    /// the window actually has.
    #[test]
    fn the_side_buttons_name_actions_that_exist() {
        assert_eq!(nav_action(8), Some("win.back"));
        assert_eq!(nav_action(9), Some("win.forward"));
        // The three GTK does name are everyone else's: click, paste, context menu.
        for button in [1, 2, 3] {
            assert_eq!(nav_action(button), None);
        }
        for action in [8, 9].into_iter().filter_map(nav_action) {
            assert!(
                ACTIONS.iter().any(|(name, _, _)| *name == action),
                "{action} is on a mouse button but not in ACTIONS"
            );
        }
    }

    /// The reserved set is what a focused shell does not get, and it is pure data: everything
    /// else in the table is unbound for as long as a terminal has the keyboard.
    #[test]
    fn a_focused_shell_keeps_everything_but_the_reserved_set() {
        // Kept: closing a tab, opening a shell, scaling one, leaving fullscreen, and every
        // Ctrl+Shift chord in the table — Copy and Paste in Terminal among them.
        assert!(reserved("win.close-tab", "<Control>w"));
        assert!(reserved("win.terminal", "<Control>j"));
        assert!(reserved("win.fullscreen", "F11"));
        assert!(reserved("win.new-folder", "<Control><Shift>n"));
        assert!(reserved("win.terminal-copy", "<Control><Shift>c"));
        assert!(reserved("win.terminal-paste", "<Control><Shift>v"));
        // Moving a tab has to mean the same thing over every tab, terminals included, and
        // `Shift+Alt`+arrow has no readline meaning to cost a shell.
        assert!(reserved("win.move-tab-left", "<Shift><Alt>Left"));
        assert!(reserved("win.move-tab-right", "<Shift><Alt>Right"));
        // Every spelling of the zoom chords, or Ctrl+= would zoom the shell while Ctrl+plus went
        // to readline.
        for accel in ["<Control>plus", "<Control>equal", "<Control>KP_Add"] {
            assert!(reserved("win.zoom-in", accel));
        }
        assert!(reserved("win.zoom-out", "<Control>minus"));
        assert!(reserved("win.zoom-reset", "<Control>0"));
        // The shell's: plain Ctrl, function keys, and the chords readline reaches for most.
        for (action, accel) in [
            ("win.save", "<Control>s"),
            ("win.duplicate-line", "<Control>d"),
            ("win.toggle-comment", "<Control>k"),
            ("win.delete-line", "<Control>l"),
            ("win.palette-files", "<Control>e"),
            ("win.palette-commands", "<Control>p"),
            ("win.find-previous", "<Shift>F3"),
            ("win.menu", "F10"),
        ] {
            assert!(!reserved(action, accel), "{action} eats {accel}");
        }
    }

    /// A rebound chord follows the same rule as the default it replaced, because the filter reads
    /// the spelling in force rather than the table's.
    #[test]
    fn a_rebound_chord_follows_the_same_rule() {
        let mut config = Config::default();
        config.shortcuts.insert(
            "win.save".to_string(),
            vec!["<Control><Shift>s".to_string()],
        );
        for accel in accels_for(&config, "win.save") {
            assert!(reserved("win.save", &accel));
        }
        // The other direction: Close Tab moved off Ctrl+W is still Close Tab, and still reserved.
        config
            .shortcuts
            .insert("win.close-tab".to_string(), vec!["<Control>y".to_string()]);
        for accel in accels_for(&config, "win.close-tab") {
            assert!(reserved("win.close-tab", &accel));
        }
    }

    /// The chords DESIGN.md's never-bind list reserves, as they are spelled in an accelerator.
    /// `Super`+anything and `Ctrl+Alt`+anything are patterns rather than chords, so they are not
    /// here; nothing binds a modifier by itself.
    const NEVER_BIND: &[&str] = &[
        "<Alt>Tab",
        "<Alt>F4",
        "<Alt>F7",
        "<Alt>F8",
        "F1",
        "<Control><Shift>u",
        "<Control>space",
        "<Control>z",
        "<Control>y",
        "<Control>a",
        "<Control>x",
        "<Control>c",
        "<Control>v",
        "<Alt>Up",
        "<Alt>Down",
        "<Control>Home",
        "<Control>End",
        "<Control><Shift>Home",
        "<Control><Shift>End",
    ];

    /// `AdwTabView` binds its own chords in the capture phase, ahead of both our accelerators and
    /// the focused view's class shortcuts, so a chord it keeps is a chord nothing else can have.
    /// Neither half of that is visible in `ACTIONS`, which is why it takes a test: one direction
    /// is two controllers fighting over one chord, the other is the tab bar quietly holding a
    /// GtkSourceView built-in — how `Ctrl+Home` in a note left the note instead of going to its
    /// start.
    #[test]
    fn the_tab_bar_keeps_no_chord_that_is_ours_or_forbidden() {
        for (accel, what) in panes::widget_chords() {
            for (name, _, accels) in ACTIONS {
                assert!(
                    !accels.contains(&accel),
                    "{accel} is {name} and AdwTabView's {what}"
                );
            }
            assert!(
                !NEVER_BIND.contains(&accel),
                "AdwTabView holds {accel} for {what}, which the never-bind list reserves"
            );
        }
    }

    /// The reader's own keys are in the table so they list in the palette, and carry no
    /// accelerator there on purpose: a reader pages with `Space`, `n`, `p` and the arrows and
    /// undoes a stroke with `Ctrl+Z`, and an application accelerator is dispatched at the window
    /// ahead of whatever has the keyboard — so a bare `space` in the table would stop the editor,
    /// the terminal and every entry in the app from taking one, and `<Control>z` would take undo
    /// from every text view. The keys stay on the PDF tab's own controller, which fires these.
    #[test]
    fn the_paging_commands_are_listed_and_carry_no_accelerator() {
        for action in [
            "win.pdf-next-page",
            "win.pdf-previous-page",
            "win.pdf-undo",
            "win.pdf-redo",
            "win.diagram-undo",
            "win.diagram-redo",
            "win.diagram-delete",
            "win.diagram-select-all",
            "win.diagram-edit-label",
            "win.diagram-next-page",
            "win.diagram-previous-page",
        ] {
            let row = ACTIONS.iter().find(|(name, _, _)| *name == action);
            let (_, _, accels) = row.unwrap_or_else(|| panic!("{action} is not in ACTIONS"));
            assert!(
                accels.is_empty(),
                "{action} would be a window-wide accelerator over every typing key"
            );
        }
    }

    #[test]
    fn no_two_actions_claim_the_same_accelerator() {
        let mut seen = std::collections::HashMap::new();
        for (name, _, accels) in ACTIONS {
            for accel in *accels {
                if let Some(other) = seen.insert(*accel, *name) {
                    panic!("{accel} is bound to both {other} and {name}");
                }
            }
        }
    }
}
