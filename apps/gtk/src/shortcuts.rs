//! Keyboard Shortcuts: an `AdwShortcutsDialog` over the action table as it stands.
//!
//! Its rows are [`ACTIONS`] through [`accels_for`], the palette's command mode and `[shortcuts]`
//! overrides included, so a rebind shows and an unbound command does not: the palette lists every
//! command, bound or not, and is where they are rebound.

use crate::actions::{ACTIONS, accels_for};
use accent_api::Config;
use adw::prelude::*;

/// DESIGN.md Keyboard's headings that hold commands, in its order.
const SECTIONS: [&str; 19] = [
    "Files",
    "Palette and find",
    "Editing",
    "Bibliography",
    "PDF",
    "Diagram",
    "Notebook",
    "History",
    "Zoom",
    "Terminal",
    "Panes",
    "Git",
    "Notes",
    "Code",
    "Debugging",
    "Views",
    "Window",
    "Tab menu",
    "Tabs and panes",
];

/// The keys a PDF tab, a diagram's canvas or a notebook answers itself (`PdfTab::wire_keys`,
/// `DiagramTab::wire_keys`, `NotebookTab::wire_keys`) with these commands, which carry none in the
/// table: a bare `space` or `Delete` there would be taken from every entry (DESIGN.md, PDF and
/// Diagram). A presented diagram's paging keys are left out, being its only keys then.
const TAB_KEYS: [(&str, &[&str]); 26] = [
    ("win.bib-undo", &["<Primary>z"]),
    ("win.bib-redo", &["<Primary><Shift>z", "<Primary>y"]),
    ("win.pdf-next-page", &["space", "n", "Right"]),
    ("win.pdf-previous-page", &["<Shift>space", "p", "Left"]),
    ("win.pdf-copy", &["<Primary>c"]),
    ("win.pdf-undo", &["<Primary>z"]),
    ("win.pdf-redo", &["<Primary><Shift>z", "<Primary>y"]),
    ("win.diagram-undo", &["<Primary>z"]),
    ("win.diagram-redo", &["<Primary><Shift>z", "<Primary>y"]),
    ("win.diagram-delete", &["Delete", "BackSpace"]),
    (
        "win.diagram-delete-all",
        &["<Primary>Delete", "<Primary>BackSpace"],
    ),
    ("win.diagram-select-all", &["<Primary>a"]),
    ("win.diagram-cut", &["<Primary>x"]),
    ("win.diagram-copy", &["<Primary>c"]),
    ("win.diagram-paste", &["<Primary>v"]),
    ("win.diagram-edit-label", &["Return"]),
    ("win.diagram-next-page", &["Page_Down"]),
    ("win.diagram-previous-page", &["Page_Up"]),
    ("win.notebook-undo", &["<Primary>z"]),
    ("win.notebook-redo", &["<Primary><Shift>z", "<Primary>y"]),
    ("win.notebook-insert-above", &["<Alt><Shift>Return"]),
    ("win.notebook-insert-below", &["<Alt>b"]),
    ("win.notebook-move-up", &["<Primary><Shift>Up"]),
    ("win.notebook-move-down", &["<Primary><Shift>Down"]),
    ("win.notebook-split", &["<Primary><Shift>minus"]),
    ("win.notebook-run-advance", &["<Shift>Return"]),
];

/// Commands a diagram or a notebook in front runs on another command's chord
/// (`App::run_action`), so they show whatever that one is bound to.
const BORROWED: [(&str, &str); 3] = [
    ("win.diagram-duplicate", "win.duplicate-line"),
    ("win.diagram-group", "win.goto-line"),
    ("win.notebook-run-cell", "win.newline-below"),
];

/// The heading `action` is listed under in DESIGN.md Keyboard, `None` for one not placed yet.
fn section(action: &str) -> Option<&'static str> {
    let name = action.split_once('.')?.1;
    let starts = |prefixes: &[&str]| prefixes.iter().any(|p| name.starts_with(p));
    Some(match name {
        "save" | "save-as" | "open-file" | "new-file" | "new-folder" | "upload" | "print"
        | "close-tab" | "new-window" | "open-local" | "open-remote" | "open-recent"
        | "close-vault" | "reload-window" | "quit" => "Files",
        _ if starts(&["export-"]) => "Files",
        "find" | "find-next" | "find-previous" | "replace" | "replace-in-files" | "search-all"
        | "goto-line" => "Palette and find",
        _ if starts(&["palette-"]) => "Palette and find",
        "duplicate-line" | "delete-line" | "newline-below" | "toggle-comment" | "toggle-wrap"
        | "scroll-up" | "scroll-down" | "caret-above" | "caret-below" | "next-occurrence"
        | "all-occurrences" | "paste-plain" => "Editing",
        "image-invert" | "insert-sketch" | "show-in-pdf" => "PDF",
        _ if starts(&["pdf-"]) => "PDF",
        "new-drawing" => "Diagram",
        _ if starts(&["diagram-"]) => "Diagram",
        _ if starts(&["notebook-"]) => "Notebook",
        _ if starts(&["bib-"]) => "Bibliography",
        "back" | "forward" => "History",
        _ if starts(&["zoom-"]) => "Zoom",
        "save-session" | "close-session" => "Terminal",
        _ if starts(&["terminal"]) => "Terminal",
        "sidebar" | "show-hidden-files" | "file-details" => "Panes",
        _ if starts(&["pane-"]) => "Panes",
        "merge-resolved" => "Git",
        _ if starts(&["git-", "conflict-", "hunk-"]) => "Git",
        "rename" | "trash" | "new-from-template" | "insert-template" => "Notes",
        "follow-link" | "quick-fix" | "fold" | "unfold" | "fold-all" | "unfold-all"
        | "lsp-restart-file" | "python-environment" => "Code",
        _ if starts(&["debug-"]) => "Debugging",
        "view-mode" | "minimap" | "present" => "Views",
        "fullscreen" | "preferences" | "shortcuts" | "menu" | "about" => "Window",
        "copy-name" | "copy-relative-path" | "copy-absolute-path" | "show-in-files"
        | "reveal-in-sidebar" | "move-to-new-window" | "pin-tab" | "unpin-tab" => "Tab menu",
        "next-tab" | "previous-tab" | "close-pane-tabs" => "Tabs and panes",
        _ if starts(&["split-", "move-tab-", "divider-"]) => "Tabs and panes",
        _ => return None,
    })
}

/// A heading and its rows, each a command's label and the chords that run it.
type Section = (&'static str, Vec<(&'static str, Vec<String>)>);

/// Each heading with its bound commands, in the table's order; a heading with none is left out.
fn rows(config: &Config) -> Vec<Section> {
    SECTIONS
        .into_iter()
        .filter_map(|title| {
            let items: Vec<_> = ACTIONS
                .iter()
                .filter(|(action, _, _)| section(action) == Some(title))
                .map(|(action, label, _)| {
                    let own = TAB_KEYS.iter().find(|(a, _)| a == action);
                    let mut accels: Vec<String> = own
                        .into_iter()
                        .flat_map(|(_, keys)| keys.iter().map(|k| k.to_string()))
                        .collect();
                    accels.extend(accels_for(config, action));
                    if let Some((_, lender)) = BORROWED.iter().find(|(a, _)| a == action) {
                        accels.extend(accels_for(config, lender));
                    }
                    (*label, accels)
                })
                .filter(|(_, accels)| !accels.is_empty())
                .collect();
            (!items.is_empty()).then_some((title, items))
        })
        .collect()
}

/// Present the dialog over `window`, built afresh so it shows the bindings in force now.
pub fn present(window: &adw::ApplicationWindow, config: &Config) {
    let dialog = adw::ShortcutsDialog::new();
    for (title, items) in rows(config) {
        let section = adw::ShortcutsSection::new(Some(title));
        for (label, accels) in items {
            let mut shown: Vec<String> = Vec::new();
            // X11's `ISO_Left_Tab` is Shift+Tab, which Previous Tab spells both ways. A
            // hand-written chord GTK cannot read is left out, as the palette leaves it.
            for accel in accels.iter().map(|a| a.replace("ISO_Left_Tab", "Tab")) {
                if gtk::accelerator_parse(&accel).is_some() && !shown.contains(&accel) {
                    shown.push(accel);
                }
            }
            if !shown.is_empty() {
                section.add(adw::ShortcutsItem::new(label, &shown.join(" ")));
            }
        }
        dialog.add(section);
    }
    dialog.present(Some(window));
    crate::dialogs::close_on_outside_press(dialog.upcast_ref());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn accels_of(rows: &[Section], label: &str) -> Option<Vec<String>> {
        rows.iter()
            .flat_map(|(_, items)| items)
            .find(|(l, _)| *l == label)
            .map(|(_, accels)| accels.clone())
    }

    /// Every command has a heading, so none can be missing from the dialog once bound, and a
    /// misspelt name in the tabs' own keys would leave them out.
    #[test]
    fn every_action_has_a_section() {
        for (action, _, _) in ACTIONS {
            assert!(
                section(action).is_some(),
                "{action} has no Keyboard section"
            );
        }
        let named = TAB_KEYS.iter().map(|(a, _)| *a);
        for action in named.chain(BORROWED.iter().flat_map(|(a, b)| [*a, *b])) {
            assert!(
                ACTIONS.iter().any(|(name, _, _)| *name == action),
                "{action} is not in ACTIONS"
            );
        }
    }

    #[test]
    fn rows_follow_the_effective_bindings() {
        let mut config = Config::default();
        let shown = rows(&config);
        assert_eq!(shown[0].0, "Files");
        assert_eq!(
            accels_of(&shown, "Save"),
            Some(vec!["<Primary>s".to_string()])
        );
        // Unbound by default, so not listed.
        assert_eq!(accels_of(&shown, "About accent"), None);
        assert!(!shown.iter().any(|(title, _)| *title == "Tab menu"));
        // The PDF tab's own keys, with no binding in the table.
        assert_eq!(
            accels_of(&shown, "Next Page"),
            Some(vec!["space".into(), "n".into(), "Right".into()])
        );
        assert_eq!(
            accels_of(&shown, "Duplicate Selection"),
            Some(vec!["<Primary>d".into()])
        );
        assert_eq!(
            accels_of(&shown, "Insert Cell Below"),
            Some(vec!["<Alt>b".into()])
        );
        assert_eq!(
            accels_of(&shown, "Run Cell"),
            Some(vec!["<Primary>Return".into()])
        );

        // A rebind shows, under its heading; an unbind hides; a borrowed chord follows its lender.
        config
            .shortcuts
            .insert("win.copy-name".into(), vec!["<Control><Shift>m".into()]);
        config.shortcuts.insert("win.save".into(), Vec::new());
        config
            .shortcuts
            .insert("win.duplicate-line".into(), Vec::new());
        let shown = rows(&config);
        assert_eq!(accels_of(&shown, "Save"), None);
        assert_eq!(accels_of(&shown, "Duplicate Selection"), None);
        let (_, tab_menu) = shown
            .iter()
            .find(|(title, _)| *title == "Tab menu")
            .unwrap();
        assert_eq!(
            tab_menu.as_slice(),
            [("Copy Name", vec!["<Control><Shift>m".to_string()])]
        );
    }
}
