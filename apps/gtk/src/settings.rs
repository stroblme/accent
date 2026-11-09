//! Preferences with immediate saving and change notifications, following GNOME convention.
//!
//! Shared config, vault and `on_change` are supplied by the caller, avoiding a dependency
//! on `App`.

use crate::editors;
use accent_core::config::{
    Config, CustomCommand, DebugTerminal, Editor, FocusMode, Theme, VaultConfig,
};
use adw::prelude::*;
use gtk::{gio, glib, pango};
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
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
    rows: Rc<Rows>,
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
        let rows = fill(&dialog, &config, &root, &on_change);

        // A config taken in from the file sets the rows whose values it moved, as a reset does.
        // The action hangs off the dialog, and the rows hold their widgets weakly.
        let action = gio::SimpleAction::new("reload", None);
        action.connect_activate({
            let rows = rows.clone();
            move |_, _| rows.refresh()
        });
        let actions = gio::SimpleActionGroup::new();
        actions.add_action(&action);
        dialog.insert_action_group("preferences", Some(&actions));
        crate::dialogs::close_on_outside_press(dialog.upcast_ref());
        Preferences { dialog, rows }
    }

    /// Show the dialog, its rows set to the config first: a value moved while it was shut, by a
    /// hand edit or the palette, shows as it now is.
    pub fn present(&self, parent: &impl IsA<gtk::Widget>) {
        self.rows.refresh();
        self.dialog.present(Some(parent));
    }
}

/// What the rows are built from and write to, and how each is set from the config again. A row
/// reads its value as it is built and follows the config from then on, so a value moved elsewhere
/// changes only the rows showing it, where a new page of rows cost an opening as much as the
/// first one did (`ACCENT_BENCH_SWITCHER=prefs`).
struct Rows {
    config: Rc<RefCell<Config>>,
    save: Rc<dyn Fn()>,
    /// Set while the rows are set from the config, when what a row's handler would write is
    /// already there.
    quiet: Cell<bool>,
    /// What sets each row from the config, holding it weakly: its handlers hold these rows.
    setters: RefCell<Vec<Setter>>,
}

/// Sets a row from the config.
type Setter = Box<dyn Fn(&mut Config)>;

impl Rows {
    /// Change the config as a row asks and write it, unless the row is being set from it.
    fn set(&self, change: impl FnOnce(&mut Config)) {
        if self.quiet.get() {
            return;
        }
        change(&mut self.config.borrow_mut());
        (self.save)();
    }

    /// Set `row` from the config through `show` at each [`refresh`](Self::refresh).
    fn follow<W: IsA<glib::Object>>(&self, row: &W, show: impl Fn(&W, &mut Config) + 'static) {
        let row = row.downgrade();
        self.setters.borrow_mut().push(Box::new(move |config| {
            if let Some(row) = row.upgrade() {
                show(&row, config);
            }
        }));
    }

    /// Set every row to what the config holds, writing nothing back. A row given the value it
    /// shows changes nothing, so only the rows whose value moved are drawn again.
    fn refresh(&self) {
        let mut config = self.config.borrow().clone();
        self.quiet.set(true);
        for set in self.setters.borrow().iter() {
            set(&mut config);
        }
        self.quiet.set(false);
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

/// The action a Preferences dialog answers by setting its rows from the config ([`reload`]).
const RELOAD: &str = "preferences.reload";

/// Set an open Preferences dialog's rows from a config taken in from the file. Ignore dialogs
/// without the reload action.
pub fn reload(dialog: &adw::Dialog) {
    let _ = dialog.activate_action(RELOAD, None);
}

/// How many groups the dialog's first opening shows in the frame that shows it: Appearance and
/// Match Colours, what the top of the dialog holds.
const SHOWN_FIRST: usize = 2;

/// Build the dialog's one page from `config`; from then on its rows follow the config ([`Rows`]).
fn fill(
    dialog: &adw::PreferencesDialog,
    config: &Rc<RefCell<Config>>,
    root: &Option<PathBuf>,
    on_change: &Rc<dyn Fn(&Config)>,
) -> Rc<Rows> {
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
    let rows = Rc::new(Rows {
        config: config.clone(),
        save,
        quiet: Cell::new(false),
        setters: RefCell::default(),
    });

    let mut groups = vec![
        appearance_group(&rows),
        recolour_group(&rows),
        editor_group(&rows),
        files_group(&rows),
        git_group(&rows),
        pdf_group(&rows),
        terminal_group(&rows),
        commands_group(&rows),
    ];
    if let Some(root) = root {
        groups.push(vault_group(&rows, root));
    }
    groups.push(reset_group(dialog, &rows));
    let page = adw::PreferencesPage::new();
    for group in &groups {
        page.add(group);
    }
    stage(&page, &groups[SHOWN_FIRST..]);
    crate::widgets::smooth_wheels(page.upcast_ref());
    dialog.add(&page);
    rows
}

/// Hide `later` until the page's first frame is drawn, then show one a frame. GTK styles and lays
/// out a page never shown in the frame that shows it, which held the dialog's first opening about
/// three times as long as a reopening before its animation could run (main-thread CPU,
/// `ACCENT_BENCH_SWITCHER=prefs`); the groups below the first screen come in while it fades in.
fn stage(page: &adw::PreferencesPage, later: &[adw::PreferencesGroup]) {
    for group in later {
        group.set_visible(false);
    }
    let later: RefCell<Vec<_>> = RefCell::new(later.iter().rev().map(|g| g.downgrade()).collect());
    let first = Cell::new(true);
    // A tick callback runs from the frame that shows the page first, its first opening.
    page.add_tick_callback(move |_, _| {
        if first.replace(false) {
            return glib::ControlFlow::Continue;
        }
        match later.borrow_mut().pop() {
            Some(group) => {
                if let Some(group) = group.upgrade() {
                    group.set_visible(true);
                }
                glib::ControlFlow::Continue
            }
            None => glib::ControlFlow::Break,
        }
    });
}

// ------------------------------------------------------------------------------- appearance

fn appearance_group(rows: &Rc<Rows>) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title("Appearance").build();

    let chosen = rows.config.borrow().theme;
    let names: Vec<&str> = THEMES.iter().map(|(_, name)| *name).collect();
    let row = adw::ComboRow::builder()
        .title("Theme")
        .subtitle("Solarized follows system appearance")
        .model(&gtk::StringList::new(&names))
        .selected(index_of(chosen))
        .build();
    row.connect_selected_notify({
        let rows = rows.clone();
        move |r| {
            let Some((theme, _)) = THEMES.get(r.selected() as usize) else {
                return;
            };
            rows.set(|c| c.theme = *theme);
        }
    });
    rows.follow(&row, |row, c| row.set_selected(index_of(c.theme)));
    group.add(&row);

    // The subtitle follows the pick, so the row says what the chosen level will fade.
    let chosen = focus_index(rows.config.borrow().focus_mode);
    let names: Vec<&str> = FOCUS_MODES.iter().map(|(_, name, _)| *name).collect();
    let focus = adw::ComboRow::builder()
        .title("Focus Mode")
        .subtitle(FOCUS_MODES[chosen as usize].2)
        .model(&gtk::StringList::new(&names))
        .selected(chosen)
        .build();
    focus.connect_selected_notify({
        let rows = rows.clone();
        move |r| {
            let Some((mode, _, what)) = FOCUS_MODES.get(r.selected() as usize) else {
                return;
            };
            r.set_subtitle(what);
            rows.set(|c| c.focus_mode = *mode);
        }
    });
    rows.follow(&focus, |row, c| row.set_selected(focus_index(c.focus_mode)));
    group.add(&focus);
    group
}

/// A switch for each kind of file a dark or Solarized theme recolours. Off, a kind's files show as
/// they are, and its Invert command recolours one of them instead.
fn recolour_group(rows: &Rc<Rows>) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("Match Colours to Theme")
        .build();
    let switches: [(&str, Option<&str>, Switch); 3] = [
        (
            "Images",
            Some("Scans, plots and screenshots. Photos keep their colours"),
            |c| &mut c.recolour.images,
        ),
        ("PDFs", None, |c| &mut c.recolour.pdfs),
        ("Diagrams", Some("On the canvas and in notes"), |c| {
            &mut c.recolour.diagrams
        }),
    ];
    for (title, subtitle, field) in switches {
        group.add(&switch_row(rows, title, subtitle, field));
    }
    group
}

/// A switch's value in the config.
type Switch = fn(&mut Config) -> &mut bool;

/// A switch row for `field`.
fn switch_row(
    rows: &Rc<Rows>,
    title: &str,
    subtitle: Option<&str>,
    field: Switch,
) -> adw::SwitchRow {
    let row = adw::SwitchRow::builder()
        .title(title)
        .active(*field(&mut rows.config.borrow_mut()))
        .build();
    if let Some(subtitle) = subtitle {
        row.set_subtitle(subtitle);
    }
    row.connect_active_notify({
        let rows = rows.clone();
        move |r| rows.set(|c| *field(c) = r.is_active())
    });
    rows.follow(&row, move |row, c| row.set_active(*field(c)));
    row
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
fn text_editor_row(rows: &Rc<Rows>) -> adw::ComboRow {
    let row = adw::ComboRow::builder()
        .title("Text Editor")
        .subtitle("A terminal editor opens files in a terminal tab")
        .build();
    // What each line of the model picks, and whether the model is being replaced, whose
    // selection changes say nothing about the reader's pick.
    let listed: Rc<RefCell<Vec<Editor>>> = Rc::default();
    let filling = Rc::new(Cell::new(false));
    // The shell's answer, once it has given one, which a pick moved elsewhere is listed against.
    let found: Rc<RefCell<Option<Vec<editors::Found>>>> = Rc::default();
    let fill = Rc::new({
        let (config, listed, filling) = (rows.config.clone(), listed.clone(), filling.clone());
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
        let (rows, listed) = (rows.clone(), listed.clone());
        move |r| {
            let picked = listed.borrow().get(r.selected() as usize).copied();
            if filling.get() || picked.is_none_or(|e| e == rows.config.borrow().editor) {
                return;
            }
            rows.set(|c| c.editor = picked.unwrap_or_default());
        }
    });
    rows.follow(&row, {
        let (fill, found) = (fill.clone(), found.clone());
        move |row, c| {
            if listed.borrow().get(row.selected() as usize) != Some(&c.editor) {
                fill(row, found.borrow().as_deref());
            }
        }
    });
    let asked = Cell::new(false);
    row.connect_map(move |row| {
        if asked.replace(true) {
            return;
        }
        let (row, fill, found) = (row.downgrade(), fill.clone(), found.clone());
        editors::installed(None, move |answer| {
            found.replace(Some(answer.to_vec()));
            if let Some(row) = row.upgrade() {
                fill(&row, Some(answer));
            }
        });
    });
    row
}

fn editor_group(rows: &Rc<Rows>) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title("Editor").build();

    group.add(&text_editor_row(rows));
    group.add(&font_row(
        rows,
        "Document Font",
        |c| &mut c.editor_font,
        crate::editor::default_font,
    ));
    group.add(&font_row(
        rows,
        "Preview Font",
        |c| &mut c.preview_font,
        crate::platform::document_font,
    ));

    // A share rather than a pixel count: the same setting has to read the same on a laptop and on
    // a wide monitor, and the editor is not the window (the sidebar and the preview take theirs).
    // The minimum is 30 % because below that the editor's own floor takes over and the number
    // would stop meaning anything.
    group.add(&spin_row(
        rows,
        (
            "Column Width",
            "Document width as a percentage of the editor",
        ),
        (30.0, 100.0, 5.0),
        |c| &mut c.column_width,
    ));
    group.add(&switch_row(
        rows,
        "Full-Width Preview",
        Some("Use the full preview width"),
        |c| &mut c.full_width_preview,
    ));
    // Columns rather than a choice of 2/4/8: the number is what a tab character is worth on
    // screen, and a language whose house style is three of them is not ours to rule out.
    group.add(&spin_row(
        rows,
        ("Indent Width", "Columns per indent"),
        (1.0, 16.0, 1.0),
        |c| &mut c.indent_width,
    ));
    let switches: [(&str, &str, Switch); 5] = [
        (
            "Spell Checking",
            "Underline misspelled words as you type",
            |c| &mut c.spellcheck,
        ),
        (
            "Word Suggestions",
            "Suggest words from the document and dictionary",
            |c| &mut c.word_suggestions,
        ),
        (
            "Ghost Text",
            "Complete lines using your vault. Press Tab to accept",
            |c| &mut c.ghost_text,
        ),
        (
            "Line Numbers",
            "Show line numbers beside the document",
            |c| &mut c.line_numbers,
        ),
        (
            "Minimap",
            "Replace the scrollbar with a document overview",
            |c| &mut c.minimap,
        ),
    ];
    for (title, subtitle, field) in switches {
        group.add(&switch_row(rows, title, Some(subtitle), field));
    }
    group
}

/// A spin row for `field`, titled and subtitled, over a range and step.
fn spin_row(
    rows: &Rc<Rows>,
    (title, subtitle): (&str, &str),
    (min, max, step): (f64, f64, f64),
    field: fn(&mut Config) -> &mut u32,
) -> adw::SpinRow {
    let row = adw::SpinRow::with_range(min, max, step);
    row.set_title(title);
    row.set_subtitle(subtitle);
    row.set_value(f64::from(*field(&mut rows.config.borrow_mut())));
    row.connect_value_notify({
        let rows = rows.clone();
        move |r| rows.set(|c| *field(c) = r.value().round() as u32)
    });
    rows.follow(&row, move |row, c| row.set_value(f64::from(*field(c))));
    row
}

/// A font row: `field` of the config, a font button and a Reset back to `default`, which
/// `None` stands for.
fn font_row(
    rows: &Rc<Rows>,
    title: &str,
    field: fn(&mut Config) -> &mut Option<String>,
    default: fn() -> String,
) -> adw::ActionRow {
    let chosen = field(&mut rows.config.borrow_mut()).clone();
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

    // Set while the button is handed a font rather than one picked in it, the system font by
    // Reset or the config's by a refresh, so the ::font-desc notification that follows is not
    // stored as a deliberate choice.
    let setting = Rc::new(Cell::new(false));
    // The font the row shows, `None` for the default, which a refresh sets the row from the
    // config only where it differs from: a family drawn in another one's place is not a change.
    let shown = Rc::new(RefCell::new(chosen));

    button.connect_font_desc_notify({
        let (rows, row, reset, setting, shown) = (
            rows.clone(),
            row.clone(),
            reset.clone(),
            setting.clone(),
            shown.clone(),
        );
        move |b| {
            let Some(desc) = b.font_desc().filter(|_| !setting.get()) else {
                return;
            };
            let font = desc.to_str().to_string();
            shown.replace(Some(font.clone()));
            row.set_subtitle(&font_subtitle(Some(&font), default));
            reset.set_visible(true);
            rows.set(|c| *field(c) = Some(font));
        }
    });

    reset.connect_clicked({
        let (rows, row, button, setting, shown) = (
            rows.clone(),
            row.clone(),
            button.clone(),
            setting.clone(),
            shown.clone(),
        );
        move |b| {
            shown.replace(None);
            // ponytail: GtkFontDialogButton has no "unset", so Reset shows the system font while
            // the config says None. Both mean the same thing to the view.
            setting.set(true);
            button.set_font_desc(&drawn(&button, &default()));
            setting.set(false);
            row.set_subtitle(&font_subtitle(None, default));
            // The keyboard stays in the row rather than leaving with the hidden button.
            button.grab_focus();
            b.set_visible(false);
            rows.set(|c| *field(c) = None);
        }
    });

    rows.follow(&row, {
        let (button, reset) = (button.downgrade(), reset.downgrade());
        move |row, c| {
            let font = field(c).clone();
            let (Some(button), Some(reset)) = (button.upgrade(), reset.upgrade()) else {
                return;
            };
            if *shown.borrow() == font {
                return;
            }
            setting.set(true);
            button.set_font_desc(&drawn(&button, &font.clone().unwrap_or_else(default)));
            setting.set(false);
            row.set_subtitle(&font_subtitle(font.as_deref(), default));
            reset.set_visible(font.is_some());
            shown.replace(font);
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

fn files_group(rows: &Rc<Rows>) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title("Files").build();

    // How the tree lists, which is no single row's business — so it is a preference rather than
    // an item on the menu a right-click over one row opens. `win.show-hidden-files` in the
    // palette toggles the same value.
    group.add(&switch_row(
        rows,
        "Show Hidden Files",
        Some("Show files and folders with names starting with a dot"),
        |c| &mut c.show_hidden,
    ));
    // Every vault's: the sort itself is the vault's, picked on the tree's Sort Files menu.
    group.add(&switch_row(
        rows,
        "Sort Folders Among Files",
        Some("Order folders like files instead of keeping them on top"),
        |c| &mut c.folders_among_files,
    ));
    group.add(&switch_row(
        rows,
        "Colour Changed Files",
        Some("Use the accent colour for changes and green for new files"),
        |c| &mut c.git_tint,
    ));
    group
}

// -------------------------------------------------------------------------------------- git

fn git_group(rows: &Rc<Rows>) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title("Git").build();
    group.add(&switch_row(
        rows,
        "Group Changes by Folder",
        Some("Organise changes by folder in the Git pane"),
        |c| &mut c.git_tree,
    ));
    group
}

// -------------------------------------------------------------------------------------- pdf

fn pdf_group(rows: &Rc<Rows>) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title("PDF").build();
    group.add(&switch_row(
        rows,
        "Draw with the Mouse",
        Some("Even when a pen is attached"),
        |c| &mut c.drawing.mouse,
    ));
    group
}

// --------------------------------------------------------------------------------- terminal

fn terminal_group(rows: &Rc<Rows>) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title("Terminal").build();

    // Off, a focused shell leaves the window a small reserved set of chords (`actions::kept`).
    group.add(&switch_row(
        rows,
        "Forward All Keys",
        Some("Send all keys except copy, paste and Ctrl+Page Up/Down"),
        |c| &mut c.forward_keys_to_terminal,
    ));

    let at = |place: DebugTerminal| {
        (DEBUG_TERMINALS.iter())
            .position(|(p, _)| *p == place)
            .unwrap_or(0) as u32
    };
    let names: Vec<&str> = DEBUG_TERMINALS.iter().map(|(_, name)| *name).collect();
    let place = adw::ComboRow::builder()
        .title("Debug Terminal")
        .subtitle("Pane for the debug terminal and console")
        .model(&gtk::StringList::new(&names))
        .selected(at(rows.config.borrow().debug_terminal))
        .build();
    place.connect_selected_notify({
        let rows = rows.clone();
        move |r| {
            if let Some((place, _)) = DEBUG_TERMINALS.get(r.selected() as usize) {
                rows.set(|c| c.debug_terminal = *place);
            }
        }
    });
    rows.follow(&place, move |row, c| row.set_selected(at(c.debug_terminal)));
    group.add(&place);

    group
}

// --------------------------------------------------------------------------------- commands

/// The user's own commands (`[[commands]]`): a row each, its name over its command line, opening
/// on the two, its shortcut and Remove; Add Command puts a new one at the end, opened.
fn commands_group(rows: &Rc<Rows>) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title("Commands").build();
    let shown: Rc<RefCell<Drawn>> = Rc::default();
    command_rows(&group, &shown, rows, None);
    // Drawn again only once the commands or a shortcut moved elsewhere, so an opened row stays
    // open. Weak: the rows hold this.
    let weak = Rc::downgrade(rows);
    rows.follow(&group, move |group, c| {
        if let Some(rows) = weak.upgrade()
            && shown.borrow().from != drawn_from(c)
        {
            command_rows(group, &shown, &rows, None);
        }
    });
    group
}

/// The command rows on the page, and what of the config they show.
#[derive(Default)]
struct Drawn {
    rows: Vec<glib::WeakRef<gtk::Widget>>,
    from: (Vec<CustomCommand>, BTreeMap<String, Vec<String>>),
}

/// What the command rows show: the commands, and the shortcuts a clash is marked against.
fn drawn_from(config: &Config) -> (Vec<CustomCommand>, BTreeMap<String, Vec<String>>) {
    (config.commands.clone(), config.shortcuts.clone())
}

/// Change the commands, write them and draw their rows again, the one at the index opened.
type Redo = dyn Fn(&dyn Fn(&mut Vec<CustomCommand>), Option<usize>);

/// Put `group`'s rows in `shown` back from the config, the command at `open` opened: after an
/// add, a removal or a new shortcut, so each row's index is the command's again. Weak: the rows'
/// handlers hold what redraws them.
fn command_rows(
    group: &adw::PreferencesGroup,
    shown: &Rc<RefCell<Drawn>>,
    rows: &Rc<Rows>,
    open: Option<usize>,
) {
    let drawn = std::mem::take(&mut shown.borrow_mut().rows);
    for row in drawn.iter().filter_map(|row| row.upgrade()) {
        group.remove(&row);
    }
    shown.borrow_mut().from = drawn_from(&rows.config.borrow());
    // What the rows' buttons do: change the config, write it and draw the rows again.
    let redo: Rc<Redo> = Rc::new({
        let (group, shown, rows) = (group.downgrade(), shown.clone(), rows.clone());
        move |change, open| {
            rows.set(|c| change(&mut c.commands));
            if let Some(group) = group.upgrade() {
                command_rows(&group, &shown, &rows, open);
            }
        }
    });
    let count = rows.config.borrow().commands.len();
    for i in 0..count {
        let row = command_row(rows, shown, &redo, i);
        row.set_expanded(open == Some(i));
        group.add(&row);
        shown
            .borrow_mut()
            .rows
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
        .rows
        .push(add.upcast_ref::<gtk::Widget>().downgrade());
}

/// The command at `i`: its name and command line edited as typed, its shortcut recorded as the
/// palette records one, refusing a chord in use by name, and a clash from a hand edit marked.
fn command_row(
    rows: &Rc<Rows>,
    shown: &Rc<RefCell<Drawn>>,
    redo: &Rc<Redo>,
    i: usize,
) -> adw::ExpanderRow {
    let config = &rows.config;
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
            let (rows, shown, row) = (rows.clone(), shown.clone(), row.downgrade());
            move |entry| {
                let text = entry.text().to_string();
                if let Some(row) = row.upgrade() {
                    match title {
                        "Name" => row.set_title(&text),
                        _ => row.set_subtitle(&text),
                    }
                }
                rows.set(|c| {
                    if let Some(command) = c.commands.get_mut(i) {
                        *field(command) = text;
                    }
                });
                shown.borrow_mut().from = drawn_from(&rows.config.borrow());
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

fn vault_group(rows: &Rc<Rows>, root: &Path) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title("This Vault").build();
    group.add(&entry_row(
        rows,
        ("Templates Folder", None),
        root,
        |vault| &vault.templates_dir,
        |vault, text| vault.templates_dir = folder(text),
    ));
    group.add(&entry_row(
        rows,
        (
            "New Files Folder",
            Some("Leave empty to use the vault root"),
        ),
        root,
        |vault| &vault.new_file_dir,
        |vault, text| vault.new_file_dir = folder(text),
    ));
    // Kept as typed but for spaces at the ends: `/` is the vault root and `./` the note's folder.
    group.add(&entry_row(
        rows,
        ("Attachments Folder", Some("Folder for pasted and dropped images. Leave empty for the note's folder, or use ./name for a subfolder")),
        root,
        |vault| &vault.attachment_folder,
        |vault, text| vault.attachment_folder = text.trim().to_string(),
    ));
    group
}

// ---------------------------------------------------------------------------- restore defaults

/// Confirm resetting settings because it changes values beyond those shown; name preserved settings.
fn reset_group(dialog: &adw::PreferencesDialog, rows: &Rc<Rows>) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::new();
    let row = adw::ButtonRow::builder().title("Restore Defaults").build();
    row.add_css_class("destructive-action");
    row.connect_activated({
        let rows = rows.clone();
        // Weak: this closure hangs off a row inside the dialog, and a strong handle back to it is
        // the cycle that would keep the dialog alive past its window. Nothing here holds the
        // window either: the window holds the dialog.
        let dialog = dialog.downgrade();
        move |_| {
            let Some(dialog) = dialog.upgrade() else {
                return;
            };
            let rows = rows.clone();
            crate::dialogs::confirm(
                &dialog,
                "Restore Default Preferences?",
                "Reset appearance, editor settings, vault folders and shortcuts. \
                 Recent vaults and custom commands are kept.",
                "Restore",
                true,
                move || {
                    rows.set(|cfg| {
                        let keep = std::mem::take(&mut cfg.recent_vaults);
                        let commands = std::mem::take(&mut cfg.commands);
                        *cfg = Config {
                            recent_vaults: keep,
                            commands,
                            ..Config::default()
                        };
                    });
                    rows.refresh();
                },
            );
        }
    });
    group.add(&row);
    group
}

/// Bind an entry to the vault's config: `get` reads its value and `set` writes what is typed,
/// preserving typed text and caret position.
fn entry_row(
    rows: &Rc<Rows>,
    (title, tooltip): (&str, Option<&str>),
    root: &Path,
    get: fn(&VaultConfig) -> &String,
    set: fn(&mut VaultConfig, &str),
) -> adw::EntryRow {
    let vault = rows.config.borrow().vault(root);
    let row = adw::EntryRow::builder()
        .title(title)
        .text(get(&vault))
        .build();
    row.set_tooltip_text(tooltip);

    let root = root.to_path_buf();
    // ponytail: writes and saves on every keystroke. The file is about a kilobyte and this is a
    // dialog, so a debounce timer per row would cost more than it saves; add one if typing here
    // ever stutters on a slow disk.
    row.connect_changed({
        let (rows, root) = (rows.clone(), root.clone());
        move |r| {
            let text = r.text().to_string();
            rows.set(|cfg| {
                let mut vault = cfg.vault(&root);
                set(&mut vault, &text);
                cfg.set_vault(&root, vault);
            });
        }
    });
    // Set only where what the row says no longer comes to the config's value, so a folder typed
    // as `/Daily/` stays as typed.
    rows.follow(&row, move |row, c| {
        let vault = c.vault(&root);
        let mut typed = vault.clone();
        set(&mut typed, &row.text());
        if typed != vault {
            row.set_text(get(&vault));
        }
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
}
