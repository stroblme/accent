//! Where config and state live, and how `config.toml` is read, merged with a hand edit and written.

use super::{Config, VaultConfig};
use crate::{Error, Result};
use serde::de::{Deserialize, Deserializer, Visitor};
use std::path::{Path, PathBuf};

impl Config {
    /// Load defaults on a missing or broken config so hand-edit errors cannot prevent startup.
    ///
    /// An invalid file is first moved to `config.toml.broken`, preserving preferences for repair
    /// before a later save writes defaults.
    pub fn load() -> Config {
        let path = config_path();
        let text = std::fs::read_to_string(&path);
        let parsed = match &text {
            Ok(text) => Config::parse(text, &path),
            Err(e) => Err(Error::Io(format!("{}: {e}", path.display()))),
        };
        match parsed {
            Ok(c) => {
                set_known(text.ok());
                c
            }
            Err(e) => {
                if path.exists() {
                    let aside = path.with_extension("toml.broken");
                    match std::fs::rename(&path, &aside) {
                        Ok(()) => tracing::warn!(
                            "{}: {e}; using defaults, the file is kept as {}",
                            path.display(),
                            aside.display()
                        ),
                        Err(re) => tracing::warn!(
                            "{}: {e}; using defaults, and could not move it aside: {re}",
                            path.display()
                        ),
                    }
                }
                set_known(None);
                Config::default()
            }
        }
    }

    pub fn read(path: &Path) -> Result<Config> {
        let text = std::fs::read_to_string(path).map_err(|e| Error::io(path.display(), e))?;
        Config::parse(&text, path)
    }

    /// `text` as a config; `path` is only for the messages.
    fn parse(text: &str, path: &Path) -> Result<Config> {
        let table: toml::Table = text.parse().map_err(|e| unreadable(path, e))?;
        for key in unknown_keys(&table) {
            tracing::warn!("{}: unknown key `{key}` is ignored", path.display());
        }
        table.try_into().map_err(|e| unreadable(path, e))
    }

    /// Write the config, unless the file holds a change accent has not taken in yet
    /// ([`reread`](Self::reread)): then nothing is written and the answer is `false`. That is
    /// what keeps a hand edit — or a file that no longer parses — from being written over.
    pub fn save(&self) -> Result<bool> {
        let path = config_path();
        // An error rather than `None`: a file that cannot be read is not a file that is not
        // there, and reading the second out of the first wrote the defaults over a config whose
        // bytes nobody had seen.
        let on_disk = on_disk(&path)?;
        if on_disk.is_some() && on_disk != known() {
            return Ok(false);
        }
        let text = edited(on_disk.as_deref(), self).map_err(|e| unreadable(&path, e))?;
        replace(&path, text.as_bytes())?;
        set_known(Some(text));
        Ok(true)
    }

    pub fn write(&self, path: &Path) -> Result<()> {
        let text = toml::to_string_pretty(self).map_err(|e| unreadable(path, e))?;
        replace(path, text.as_bytes())
    }

    /// Take in a change someone else made to `config.toml`: the file's config, with whatever
    /// accent changed since it last read or wrote the file kept on top, key by key. Where both
    /// changed one key the file wins, since that is what someone just typed, and the log names
    /// the key. `None` when the file holds nothing new; an error when it does not parse, and then
    /// nothing is taken in and [`save`](Self::save) keeps refusing until it does.
    pub fn reread(&self) -> Result<Option<Reread>> {
        let path = config_path();
        let text = on_disk(&path)?;
        let known = known();
        let Some(file) = theirs(text.as_deref(), known.as_deref(), &path)? else {
            return Ok(None);
        };
        // What accent last had on disk, which is what both sides moved on from.
        let base = known
            .as_deref()
            .and_then(|k| Config::parse(k, &path).ok())
            .unwrap_or_default();
        let value = |c: &Config| toml::Value::try_from(c).map_err(|e| unreadable(&path, e));
        let (base, ours, file) = (value(&base)?, value(self)?, value(&file)?);
        let mut lost = Vec::new();
        let merged = merge(Some(&base), Some(&ours), Some(&file), "", &mut lost)
            .ok_or_else(|| Error::Config("merging config.toml".to_string()))?;
        for key in lost {
            tracing::warn!(
                "config.toml: `{key}` was changed by hand and here at once; the file's value is kept"
            );
        }
        set_known(text);
        Ok(Some(Reread {
            unwritten: merged != file,
            config: merged.try_into().map_err(|e| unreadable(&path, e))?,
        }))
    }

    /// The stored entry for `root`, or the defaults.
    pub fn vault(&self, root: &Path) -> VaultConfig {
        self.vaults
            .get(&vault_key(root))
            .cloned()
            .unwrap_or_default()
    }

    /// What a vault on this machine opens with: its entry, its own servers and debug adapters over
    /// the global `[lsp.servers]` and `[debug.adapters]` key by key, and its interpreter if it
    /// names one. A remote vault takes [`Self::vault`] alone, its host's servers being the host's
    /// own.
    pub fn local_vault(&self, root: &Path) -> VaultConfig {
        let mut cfg = self.vault(root);
        for (language, argv) in &self.lsp.servers {
            cfg.lsp
                .servers
                .entry(language.clone())
                .or_insert_with(|| argv.clone());
        }
        for (language, argv) in &self.debug.adapters {
            cfg.debug
                .adapters
                .entry(language.clone())
                .or_insert_with(|| argv.clone());
        }
        if cfg.debug.python.is_empty() {
            cfg.debug.python = self.debug.python.clone();
        }
        cfg
    }

    pub fn set_vault(&mut self, root: &Path, cfg: VaultConfig) {
        self.vaults.insert(vault_key(root), cfg);
    }

    /// Move `root` to the front of the recent list, deduplicated.
    pub fn touch_recent(&mut self, root: &Path) {
        let path = PathBuf::from(vault_key(root));
        self.recent_vaults.retain(|v| v != &path);
        self.recent_vaults.insert(0, path);
    }
}

/// Unknown or retired `config.toml` keys. Compare the raw top-level and vault tables with known
/// fields before Serde discards extras. Leave `daily_*` keys to [`daily_keys`] for migration advice.
pub(super) fn unknown_keys(table: &toml::Table) -> Vec<String> {
    let (top, vault) = (fields::<Config>(), fields::<VaultConfig>());
    let mut out: Vec<String> = table
        .keys()
        .filter(|k| !top.contains(&k.as_str()))
        .cloned()
        .collect();
    if let Some(vaults) = table.get("vaults").and_then(toml::Value::as_table) {
        for (name, v) in vaults {
            let Some(v) = v.as_table() else { continue };
            out.extend(
                v.keys()
                    .filter(|k| !vault.contains(&k.as_str()) && !k.starts_with("daily_"))
                    .map(|k| format!("vaults.{name}.{k}")),
            );
        }
    }
    out
}

/// The keys `T` reads: the field names its derived `Deserialize` hands the deserializer, which
/// [`Names`] keeps and then refuses. Taken from the struct itself, so a field written only while
/// it is not empty is known all the same.
pub(super) fn fields<'de, T: Deserialize<'de>>() -> &'static [&'static str] {
    let mut names = Names(&[]);
    let _ = T::deserialize(&mut names);
    names.0
}

/// A deserializer that reads nothing and keeps the field names a struct asks it for.
struct Names(&'static [&'static str]);

impl<'de> Deserializer<'de> for &mut Names {
    type Error = serde::de::value::Error;

    fn deserialize_struct<V: Visitor<'de>>(
        self,
        _: &'static str,
        fields: &'static [&'static str],
        visitor: V,
    ) -> std::result::Result<V::Value, Self::Error> {
        self.0 = fields;
        self.deserialize_any(visitor)
    }

    fn deserialize_any<V: Visitor<'de>>(self, _: V) -> std::result::Result<V::Value, Self::Error> {
        Err(serde::de::Error::custom("only the field names are read"))
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string bytes byte_buf option
        unit unit_struct newtype_struct seq tuple tuple_struct map enum identifier ignored_any
    }
}

/// A change to `config.toml` taken in by [`Config::reread`].
#[derive(Debug)]
pub struct Reread {
    pub config: Config,
    /// Whether it holds a change of accent's the file does not, and so still wants a save.
    pub unwritten: bool,
}

/// The text accent last read from `config.toml` or wrote to it, so a change of someone else's can
/// be told from accent's own write coming back. Process-wide, like the file.
static KNOWN: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

fn known() -> Option<String> {
    KNOWN.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

fn set_known(text: Option<String>) {
    *KNOWN.lock().unwrap_or_else(|e| e.into_inner()) = text;
}

/// Read the file text, or `None` if absent. Unreadable or non-UTF-8 files return an error so
/// their unknown contents cannot be overwritten.
fn on_disk(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::io(path.display(), e)),
    }
}

pub(super) fn theirs(
    text: Option<&str>,
    known: Option<&str>,
    path: &Path,
) -> Result<Option<Config>> {
    match text {
        Some(text) if Some(text) != known => Config::parse(text, path).map(Some),
        _ => Ok(None),
    }
}

/// `config` as the text of `config.toml`: `file`, the text on disk, with only the keys whose value
/// `config` changed rewritten, added or removed, so the comments, order and formatting of the
/// rest stay as the user wrote them, keys accent does not know included. No file yet (or one
/// that does not read as a config), the whole config pretty-printed.
pub(super) fn edited(file: Option<&str>, config: &Config) -> std::result::Result<String, String> {
    let pretty = toml::to_string_pretty(config).map_err(|e| e.to_string())?;
    let table = |c: &Config| match toml::Value::try_from(c) {
        Ok(toml::Value::Table(table)) => Some(table),
        _ => None,
    };
    let base = file.and_then(|f| f.parse::<toml::Table>().ok()?.try_into::<Config>().ok());
    let (Some(file), Some(was), Some(now)) = (file, base.as_ref().and_then(table), table(config))
    else {
        return Ok(pretty);
    };
    let mut doc: toml_edit::DocumentMut = file.parse().map_err(|e| format!("{e}"))?;
    let fresh: toml_edit::DocumentMut = pretty.parse().map_err(|e| format!("{e}"))?;
    update(doc.as_table_mut(), &was, &now, fresh.as_table());
    Ok(doc.to_string())
}

/// Bring the table `doc` from `was` to `now`, both the config's values as it reads them, taking
/// a changed or new key from `fresh`, the same table pretty-printed. A changed value keeps the
/// comment and space around it; a key whose value did not change is left as written, or left out
/// where the file left it out; a key the config does not hold goes, as it went from the whole
/// file written anew (a retired `daily_*` key, say).
fn update(
    doc: &mut dyn toml_edit::TableLike,
    was: &toml::Table,
    now: &toml::Table,
    fresh: &dyn toml_edit::TableLike,
) {
    let gone: Vec<String> = doc
        .iter()
        .map(|(key, _)| key.to_string())
        .filter(|key| !now.contains_key(key))
        .collect();
    for key in gone {
        doc.remove(&key);
    }
    let empty = toml::Table::new();
    for (key, value) in now {
        let before = was.get(key);
        let Some(new) = fresh.get(key) else {
            continue;
        };
        // Down into a table the file has, to prune it too, or into one it lacks that changed.
        if let toml::Value::Table(now) = value
            && doc
                .get(key)
                .map_or(before != Some(value), toml_edit::Item::is_table_like)
        {
            if doc.get(key).is_none() {
                let mut table = toml_edit::Table::new();
                table.set_implicit(true);
                doc.insert(key, toml_edit::Item::Table(table));
            }
            let was = match before {
                Some(toml::Value::Table(was)) => was,
                _ => &empty,
            };
            let old = doc
                .get_mut(key)
                .and_then(toml_edit::Item::as_table_like_mut);
            if let (Some(old), Some(new)) = (old, new.as_table_like()) {
                update(old, was, now, new);
            }
            continue;
        }
        if before == Some(value) {
            continue;
        }
        match doc.get_mut(key) {
            Some(old) => {
                let decor = old.as_value().map(|v| v.decor().clone());
                *old = placed(new.clone());
                if let (Some(decor), Some(value)) = (decor, old.as_value_mut()) {
                    *value.decor_mut() = decor;
                }
            }
            None => {
                doc.insert(key, placed(new.clone()));
            }
        }
    }
}

/// `item` with no place of its own in the document, so the tables in it are written after the
/// table they now follow rather than where the pretty-printed config had them.
fn placed(mut item: toml_edit::Item) -> toml_edit::Item {
    fn unplace(item: &mut toml_edit::Item) {
        match item {
            toml_edit::Item::Table(table) => {
                table.set_position(None);
                for (_, item) in table.iter_mut() {
                    unplace(item);
                }
            }
            toml_edit::Item::ArrayOfTables(tables) => {
                for table in tables.iter_mut() {
                    table.set_position(None);
                    for (_, item) in table.iter_mut() {
                        unplace(item);
                    }
                }
            }
            _ => {}
        }
    }
    unplace(&mut item);
    item
}

/// Three-way merge of two configs that both moved on from `base`, as TOML values, key by key down
/// through the tables: a key the file (`theirs`) changed takes the file's value, every other key
/// keeps accent's. A key both changed differently is pushed to `lost` by its dotted path, `at`
/// being where this value sits. `None` stands for a key that is absent.
pub(super) fn merge(
    base: Option<&toml::Value>,
    ours: Option<&toml::Value>,
    theirs: Option<&toml::Value>,
    at: &str,
    lost: &mut Vec<String>,
) -> Option<toml::Value> {
    use toml::Value::Table;
    // An empty map is not written at all (`Config::shortcuts`), so a table on one side and
    // nothing on the other is that table emptied there, and its keys still merge one by one.
    // A table the merge leaves empty is left out again, as it would be written.
    let empty = Table(toml::Table::new());
    let (ours, theirs) = match (ours, theirs) {
        (Some(Table(_)), None) => (ours, Some(&empty)),
        (None, Some(Table(_))) => (Some(&empty), theirs),
        sides => sides,
    };
    if let (Some(Table(o)), Some(Table(t))) = (ours, theirs) {
        let b = match base {
            Some(Table(b)) => b.clone(),
            _ => toml::Table::new(),
        };
        let keys: std::collections::BTreeSet<&String> =
            b.keys().chain(o.keys()).chain(t.keys()).collect();
        let merged: toml::Table = keys
            .into_iter()
            .filter_map(|k| {
                let at = if at.is_empty() {
                    k.clone()
                } else {
                    format!("{at}.{k}")
                };
                Some((k.clone(), merge(b.get(k), o.get(k), t.get(k), &at, lost)?))
            })
            .collect();
        return (!merged.is_empty()).then_some(Table(merged));
    }
    if theirs == base {
        return ours.cloned();
    }
    if ours != base && ours != theirs {
        lost.push(at.to_string());
    }
    theirs.cloned()
}

/// Atomically write config or state bytes using a temporary file and rename. This prevents torn
/// files from loading as defaults and losing all preferences.
pub(super) fn replace(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).map_err(|e| Error::io(dir.display(), e))?;
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
    write().map_err(|e| Error::io(path.display(), e))
}

/// A config or state file that does not parse, or a config that does not serialise, by its path.
pub(super) fn unreadable(path: &Path, e: impl std::fmt::Display) -> Error {
    Error::Config(format!("{}: {e}", path.display()))
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

/// Migration advice for a vault's retired `daily_*` settings: their equivalent `accent-target`
/// and template.
///
/// Returns `None` when no retired key remains for `root`. A second TOML-table parse preserves
/// keys Serde discards. Nothing is written here; the user adds the directive, and the next
/// [`Config::save`] drops the old keys, as it drops every key the config does not hold.
pub fn daily_keys(root: &Path) -> Option<(String, Option<String>)> {
    let text = std::fs::read_to_string(config_path()).ok()?;
    retired_daily(&text.parse().ok()?, &vault_key(root))
}

/// The pure half of [`daily_keys`], so the table shape is testable without a config file.
pub(super) fn retired_daily(config: &toml::Table, key: &str) -> Option<(String, Option<String>)> {
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
    state_dir().join(format!("{}.json", vault_hash(root)))
}

/// Where the sessions are written, one file per vault or terminal session, and nothing else.
pub(super) fn state_dir() -> PathBuf {
    xdg("XDG_STATE_HOME", ".local/state").join("accent")
}
