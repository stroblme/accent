//! Vault-relative template and conflict names built on [`accent_core::path`],
//! including which conflict copies the UI can offer to resolve.

use crate::Result;

use accent_core::index::Index;
use accent_core::path::{basename, is_note, parent_dir};

use crate::fs;

/// A template's target is markdown whatever its date pattern spells.
pub(crate) fn with_md(rel: &str) -> String {
    match is_note(rel) {
        true => rel.to_string(),
        false => format!("{rel}.md"),
    }
}

/// `(original, conflict copy)` for each `*.sync-conflict-*` file with an existing
/// original. Deleted originals cannot be resolved by the UI.
pub(crate) fn conflict_pairs(index: &Index) -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    for copy in index.conflicts()? {
        let Some(original) = conflict_original_rel(&copy) else {
            continue;
        };
        if index.get_file(&original)?.is_some() {
            out.push((original, copy));
        }
    }
    Ok(out)
}

/// `Dir/Note.sync-conflict-….md` -> `Dir/Note.md`, or `None` when `copy` is not one.
pub fn conflict_original_rel(copy: &str) -> Option<String> {
    let (dir, name) = (parent_dir(copy), basename(copy));
    let original = fs::conflict_original(name)?;
    Some(if dir.is_empty() {
        original
    } else {
        format!("{dir}/{original}")
    })
}

/// Name the version [`crate::Vault::adopt_conflict`] replaces using Syncthing's format,
/// with `accent` as the device id, so the vault recognizes it as a conflict copy.
pub(crate) fn accent_conflict_name(name: &str, now: chrono::NaiveDateTime) -> String {
    let (stem, ext) = match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem, format!(".{ext}")),
        _ => (name, String::new()),
    };
    format!(
        "{stem}.sync-conflict-{}-accent{ext}",
        now.format("%Y%m%d-%H%M%S")
    )
}

pub(crate) fn outside(rel: &str) -> crate::Error {
    accent_core::Error::Invalid(format!("{rel} is outside the vault")).into()
}
