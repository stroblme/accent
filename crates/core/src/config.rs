//! Global settings and per-vault session state.
//!
//! Settings are global only: nothing is ever written into the vault. A vault is a plain folder
//! that Syncthing and git already own, so an app config file dropped in it would sync accent's
//! preferences to every device and show up as noise in the tree.
//!
//! The config file lives at `~/.config/accent/config.toml`; [`Config`] documents it with a
//! worked example.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The documented example. A macro rather than a `const` because only a literal may be pasted
/// into a doc attribute, and the test below parses the same text the docs show.
macro_rules! example {
    () => {
        r#"recent_vaults = ["/home/me/Notes"]
spellcheck = true
ghost_text = true
minimap = false
line_numbers = false
git_tree = true
column_width = 50
theme = "solarized"
focus_mode = "high"

[shortcuts]
"win.find-next" = ["F3"]
"win.about" = []

[search]
exclude = ["Archive", "Code/vendor"]

[drawing]
mouse = false
pen_width = 2.0
highlighter_width = 14.0
highlighter_color = [0, 0, 0]
eraser_radius = 4.0
eraser_partial = true

[vaults."/home/me/Notes"]
templates_dir = "Templates"
new_file_dir = "Inbox"

[vaults."/home/me/Notes".lsp.servers]
python3 = ["pylsp"]
"#
    };
}

/// Which colours the window paints itself in. System, light and dark are libadwaita's own;
/// Solarized is a palette of ours that still follows the system's light and dark state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
    Solarized,
}

/// How much of the window recedes while the user types (DESIGN.md, Chrome auto-hide).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FocusMode {
    /// Nothing fades.
    None,
    /// The bars, the sidebar and the minimap fade away.
    #[default]
    Medium,
    /// As Medium, and the other panes and the text away from the caret fade too.
    High,
}

/// The global config file.
#[doc = concat!("\n```toml\n", example!(), "```")]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Most recent first, capped at 10.
    pub recent_vaults: Vec<PathBuf>,
    /// `None` follows the GNOME document font.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub editor_font: Option<String>,
    pub spellcheck: bool,
    /// Suggest the rest of the line as the caret sits, from what the vault already says. Needs
    /// `merl-rt` on the path; the switch only says whether to ask for it.
    pub ghost_text: bool,
    /// A code map beside the document instead of the scrollbar.
    pub minimap: bool,
    /// Numbers in the editor's left gutter. Off by default: a note is prose, and the gutter is
    /// what an ATX heading's markers hang in.
    pub line_numbers: bool,
    /// Group the Git pane's changed files by folder rather than listing them flat. On by default:
    /// a vault's changes arrive a folder at a time, and a flat list of thirty repeats the same
    /// directory thirty times.
    pub git_tree: bool,
    /// How much of the editor's width the document column may fill, as a percentage. 50 is what
    /// the fixed 800 px cap came to on a maximised window; the editor floors it so a narrow
    /// window keeps a readable line.
    pub column_width: u32,
    pub theme: Theme,
    pub focus_mode: FocusMode,
    /// Accelerator overrides, keyed by full action name ("win.save"). Only what the user changed
    /// is stored, so the built-in table stays the source of truth for everything else; an empty
    /// list means the action is deliberately unbound.
    pub shortcuts: BTreeMap<String, Vec<String>>,
    pub search: SearchConfig,
    pub drawing: DrawingConfig,
    /// Keyed by canonical vault path.
    pub vaults: BTreeMap<String, VaultConfig>,
}

/// What search leaves out on the user's say-so.
///
/// The one place a directory can be named. It joins what git ignores in the index's exclusion
/// column rather than replacing it, so `All` still reaches these directories, the file tree still
/// lists them dimmed, and — DESIGN.md, Sidebar — a note inside one is still found, because that
/// column is never applied to markdown. Naming a directory here is therefore a way to quieten
/// search, never a way to hide a note.
///
/// It is deliberately not a walk-level skip: [`crate::walk`]'s own skips keep whole dependency
/// trees out of the index and out of the kernel's watch budget, which is a different question
/// from "I would rather not see this folder in my results".
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SearchConfig {
    /// Vault-relative directory paths, applied to every vault; one that names nothing in this
    /// vault costs nothing. A path, not a glob: `Archive` is the top-level folder of that name,
    /// and `Code/vendor` is the one inside `Code`.
    pub exclude: Vec<String>,
}

/// How the PDF drawing tools behave. Global rather than per vault: a pen is a property of the
/// machine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DrawingConfig {
    /// Draw with the mouse or touchpad even while a pen is attached. Off, the hand selects text
    /// and only the pen draws — until no pen is attached at all, when the hand draws again.
    pub mouse: bool,
    /// Stroke widths in page points; the shapes draw in the pen's.
    pub pen_width: f32,
    pub highlighter_width: f32,
    /// `None` is the system accent, resolved when a stroke is written so the tool keeps following
    /// it; a colour picked on the ring is kept as it was.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pen_color: Option<[u8; 3]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub highlighter_color: Option<[u8; 3]>,
    /// How close the eraser has to pass to a stroke to take it, in page points.
    pub eraser_radius: f32,
    /// Whether the eraser takes only the part of a stroke it passes over, leaving the rest as
    /// strokes of their own, rather than the whole stroke.
    pub eraser_partial: bool,
}

impl Default for DrawingConfig {
    fn default() -> Self {
        DrawingConfig {
            mouse: false,
            pen_width: 2.0,
            // A line of text, near enough.
            highlighter_width: 14.0,
            pen_color: None,
            highlighter_color: None,
            eraser_radius: 4.0,
            eraser_partial: false,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            recent_vaults: Vec::new(),
            editor_font: None,
            spellcheck: true,
            ghost_text: true,
            minimap: false,
            line_numbers: false,
            git_tree: true,
            column_width: 50,
            theme: Theme::System,
            focus_mode: FocusMode::default(),
            shortcuts: BTreeMap::new(),
            search: SearchConfig::default(),
            drawing: DrawingConfig::default(),
            vaults: BTreeMap::new(),
        }
    }
}

/// Per-vault preferences. All paths are vault-relative.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct VaultConfig {
    pub templates_dir: String,
    /// Empty means the vault root.
    pub new_file_dir: String,
    pub lsp: LspConfig,
}

/// Which language server answers for a language, where the built-in choice is not the one
/// wanted. Keyed by GtkSourceView language id (`python3`, `rust`), valued by a command line.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LspConfig {
    pub servers: BTreeMap<String, Vec<String>>,
}

impl Default for VaultConfig {
    fn default() -> Self {
        VaultConfig {
            templates_dir: "Templates".to_string(),
            new_file_dir: String::new(),
            lsp: LspConfig::default(),
        }
    }
}

/// What a window looked like when it was last closed. Cheap to lose, so it lives in the state
/// dir rather than in the config.
/// How a PDF is sized to its window. Here rather than in the app because the session remembers
/// it per document, and the session is core's to write.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum PdfZoom {
    /// The widest page fills the width. What a reader wants for text, so it is the default.
    #[default]
    FitWidth,
    /// The tallest page fits entirely, so one page is one screen.
    FitPage,
    /// A fixed multiple of the page's natural size.
    Scale(f64),
}

/// Where a PDF was last being read.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PdfPlace {
    pub page: usize,
    pub zoom: PdfZoom,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    /// Vault-relative paths of the open tabs.
    pub open: Vec<String>,
    pub active: Option<String>,
    pub sidebar: bool,
    pub sidebar_width: i32,
    pub view: String,
    /// Document zoom, 1.0 being the font as GNOME sets it.
    pub zoom: f64,
    /// Notes opened in this vault, most recent first. Filesystem mtime is what the index can
    /// offer, and it says when a note last changed, not when it was last read; the palette wants
    /// the second, so the window records it.
    pub recent_notes: Vec<String>,
    /// Full action names of the commands run from the palette or a menu, most recent first.
    pub recent_commands: Vec<String>,
    /// Where each PDF this vault has opened was left, keyed by vault-relative path.
    ///
    /// ponytail: never pruned, so a state file grows by an entry per PDF ever opened. Drop the
    /// ones the index no longer has the day that is measurable.
    pub pdf: BTreeMap<String, PdfPlace>,
}

impl Default for Session {
    fn default() -> Self {
        Session {
            open: Vec::new(),
            active: None,
            sidebar: true,
            sidebar_width: 280,
            view: "editor".to_string(),
            zoom: 1.0,
            recent_notes: Vec::new(),
            recent_commands: Vec::new(),
            pdf: BTreeMap::new(),
        }
    }
}

impl Config {
    /// Never fails: a missing or broken file logs and yields the defaults, because a typo in a
    /// hand-edited config must not keep the app from starting.
    ///
    /// A file that exists but does not parse is moved to `config.toml.broken` first. The
    /// defaults are what [`save`](Self::save) writes next, and writing them over the user's
    /// recent vaults, shortcuts and exclusions because of one typo is the one thing this must
    /// not do; the copy is theirs to fix and move back.
    pub fn load() -> Config {
        let path = config_path();
        match Config::read(&path) {
            Ok(c) => c,
            Err(e) => {
                if path.exists() {
                    let aside = path.with_extension("toml.broken");
                    match std::fs::rename(&path, &aside) {
                        Ok(()) => tracing::warn!(
                            "{}: {e:#}; using defaults, the file is kept as {}",
                            path.display(),
                            aside.display()
                        ),
                        Err(re) => tracing::warn!(
                            "{}: {e:#}; using defaults, and could not move it aside: {re}",
                            path.display()
                        ),
                    }
                }
                Config::default()
            }
        }
    }

    pub fn read(path: &Path) -> Result<Config> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let table: toml::Table = text
            .parse()
            .with_context(|| format!("parsing {}", path.display()))?;
        for key in unknown_keys(&table) {
            tracing::warn!("{}: unknown key `{key}` is ignored", path.display());
        }
        table
            .try_into()
            .with_context(|| format!("parsing {}", path.display()))
    }

    pub fn save(&self) -> Result<()> {
        self.write(&config_path())
    }

    pub fn write(&self, path: &Path) -> Result<()> {
        replace(path, toml::to_string_pretty(self)?.as_bytes())
    }

    /// The stored entry for `root`, or the defaults.
    pub fn vault(&self, root: &Path) -> VaultConfig {
        self.vaults
            .get(&vault_key(root))
            .cloned()
            .unwrap_or_default()
    }

    pub fn set_vault(&mut self, root: &Path, cfg: VaultConfig) {
        self.vaults.insert(vault_key(root), cfg);
    }

    /// Move `root` to the front of the recent list, deduplicated and capped at 10.
    pub fn touch_recent(&mut self, root: &Path) {
        let path = PathBuf::from(vault_key(root));
        self.recent_vaults.retain(|v| v != &path);
        self.recent_vaults.insert(0, path);
        self.recent_vaults.truncate(10);
    }
}

impl Session {
    /// Missing or broken state is not an error: the window simply opens empty. A broken file is
    /// only worth a warning, because the next close rewrites it and nothing in it is the user's
    /// own work.
    pub fn load(root: &Path) -> Session {
        let path = state_path(root);
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Session::default();
        };
        serde_json::from_str(&text).unwrap_or_else(|e| {
            tracing::warn!("{}: {e}, opening empty", path.display());
            Session::default()
        })
    }

    pub fn save(&self, root: &Path) -> Result<()> {
        replace(&state_path(root), &serde_json::to_vec_pretty(self)?)
    }
}

/// Move `value` to the front of a recently-used list, deduplicated and capped at `cap`.
pub fn touch(list: &mut Vec<String>, value: &str, cap: usize) {
    list.retain(|v| v != value);
    list.insert(0, value.to_string());
    list.truncate(cap);
}

/// Follow a rename through a list of vault-relative paths, so a renamed note keeps its place
/// instead of leaving a dead entry behind. `from` may be a folder, in which case everything under
/// it moves with it.
pub fn rename_in(list: &mut [String], from: &str, to: &str) {
    let prefix = format!("{from}/");
    for rel in list {
        if rel == from {
            *rel = to.to_string();
        } else if let Some(rest) = rel.strip_prefix(&prefix) {
            *rel = format!("{to}/{rest}");
        }
    }
}

/// Keys in `config.toml` that nothing reads: a typo, or a setting that was retired. Serde drops
/// an unknown field before anything can see it, so the file is compared against the known names
/// as a plain table first. Checked at the top level and inside each vault table, where the
/// `daily_*` keys are left to [`daily_keys`], which explains them properly.
fn unknown_keys(table: &toml::Table) -> Vec<String> {
    // The struct's own field names, read off a serialised value rather than kept as a second
    // list. `editor_font` is skipped when `None`, so it is filled in to be seen.
    fn names<T: Serialize>(v: T) -> Vec<String> {
        toml::Value::try_from(v)
            .ok()
            .and_then(|v| v.as_table().map(|t| t.keys().cloned().collect()))
            .unwrap_or_default()
    }
    let top = names(Config {
        editor_font: Some(String::new()),
        ..Config::default()
    });
    let vault = names(VaultConfig::default());
    let mut out: Vec<String> = table.keys().filter(|k| !top.contains(k)).cloned().collect();
    if let Some(vaults) = table.get("vaults").and_then(toml::Value::as_table) {
        for (name, v) in vaults {
            let Some(v) = v.as_table() else { continue };
            out.extend(
                v.keys()
                    .filter(|k| !vault.contains(k) && !k.starts_with("daily_"))
                    .map(|k| format!("vaults.{name}.{k}")),
            );
        }
    }
    out
}

/// Write `bytes` where `path` is, atomically: a torn config or state file is read back as the
/// defaults, which is the difference between losing one edit and losing every preference. Same
/// temp-in-the-same-directory-then-rename shape as [`crate::fs::write_note`], minus the etag gate
/// and the ownership dance a vault file needs.
fn replace(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    // A unique temp name: the GUI and `accent-cli`, or two windows, may write at once, and a
    // fixed `<name>.tmp` would have them tear each other's file.
    let write = || -> std::io::Result<()> {
        let mut tmp = tempfile::Builder::new()
            .prefix(".accent-")
            .tempfile_in(dir)?;
        tmp.write_all(bytes)?;
        tmp.as_file().sync_all()?;
        tmp.persist(path).map_err(|e| e.error)?;
        Ok(())
    };
    write().with_context(|| format!("writing {}", path.display()))
}

/// `$XDG_<var>` when set and absolute, else `$HOME/<fallback>`, else the temp dir.
pub fn xdg(var: &str, home_fallback: &str) -> PathBuf {
    std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(home_fallback)))
        .unwrap_or_else(std::env::temp_dir)
}

/// The canonical path a vault is keyed by, so the same tree reached through a symlink or a
/// relative path is one vault. Falls back to the path as given when it does not exist yet.
fn canonical(root: &Path) -> PathBuf {
    root.canonicalize().unwrap_or_else(|_| root.to_path_buf())
}

fn vault_key(root: &Path) -> String {
    canonical(root).to_string_lossy().into_owned()
}

/// First 16 hex of the blake3 of the canonical vault path. Deliberately byte-for-byte what
/// `index::default_db_path` names its file, so a vault has one name across cache and state.
pub fn vault_hash(root: &Path) -> String {
    let digest = blake3::hash(canonical(root).as_os_str().as_encoded_bytes()).to_hex();
    digest[..16].to_string()
}

/// What a vault's retired daily-note settings mean now: the `accent-target` its three `daily_*`
/// keys spelled, and the template file they named.
///
/// `None` unless `config.toml` still carries one of them for `root`. Serde drops an unknown field
/// before anything can see it, so a second parse into a table is the only place they are still
/// visible. Nothing is written here, and nothing is ever written into the vault: the directive is
/// the user's line to add, and the dead keys leave `config.toml` the next time [`Config::save`]
/// writes it, the struct they were parsed into no longer having them.
pub fn daily_keys(root: &Path) -> Option<(String, Option<String>)> {
    let text = std::fs::read_to_string(config_path()).ok()?;
    retired_daily(&text.parse().ok()?, &vault_key(root))
}

/// The pure half of [`daily_keys`], so the table shape is testable without a config file.
fn retired_daily(config: &toml::Table, key: &str) -> Option<(String, Option<String>)> {
    let vault = config.get("vaults")?.get(key)?;
    let at = |k| vault.get(k).and_then(toml::Value::as_str);
    let (dir, pattern, template) = (at("daily_dir"), at("daily_pattern"), at("daily_template"));
    (dir.is_some() || pattern.is_some() || template.is_some())
        .then(|| (daily_target(dir, pattern), template.map(str::to_string)))
}

/// The `accent-target` pattern a pair of old daily keys spelled, e.g. `Daily/{{date:%Y-%m-%d}}.md`.
/// An absent key means what its default meant.
pub fn daily_target(dir: Option<&str>, pattern: Option<&str>) -> String {
    let name = format!("{{{{date:{}}}}}.md", pattern.unwrap_or("%Y-%m-%d"));
    match dir.unwrap_or("Daily").trim_matches('/') {
        "" => name,
        dir => format!("{dir}/{name}"),
    }
}

pub fn config_path() -> PathBuf {
    xdg("XDG_CONFIG_HOME", ".config")
        .join("accent")
        .join("config.toml")
}

pub fn state_path(root: &Path) -> PathBuf {
    xdg("XDG_STATE_HOME", ".local/state")
        .join("accent")
        .join(format!("{}.json", vault_hash(root)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    const EXAMPLE: &str = example!();

    /// The XDG lookups read process-wide env vars, so the tests that set them take turns.
    static ENV: Mutex<()> = Mutex::new(());

    /// Point config and state at `dir` for the duration of `f`. The real `~/.config` is never
    /// read or written by these tests.
    fn with_xdg<T>(dir: &Path, f: impl FnOnce() -> T) -> T {
        let _guard = ENV.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", dir);
            std::env::set_var("XDG_STATE_HOME", dir);
        }
        f()
    }

    #[test]
    fn daily_target_spells_what_the_old_keys_meant() {
        assert_eq!(
            daily_target(Some("Daily"), Some("%Y-%m-%d")),
            "Daily/{{date:%Y-%m-%d}}.md"
        );
        // Absent keys are the defaults they used to have.
        assert_eq!(daily_target(None, None), "Daily/{{date:%Y-%m-%d}}.md");
        // An empty folder was the vault root, and a root-relative target carries no leading slash.
        assert_eq!(daily_target(Some(""), Some("%d")), "{{date:%d}}.md");
    }

    #[test]
    fn retired_daily_keys_are_only_reported_where_they_are() {
        let with: toml::Table = r#"[vaults."/v"]
daily_pattern = "%d"
daily_template = "DailyNote.md"
"#
        .parse()
        .unwrap();
        assert_eq!(
            retired_daily(&with, "/v"),
            Some((
                "Daily/{{date:%d}}.md".to_string(),
                Some("DailyNote.md".to_string())
            ))
        );
        assert_eq!(retired_daily(&with, "/other"), None);

        let without: toml::Table = "[vaults.\"/v\"]\ntemplates_dir = \"T\"\n".parse().unwrap();
        assert_eq!(retired_daily(&without, "/v"), None);
        assert_eq!(retired_daily(&toml::Table::new(), "/v"), None);
    }

    #[test]
    fn config_roundtrip_parses_the_documented_example() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("config.toml");
        std::fs::write(&p, EXAMPLE).unwrap();

        let c = Config::read(&p).unwrap();
        assert_eq!(c.recent_vaults, [PathBuf::from("/home/me/Notes")]);
        assert!(c.spellcheck);
        assert!(c.ghost_text);
        assert!(!c.minimap);
        assert!(!c.line_numbers);
        assert!(c.git_tree);
        assert_eq!(c.column_width, 50);
        assert_eq!(c.theme, Theme::Solarized);
        assert_eq!(c.focus_mode, FocusMode::High);
        assert_eq!(c.editor_font, None);
        let v = &c.vaults["/home/me/Notes"];
        assert_eq!(v.templates_dir, "Templates");
        assert_eq!(v.new_file_dir, "Inbox");
        // An override is a list, so an action can keep several chords, and an empty list is how
        // the user says "no shortcut at all" rather than "fall back to the default".
        assert_eq!(c.shortcuts["win.find-next"], ["F3"]);
        assert!(c.shortcuts["win.about"].is_empty());
        assert_eq!(c.search.exclude, ["Archive", "Code/vendor"]);
        assert!(!c.drawing.mouse);
        assert_eq!(c.drawing.pen_width, 2.0);
        assert_eq!(c.drawing.pen_color, None);
        assert_eq!(c.drawing.highlighter_color, Some([0, 0, 0]));
        assert_eq!(c.drawing.eraser_radius, 4.0);
        assert!(c.drawing.eraser_partial);

        let back = tmp.path().join("written.toml");
        c.write(&back).unwrap();
        let again = Config::read(&back).unwrap();
        assert_eq!(again.recent_vaults, c.recent_vaults);
        assert_eq!(again.theme, Theme::Solarized);
        assert_eq!(again.focus_mode, FocusMode::High);
        assert_eq!(again.vaults["/home/me/Notes"].new_file_dir, "Inbox");
        assert_eq!(again.shortcuts, c.shortcuts);
        assert_eq!(again.search.exclude, c.search.exclude);
        assert_eq!(again.drawing, c.drawing);
    }

    #[test]
    fn touch_moves_to_the_front_and_caps() {
        let mut list = Vec::new();
        for i in 0..12 {
            touch(&mut list, &format!("n{i}.md"), 10);
        }
        touch(&mut list, "n5.md", 10);
        assert_eq!(list.len(), 10);
        assert_eq!(list[0], "n5.md");
        assert_eq!(list[1], "n11.md");
        assert_eq!(list.iter().filter(|n| *n == "n5.md").count(), 1);
    }

    #[test]
    fn rename_in_follows_a_note_and_a_folder() {
        let mut list = vec![
            "Inbox/idea.md".to_string(),
            "Areas/Work/plan.md".to_string(),
            "Areas/Workshop.md".to_string(),
        ];
        rename_in(&mut list, "Areas/Work", "Areas/Job");
        rename_in(&mut list, "Inbox/idea.md", "Inbox/thought.md");
        assert_eq!(
            list,
            [
                "Inbox/thought.md",
                "Areas/Job/plan.md",
                // A sibling that merely starts with the old name is not part of the folder.
                "Areas/Workshop.md"
            ]
        );
    }

    #[test]
    fn missing_config_file_is_default() {
        let tmp = tempfile::tempdir().unwrap();
        let c = with_xdg(tmp.path(), Config::load);
        assert!(c.spellcheck);
        assert_eq!(c.focus_mode, FocusMode::Medium);
        assert!(c.recent_vaults.is_empty());
        assert!(!config_path_exists(tmp.path()));
    }

    /// The `!BUG`: a typo in `config.toml` used to load as the defaults, and the next save wrote
    /// those defaults over everything the user had set.
    #[test]
    fn broken_config_file_is_default_and_kept_aside_for_the_next_save() {
        let tmp = tempfile::tempdir().unwrap();
        let c = with_xdg(tmp.path(), || {
            let p = config_path();
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, "recent_vaults = [ oops").unwrap();
            let c = Config::load();
            c.save().unwrap();
            c
        });
        assert!(c.spellcheck);
        assert!(c.recent_vaults.is_empty());
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("accent/config.toml.broken")).unwrap(),
            "recent_vaults = [ oops",
            "the user's file survives the save that follows"
        );
    }

    #[test]
    fn unknown_keys_are_named_at_both_levels() {
        let table: toml::Table = r#"
spellcheck = true
new_note_dir = "Inbox"
[vaults."/v"]
new_note_dir = "Inbox"
new_file_dir = "Inbox"
daily_dir = "Daily"
"#
        .parse()
        .unwrap();
        assert_eq!(
            unknown_keys(&table),
            ["new_note_dir", "vaults./v.new_note_dir"]
        );
        assert!(unknown_keys(&EXAMPLE.parse().unwrap()).is_empty());
    }

    fn config_path_exists(base: &Path) -> bool {
        base.join("accent/config.toml").exists()
    }

    #[test]
    fn vault_lookup_by_canonical_path() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("Notes");
        std::fs::create_dir(&real).unwrap();
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let mut c = Config::default();
        c.set_vault(
            &link,
            VaultConfig {
                new_file_dir: "Inbox".to_string(),
                ..Default::default()
            },
        );
        assert_eq!(c.vaults.len(), 1);
        assert_eq!(c.vault(&real).new_file_dir, "Inbox");
        assert_eq!(c.vault(&tmp.path().join("Other")).new_file_dir, "");
    }

    #[test]
    fn touch_recent_dedups_and_caps_at_ten() {
        let mut c = Config::default();
        for i in 0..12 {
            c.touch_recent(Path::new(&format!("/vault/{i}")));
        }
        c.touch_recent(Path::new("/vault/5"));
        assert_eq!(c.recent_vaults.len(), 10);
        assert_eq!(c.recent_vaults[0], PathBuf::from("/vault/5"));
        assert_eq!(c.recent_vaults[1], PathBuf::from("/vault/11"));
        assert_eq!(
            c.recent_vaults.iter().filter(|p| p.ends_with("5")).count(),
            1
        );
    }

    #[test]
    fn vault_hash_matches_the_index_cache_name() {
        let tmp = tempfile::tempdir().unwrap();
        let db = crate::index::default_db_path(tmp.path());
        assert_eq!(db.file_stem().unwrap(), vault_hash(tmp.path()).as_str());
    }

    #[test]
    fn session_roundtrips_as_json() {
        let tmp = tempfile::tempdir().unwrap();
        let vault = tmp.path().join("vault");
        std::fs::create_dir(&vault).unwrap();
        let state = tmp.path().join("state");

        let s = Session {
            open: vec!["Daily/2026-09-03.md".to_string()],
            active: Some("Daily/2026-09-03.md".to_string()),
            sidebar: false,
            sidebar_width: 320,
            view: "preview".to_string(),
            zoom: 1.2,
            recent_notes: vec!["Daily/2026-09-03.md".to_string()],
            recent_commands: vec!["win.save".to_string()],
            pdf: BTreeMap::from([(
                "Attachments/paper.pdf".to_string(),
                PdfPlace {
                    page: 4,
                    zoom: PdfZoom::Scale(1.5),
                },
            )]),
        };
        with_xdg(&state, || {
            assert_eq!(Session::load(&vault).open, Vec::<String>::new());
            s.save(&vault).unwrap();
            let path = state_path(&vault);
            assert!(path.starts_with(state.join("accent")));
            assert_eq!(path.file_stem().unwrap(), vault_hash(&vault).as_str());

            let back = Session::load(&vault);
            assert_eq!(back.open, s.open);
            assert_eq!(back.active, s.active);
            assert_eq!(back.pdf, s.pdf);
            assert!(!back.sidebar);
            assert_eq!(back.sidebar_width, 320);
            assert_eq!(back.view, "preview");
            assert_eq!(back.zoom, 1.2);
            assert_eq!(back.recent_notes, s.recent_notes);
            assert_eq!(back.recent_commands, s.recent_commands);
        });
    }

    /// A state file from another version must still load: one written before `zoom` existed gets
    /// the default, and the `pane` every file written until now carries is simply dropped — which
    /// sidebar pane was showing is no longer restored.
    #[test]
    fn session_from_another_version_loads() {
        let tmp = tempfile::tempdir().unwrap();
        let vault = tmp.path().join("vault");
        std::fs::create_dir(&vault).unwrap();
        let state = tmp.path().join("state");

        with_xdg(&state, || {
            let path = state_path(&vault);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(
                &path,
                r#"{"open":["a.md"],"active":"a.md","sidebar":true,"sidebar_width":280,"view":"editor","pane":"git"}"#,
            )
            .unwrap();

            let back = Session::load(&vault);
            assert_eq!(back.open, ["a.md"]);
            assert_eq!(back.zoom, 1.0);
        });
    }
}
