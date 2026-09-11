//! Preferences dialog.
//!
//! Every row writes the config and saves it as it is edited, then reports it through `on_change`,
//! so there is no OK button (GNOME convention). Nothing here knows about `App`: the dialog takes
//! the shared config, the vault it is editing and one callback, which is what lets `main` wire it
//! up without a cycle.

use accent_core::config::{Config, FocusMode, Theme, VaultConfig};
use adw::prelude::*;
use gtk::pango;
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// The theme choices, in the order the combo row lists them.
const THEMES: [(Theme, &str); 4] = [
    (Theme::System, "System"),
    (Theme::Light, "Light"),
    (Theme::Dark, "Dark"),
    (Theme::Solarized, "Solarized"),
];

/// The focus mode levels, in the order the combo row lists them, each with what it fades.
const FOCUS_MODES: [(FocusMode, &str, &str); 3] = [
    (FocusMode::None, "None", "Nothing fades while you type"),
    (
        FocusMode::Medium,
        "Medium",
        "The bars and the sidebar fade while you type",
    ),
    (
        FocusMode::High,
        "High",
        "The text away from the caret and the other panes fade too",
    ),
];

/// `root` identifies which vault's per-vault settings are being edited, and is `None` in a window
/// opened on a file rather than a folder, where that group has no vault to be about. `on_change`
/// is called
/// after every edit, with the config already saved to disk, so the caller can apply it live.
pub fn present(
    parent: &impl IsA<gtk::Widget>,
    config: Rc<RefCell<Config>>,
    root: Option<PathBuf>,
    on_change: impl Fn(&Config) + 'static,
) {
    page(parent.as_ref(), config, root, Rc::new(on_change));
}

/// Write `config` to disk, and say so in the log when that fails: the change is already in force
/// on screen, and a preference that will not outlive the process is not worth a dialog.
///
/// A file changed by someone else is not written over. Taking the change in is the config
/// watcher's (`Shell::config_file_changed`), which keeps this change on top and writes both, or
/// warns once that the file does not parse; so a refusal here is only a debug line.
pub fn save(config: &Config) {
    match config.save() {
        Ok(true) => {}
        Ok(false) => tracing::debug!("config.toml changed on disk; the write waits for it"),
        Err(e) => tracing::warn!("saving config: {e:#}"),
    }
}

/// The dialog itself, split off so Restore Defaults can build it a second time. Every row reads
/// its value once, at construction, so a reset that changes all of them is a new page rather than
/// a handle kept on each row.
fn page(
    parent: &gtk::Widget,
    config: Rc<RefCell<Config>>,
    root: Option<PathBuf>,
    on_change: Rc<dyn Fn(&Config)>,
) {
    // The config is cloned out of the cell before saving, so `on_change` is free to borrow it
    // again without meeting an outstanding borrow of ours.
    let save: Rc<dyn Fn()> = Rc::new({
        let (config, on_change) = (config.clone(), on_change.clone());
        move || {
            let snapshot = config.borrow().clone();
            save(&snapshot);
            on_change(&snapshot);
        }
    });

    let dialog = adw::PreferencesDialog::builder()
        .title("Preferences")
        .build();

    let page = adw::PreferencesPage::new();
    page.add(&appearance_group(&config, &save));
    page.add(&editor_group(&config, &save));
    page.add(&git_group(&config, &save));
    page.add(&pdf_group(&config, &save));
    if let Some(root) = &root {
        page.add(&vault_group(&config, root, &save));
    }
    page.add(&reset_group(
        &dialog, parent, &config, &root, &save, &on_change,
    ));

    dialog.add(&page);
    dialog.present(Some(parent));
}

// ------------------------------------------------------------------------------- appearance

fn appearance_group(config: &Rc<RefCell<Config>>, save: &Rc<dyn Fn()>) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title("Appearance").build();

    let chosen = config.borrow().theme;
    let names: Vec<&str> = THEMES.iter().map(|(_, name)| *name).collect();
    let row = adw::ComboRow::builder()
        .title("Theme")
        .subtitle("Solarized follows the system light and dark setting")
        .model(&gtk::StringList::new(&names))
        .selected(index_of(chosen))
        .build();
    row.connect_selected_notify({
        let (config, save) = (config.clone(), save.clone());
        move |r| {
            let Some((theme, _)) = THEMES.get(r.selected() as usize) else {
                return;
            };
            config.borrow_mut().theme = *theme;
            save();
        }
    });
    group.add(&row);

    // The subtitle follows the pick, so the row says what the chosen level will fade.
    let chosen = focus_index(config.borrow().focus_mode);
    let names: Vec<&str> = FOCUS_MODES.iter().map(|(_, name, _)| *name).collect();
    let focus = adw::ComboRow::builder()
        .title("Focus Mode")
        .subtitle(FOCUS_MODES[chosen as usize].2)
        .model(&gtk::StringList::new(&names))
        .selected(chosen)
        .build();
    focus.connect_selected_notify({
        let (config, save) = (config.clone(), save.clone());
        move |r| {
            let Some((mode, _, what)) = FOCUS_MODES.get(r.selected() as usize) else {
                return;
            };
            r.set_subtitle(what);
            config.borrow_mut().focus_mode = *mode;
            save();
        }
    });
    group.add(&focus);
    group
}

/// Where `theme` sits in [`THEMES`], which is the row's selected index.
fn index_of(theme: Theme) -> u32 {
    THEMES.iter().position(|(t, _)| *t == theme).unwrap_or(0) as u32
}

/// Where `mode` sits in [`FOCUS_MODES`].
fn focus_index(mode: FocusMode) -> u32 {
    FOCUS_MODES
        .iter()
        .position(|(m, ..)| *m == mode)
        .unwrap_or(0) as u32
}

// ----------------------------------------------------------------------------------- editor

fn editor_group(config: &Rc<RefCell<Config>>, save: &Rc<dyn Fn()>) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title("Editor").build();

    let chosen = config.borrow().editor_font.clone();
    let row = adw::ActionRow::builder()
        .title("Document Font")
        .subtitle(font_subtitle(chosen.as_deref()))
        // Font names and vault paths are plain text, not Pango markup.
        .use_markup(false)
        .build();

    let button = gtk::FontDialogButton::new(Some(gtk::FontDialog::new()));
    button.set_level(gtk::FontLevel::Font);
    button.set_valign(gtk::Align::Center);
    button.set_font_desc(&pango::FontDescription::from_string(
        &chosen.clone().unwrap_or_else(system_font),
    ));

    // An icon rather than the word: the row already carries a font button whose label is the
    // whole font name, and two text buttons beside it leave the name nowhere to go on a narrow
    // dialog. `document-revert-symbolic` is the arrow back to a saved state, which is what this
    // does; the tooltip is what keeps it discoverable (DESIGN.md, Iconography).
    let reset = gtk::Button::builder()
        .icon_name("document-revert-symbolic")
        .tooltip_text("Reset to the System Font")
        .valign(gtk::Align::Center)
        .sensitive(chosen.is_some())
        .build();
    reset.add_css_class("flat");

    // Set while Reset writes the system font back into the button, so the ::font-desc
    // notification that follows is not stored as a deliberate choice.
    let resetting = Rc::new(Cell::new(false));

    button.connect_font_desc_notify({
        let (config, save, row, reset, resetting) = (
            config.clone(),
            save.clone(),
            row.clone(),
            reset.clone(),
            resetting.clone(),
        );
        move |b| {
            let Some(desc) = b.font_desc().filter(|_| !resetting.get()) else {
                return;
            };
            let font = desc.to_str().to_string();
            config.borrow_mut().editor_font = Some(font.clone());
            row.set_subtitle(&font_subtitle(Some(&font)));
            reset.set_sensitive(true);
            save();
        }
    });

    reset.connect_clicked({
        let (config, save, row, button, resetting) = (
            config.clone(),
            save.clone(),
            row.clone(),
            button.clone(),
            resetting.clone(),
        );
        move |b| {
            config.borrow_mut().editor_font = None;
            // ponytail: GtkFontDialogButton has no "unset", so Reset shows the system document
            // font while the config says None. Both mean the same thing to the editor.
            resetting.set(true);
            button.set_font_desc(&pango::FontDescription::from_string(&system_font()));
            resetting.set(false);
            row.set_subtitle(&font_subtitle(None));
            b.set_sensitive(false);
            save();
        }
    });

    row.add_suffix(&button);
    row.add_suffix(&reset);
    group.add(&row);

    // A share rather than a pixel count: the same setting has to read the same on a laptop and on
    // a wide monitor, and the editor is not the window (the sidebar and the preview take theirs).
    // The minimum is 30 % because below that the editor's own floor takes over and the number
    // would stop meaning anything.
    let width = adw::SpinRow::with_range(30.0, 100.0, 5.0);
    width.set_title("Column Width");
    width.set_subtitle("Percentage of the editor the document column fills");
    width.set_value(f64::from(config.borrow().column_width));
    width.connect_value_notify({
        let (config, save) = (config.clone(), save.clone());
        move |r| {
            config.borrow_mut().column_width = r.value().round() as u32;
            save();
        }
    });
    group.add(&width);

    let spell = adw::SwitchRow::builder()
        .title("Spell Checking")
        .subtitle("Underline misspelled words as you type")
        .active(config.borrow().spellcheck)
        .build();
    spell.connect_active_notify({
        let (config, save) = (config.clone(), save.clone());
        move |r| {
            config.borrow_mut().spellcheck = r.is_active();
            save();
        }
    });
    group.add(&spell);

    let ghost = adw::SwitchRow::builder()
        .title("Ghost Text")
        .subtitle("Suggest the rest of the line from what the vault already says; Tab accepts")
        .active(config.borrow().ghost_text)
        .build();
    ghost.connect_active_notify({
        let (config, save) = (config.clone(), save.clone());
        move |r| {
            config.borrow_mut().ghost_text = r.is_active();
            save();
        }
    });
    group.add(&ghost);

    let numbers = adw::SwitchRow::builder()
        .title("Line Numbers")
        .subtitle("Number every line in a gutter of its own, left of the page")
        .active(config.borrow().line_numbers)
        .build();
    numbers.connect_active_notify({
        let (config, save) = (config.clone(), save.clone());
        move |r| {
            config.borrow_mut().line_numbers = r.is_active();
            save();
        }
    });
    group.add(&numbers);

    let minimap = adw::SwitchRow::builder()
        .title("Minimap")
        .subtitle("Show a code map beside the document instead of the scrollbar")
        .active(config.borrow().minimap)
        .build();
    minimap.connect_active_notify({
        let (config, save) = (config.clone(), save.clone());
        move |r| {
            config.borrow_mut().minimap = r.is_active();
            save();
        }
    });
    group.add(&minimap);

    group
}

/// What the font row says under its title: the chosen font, or the default it follows.
fn font_subtitle(font: Option<&str>) -> String {
    match font {
        Some(f) => f.to_string(),
        None => format!("{} (default)", crate::editor::default_font()),
    }
}

fn system_font() -> String {
    crate::editor::default_font()
}

// -------------------------------------------------------------------------------------- git

fn git_group(config: &Rc<RefCell<Config>>, save: &Rc<dyn Fn()>) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title("Git").build();

    // The Git pane's own toggle writes the same value, so the two surfaces are one preference.
    let tree = adw::SwitchRow::builder()
        .title("Group Changes by Folder")
        .subtitle("Show the Git pane's changed files as a tree instead of a flat list")
        .active(config.borrow().git_tree)
        .build();
    tree.connect_active_notify({
        let (config, save) = (config.clone(), save.clone());
        move |r| {
            config.borrow_mut().git_tree = r.is_active();
            save();
        }
    });
    group.add(&tree);

    group
}

// -------------------------------------------------------------------------------------- pdf

fn pdf_group(config: &Rc<RefCell<Config>>, save: &Rc<dyn Fn()>) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title("PDF").build();

    let mouse = adw::SwitchRow::builder()
        .title("Draw with the Mouse")
        .subtitle("Even when a pen is attached")
        .active(config.borrow().drawing.mouse)
        .build();
    mouse.connect_active_notify({
        let (config, save) = (config.clone(), save.clone());
        move |r| {
            config.borrow_mut().drawing.mouse = r.is_active();
            save();
        }
    });
    group.add(&mouse);

    group
}

// ------------------------------------------------------------------------------- this vault

fn vault_group(
    config: &Rc<RefCell<Config>>,
    root: &Path,
    save: &Rc<dyn Fn()>,
) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("This Vault")
        // AdwEntryRow has no subtitle, so what is true of every path below is said once here.
        .description("Paths are relative to the vault root.")
        .build();
    let vault = config.borrow().vault(root);

    group.add(&entry_row(
        "Templates Folder",
        None,
        &vault.templates_dir,
        config,
        root,
        save,
        |cfg, text| cfg.templates_dir = folder(text),
    ));
    group.add(&entry_row(
        "New Files Folder",
        Some("Empty means the vault root"),
        &vault.new_file_dir,
        config,
        root,
        save,
        |cfg, text| cfg.new_file_dir = folder(text),
    ));

    group
}

// ---------------------------------------------------------------------------- restore defaults

/// The one row that changes settings it does not show, so it asks first and the question names
/// what survives. DESIGN.md, States: an alert dialog is for a choice that can lose data.
fn reset_group(
    dialog: &adw::PreferencesDialog,
    parent: &gtk::Widget,
    config: &Rc<RefCell<Config>>,
    root: &Option<PathBuf>,
    save: &Rc<dyn Fn()>,
    on_change: &Rc<dyn Fn(&Config)>,
) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::new();
    let row = adw::ButtonRow::builder().title("Restore Defaults").build();
    row.add_css_class("destructive-action");
    row.connect_activated({
        let (config, root, save, on_change) = (
            config.clone(),
            root.clone(),
            save.clone(),
            on_change.clone(),
        );
        // Weak: this closure hangs off a row inside the dialog, and a strong handle back to it is
        // the cycle that would keep every preferences dialog ever opened alive.
        let (dialog, parent) = (dialog.downgrade(), parent.clone());
        move |_| {
            let Some(dialog) = dialog.upgrade() else {
                return;
            };
            let confirm = crate::dialogs::alert(
                "Restore Default Preferences?",
                "Theme, fonts, editor options, every vault's folders and any shortcuts you \
                 changed go back to their defaults. Your recent vaults are kept.",
                &[
                    ("cancel", "Cancel", adw::ResponseAppearance::Default),
                    ("restore", "Restore", adw::ResponseAppearance::Destructive),
                ],
                "cancel",
            );
            confirm.connect_response(Some("restore"), {
                let (config, root, save, on_change) = (
                    config.clone(),
                    root.clone(),
                    save.clone(),
                    on_change.clone(),
                );
                let (dialog, parent) = (dialog.downgrade(), parent.clone());
                move |_, _| {
                    let Some(dialog) = dialog.upgrade() else {
                        return;
                    };
                    {
                        let mut cfg = config.borrow_mut();
                        let keep = std::mem::take(&mut cfg.recent_vaults);
                        *cfg = Config {
                            recent_vaults: keep,
                            ..Config::default()
                        };
                    }
                    save();
                    dialog.close();
                    page(&parent, config.clone(), root.clone(), on_change.clone());
                }
            });
            confirm.present(Some(&dialog));
        }
    });
    group.add(&row);
    group
}

/// One entry row wired to a field of the vault config; `set` decides how the typed text lands in
/// it. The entry itself is never rewritten while the user types, or the caret would jump.
fn entry_row(
    title: &str,
    tooltip: Option<&str>,
    value: &str,
    config: &Rc<RefCell<Config>>,
    root: &Path,
    save: &Rc<dyn Fn()>,
    set: impl Fn(&mut VaultConfig, &str) + 'static,
) -> adw::EntryRow {
    let row = adw::EntryRow::builder().title(title).text(value).build();
    row.set_tooltip_text(tooltip);

    let (config, root, save) = (config.clone(), root.to_path_buf(), save.clone());
    // ponytail: writes and saves on every keystroke. The file is about a kilobyte and this is a
    // dialog, so a debounce timer per row would cost more than it saves; add one if typing here
    // ever stutters on a slow disk.
    row.connect_changed(move |r| {
        let text = r.text().to_string();
        {
            let mut cfg = config.borrow_mut();
            let mut vault = cfg.vault(&root);
            set(&mut vault, &text);
            cfg.set_vault(&root, vault);
        }
        save();
    });
    row
}

/// Trim a folder entry and drop any leading or trailing `/`: these paths are vault-relative, and
/// a leading slash is the mistake that would quietly point one outside the vault.
fn folder(text: &str) -> String {
    text.trim().trim_matches('/').trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_theme_has_a_row_to_pick_it_with() {
        for (theme, _) in THEMES {
            assert_eq!(THEMES[index_of(theme) as usize].0, theme);
        }
    }

    #[test]
    fn every_focus_mode_has_a_row_to_pick_it_with() {
        for (mode, _, _) in FOCUS_MODES {
            assert_eq!(FOCUS_MODES[focus_index(mode) as usize].0, mode);
        }
    }

    #[test]
    fn folder_entries_are_vault_relative_and_trimmed() {
        assert_eq!(folder("/Daily/"), "Daily");
        assert_eq!(folder("  Daily Notes  "), "Daily Notes");
        assert_eq!(folder("/ Templates/Daily "), "Templates/Daily");
        assert_eq!(folder(""), "");
        assert_eq!(folder("   "), "");
    }
}
