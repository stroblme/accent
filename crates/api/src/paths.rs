//! The vault-relative names the façade makes up or takes apart, on top of the plain arithmetic
//! in [`accent_core::path`]: what a template's target is called, what a conflict copy is called,
//! and which of them are worth offering the user.

use std::io;

use anyhow::Result;

use accent_core::index::Index;
use accent_core::path::{basename, parent_dir};

use crate::fs;

/// A template's target is markdown whatever its date pattern spells.
pub(crate) fn with_md(rel: &str) -> String {
    match rel.rsplit_once('.') {
        Some((_, ext))
            if ext.eq_ignore_ascii_case("md") || ext.eq_ignore_ascii_case("markdown") =>
        {
            rel.to_string()
        }
        _ => format!("{rel}.md"),
    }
}

/// `(original, conflict copy)` for every `*.sync-conflict-*` file whose original still exists.
/// A copy of a note that has since been deleted is nothing the resolve UI can act on.
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

/// What [`Vault::adopt_conflict`] calls the version it replaces: Syncthing's own naming, with
/// `accent` where the device id would be, so the vault treats it as the conflict copy it is.
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

pub(crate) fn outside(rel: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("{rel} is outside the vault"),
    )
}
