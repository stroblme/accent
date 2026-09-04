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
minimap = false

[vaults."/home/me/Notes"]
daily_dir = "Daily"
daily_pattern = "%Y-%m-%d"
daily_template = "Templates/Daily.md"
templates_dir = "Templates"
new_note_dir = "Inbox"
"#
    };
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
    /// A code map beside the document instead of the scrollbar.
    pub minimap: bool,
    /// Keyed by canonical vault path.
    pub vaults: BTreeMap<String, VaultConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            recent_vaults: Vec::new(),
            editor_font: None,
            spellcheck: true,
            minimap: false,
            vaults: BTreeMap::new(),
        }
    }
}

/// Per-vault preferences. All paths are vault-relative.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct VaultConfig {
    pub daily_dir: String,
    pub daily_pattern: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub daily_template: Option<String>,
    pub templates_dir: String,
    /// Empty means the vault root.
    pub new_note_dir: String,
}

impl Default for VaultConfig {
    fn default() -> Self {
        VaultConfig {
            daily_dir: "Daily".to_string(),
            daily_pattern: "%Y-%m-%d".to_string(),
            daily_template: None,
            templates_dir: "Templates".to_string(),
            new_note_dir: String::new(),
        }
    }
}

/// What a window looked like when it was last closed. Cheap to lose, so it lives in the state
/// dir rather than in the config.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    /// Vault-relative paths of the open tabs.
    pub open: Vec<String>,
    pub active: Option<String>,
    pub sidebar: bool,
    pub sidebar_width: i32,
    pub view: String,
    /// Which sidebar pane was showing: an older state file without the key gets the default.
    pub pane: String,
    /// Document zoom, 1.0 being the font as GNOME sets it.
    pub zoom: f64,
}

impl Default for Session {
    fn default() -> Self {
        Session {
            open: Vec::new(),
            active: None,
            sidebar: true,
            sidebar_width: 280,
            view: "editor".to_string(),
            pane: "files".to_string(),
            zoom: 1.0,
        }
    }
}

impl Config {
    /// Never fails: a missing or broken file logs and yields the defaults, because a typo in a
    /// hand-edited config must not keep the app from starting.
    pub fn load() -> Config {
        let path = config_path();
        match Config::read(&path) {
            Ok(c) => c,
            Err(e) => {
                if path.exists() {
                    tracing::warn!("{}: {e:#}, using defaults", path.display());
                }
                Config::default()
            }
        }
    }

    pub fn read(path: &Path) -> Result<Config> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn save(&self) -> Result<()> {
        self.write(&config_path())
    }

    // ponytail: a plain write, no temp-and-rename dance. This is a 1 KB file that only accent
    // writes and a torn one falls back to the defaults; copy what `fs::write_note` does the day
    // two windows write it at once.
    pub fn write(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        std::fs::write(path, toml::to_string_pretty(self)?)
            .with_context(|| format!("writing {}", path.display()))
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
    /// Missing or broken state is not an error: the window simply opens empty.
    pub fn load(root: &Path) -> Session {
        std::fs::read_to_string(state_path(root))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, root: &Path) -> Result<()> {
        let path = state_path(root);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        std::fs::write(&path, serde_json::to_vec_pretty(self)?)
            .with_context(|| format!("writing {}", path.display()))
    }
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
    fn config_roundtrip_parses_the_documented_example() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("config.toml");
        std::fs::write(&p, EXAMPLE).unwrap();

        let c = Config::read(&p).unwrap();
        assert_eq!(c.recent_vaults, [PathBuf::from("/home/me/Notes")]);
        assert!(c.spellcheck);
        assert!(!c.minimap);
        assert_eq!(c.editor_font, None);
        let v = &c.vaults["/home/me/Notes"];
        assert_eq!(v.daily_dir, "Daily");
        assert_eq!(v.daily_pattern, "%Y-%m-%d");
        assert_eq!(v.daily_template.as_deref(), Some("Templates/Daily.md"));
        assert_eq!(v.templates_dir, "Templates");
        assert_eq!(v.new_note_dir, "Inbox");

        let back = tmp.path().join("written.toml");
        c.write(&back).unwrap();
        let again = Config::read(&back).unwrap();
        assert_eq!(again.recent_vaults, c.recent_vaults);
        assert_eq!(again.vaults["/home/me/Notes"].new_note_dir, "Inbox");
    }

    #[test]
    fn missing_config_file_is_default() {
        let tmp = tempfile::tempdir().unwrap();
        let c = with_xdg(tmp.path(), Config::load);
        assert!(c.spellcheck);
        assert!(c.recent_vaults.is_empty());
        assert!(!config_path_exists(tmp.path()));
    }

    #[test]
    fn broken_config_file_is_default() {
        let tmp = tempfile::tempdir().unwrap();
        let c = with_xdg(tmp.path(), || {
            let p = config_path();
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, "recent_vaults = [ oops").unwrap();
            Config::load()
        });
        assert!(c.spellcheck);
        assert!(c.recent_vaults.is_empty());
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
                new_note_dir: "Inbox".to_string(),
                ..Default::default()
            },
        );
        assert_eq!(c.vaults.len(), 1);
        assert_eq!(c.vault(&real).new_note_dir, "Inbox");
        assert_eq!(c.vault(&tmp.path().join("Other")).new_note_dir, "");
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
            pane: "search".to_string(),
            zoom: 1.2,
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
            assert!(!back.sidebar);
            assert_eq!(back.sidebar_width, 320);
            assert_eq!(back.view, "preview");
            assert_eq!(back.pane, "search");
            assert_eq!(back.zoom, 1.2);
        });
    }

    /// A state file written before `pane` and `zoom` existed must still load, with their
    /// defaults.
    #[test]
    fn session_from_an_older_file_defaults_the_missing_pane() {
        let tmp = tempfile::tempdir().unwrap();
        let vault = tmp.path().join("vault");
        std::fs::create_dir(&vault).unwrap();
        let state = tmp.path().join("state");

        with_xdg(&state, || {
            let path = state_path(&vault);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(
                &path,
                r#"{"open":["a.md"],"active":"a.md","sidebar":true,"sidebar_width":280,"view":"editor"}"#,
            )
            .unwrap();

            let back = Session::load(&vault);
            assert_eq!(back.open, ["a.md"]);
            assert_eq!(back.pane, "files");
            assert_eq!(back.zoom, 1.0);
        });
    }
}
