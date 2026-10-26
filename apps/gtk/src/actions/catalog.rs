/// Action names, labels and accelerators shared by menus and palette so commands
/// remain discoverable. AdwTabView owns positional tab navigation; MRU cycling is
/// implemented here because libadwaita does not track usage order.
pub const ACTIONS: &[(&str, &str, &[&str])] = &[
    ("win.pane-bibliography", "Bibliography Pane", &[]),
    ("win.bib-new", "New Bibliography…", &[]),
    ("win.bib-undo", "Undo Bibliography Edit", &[]),
    ("win.bib-redo", "Redo Bibliography Edit", &[]),
    ("win.bib-add", "Add Reference…", &[]),
    ("win.bib-open", "Open Bibliography", &[]),
    ("win.bib-edit", "Edit Reference…", &[]),
    ("win.bib-check", "Check Bibliography…", &[]),
    ("win.bib-merge", "Merge Duplicate References…", &[]),
    ("win.bib-refresh", "Refresh Reference Metadata…", &[]),
    ("win.bib-key", "Generate Citation Key…", &[]),
    ("win.bib-copy-keys", "Copy Citation Keys", &[]),
    ("win.bib-cite", "Insert Citation", &[]),
    ("win.bib-usages", "Find Citation Usages…", &[]),
    ("win.bib-pdf", "Open Attached PDF", &[]),
    ("win.bib-attach", "Attach PDF…", &[]),
    ("win.bib-download", "Download PDF…", &[]),
    ("win.bib-copy", "Copy to Bibliography…", &[]),
    ("win.bib-move", "Move to Bibliography…", &[]),
    ("win.bib-remove", "Remove References…", &[]),
    ("win.save", "Save", &["<Primary>s"]),
    ("win.save-as", "Save As…", &["<Primary><Shift>s"]),
    ("win.open-file", "Open File…", &["<Primary>o"]),
    ("win.new-file", "New File", &["<Primary>n"]),
    ("win.new-folder", "New Folder", &["<Primary><Shift>n"]),
    ("win.new-drawing", "New Drawing", &[]),
    ("win.upload", "Upload Files…", &[]),
    // No chords: `Ctrl+P`, the HIG's Print, is the palette (DESIGN.md, Deviations).
    ("win.print", "Print…", &[]),
    ("win.export-pdf", "Export as PDF…", &[]),
    ("win.export-html", "Export as HTML…", &[]),
    ("win.export-png", "Export as PNG…", &[]),
    ("win.export-svg", "Export as SVG…", &[]),
    ("win.close-tab", "Close Tab", &["<Primary>w"]),
    ("win.close-pane-tabs", "Close Tabs in Pane", &[]),
    // Most-recently-used order, so one press is the note before this one. Both spellings of the
    // backwards chord, because X11 delivers Shift+Tab as `ISO_Left_Tab` and which of the two a
    // GTK trigger matches is a question of the keymap rather than of the table.
    ("win.next-tab", "Next Tab", &["<Primary>Tab"]),
    (
        "win.previous-tab",
        "Previous Tab",
        &["<Primary><Shift>Tab", "<Primary><Shift>ISO_Left_Tab"],
    ),
    ("win.terminal", "New Terminal", &["<Primary>j"]),
    ("win.terminal-remote", "New Remote Terminal…", &[]),
    // A shell here from a remote vault's window, where New Terminal opens one on the host.
    ("win.terminal-local", "New Local Terminal", &[]),
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
    ("win.terminal-share", "Share with MCP", &[]),
    ("win.terminal-revoke", "Revoke MCP Access", &[]),
    // A paste that never makes a link of a URL over a selection. Unbound: `Ctrl+Shift+V` is
    // Paste in Terminal's chord, which a text tab in front answers with this.
    ("win.paste-plain", "Paste as Plain Text", &[]),
    // Split Right takes VS Code's chord; the other three are menu and palette only, because
    // three more accelerators for the same idea is three more chords nobody has to spare.
    ("win.split-right", "Split Right", &["<Primary>backslash"]),
    ("win.split-left", "Split Left", &[]),
    ("win.split-up", "Split Up", &[]),
    ("win.split-down", "Split Down", &[]),
    // Move the tab into the pane that way, splitting one off only when there is none. Left and
    // right alone carry chords: `Shift+Alt+Up` / `Shift+Alt+Down` are the multi-caret pair, and
    // every plainer arrow chord is spoken for — `Alt+Left` / `Alt+Right` are Back and Forward,
    // and `Alt+Up` / `Alt+Down` and `Ctrl+Alt`+arrow are on DESIGN.md's never-bind list.
    ("win.move-tab-left", "Move Tab Left", &["<Shift><Alt>Left"]),
    (
        "win.move-tab-right",
        "Move Tab Right",
        &["<Shift><Alt>Right"],
    ),
    ("win.move-tab-up", "Move Tab Up", &[]),
    ("win.move-tab-down", "Move Tab Down", &[]),
    // Out of the window altogether, into one of its own with no vault (`Shell::move_apart`).
    ("win.move-to-new-window", "Move to New Window", &[]),
    // Held at the start of its pane's bar, keeping its title and its close button. Two actions
    // rather than one toggle, so the menu names what it will do and the label still comes from
    // this table.
    ("win.pin-tab", "Pin Tab", &[]),
    ("win.unpin-tab", "Unpin Tab", &[]),
    // The divider of the nearest split around the active pane that runs that way, one step of a
    // grid anchored at the centre (`panes::divider_step`). No chord (user decision 2026-10-13):
    // the obvious four, `Ctrl+Alt`+arrows, are GNOME's workspace switching, which a stock desktop
    // takes before the window sees them; the palette runs them, and `[shortcuts]` binds them.
    ("win.divider-left", "Move Divider Left", &[]),
    ("win.divider-right", "Move Divider Right", &[]),
    ("win.divider-up", "Move Divider Up", &[]),
    ("win.divider-down", "Move Divider Down", &[]),
    ("app.new-window", "New Window", &[]),
    ("app.open-local", "Open Local…", &["<Primary><Shift>o"]),
    ("app.open-remote", "Open Remote…", &[]),
    ("win.open-recent", "Open Recent…", &["<Primary>r"]),
    ("app.close-vault", "Close Vault", &[]),
    ("win.reload-window", "Reload Window", &[]),
    // Save's session half, for the primary menu of a window of shells: a focused shell keeps
    // `Ctrl+S`, and Save over a note in front saves the note.
    ("win.save-session", "Save Session", &[]),
    ("win.close-session", "Close Session", &[]),
    ("app.quit", "Quit", &["<Primary>q"]),
    ("win.palette-files", "Go to File…", &["<Primary>e"]),
    (
        "win.palette-commands",
        "Run a Command…",
        &["<Primary>p", "<Primary><Shift>p"],
    ),
    // The palette's `@` and `#` modes, for a reader who would rather bind them than type them.
    ("win.palette-symbols", "Go to Symbol…", &[]),
    ("win.palette-tags", "Filter by Tag…", &[]),
    ("win.find", "Find", &["<Primary>f"]),
    ("win.replace", "Replace", &["<Primary>h"]),
    (
        "win.replace-in-files",
        "Replace in Files",
        &["<Primary><Shift>h"],
    ),
    ("win.find-next", "Find Next", &["F3"]),
    ("win.find-previous", "Find Previous", &["<Shift>F3"]),
    ("win.goto-line", "Go to Line", &["<Primary>g"]),
    ("win.duplicate-line", "Duplicate Line", &["<Primary>d"]),
    ("win.delete-line", "Delete Line", &["<Primary>l"]),
    (
        "win.newline-below",
        "Insert Line Below",
        &["<Primary>Return"],
    ),
    ("win.toggle-comment", "Toggle Comment", &["<Primary>k"]),
    // Notes only. `Ctrl+L` is Delete Line, `Ctrl+Shift+L` Select All Occurrences.
    ("win.toggle-task", "Toggle Task", &["<Alt>x"]),
    ("win.toggle-wrap", "Toggle Word Wrap", &["<Alt>z"]),
    ("win.scroll-up", "Scroll Up", &["<Primary>Up"]),
    ("win.scroll-down", "Scroll Down", &["<Primary>Down"]),
    ("win.caret-above", "Add Caret Above", &["<Shift><Alt>Up"]),
    ("win.caret-below", "Add Caret Below", &["<Shift><Alt>Down"]),
    // JetBrains' chord: `Ctrl+D`, VS Code's, is Duplicate Line here.
    (
        "win.next-occurrence",
        "Add Caret at Next Occurrence",
        &["<Alt>j"],
    ),
    // VS Code's chord, which moved Outline Pane to `Ctrl+Shift+W`.
    (
        "win.all-occurrences",
        "Select All Occurrences",
        &["<Primary><Shift>l"],
    ),
    (
        "win.zoom-in",
        "Zoom In",
        &["<Primary>plus", "<Primary>equal", "<Primary>KP_Add"],
    ),
    (
        "win.zoom-out",
        "Zoom Out",
        &["<Primary>minus", "<Primary>KP_Subtract"],
    ),
    (
        "win.zoom-reset",
        "Reset Zoom",
        &["<Primary>0", "<Primary>KP_0"],
    ),
    ("win.sidebar", "Toggle Sidebar", &["F9"]),
    ("win.pane-files", "Files Pane", &["<Primary><Shift>e"]),
    ("win.pane-search", "Search Pane", &["<Primary><Shift>f"]),
    // No chord: it is the Search pane's own All button, and the palette is how a command with
    // no chord is found.
    ("win.search-all", "Search Ignored Files", &[]),
    ("win.pane-info", "Info Pane", &["<Primary><Shift>b"]),
    // No chord: the Info pane's own section, found by name.
    ("win.file-details", "Show File Details", &[]),
    ("win.pane-git", "Git Pane", &["<Primary><Shift>g"]),
    ("win.git-sync", "Sync", &[]),
    ("win.git-sync-all", "Sync All Repositories", &[]),
    ("win.git-merge", "Merge Branch…", &[]),
    ("win.git-merge-abort", "Abort Merge", &[]),
    ("win.git-delete-branch", "Delete Branch…", &[]),
    // The palette's alone: a vault without a repository has no Git pane to put a button in.
    ("win.git-init", "Initialize Git Repository", &[]),
    // The conflict block holding the caret (`conflict.rs`), unbound as VS Code leaves them: the
    // buttons over each block are the pointer's way.
    ("win.conflict-current", "Accept Current Change", &[]),
    ("win.conflict-incoming", "Accept Incoming Change", &[]),
    ("win.conflict-both", "Accept Both Changes", &[]),
    (
        "win.conflict-all-current",
        "Accept All Current Changes",
        &[],
    ),
    (
        "win.conflict-all-incoming",
        "Accept All Incoming Changes",
        &[],
    ),
    ("win.merge-resolved", "Mark Resolved", &[]),
    // The hunk holding the caret in a comparison, as unbound: the buttons on each hunk's band
    // are the pointer's way.
    ("win.hunk-stage", "Stage Hunk", &[]),
    ("win.hunk-revert", "Revert Hunk", &[]),
    ("win.hunk-take", "Take Hunk", &[]),
    ("win.hunk-both", "Keep Both Versions", &[]),
    ("win.conflict-next", "Next Conflict", &[]),
    ("win.conflict-previous", "Previous Conflict", &[]),
    ("win.pane-outline", "Outline Pane", &["<Primary><Shift>w"]),
    (
        "win.pane-properties",
        "Properties Pane",
        &["<Primary><Shift>a"],
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
    ("win.image-invert", "Invert Image Colours", &[]),
    ("win.pdf-copy", "Copy Selection", &[]),
    // No accelerator here for the same reason the paging commands have none: `Ctrl+Z` over a
    // note is GtkSourceView's own undo, and an application accelerator would take it from every
    // text view in the window. The PDF tab's key controller fires these two.
    ("win.pdf-undo", "Undo PDF Edit", &[]),
    ("win.pdf-redo", "Redo PDF Edit", &[]),
    ("win.pdf-copy-link", "Copy Link to Selection", &[]),
    ("win.pdf-export-highlights", "Export Highlights to PDF", &[]),
    // SyncTeX, both ways between a LaTeX build's PDF and its sources (`synctex.rs`); Ctrl+click
    // on a page is Go to Source too, as in every LaTeX viewer.
    ("win.pdf-go-to-source", "Go to Source", &[]),
    ("win.show-in-pdf", "Show in PDF", &[]),
    ("win.pdf-draw", "Drawing", &["<Primary><Shift>i"]),
    ("win.pdf-pen", "Pen", &[]),
    ("win.pdf-highlighter", "Highlighter", &[]),
    ("win.pdf-eraser", "Eraser", &[]),
    ("win.pdf-line", "Line", &[]),
    ("win.pdf-rect", "Rectangle", &[]),
    ("win.pdf-circle", "Circle", &[]),
    ("win.pdf-adjust", "Adjust", &[]),
    ("win.insert-sketch", "Insert Sketch", &[]),
    // A page put in before or after, taken out, or moved from the one being read. The thumbnail
    // strip does the same by pointer, on any page.
    ("win.pdf-add-page-before", "Add Page Before", &[]),
    ("win.pdf-add-page-after", "Add Page After", &[]),
    ("win.pdf-delete-page", "Delete Page", &[]),
    ("win.pdf-move-page-up", "Move Page Up", &[]),
    ("win.pdf-move-page-down", "Move Page Down", &[]),
    // The diagram tab. Undo, Redo, Delete Selection, Select All Shapes, Edit Label and the
    // paging commands are the canvas's own keys, which fire these, and carry no chord for the
    // PDF's reason: `Ctrl+Z`, `Ctrl+A`, `Delete` and `Return` belong to whatever has the
    // keyboard. Duplicate Selection is `Ctrl+D`, which is Duplicate Line's chord, and Group is
    // `Ctrl+G`, Go to Line's, which `run_action` hands to a diagram in front.
    ("win.diagram-undo", "Undo Diagram Edit", &[]),
    ("win.diagram-redo", "Redo Diagram Edit", &[]),
    ("win.diagram-delete", "Delete Selection", &[]),
    ("win.diagram-delete-all", "Delete with Edges", &[]),
    ("win.diagram-duplicate", "Duplicate Selection", &[]),
    ("win.diagram-cut", "Cut Shapes", &[]),
    ("win.diagram-copy", "Copy Shapes", &[]),
    ("win.diagram-paste", "Paste into Diagram", &[]),
    ("win.diagram-group", "Group", &[]),
    ("win.diagram-ungroup", "Ungroup", &[]),
    ("win.diagram-select-all", "Select All Shapes", &[]),
    ("win.diagram-edit-label", "Edit Label", &[]),
    ("win.diagram-next-page", "Next Diagram Page", &[]),
    ("win.diagram-previous-page", "Previous Diagram Page", &[]),
    ("win.diagram-add-page", "Add Diagram Page", &[]),
    ("win.diagram-rename-page", "Rename Diagram Page…", &[]),
    ("win.diagram-delete-page", "Delete Diagram Page", &[]),
    ("win.diagram-move-page-up", "Move Diagram Page Up", &[]),
    ("win.diagram-move-page-down", "Move Diagram Page Down", &[]),
    ("win.diagram-to-front", "Bring to Front", &[]),
    ("win.diagram-to-back", "Send to Back", &[]),
    ("win.diagram-invert", "Invert Diagram Colours", &[]),
    ("win.diagram-grid", "Show Grid", &[]),
    ("win.diagram-select", "Select and Move", &[]),
    ("win.diagram-rect", "Add Rectangle", &[]),
    ("win.diagram-ellipse", "Add Ellipse", &[]),
    ("win.diagram-text", "Add Text", &[]),
    ("win.diagram-connector", "Add Connector", &[]),
    ("win.diagram-image", "Add Image…", &[]),
    // The notebook tab. Undo and Redo are its own keys, as a diagram's canvas's are, and carry no
    // chord for the same reason; so are the cell chords (`notebook::own_key`), which a cell's
    // editor would take first. The cell commands act on the selected cell, and its buttons and
    // its More menu are the pointer's way to them.
    ("win.notebook-undo", "Undo Notebook Edit", &[]),
    ("win.notebook-redo", "Redo Notebook Edit", &[]),
    ("win.notebook-insert-above", "Insert Cell Above", &[]),
    ("win.notebook-insert-below", "Insert Cell Below", &[]),
    (
        "win.notebook-insert-markdown",
        "Insert Markdown Cell Below",
        &[],
    ),
    ("win.notebook-delete-cell", "Delete Cell", &[]),
    ("win.notebook-move-up", "Move Cell Up", &[]),
    ("win.notebook-move-down", "Move Cell Down", &[]),
    ("win.notebook-to-code", "Change Cell to Code", &[]),
    ("win.notebook-to-markdown", "Change Cell to Markdown", &[]),
    ("win.notebook-to-raw", "Change Cell to Raw", &[]),
    ("win.notebook-split", "Split Cell", &[]),
    ("win.notebook-join", "Join with Next Cell", &[]),
    // A notebook's kernel (`notebook::kernel`): Run Cell answers Insert Line Below's chord in a
    // notebook, and Run Cell and Advance is its own key.
    ("win.notebook-run-cell", "Run Cell", &[]),
    ("win.notebook-run-advance", "Run Cell and Advance", &[]),
    ("win.notebook-run-all", "Run All", &[]),
    ("win.notebook-interrupt", "Interrupt Kernel", &[]),
    ("win.notebook-restart", "Restart Kernel", &[]),
    ("win.notebook-shut-down", "Shut Down Kernel", &[]),
    ("win.notebook-clear-outputs", "Clear Outputs", &[]),
    ("win.notebook-clear-all-outputs", "Clear All Outputs", &[]),
    ("win.notebook-open-text", "Open as Text", &[]),
    ("win.python-environment", "Select Python Environment", &[]),
    ("win.view-mode", "Toggle Preview", &["<Primary>m"]),
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
        &["<Primary><Shift>Return", "F12"],
    ),
    // `Ctrl+.`, VS Code's, is GtkTextView's emoji chooser (user decision 2026-10-22).
    ("win.quick-fix", "Quick Fix…", &["<Alt>Return"]),
    ("win.source-action", "Source Action…", &[]),
    // GTK reports Shift+[ and Shift+] as brace keysyms on a US keyboard.
    ("win.fold", "Fold", &["<Primary>braceleft"]),
    ("win.unfold", "Unfold", &["<Primary>braceright"]),
    ("win.fold-all", "Fold All", &[]),
    ("win.unfold-all", "Unfold All", &[]),
    (
        "win.lsp-restart-file",
        "Restart File in Language Server",
        &[],
    ),
    // VS Code's F9 is the sidebar's here.
    (
        "win.debug-toggle-breakpoint",
        "Toggle Breakpoint",
        &["<Primary>F9"],
    ),
    (
        "win.debug-remove-breakpoints",
        "Remove All Breakpoints",
        &[],
    ),
    ("win.debug-watch", "Add to Watch", &[]),
    // VS Code's F5 is Presentation Mode's outside a session here.
    ("win.debug-start", "Start Debugging", &["<Primary>F5"]),
    // Enabled as the session's state allows (`App::sync_debug`), so the Code pane's strip greys
    // what cannot be done now and the palette's entries do nothing. VS Code's keys, heard only
    // while a session runs ([`SESSION`]).
    ("win.debug-continue", "Continue", &["F5"]),
    ("win.debug-pause", "Pause", &["F6"]),
    ("win.debug-step-over", "Step Over", &["F10"]),
    ("win.debug-step-into", "Step Into", &["F11"]),
    ("win.debug-step-out", "Step Out", &["<Shift>F11"]),
    (
        "win.debug-restart",
        "Restart Debugging",
        &["<Primary><Shift>F5"],
    ),
    ("win.debug-stop", "Stop Debugging", &["<Shift>F5"]),
    ("win.debug-console", "Debug Console", &[]),
    ("win.rename", "Rename", &["F2"]),
    // No chord: Delete over a tree row already trashes, from the tree's own key controller, and
    // a chord that reaches a focused editor is the last thing a delete should have.
    ("win.trash", "Move to Trash", &[]),
    (
        "win.new-from-template",
        "New from Template…",
        &["<Primary><Shift>d"],
    ),
    ("win.insert-template", "Insert Template…", &[]),
    ("win.present", "Presentation Mode", &["F5"]),
    ("win.fullscreen", "Fullscreen", &["F11"]),
    ("win.preferences", "Preferences", &["<Primary>comma"]),
    // GNOME's chord for it (`Ctrl+?`); `F1`, help, is on the never-bind list.
    (
        "win.shortcuts",
        "Keyboard Shortcuts",
        &["<Primary>question"],
    ),
    ("win.menu", "Primary Menu", &["F10"]),
    ("win.about", "About accent", &[]),
];

#[cfg(test)]
mod tests {
    use super::super::*;

    #[test]
    fn accels_for_prefers_the_override() {
        let mut config = Config::default();
        assert_eq!(accels_for(&config, "win.save"), ["<Primary>s"]);
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

    /// `Ctrl+Alt` with anything is GNOME's (workspace switching, the never-bind list), which a stock
    /// desktop takes before the window sees it: no default may need it rebound to arrive.
    #[test]
    fn no_default_chord_is_the_desktops() {
        for (action, _, accels) in ACTIONS {
            for accel in *accels {
                assert!(
                    !(accel.contains("<Control>") && accel.contains("<Alt>")),
                    "{action} defaults to {accel}"
                );
            }
        }
    }

    /// A misspelt name in [`EDITING`] would leave that chord bound everywhere.
    #[test]
    fn editing_actions_are_in_the_action_table() {
        for action in EDITING {
            assert!(
                ACTIONS.iter().any(|(name, _, _)| name == action),
                "{action} is an editing chord but not in ACTIONS"
            );
        }
    }

    /// Every action the capture controllers claim has to be in the table they read their chords
    /// from, or a rebind would silently drop it; a session's too, and those it shadows.
    #[test]
    fn captured_actions_are_in_the_action_table() {
        for action in CAPTURED.iter().chain(SESSION).chain(SHADOWED) {
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
        for action in [
            "win.rename",
            "win.trash",
            "win.move-to-new-window",
            "win.pin-tab",
            "win.unpin-tab",
            "win.print",
            "win.export-pdf",
            "win.export-html",
            "win.export-png",
            "win.export-svg",
            "win.diagram-grid",
            "win.notebook-open-text",
        ] {
            assert!(
                ACTIONS.iter().any(|(name, _, _)| *name == action),
                "{action} is on the tab menu but not in ACTIONS"
            );
        }
    }

    /// Move to New Window ends the moves before the pin, on a tab another window can open: a
    /// shell, a comparison and a remote vault's file cannot go.
    #[test]
    fn only_a_tab_that_can_move_offers_a_new_window() {
        let moves = |movable| {
            let menu = gio::Menu::new();
            fill_tab_menu(&menu, false, movable, false, None);
            let section = menu.item_link(1, "section").expect("the move section");
            menu_actions(&section)[4..].to_vec()
        };
        assert_eq!(moves(true), ["win.move-to-new-window", "win.pin-tab"]);
        assert_eq!(moves(false), ["win.pin-tab"]);
    }

    #[test]
    fn the_primary_menu_offers_only_what_the_window_can_do() {
        use crate::shell::{Loose, WindowKey};
        let vault = primary_actions(&WindowKey::Vault("/home/me/Notes".into()));
        // Save acts on the tab in front, so it is not here.
        assert_eq!(
            vault[0],
            ["win.new-file", "win.new-folder", "win.open-file"]
        );
        assert_eq!(
            vault[1],
            [
                "app.open-local",
                "app.open-remote",
                "win.open-recent",
                "win.reload-window",
                "app.close-vault"
            ]
        );
        // Without a vault there is nothing to make a file in and no vault to close. A window of
        // shells saves its session, named or not, and a named session is closed as one.
        let named = primary_actions(&WindowKey::Terminal("terminal://dev".into()));
        let unnamed = primary_actions(&WindowKey::Loose(Loose::Terminal));
        for menu in [&named, &unnamed] {
            assert_eq!(menu[0], ["win.open-file"]);
            assert_eq!(menu[2..], vault[2..]);
        }
        assert_eq!(
            named[1],
            [
                "app.open-local",
                "app.open-remote",
                "win.open-recent",
                "win.reload-window",
                "win.save-session",
                "win.close-session"
            ]
        );
        assert_eq!(
            unnamed[1],
            [
                "app.open-local",
                "app.open-remote",
                "win.open-recent",
                "win.reload-window",
                "win.save-session"
            ]
        );
        let documents = primary_actions(&WindowKey::Loose(Loose::Documents));
        assert_eq!(documents[0], ["win.open-file"]);
        assert_eq!(
            documents[1],
            [
                "app.open-local",
                "app.open-remote",
                "win.open-recent",
                "win.reload-window"
            ]
        );
        // New Terminal in a remote vault's window opens a shell on the host, so there a shell on
        // this machine is an item of its own.
        let remote = primary_actions(&WindowKey::Vault("ssh://box/srv/vault".into()));
        // Find and Toggle Preview act on the document, not the window.
        assert_eq!(
            vault[2],
            ["win.terminal", "win.terminal-remote", "win.present"]
        );
        assert_eq!(
            remote[2],
            [
                "win.terminal",
                "win.terminal-remote",
                "win.terminal-local",
                "win.present"
            ]
        );
        // Every item is a command the window has, which is where its label comes from.
        for menu in [vault, documents, remote, named, unnamed] {
            for action in menu.iter().flatten() {
                assert!(
                    ACTIONS.iter().any(|(name, _, _)| name == action),
                    "{action}"
                );
            }
        }
        assert!(
            ACTIONS
                .iter()
                .any(|(name, _, _)| *name == "win.close-session")
        );
    }

    /// The two items a shell or a comparison must not be offered, and that a pane built before
    /// any page exists does not carry either.
    #[test]
    fn the_tab_menu_names_a_file_only_where_there_is_one() {
        let sections = |file| {
            let menu = gio::Menu::new();
            fill_tab_menu(&menu, file, false, false, None);
            menu.n_items()
        };
        assert_eq!(sections(true), sections(false) + 2);
        assert_eq!(tab_menu().n_items(), sections(false));
    }

    /// Pin Tab or Unpin Tab, whichever the page is not yet, closing the section that moves it.
    #[test]
    fn the_tab_menu_offers_the_pin_the_page_does_not_have() {
        let last_move = |pinned| {
            let menu = gio::Menu::new();
            fill_tab_menu(&menu, false, false, pinned, None);
            let moves = menu.item_link(1, "section").expect("the move section");
            let last = moves.n_items() - 1;
            moves
                .item_attribute_value(last, "action", None)
                .and_then(|action| action.get::<String>())
        };
        assert_eq!(last_move(false).as_deref(), Some("win.pin-tab"));
        assert_eq!(last_move(true).as_deref(), Some("win.unpin-tab"));
    }

    /// Every action a menu names, its sections' and submenus' included, in the order it shows
    /// them.
    fn menu_actions(menu: &gio::MenuModel) -> Vec<String> {
        (0..menu.n_items())
            .flat_map(
                |i| match (menu.item_link(i, "section")).or(menu.item_link(i, "submenu")) {
                    Some(section) => menu_actions(&section),
                    None => menu
                        .item_attribute_value(i, "action", None)
                        .and_then(|action| action.get::<String>())
                        .into_iter()
                        .collect(),
                },
            )
            .collect()
    }

    /// Print… and every export follow the files wherever a document can be in front; a window of
    /// shells has none of them.
    #[test]
    fn the_primary_menu_prints_where_a_document_can_be() {
        use crate::shell::{Loose, WindowKey};
        let actions = |key| menu_actions(primary_menu(&key).upcast_ref());
        for key in [
            WindowKey::Vault("/home/me/Notes".into()),
            WindowKey::Loose(Loose::Documents),
        ] {
            let menu = actions(key);
            let at = menu
                .iter()
                .position(|a| a == "win.open-file")
                .expect("Open File…");
            assert_eq!(menu[at + 1..at + 6], PRINTS);
        }
        for key in [
            WindowKey::Loose(Loose::Terminal),
            WindowKey::Terminal("terminal://dev".into()),
        ] {
            assert!(!actions(key).iter().any(|a| PRINTS.contains(&a.as_str())));
        }
    }

    /// A note is printed and exported three ways, a PDF two and a diagram four, right after Reveal
    /// in Sidebar, a diagram's Show Grid after them; a shell, a comparison or any other tab is
    /// offered none.
    #[test]
    fn the_tab_menu_prints_what_can_be_printed() {
        let after_reveal = |prints| {
            let menu = gio::Menu::new();
            fill_tab_menu(&menu, true, false, false, prints);
            let actions = menu_actions(menu.upcast_ref());
            let at = actions
                .iter()
                .position(|a| a == "win.reveal-in-sidebar")
                .expect("Reveal in Sidebar");
            actions[at + 1..].to_vec()
        };
        let [print, pdf, html, rename, trash] = [
            "win.print",
            "win.export-pdf",
            "win.export-html",
            "win.rename",
            "win.trash",
        ];
        assert_eq!(
            after_reveal(Some(Kind::Note)),
            [print, pdf, html, rename, trash]
        );
        assert_eq!(after_reveal(Some(Kind::Pdf)), [print, pdf, rename, trash]);
        let (png, svg, grid) = ("win.export-png", "win.export-svg", "win.diagram-grid");
        assert_eq!(
            after_reveal(Some(Kind::Diagram)),
            [print, pdf, png, svg, grid, rename, trash]
        );
        assert_eq!(after_reveal(None), [rename, trash]);
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
        assert!(reserved("win.close-tab", "<Primary>w"));
        assert!(reserved("win.terminal", "<Primary>j"));
        assert!(reserved("win.fullscreen", "F11"));
        assert!(reserved("win.new-folder", "<Primary><Shift>n"));
        assert!(reserved("win.terminal-copy", "<Control><Shift>c"));
        assert!(reserved("win.terminal-paste", "<Control><Shift>v"));
        // Moving a tab has to mean the same thing over every tab, terminals included, and
        // `Shift+Alt`+arrow has no readline meaning to cost a shell.
        assert!(reserved("win.move-tab-left", "<Shift><Alt>Left"));
        assert!(reserved("win.move-tab-right", "<Shift><Alt>Right"));
        // So does moving a divider, which is the pane's and not the tab's.
        assert!(reserved("win.divider-left", "<Primary><Alt>Left"));
        assert!(reserved("win.divider-down", "<Primary><Alt>Down"));
        // Every spelling of the zoom chords, or Ctrl+= would zoom the shell while Ctrl+plus went
        // to readline.
        for accel in ["<Primary>plus", "<Primary>equal", "<Primary>KP_Add"] {
            assert!(reserved("win.zoom-in", accel));
        }
        assert!(reserved("win.zoom-out", "<Primary>minus"));
        assert!(reserved("win.zoom-reset", "<Primary>0"));
        // Go to File, so a file is one chord away from a shell as it is from a note.
        assert!(reserved("win.palette-files", "<Primary>e"));
        assert!(reserved("win.fold", "<Primary>braceleft"));
        assert!(reserved("win.unfold", "<Primary>braceright"));
        // The shell's: plain Ctrl, function keys, and the chords readline reaches for most.
        for (action, accel) in [
            ("win.save", "<Primary>s"),
            ("win.duplicate-line", "<Primary>d"),
            ("win.next-occurrence", "<Alt>j"),
            ("win.toggle-comment", "<Primary>k"),
            ("win.delete-line", "<Primary>l"),
            ("win.palette-commands", "<Primary>p"),
            ("win.find-previous", "<Shift>F3"),
            ("win.menu", "F10"),
        ] {
            assert!(!reserved(action, accel), "{action} eats {accel}");
        }
    }

    /// Forwarding every key leaves the window Copy and Paste in Terminal alone: the reserved set
    /// goes to the shell with the rest of the table.
    #[test]
    fn a_shell_given_every_key_keeps_only_copy_and_paste() {
        assert!(kept("win.terminal-copy", "<Control><Shift>c", true));
        assert!(kept("win.terminal-paste", "<Control><Shift>v", true));
        for (action, accel) in [
            ("win.close-tab", "<Primary>w"),
            ("win.next-tab", "<Primary>Tab"),
            ("win.palette-files", "<Primary>e"),
            ("win.palette-commands", "<Primary><Shift>p"),
            ("win.fullscreen", "F11"),
        ] {
            assert!(!kept(action, accel, true), "{action} eats {accel}");
            assert!(
                kept(action, accel, false),
                "{action} is reserved when capturing"
            );
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

    /// Ensure AdwTabView capture shortcuts do not overlap actions or editor built-ins.
    ///
    /// Its shortcuts are absent from `ACTIONS` and run before focused widgets, so overlaps
    /// would silently replace document navigation or compete with application actions.
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

    /// Keep PDF reader commands in the palette without application accelerators.
    ///
    /// Their tab-local controller owns Space, arrows and Undo. Global bindings would intercept
    /// normal typing and text Undo before the focused widget.
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
            "win.diagram-delete-all",
            "win.diagram-select-all",
            "win.diagram-copy",
            "win.diagram-cut",
            "win.diagram-paste",
            "win.diagram-edit-label",
            "win.diagram-next-page",
            "win.diagram-previous-page",
            "win.notebook-undo",
            "win.notebook-redo",
            "win.notebook-insert-above",
            "win.notebook-insert-below",
            "win.notebook-move-up",
            "win.notebook-move-down",
            "win.notebook-split",
            "win.notebook-run-advance",
        ] {
            let row = ACTIONS.iter().find(|(name, _, _)| *name == action);
            let (_, _, accels) = row.unwrap_or_else(|| panic!("{action} is not in ACTIONS"));
            assert!(
                accels.is_empty(),
                "{action} would be a window-wide accelerator over every typing key"
            );
        }
    }

    /// A session's chord and the one it shadows meanwhile are the only two a chord may have.
    #[test]
    fn no_two_actions_claim_the_same_accelerator() {
        let mut seen = std::collections::HashMap::<&str, Vec<&str>>::new();
        for (name, _, accels) in ACTIONS {
            for accel in *accels {
                let holders = seen.entry(*accel).or_default();
                if let Some(other) = holders.iter().find(|other| !may_share(other, name)) {
                    panic!("{accel} is bound to both {other} and {name}");
                }
                holders.push(name);
            }
        }
    }

    /// While a session runs its chords are the window's to hear, never the table's, and
    /// Presentation, Primary Menu and Fullscreen give up the ones they share; nothing else does.
    #[test]
    fn a_running_session_takes_f5_from_presentation_alone() {
        let f5 = ["F5".to_string()];
        assert!(!installed("win.present", "F5", Keys::Tab, false, &f5));
        assert!(installed("win.present", "F5", Keys::Tab, false, &[]));
        for keys in [Keys::Tab, Keys::Shell, Keys::Elsewhere] {
            for shadowed in [&f5[..], &[]] {
                let continues = installed("win.debug-continue", "F5", keys, false, shadowed);
                assert!(!continues, "{keys:?}");
            }
        }
        assert!(installed(
            "win.find-next",
            "F3",
            Keys::Tab,
            false,
            &["F3".to_string()]
        ));
    }
}
