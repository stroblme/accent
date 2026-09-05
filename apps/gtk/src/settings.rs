//! Preferences dialog.
//!
//! Every row writes the config and saves it as it is edited, then reports it through `on_change`,
//! so there is no OK button (GNOME convention). Nothing here knows about `App`: the dialog takes
//! the shared config, the vault it is editing and one callback, which is what lets `main` wire it
//! up without a cycle.

use accent_core::config::{Config, Theme, VaultConfig};
use adw::prelude::*;
use gtk::glib;
use gtk::pango;
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// Shown instead of today's file name when chrono rejects the daily pattern.
const INVALID: &str = "Invalid format";

/// The theme choices, in the order the combo row lists them.
const THEMES: [(Theme, &str); 4] = [
    (Theme::System, "System"),
    (Theme::Light, "Light"),
    (Theme::Dark, "Dark"),
    (Theme::Solarized, "Solarized"),
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
            if let Err(e) = snapshot.save() {
                tracing::warn!("saving config: {e:#}");
            }
            on_change(&snapshot);
        }
    });

    let dialog = adw::PreferencesDialog::builder()
        .title("Preferences")
        .build();

    let page = adw::PreferencesPage::new();
    page.add(&appearance_group(&config, &save));
    page.add(&editor_group(&config, &save));
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
    group
}

/// Where `theme` sits in [`THEMES`], which is the row's selected index.
fn index_of(theme: Theme) -> u32 {
    THEMES.iter().position(|(t, _)| *t == theme).unwrap_or(0) as u32
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

    let reset = gtk::Button::builder()
        .label("Reset")
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
        "Daily Notes Folder",
        None,
        &vault.daily_dir,
        config,
        root,
        save,
        |cfg, text| cfg.daily_dir = folder(text),
    ));

    let pattern = entry_row(
        "Daily Note Filename",
        Some("A strftime format; the .md is added for you"),
        &vault.daily_pattern,
        config,
        root,
        save,
        |cfg, text| cfg.daily_pattern = text.trim().to_string(),
    );
    // The one setting a user can silently break, so the row carries today's name as it is typed.
    // It hangs off the suffix rather than a subtitle, which AdwEntryRow does not have.
    let preview = gtk::Label::builder().valign(gtk::Align::Center).build();
    set_preview(&preview, &vault.daily_pattern);
    pattern.connect_changed({
        let preview = preview.clone();
        move |r| set_preview(&preview, r.text().trim())
    });
    pattern.add_suffix(&preview);
    group.add(&pattern);

    group.add(&entry_row(
        "Daily Note Template",
        Some("Leave empty for no template"),
        vault.daily_template.as_deref().unwrap_or_default(),
        config,
        root,
        save,
        |cfg, text| cfg.daily_template = Some(folder(text)).filter(|t| !t.is_empty()),
    ));
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
        "New Notes Folder",
        Some("Empty means the vault root"),
        &vault.new_note_dir,
        config,
        root,
        save,
        |cfg, text| cfg.new_note_dir = folder(text),
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
            let confirm = adw::AlertDialog::builder()
                .heading("Restore Default Preferences?")
                .body(
                    "Theme, fonts, editor options, every vault's folders and any shortcuts you \
                     changed go back to their defaults. Your recent vaults are kept.",
                )
                .close_response("cancel")
                .build();
            confirm.add_response("cancel", "Cancel");
            confirm.add_response("restore", "Restore");
            confirm.set_response_appearance("restore", adw::ResponseAppearance::Destructive);
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
/// a leading slash is the mistake that would quietly point daily notes outside the vault.
fn folder(text: &str) -> String {
    text.trim().trim_matches('/').trim().to_string()
}

fn set_preview(label: &gtk::Label, pattern: &str) {
    let text = daily_preview(pattern, &now_local_iso());
    let broken = text == INVALID;
    label.set_label(&text);
    label.set_css_classes(if broken { &["error"] } else { &["dim-label"] });
}

/// Today's daily-note name for `pattern`, or [`INVALID`] when chrono rejects it. `now` is an
/// ISO-8601 local timestamp.
fn daily_preview(pattern: &str, now: &str) -> String {
    let name = now
        .parse()
        .ok()
        .and_then(|now| accent_core::template::strftime(pattern, now))
        .filter(|name| !name.is_empty());
    match name {
        Some(name) => format!("{name}.md"),
        None => INVALID.to_string(),
    }
}

/// Local wall-clock time, ISO-8601, seconds resolution.
///
/// ponytail: apps/gtk has no chrono dependency and `template::strftime` wants a
/// `chrono::NaiveDateTime`, so GLib supplies the local time and chrono's `FromStr` turns it back
/// into one, with the type inferred rather than named. Replace both halves with
/// `chrono::Local::now().naive_local()` the day the GTK app depends on chrono directly.
fn now_local_iso() -> String {
    glib::DateTime::now_local()
        .and_then(|t| t.format("%Y-%m-%dT%H:%M:%S"))
        .map(|s| s.to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Thursday afternoon, the same instant `template`'s own tests use.
    const NOW: &str = "2026-09-03T14:05:00";

    #[test]
    fn every_theme_has_a_row_to_pick_it_with() {
        for (theme, _) in THEMES {
            assert_eq!(THEMES[index_of(theme) as usize].0, theme);
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

    #[test]
    fn daily_preview_shows_todays_file_name() {
        assert_eq!(daily_preview("%Y-%m-%d", NOW), "2026-09-03.md");
        assert_eq!(daily_preview("%Y/%B/%d", NOW), "2026/September/03.md");
    }

    #[test]
    fn daily_preview_marks_a_pattern_chrono_rejects() {
        assert_eq!(daily_preview("%Q", NOW), INVALID);
        assert_eq!(daily_preview("", NOW), INVALID);
    }
}
