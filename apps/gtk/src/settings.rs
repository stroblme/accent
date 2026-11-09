//! Preferences with immediate saving and change notifications, following GNOME convention.
//!
//! Shared config, vault and `on_change` are supplied by the caller, avoiding a dependency
//! on `App`.

use crate::editors;
use accent_core::config::{
    Config, CustomCommand, DebugTerminal, DiagramConfig, DrawingConfig, Editor, FocusMode,
    RecolourConfig, Theme, VaultConfig,
};
use adw::prelude::*;
use gtk::{gio, glib, pango};
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
    (
        FocusMode::None,
        "None",
        "Keep the interface visible while typing",
    ),
    (
        FocusMode::Medium,
        "Medium",
        "Fade toolbars and sidebar while typing",
    ),
    (
        FocusMode::High,
        "High",
        "Also dim surrounding text and other panes",
    ),
];

/// Where a debugged program's terminal opens, in the order the combo row lists them, named as the
/// tab menu's splits are.
const DEBUG_TERMINALS: [(DebugTerminal, &str); 5] = [
    (DebugTerminal::Same, "Same Pane"),
    (DebugTerminal::Right, "Split Right"),
    (DebugTerminal::Left, "Split Left"),
    (DebugTerminal::Up, "Split Up"),
    (DebugTerminal::Down, "Split Down"),
];

/// Cached Preferences dialog, built before its first opening to reduce main-thread
/// work during the presentation animation (`ACCENT_BENCH_SWITCHER=prefs`).
pub struct Preferences {
    dialog: adw::PreferencesDialog,
    config: Rc<RefCell<Config>>,
    /// What the rows showed of the config ([`on_page`]) while closed, `None` while shown. Rebuild
    /// rows if it changed between openings because each row reads its initial value only once.
    shown: Rc<RefCell<Option<Config>>>,
}

impl Preferences {
    /// `root` enables per-vault settings; `None` omits that group for standalone files.
    /// `on_change` applies edits live after config is saved.
    pub fn new(
        config: Rc<RefCell<Config>>,
        root: Option<PathBuf>,
        on_change: impl Fn(&Config) + 'static,
    ) -> Preferences {
        let on_change: Rc<dyn Fn(&Config)> = Rc::new(on_change);
        let dialog = adw::PreferencesDialog::builder()
            .title("Preferences")
            .build();
        fill(&dialog, &config, &root, &on_change);

        // A config taken in from the file is a new page in the same dialog, for the reason a
        // reset is: every row is built reading its new value, so no row's handler runs and
        // nothing is written back. Weak: the action hangs off the dialog.
        let action = gio::SimpleAction::new("reload", None);
        action.connect_activate({
            let (weak, config) = (dialog.downgrade(), config.clone());
            move |_, _| {
                if let Some(dialog) = weak.upgrade() {
                    fill(&dialog, &config, &root, &on_change);
                }
            }
        });
        let actions = gio::SimpleActionGroup::new();
        actions.add_action(&action);
        dialog.insert_action_group("preferences", Some(&actions));

        let shown = Rc::new(RefCell::new(Some(on_page(&config.borrow()))));
        dialog.connect_closed({
            let (config, shown) = (config.clone(), shown.clone());
            move |_| {
                shown.replace(Some(on_page(&config.borrow())));
            }
        });
        crate::dialogs::close_on_outside_press(dialog.upcast_ref());
        Preferences {
            dialog,
            config,
            shown,
        }
    }

    pub fn present(&self, parent: &impl IsA<gtk::Widget>) {
        let shown = self.shown.take();
        if shown.is_some_and(|shown| shown != on_page(&self.config.borrow())) {
            reload(self.dialog.upcast_ref());
        }
        self.dialog.present(Some(parent));
    }
}

/// `config` less what no row shows and using accent moves: the recent vaults (a vault opened),
/// the drawing ring's pens and a diagram's tools. A new page costs the opening its build and first
/// layout, as the first opening did, where a page shown before takes about 5 ms
/// (`ACCENT_BENCH_SWITCHER=prefs`).
fn on_page(config: &Config) -> Config {
    Config {
        recent_vaults: Vec::new(),
        drawing: DrawingConfig {
            mouse: config.drawing.mouse,
            ..DrawingConfig::default()
        },
        diagram: DiagramConfig::default(),
        ..config.clone()
    }
}

/// Save preferences, logging failures without interrupting the already-applied UI change.
///
/// Do not overwrite external edits. `Shell::config_file_changed` merges them with current
/// changes or warns about invalid config; the refusal here stays at debug level.
pub fn save(config: &Config) {
    match config.save() {
        Ok(true) => {}
        Ok(false) => tracing::debug!("config.toml changed on disk; the write waits for it"),
        Err(e) => tracing::warn!("saving config: {e:#}"),
    }
}

/// The action a Preferences dialog answers by building its rows again ([`reload`]).
const RELOAD: &str = "preferences.reload";

/// Rebuild Preferences rows after a config change, on an open dialog or before
/// reopening a cached one. Ignore dialogs without the reload action.
pub fn reload(dialog: &adw::Dialog) {
    let _ = dialog.activate_action(RELOAD, None);
}

/// Replace the page from `config`: rows read values only at construction, so reset
/// and external changes rebuild them rather than retain individual row handles.
fn fill(
    dialog: &adw::PreferencesDialog,
    config: &Rc<RefCell<Config>>,
    root: &Option<PathBuf>,
    on_change: &Rc<dyn Fn(&Config)>,
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

    let page = adw::PreferencesPage::new();
    page.add(&appearance_group(config, &save));
    page.add(&recolour_group(config, &save));
    page.add(&editor_group(config, &save));
    page.add(&files_group(config, &save));
    page.add(&git_group(config, &save));
    page.add(&pdf_group(config, &save));
    page.add(&terminal_group(config, &save));
    page.add(&commands_group(config, &save));
    if let Some(root) = root {
        page.add(&vault_group(config, root, &save));
    }
    page.add(&reset_group(dialog, config, root, &save, on_change));
    crate::widgets::smooth_wheels(page.upcast_ref());

    // The new page opens where the old one was scrolled to: a hand edit or a reset is not the
    // reader scrolling back to the top.
    let top = dialog.visible_page().map(|old| {
        let top = scroll(&old).map_or(0.0, |adjustment| adjustment.value());
        dialog.remove(&old);
        top
    });
    dialog.add(&page);
    if let (Some(top), Some(adjustment)) = (top.filter(|top| *top > 0.0), scroll(&page)) {
        scroll_once_laid_out(&adjustment, top);
    }
}

/// A page's vertical scroll: its one child is the scrolled window it holds its groups in, which
/// AdwPreferencesPage has no getter for.
fn scroll(page: &adw::PreferencesPage) -> Option<gtk::Adjustment> {
    let scroller = page.first_child().and_downcast::<gtk::ScrolledWindow>()?;
    Some(scroller.vadjustment())
}

/// Scroll to `top` once the page has a height: until it is laid out the adjustment's range is
/// empty and would clamp the value to 0.
fn scroll_once_laid_out(adjustment: &gtk::Adjustment, top: f64) {
    let handler = Rc::new(RefCell::new(None));
    let id = adjustment.connect_changed({
        let handler = handler.clone();
        move |adjustment| {
            if adjustment.page_size() <= 0.0 {
                return;
            }
            if let Some(id) = handler.take() {
                adjustment.disconnect(id);
            }
            adjustment.set_value(top);
        }
    });
    handler.replace(Some(id));
}

// ------------------------------------------------------------------------------- appearance

fn appearance_group(config: &Rc<RefCell<Config>>, save: &Rc<dyn Fn()>) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title("Appearance").build();

    let chosen = config.borrow().theme;
    let names: Vec<&str> = THEMES.iter().map(|(_, name)| *name).collect();
    let row = adw::ComboRow::builder()
        .title("Theme")
        .subtitle("Solarized follows system appearance")
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

/// A switch for each kind of file a dark or Solarized theme recolours. Off, a kind's files show as
/// they are, and its Invert command recolours one of them instead.
fn recolour_group(config: &Rc<RefCell<Config>>, save: &Rc<dyn Fn()>) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("Match Colours to Theme")
        .build();
    type Switch = fn(&mut RecolourConfig) -> &mut bool;
    let rows: [(&str, Option<&str>, Switch); 3] = [
        (
            "Images",
            Some("Scans, plots and screenshots. Photos keep their colours"),
            |k| &mut k.images,
        ),
        ("PDFs", None, |k| &mut k.pdfs),
        ("Diagrams", Some("On the canvas and in notes"), |k| {
            &mut k.diagrams
        }),
    ];
    let mut kinds = config.borrow().recolour;
    for (title, subtitle, switch) in rows {
        let row = adw::SwitchRow::builder()
            .title(title)
            .active(*switch(&mut kinds))
            .build();
        if let Some(subtitle) = subtitle {
            row.set_subtitle(subtitle);
        }
        row.connect_active_notify({
            let (config, save) = (config.clone(), save.clone());
            move |r| {
                *switch(&mut config.borrow_mut().recolour) = r.is_active();
                save();
            }
        });
        group.add(&row);
    }
    group
}

/// Return `theme`'s index in [`THEMES`] for the selected row.
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

/// Text Editor: accent's own, the terminal editors the shell finds, and the one picked marked
/// where the shell finds no such program; a custom command, which only `config.toml` sets, is
/// listed while it is picked. The shell is asked once the row is first shown
/// ([`editors::installed`]), so a window that never opens Preferences never asks it.
fn text_editor_row(config: &Rc<RefCell<Config>>, save: &Rc<dyn Fn()>) -> adw::ComboRow {
    let row = adw::ComboRow::builder()
        .title("Text Editor")
        .subtitle("A terminal editor opens files in a terminal tab")
        .build();
    // What each line of the model picks, and whether the model is being replaced, whose
    // selection changes say nothing about the reader's pick.
    let listed: Rc<RefCell<Vec<Editor>>> = Rc::default();
    let filling = Rc::new(Cell::new(false));
    let fill = Rc::new({
        let (config, listed, filling) = (config.clone(), listed.clone(), filling.clone());
        move |row: &adw::ComboRow, found: Option<&[editors::Found]>| {
            let chosen = config.borrow().editor;
            let mut items = vec![(Editor::Accent, editors::name(Editor::Accent).to_string())];
            let found = found.map(|found| found.iter().map(|(editor, _)| *editor));
            let answered = found.is_some();
            items.extend(
                found
                    .into_iter()
                    .flatten()
                    .map(|e| (e, editors::name(e).into())),
            );
            if items.iter().all(|(editor, _)| *editor != chosen) {
                let name = editors::name(chosen);
                items.push((
                    chosen,
                    match answered && chosen != Editor::Custom {
                        true => format!("{name} (not found)"),
                        false => name.to_string(),
                    },
                ));
            }
            let names: Vec<&str> = items.iter().map(|(_, name)| name.as_str()).collect();
            filling.set(true);
            row.set_model(Some(&gtk::StringList::new(&names)));
            let at = items.iter().position(|(editor, _)| *editor == chosen);
            row.set_selected(at.unwrap_or(0) as u32);
            filling.set(false);
            *listed.borrow_mut() = items.into_iter().map(|(editor, _)| editor).collect();
        }
    });
    fill(&row, None);
    row.connect_selected_notify({
        let (config, save) = (config.clone(), save.clone());
        move |r| {
            let picked = listed.borrow().get(r.selected() as usize).copied();
            if filling.get() || picked.is_none_or(|e| e == config.borrow().editor) {
                return;
            }
            config.borrow_mut().editor = picked.unwrap_or_default();
            save();
        }
    });
    let asked = Cell::new(false);
    row.connect_map(move |row| {
        if asked.replace(true) {
            return;
        }
        let (row, fill) = (row.downgrade(), fill.clone());
        editors::installed(None, move |found| {
            if let Some(row) = row.upgrade() {
                fill(&row, Some(found));
            }
        });
    });
    row
}

fn editor_group(config: &Rc<RefCell<Config>>, save: &Rc<dyn Fn()>) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title("Editor").build();

    group.add(&text_editor_row(config, save));
    group.add(&font_row(
        "Document Font",
        config,
        save,
        |c| &mut c.editor_font,
        crate::editor::default_font,
    ));
    group.add(&font_row(
        "Preview Font",
        config,
        save,
        |c| &mut c.preview_font,
        crate::platform::document_font,
    ));

    // A share rather than a pixel count: the same setting has to read the same on a laptop and on
    // a wide monitor, and the editor is not the window (the sidebar and the preview take theirs).
    // The minimum is 30 % because below that the editor's own floor takes over and the number
    // would stop meaning anything.
    let width = adw::SpinRow::with_range(30.0, 100.0, 5.0);
    width.set_title("Column Width");
    width.set_subtitle("Document width as a percentage of the editor");
    width.set_value(f64::from(config.borrow().column_width));
    width.connect_value_notify({
        let (config, save) = (config.clone(), save.clone());
        move |r| {
            config.borrow_mut().column_width = r.value().round() as u32;
            save();
        }
    });
    group.add(&width);

    let full = adw::SwitchRow::builder()
        .title("Full-Width Preview")
        .subtitle("Use the full preview width")
        .active(config.borrow().full_width_preview)
        .build();
    full.connect_active_notify({
        let (config, save) = (config.clone(), save.clone());
        move |r| {
            config.borrow_mut().full_width_preview = r.is_active();
            save();
        }
    });
    group.add(&full);

    // Columns rather than a choice of 2/4/8: the number is what a tab character is worth on
    // screen, and a language whose house style is three of them is not ours to rule out.
    let indent = adw::SpinRow::with_range(1.0, 16.0, 1.0);
    indent.set_title("Indent Width");
    indent.set_subtitle("Columns per indent");
    indent.set_value(f64::from(config.borrow().indent_width));
    indent.connect_value_notify({
        let (config, save) = (config.clone(), save.clone());
        move |r| {
            config.borrow_mut().indent_width = r.value().round() as u32;
            save();
        }
    });
    group.add(&indent);

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

    let words = adw::SwitchRow::builder()
        .title("Word Suggestions")
        .subtitle("Suggest words from the document and dictionary")
        .active(config.borrow().word_suggestions)
        .build();
    words.connect_active_notify({
        let (config, save) = (config.clone(), save.clone());
        move |r| {
            config.borrow_mut().word_suggestions = r.is_active();
            save();
        }
    });
    group.add(&words);

    let ghost = adw::SwitchRow::builder()
        .title("Ghost Text")
        .subtitle("Complete lines using your vault. Press Tab to accept")
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
        .subtitle("Show line numbers beside the document")
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
        .subtitle("Replace the scrollbar with a document overview")
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

/// A font row: `field` of the config, a font button and a Reset back to `default`, which
/// `None` stands for.
fn font_row(
    title: &str,
    config: &Rc<RefCell<Config>>,
    save: &Rc<dyn Fn()>,
    field: fn(&mut Config) -> &mut Option<String>,
    default: fn() -> String,
) -> adw::ActionRow {
    let chosen = field(&mut config.borrow_mut()).clone();
    let row = adw::ActionRow::builder()
        .title(title)
        .subtitle(font_subtitle(chosen.as_deref(), default))
        // Font names and vault paths are plain text, not Pango markup.
        .use_markup(false)
        .build();

    let button = gtk::FontDialogButton::new(Some(gtk::FontDialog::new()));
    button.set_level(gtk::FontLevel::Font);
    button.set_valign(gtk::Align::Center);
    button.set_font_desc(&drawn(&button, &chosen.clone().unwrap_or_else(default)));

    // An icon rather than the word: the row already carries a font button whose label is the
    // whole font name, and two text buttons beside it leave the name nowhere to go on a narrow
    // dialog. `edit-clear-symbolic` is GNOME Settings' Reset Shortcut, a setting cleared back to
    // its default, which is what this does; the tooltip is what keeps it discoverable (DESIGN.md,
    // Iconography). Hidden rather than greyed while the system font is in use, as there is
    // nothing to go back to.
    let reset = gtk::Button::builder()
        .icon_name("edit-clear-symbolic")
        .tooltip_text("Use System Font")
        .valign(gtk::Align::Center)
        .visible(chosen.is_some())
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
            *field(&mut config.borrow_mut()) = Some(font.clone());
            row.set_subtitle(&font_subtitle(Some(&font), default));
            reset.set_visible(true);
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
            *field(&mut config.borrow_mut()) = None;
            // ponytail: GtkFontDialogButton has no "unset", so Reset shows the system font while
            // the config says None. Both mean the same thing to the view.
            resetting.set(true);
            button.set_font_desc(&drawn(&button, &default()));
            resetting.set(false);
            row.set_subtitle(&font_subtitle(None, default));
            // The keyboard stays in the row rather than leaving with the hidden button.
            button.grab_focus();
            b.set_visible(false);
            save();
        }
    });

    // Left of the font button, so the button keeps its place as Reset comes and goes.
    row.add_suffix(&reset);
    row.add_suffix(&button);
    row
}

/// `font` in the family Pango draws it with: a family the font map lacks falls back to another,
/// and the button would otherwise read "None" for the one it cannot find.
fn drawn(button: &gtk::FontDialogButton, font: &str) -> pango::FontDescription {
    let mut desc = pango::FontDescription::from_string(font);
    let used = button.pango_context().load_font(&desc);
    if let Some(family) = used.and_then(|font| font.describe().family()) {
        desc.set_family(&family);
    }
    desc
}

/// What the font row says under its title: the chosen font, or the default it follows.
fn font_subtitle(font: Option<&str>, default: fn() -> String) -> String {
    match font {
        Some(f) => f.to_string(),
        None => format!("{} (default)", default()),
    }
}

// ------------------------------------------------------------------------------------ files

fn files_group(config: &Rc<RefCell<Config>>, save: &Rc<dyn Fn()>) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title("Files").build();

    // How the tree lists, which is no single row's business — so it is a preference rather than
    // an item on the menu a right-click over one row opens. `win.show-hidden-files` in the
    // palette toggles the same value.
    let hidden = adw::SwitchRow::builder()
        .title("Show Hidden Files")
        .subtitle("Show files and folders with names starting with a dot")
        .active(config.borrow().show_hidden)
        .build();
    hidden.connect_active_notify({
        let (config, save) = (config.clone(), save.clone());
        move |r| {
            config.borrow_mut().show_hidden = r.is_active();
            save();
        }
    });
    group.add(&hidden);

    // Every vault's: the sort itself is the vault's, picked on the tree's Sort Files menu.
    let among = adw::SwitchRow::builder()
        .title("Sort Folders Among Files")
        .subtitle("Order folders like files instead of keeping them on top")
        .active(config.borrow().folders_among_files)
        .build();
    among.connect_active_notify({
        let (config, save) = (config.clone(), save.clone());
        move |r| {
            config.borrow_mut().folders_among_files = r.is_active();
            save();
        }
    });
    group.add(&among);

    let tint = adw::SwitchRow::builder()
        .title("Colour Changed Files")
        .subtitle("Use the accent colour for changes and green for new files")
        .active(config.borrow().git_tint)
        .build();
    tint.connect_active_notify({
        let (config, save) = (config.clone(), save.clone());
        move |r| {
            config.borrow_mut().git_tint = r.is_active();
            save();
        }
    });
    group.add(&tint);

    group
}

// -------------------------------------------------------------------------------------- git

fn git_group(config: &Rc<RefCell<Config>>, save: &Rc<dyn Fn()>) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title("Git").build();

    let tree = adw::SwitchRow::builder()
        .title("Group Changes by Folder")
        .subtitle("Organise changes by folder in the Git pane")
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

// --------------------------------------------------------------------------------- terminal

fn terminal_group(config: &Rc<RefCell<Config>>, save: &Rc<dyn Fn()>) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title("Terminal").build();

    // Off, a focused shell leaves the window a small reserved set of chords (`actions::kept`).
    let forward = adw::SwitchRow::builder()
        .title("Forward All Keys")
        .subtitle("Send all keys except copy, paste and Ctrl+Page Up/Down")
        .active(config.borrow().forward_keys_to_terminal)
        .build();
    forward.connect_active_notify({
        let (config, save) = (config.clone(), save.clone());
        move |r| {
            config.borrow_mut().forward_keys_to_terminal = r.is_active();
            save();
        }
    });
    group.add(&forward);

    let chosen = config.borrow().debug_terminal;
    let names: Vec<&str> = DEBUG_TERMINALS.iter().map(|(_, name)| *name).collect();
    let place = adw::ComboRow::builder()
        .title("Debug Terminal")
        .subtitle("Pane for the debug terminal and console")
        .model(&gtk::StringList::new(&names))
        .selected(
            DEBUG_TERMINALS
                .iter()
                .position(|(p, _)| *p == chosen)
                .unwrap_or(0) as u32,
        )
        .build();
    place.connect_selected_notify({
        let (config, save) = (config.clone(), save.clone());
        move |r| {
            if let Some((place, _)) = DEBUG_TERMINALS.get(r.selected() as usize) {
                config.borrow_mut().debug_terminal = *place;
                save();
            }
        }
    });
    group.add(&place);

    group
}

// --------------------------------------------------------------------------------- commands

/// The user's own commands (`[[commands]]`): a row each, its name over its command line, opening
/// on the two, its shortcut and Remove; Add Command puts a new one at the end, opened.
fn commands_group(config: &Rc<RefCell<Config>>, save: &Rc<dyn Fn()>) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title("Commands").build();
    command_rows(&group, &Rc::default(), config, save, None);
    group
}

/// Change the commands, write them and draw their rows again, the one at the index opened.
type Redo = dyn Fn(&dyn Fn(&mut Vec<CustomCommand>), Option<usize>);

/// Put `group`'s rows in `shown` back from the config, the command at `open` opened: after an
/// add, a removal or a new shortcut, so each row's index is the command's again. Weak: the rows'
/// handlers hold what redraws them.
fn command_rows(
    group: &adw::PreferencesGroup,
    shown: &Rc<RefCell<Vec<glib::WeakRef<gtk::Widget>>>>,
    config: &Rc<RefCell<Config>>,
    save: &Rc<dyn Fn()>,
    open: Option<usize>,
) {
    for row in shown.borrow_mut().drain(..).filter_map(|row| row.upgrade()) {
        group.remove(&row);
    }
    // What the rows' buttons do: change the config, write it and draw the rows again.
    let redo: Rc<Redo> = Rc::new({
        let (group, shown) = (group.downgrade(), shown.clone());
        let (config, save) = (config.clone(), save.clone());
        move |change, open| {
            change(&mut config.borrow_mut().commands);
            save();
            if let Some(group) = group.upgrade() {
                command_rows(&group, &shown, &config, &save, open);
            }
        }
    });
    let count = config.borrow().commands.len();
    for i in 0..count {
        let row = command_row(config, save, &redo, i);
        row.set_expanded(open == Some(i));
        group.add(&row);
        shown
            .borrow_mut()
            .push(row.upcast_ref::<gtk::Widget>().downgrade());
    }
    let add = adw::ButtonRow::builder()
        .title("Add Command")
        .start_icon_name("list-add-symbolic")
        .build();
    add.connect_activated(move |_| {
        let new = CustomCommand {
            name: "New Command".to_string(),
            ..CustomCommand::default()
        };
        redo(&|commands| commands.push(new.clone()), Some(count));
    });
    group.add(&add);
    shown
        .borrow_mut()
        .push(add.upcast_ref::<gtk::Widget>().downgrade());
}

/// The command at `i`: its name and command line edited as typed, its shortcut recorded as the
/// palette records one, refusing a chord in use by name, and a clash from a hand edit marked.
fn command_row(
    config: &Rc<RefCell<Config>>,
    save: &Rc<dyn Fn()>,
    redo: &Rc<Redo>,
    i: usize,
) -> adw::ExpanderRow {
    let command = config.borrow().commands[i].clone();
    // Names and command lines are plain text, not Pango markup.
    let row = adw::ExpanderRow::builder()
        .title(&command.name)
        .subtitle(&command.run)
        .subtitle_lines(1)
        .use_markup(false)
        .build();
    type Field = fn(&mut CustomCommand) -> &mut String;
    let fields: [(&str, &str, Field); 2] = [
        ("Name", &command.name, |c| &mut c.name),
        ("Command", &command.run, |c| &mut c.run),
    ];
    for (title, value, field) in fields {
        let entry = adw::EntryRow::builder().title(title).text(value).build();
        // ponytail: written on every keystroke, as the vault's folder rows are.
        entry.connect_changed({
            let (config, save, row) = (config.clone(), save.clone(), row.downgrade());
            move |entry| {
                let text = entry.text().to_string();
                if let Some(command) = config.borrow_mut().commands.get_mut(i) {
                    *field(command) = text.clone();
                }
                if let Some(row) = row.upgrade() {
                    match title {
                        "Name" => row.set_title(&text),
                        _ => row.set_subtitle(&text),
                    }
                }
                save();
            }
        });
        row.add_row(&entry);
    }

    let action = crate::commands::action(i);
    let accels = crate::commands::accels(&config.borrow(), i);
    let taken = crate::commands::taken(&config.borrow(), Some(i));
    let clashes = accels
        .iter()
        .filter(|accel| taken.iter().any(|(a, _)| a == *accel))
        .cloned()
        .collect();
    let record: Rc<dyn Fn(&str)> = Rc::new({
        let (config, redo, row) = (config.clone(), redo.clone(), row.downgrade());
        move |_: &str| {
            let Some(row) = row.upgrade() else {
                return;
            };
            let name = config.borrow().commands.get(i).map(|c| c.name.clone());
            let taken = crate::commands::taken(&config.borrow(), Some(i));
            let redo = redo.clone();
            crate::palette::capture_shortcut(
                row.upcast_ref(),
                &name.unwrap_or_default(),
                taken,
                move |chosen| {
                    let chord = chosen.and_then(|accels| accels.into_iter().next());
                    redo(
                        &|commands| {
                            if let Some(command) = commands.get_mut(i) {
                                command.shortcut = chord.clone();
                            }
                        },
                        Some(i),
                    );
                },
            );
        }
    });
    let shortcut = adw::ActionRow::builder().title("Shortcut").build();
    shortcut.add_suffix(&crate::palette::accel_button(
        &action, &accels, &clashes, &record,
    ));
    row.add_row(&shortcut);

    let remove = adw::ButtonRow::builder().title("Remove Command").build();
    remove.add_css_class("destructive-action");
    remove.connect_activated({
        let redo = redo.clone();
        move |_| {
            redo(
                &|commands| {
                    if i < commands.len() {
                        commands.remove(i);
                    }
                },
                None,
            )
        }
    });
    row.add_row(&remove);
    row
}

// ------------------------------------------------------------------------------- this vault

fn vault_group(
    config: &Rc<RefCell<Config>>,
    root: &Path,
    save: &Rc<dyn Fn()>,
) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title("This Vault").build();
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
        Some("Leave empty to use the vault root"),
        &vault.new_file_dir,
        config,
        root,
        save,
        |cfg, text| cfg.new_file_dir = folder(text),
    ));
    // Kept as typed but for spaces at the ends: `/` is the vault root and `./` the note's folder.
    group.add(&entry_row(
        "Attachments Folder",
        Some("Folder for pasted and dropped images. Leave empty for the note's folder, or use ./name for a subfolder"),
        &vault.attachment_folder,
        config,
        root,
        save,
        |cfg, text| cfg.attachment_folder = text.trim().to_string(),
    ));

    group
}

// ---------------------------------------------------------------------------- restore defaults

/// Confirm resetting settings because it changes values beyond those shown; name preserved settings.
fn reset_group(
    dialog: &adw::PreferencesDialog,
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
        // the cycle that would keep the dialog alive past its window. Nothing here holds the
        // window either: the window holds the dialog.
        let dialog = dialog.downgrade();
        move |_| {
            let Some(dialog) = dialog.upgrade() else {
                return;
            };
            let (config, root, save, on_change) = (
                config.clone(),
                root.clone(),
                save.clone(),
                on_change.clone(),
            );
            let weak = dialog.downgrade();
            crate::dialogs::confirm(
                &dialog,
                "Restore Default Preferences?",
                "Reset appearance, editor settings, vault folders and shortcuts. \
                 Recent vaults and custom commands are kept.",
                "Restore",
                true,
                move || {
                    let Some(dialog) = weak.upgrade() else {
                        return;
                    };
                    {
                        let mut cfg = config.borrow_mut();
                        let keep = std::mem::take(&mut cfg.recent_vaults);
                        let commands = std::mem::take(&mut cfg.commands);
                        *cfg = Config {
                            recent_vaults: keep,
                            commands,
                            ..Config::default()
                        };
                    }
                    save();
                    fill(&dialog, &config, &root, &on_change);
                },
            );
        }
    });
    group.add(&row);
    group
}

/// Bind an entry to vault config through `set`, preserving typed text and caret position.
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

/// Trim whitespace and surrounding `/` to keep folder settings vault-relative.
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

    #[test]
    fn only_what_a_row_shows_needs_a_new_page() {
        let before = Config::default();
        let mut used = before.clone();
        used.recent_vaults.push("/elsewhere".into());
        used.drawing.pen_width += 1.0;
        used.diagram.grid = !used.diagram.grid;
        assert_eq!(on_page(&used), on_page(&before));
        for shown in [
            |c: &mut Config| c.minimap = !c.minimap,
            |c: &mut Config| c.drawing.mouse = !c.drawing.mouse,
        ] {
            let mut moved = before.clone();
            shown(&mut moved);
            assert_ne!(on_page(&moved), on_page(&before));
        }
    }
}
