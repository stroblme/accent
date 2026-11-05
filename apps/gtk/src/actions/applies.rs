//! Which commands apply to the window as it stands. Run Command lists only those; every other
//! action is disabled, so a menu greys it and its chord goes on to the focused widget
//! (DESIGN.md, Palette).

use super::{PRINTS, print_actions};
use crate::doc::Kind;

/// The document the commands act on: the tab the tab menu was opened on, else the tab in front.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Front {
    /// A markdown or org note, and which: Insert Sketch writes markdown's embed.
    Note {
        markdown: bool,
    },
    /// Any other text: source, a table, a bibliography.
    Code,
    Pdf,
    Image,
    Diagram,
    Notebook,
    Terminal,
    /// A comparison of two texts that are not files.
    Diff,
    /// A note's rendered preview tab.
    Preview,
    /// A file shown as a status page, one that cannot be opened.
    Status,
}

/// Where the front's file is: in this window's vault, or outside every vault.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum File {
    Vault,
    Loose,
}

/// What decides whether a command applies, read off the window by `App::applies_context`.
#[derive(Clone, Default, Debug)]
pub struct Context {
    pub front: Option<Front>,
    /// `None` for no tab, a shell, a comparison and a preview: none of them is a file.
    pub file: Option<File>,
    /// `Some(remote)` in a vault's window.
    pub vault: Option<bool>,
    /// A window of shells, and one saved under a name.
    pub shells: bool,
    pub session: bool,
    pub pinned: bool,
    /// Another window could open what the tab holds (`shell::movable`).
    pub movable: bool,
    pub panes: usize,
    /// The sidebar's panes this window has, by name.
    pub sidebar: Vec<&'static str>,
    /// The Git pane knows a repository, and its selected one is part way through a merge.
    pub repository: bool,
    pub merging: bool,
    /// The text tab in front holds conflict markers, a comparison or a merge.
    pub conflicts: bool,
    pub comparing: bool,
    pub merge: bool,
    /// The Code pane holds a notebook, whose kernel strip acts with another tab in front.
    pub kernel: bool,
    /// A bibliography is selected in its pane or in front.
    pub bibliography: bool,
    /// Start Debugging has a program to run and no session runs.
    pub debuggable: bool,
    /// The text tab in front is in a language a debugger runs, and breakpoints are set anywhere.
    pub breakable: bool,
    pub breakpoints: bool,
    /// The Code pane offers a Python environment to choose.
    pub python: bool,
    /// The shell in front can be shared with MCP: `Some(shared)`.
    pub shared: Option<bool>,
    /// The tab is a terminal editor's.
    pub editor: bool,
}

/// Actions another sync enables, on state of its own that changes with no tab change: SyncTeX's
/// pair as a build is found (`App::sync_synctex`), Export Highlights as the index answers
/// (`App::sync_export`), Invert Image Colours once the image is read (`App::sync_image_invert`),
/// a diagram's insert tools as its layers lock (`App::sync_diagram_tools`), the debug session's
/// controls (`App::sync_debug`), the kernel's (`App::sync_kernel`) and Recolour Selection as
/// Select's hold changes (`App::sync_recolour`). [`applies`] leaves them to those, which follow
/// the tab in front themselves.
pub const OWN_SYNC: [&str; 25] = [
    "win.pdf-go-to-source",
    "win.show-in-pdf",
    "win.pdf-export-highlights",
    "win.pdf-recolour-1",
    "win.pdf-recolour-2",
    "win.pdf-recolour-3",
    "win.pdf-recolour-4",
    "win.pdf-recolour-5",
    "win.pdf-recolour-6",
    "win.image-invert",
    "win.diagram-rect",
    "win.diagram-ellipse",
    "win.diagram-text",
    "win.diagram-connector",
    "win.diagram-image",
    "win.debug-continue",
    "win.debug-pause",
    "win.debug-step-over",
    "win.debug-step-into",
    "win.debug-step-out",
    "win.debug-restart",
    "win.debug-stop",
    "win.notebook-interrupt",
    "win.notebook-restart",
    "win.notebook-shut-down",
];

/// Whether `action` can do something in `cx`. Anything not named applies everywhere: the
/// window's own commands, the panes' toggles, Back and Forward (whose history changes with no
/// event to follow) and the [`OWN_SYNC`] ones.
pub fn applies(action: &str, cx: &Context) -> bool {
    use Front::*;
    let front = cx.front;
    let note = matches!(front, Some(Note { .. }));
    let markdown = front == Some(Note { markdown: true });
    let code = front == Some(Code);
    let text = note || code;
    let (pdf, diagram) = (front == Some(Pdf), front == Some(Diagram));
    let notebook = front == Some(Notebook);
    let preview = front == Some(Preview);
    let pane = |name| cx.sidebar.contains(&name);
    let vault = cx.vault.is_some();
    let remote = cx.vault == Some(true);
    let printable = match front {
        Some(Note { .. } | Preview) => Some(Kind::Note),
        Some(Notebook) => Some(Kind::Notebook),
        Some(Pdf) => Some(Kind::Pdf),
        Some(Diagram) => Some(Kind::Diagram),
        _ => None,
    };
    match action {
        a if OWN_SYNC.contains(&a) => true,
        a if PRINTS.contains(&a) => print_actions(printable).contains(&a),
        "win.open-in-browser" => note || notebook || preview,

        // The vault's files and what is made in them.
        "win.new-file"
        | "win.new-folder"
        | "win.new-drawing"
        | "win.new-from-template"
        | "win.rename"
        | "win.cut-file"
        | "win.copy-file"
        | "win.move-file-to"
        | "win.paste-files"
        | "win.open-in-terminal"
        | "win.exclude-folder"
        | "win.reload-folder"
        | "win.git-init"
        | "win.palette-symbols"
        | "win.palette-tags"
        | "app.close-vault" => vault,
        "win.upload" | "win.terminal-local" | "win.delete" | "win.download-file" => remote,
        // Neither is a host's: it has no trash, and a window with no vault opens a file by its
        // path on this machine.
        "win.trash" | "win.open-in-new-window" => vault && !remote,
        "win.save" => text || diagram || notebook || cx.shells,
        "win.save-as" => {
            cx.file == Some(File::Vault)
                && matches!(
                    front,
                    Some(Note { .. } | Code | Pdf | Image | Diagram | Notebook)
                )
        }
        "win.save-session" => cx.shells,
        "win.close-session" => cx.session,

        // The tab itself, wherever there is one.
        "win.close-tab" | "win.close-pane-tabs" | "win.next-tab" | "win.previous-tab" => {
            front.is_some()
        }
        a if a.starts_with("win.split-") || a.starts_with("win.move-tab-") => front.is_some(),
        "win.move-to-new-window" => cx.movable,
        "win.pin-tab" => front.is_some() && !cx.pinned,
        "win.unpin-tab" => cx.pinned,
        a if a.starts_with("win.divider-") => cx.panes > 1,
        "win.copy-name" | "win.copy-absolute-path" | "win.show-in-files" => cx.file.is_some(),
        "win.copy-relative-path" | "win.reveal-in-sidebar" => cx.file == Some(File::Vault),

        // What searches, edits and moves about in the document.
        "win.find" | "win.find-next" | "win.find-previous" => text || pdf || notebook || preview,
        "win.replace" => text || notebook,
        // `Ctrl+G` groups a diagram's selection, `Ctrl+D` duplicates it, and `Ctrl+Return`
        // finishes a diagram's label or a PDF's typed text.
        "win.goto-line" => text || notebook || pdf || diagram,
        "win.duplicate-line" => text || notebook || diagram,
        "win.newline-below" => text || notebook || diagram || pdf,
        // A staged change's panes take carets as the editor does, and no edits.
        "win.caret-above" | "win.caret-below" | "win.next-occurrence" | "win.all-occurrences" => {
            text || notebook || front == Some(Diff)
        }
        "win.delete-line" | "win.toggle-comment" | "win.scroll-up" | "win.scroll-down"
        | "win.paste-plain" | "win.follow-link" | "win.quick-fix" => text || notebook,
        "win.source-action" | "win.lsp-restart-file" => code || notebook,
        "win.toggle-task" => note,
        "win.toggle-wrap" => text || front == Some(Diff),
        "win.fold" | "win.unfold" | "win.fold-all" | "win.unfold-all" | "win.minimap" => text,
        "win.view-mode" => note || preview,
        "win.insert-template" => note && vault,
        "win.insert-sketch" => markdown && cx.file == Some(File::Vault),
        "win.zoom-in" | "win.zoom-out" | "win.zoom-reset" => {
            front.is_some_and(|front| front != Status)
        }

        // The tab kinds' own commands.
        "win.terminal-copy" => front == Some(Terminal),
        "win.terminal-paste" => front == Some(Terminal) || text || notebook,
        "win.terminal-share" => front == Some(Terminal) && cx.shared == Some(false),
        "win.terminal-revoke" => front == Some(Terminal) && cx.shared == Some(true),
        "win.open-in-accent" => cx.editor,
        "win.pdf-draw" => pdf || diagram,
        a if a.starts_with("win.pdf-") => pdf,
        a if a.starts_with("win.diagram-") => diagram,
        "win.notebook-run-all" | "win.notebook-clear-all-outputs" => notebook || cx.kernel,
        a if a.starts_with("win.notebook-") => notebook,
        "win.python-environment" => cx.python,

        // The sidebar's panes, where the window has them.
        "win.pane-files" | "win.show-hidden-files" => pane("files"),
        "win.pane-search" | "win.replace-in-files" | "win.search-all" => pane("search"),
        "win.pane-info" | "win.file-details" => pane("info"),
        "win.pane-git" => pane("git"),
        "win.pane-outline" => pane("outline"),
        "win.pane-properties" => pane("properties"),
        "win.pane-bibliography" => pane("bibliography"),

        // Git and the comparisons in a tab.
        "win.git-sync" | "win.git-sync-all" | "win.git-merge" | "win.git-delete-branch" => {
            cx.repository
        }
        "win.git-merge-abort" => cx.merging,
        a if a.starts_with("win.conflict-") => text && cx.conflicts,
        "win.merge-resolved" => cx.merge,
        a if a.starts_with("win.hunk-") => cx.comparing,

        // A bibliography's commands need one; New Bibliography and Add Reference make one.
        "win.bib-new" | "win.bib-add" => vault,
        a if a.starts_with("win.bib-") => cx.bibliography,

        "win.debug-toggle-breakpoint" | "win.debug-watch" => cx.breakable,
        "win.debug-remove-breakpoints" => cx.breakpoints,
        "win.debug-start" => cx.debuggable,
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::super::ACTIONS;
    use super::*;

    fn listed(cx: &Context) -> Vec<&'static str> {
        ACTIONS
            .iter()
            .map(|(action, _, _)| *action)
            .filter(|action| applies(action, cx))
            .collect()
    }

    fn vault_file(front: Front) -> Context {
        Context {
            front: Some(front),
            file: Some(File::Vault),
            vault: Some(false),
            ..Context::default()
        }
    }

    /// A markdown note lists none of a PDF's commands, and a PDF none of a note's.
    #[test]
    fn a_note_and_a_pdf_list_only_their_own_commands() {
        let note = listed(&vault_file(Front::Note { markdown: true }));
        let pdf = listed(&vault_file(Front::Pdf));
        for action in [
            "win.export-pdf",
            "win.pdf-fit-width",
            "win.pdf-next-page",
            "win.pdf-add-page-after",
            "win.pdf-pen",
            "win.pdf-undo",
            "win.pdf-draw",
        ] {
            assert!(!note.contains(&action), "{action} is listed over a note");
            assert!(pdf.contains(&action), "{action} is not listed over a PDF");
        }
        for action in [
            "win.export-html",
            "win.view-mode",
            "win.toggle-task",
            "win.insert-sketch",
            "win.fold",
            "win.duplicate-line",
        ] {
            assert!(!pdf.contains(&action), "{action} is listed over a PDF");
            assert!(note.contains(&action), "{action} is not listed over a note");
        }
        // The window's own commands stay wherever the reader is.
        for action in ["win.palette-files", "win.preferences", "win.terminal"] {
            assert!(note.contains(&action) && pdf.contains(&action));
        }
        // An org note previews, prints and exports as a markdown one does.
        let org = listed(&vault_file(Front::Note { markdown: false }));
        for action in [
            "win.view-mode",
            "win.export-html",
            "win.print",
            "win.open-in-browser",
        ] {
            assert!(
                org.contains(&action),
                "{action} is not listed over an org note"
            );
        }
        assert!(!org.contains(&"win.insert-sketch"));
    }

    /// Without a vault what could only say "Open a local folder" is gone, and a remote vault's
    /// window alone uploads.
    #[test]
    fn the_vault_decides_the_files_commands() {
        let loose = Context {
            front: Some(Front::Note { markdown: true }),
            file: Some(File::Loose),
            ..Context::default()
        };
        for action in [
            "win.new-file",
            "win.save-as",
            "win.copy-relative-path",
            "app.close-vault",
        ] {
            assert!(!applies(action, &loose), "{action} applies without a vault");
        }
        assert!(applies("win.copy-absolute-path", &loose));
        assert!(!applies("win.upload", &vault_file(Front::Pdf)));
        let remote = Context {
            vault: Some(true),
            ..vault_file(Front::Pdf)
        };
        assert!(applies("win.upload", &remote));
    }
}
