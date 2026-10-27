//! The files a note's page names by `accent://file/` addresses, held to the vault: what the
//! preview serves and an export puts in the file.

use accent_core::markdown::{accent_file, percent_decode};
use std::path::{Component, Path, PathBuf};

/// Resolve vault-relative assets to their key and local path through Vault::fetch.
/// Remote fetches can block on downloads, so callers must dispatch off-thread.
pub type Resolve = dyn Fn(&str) -> Option<(String, PathBuf)> + Send + Sync;

/// Resolve a note asset to a local file under lexical and canonical containment rules.
///
/// Reject empty, absolute and `..`-escaping paths before calling the resolver. The resolver
/// must enforce vault containment after symlinks because only it knows the root.
///
/// Loose notes use their own directory as root with the same rules ([`beside`]).
/// [`base_uri`](accent_core::markdown::base_uri) makes relative links arrive under that
/// directory; wikilink embeds arrive as bare names.
pub(crate) fn resolve_asset(resolve: &Resolve, note: &str, rel: &str) -> Option<(String, PathBuf)> {
    let folder = Path::new(note)
        .parent()
        .filter(|_| Path::new(note).is_absolute());
    let rel = match folder.and_then(|dir| Path::new(rel).strip_prefix(dir).ok()) {
        Some(inside) => inside.to_str()?,
        None => rel,
    };
    if rel.is_empty() || Path::new(rel).is_absolute() {
        return None;
    }
    // Never more `..` than there are directories to climb back out of.
    let mut depth = 0usize;
    for part in Path::new(rel).components() {
        match part {
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::ParentDir if depth > 0 => depth -= 1,
            _ => return None,
        }
    }
    match folder {
        Some(dir) => beside(dir, rel),
        None => resolve(rel),
    }
}

/// Resolve `rel` under `dir` after following symlinks, requiring containment and returning a loose-file key.
fn beside(dir: &Path, rel: &str) -> Option<(String, PathBuf)> {
    let root = dir.canonicalize().ok()?;
    let path = root.join(rel).canonicalize().ok()?;
    path.starts_with(&root)
        .then(|| (path.to_string_lossy().into_owned(), path))
}

/// The file on this machine an `accent://file/` address on `note`'s page names, held to the vault
/// as every request the page makes is.
pub(crate) fn asset(resolve: &Resolve, note: &str, uri: &str) -> Option<PathBuf> {
    asset_of(resolve, note, uri).map(|(_, path)| path)
}

/// [`asset`] with its key.
pub(crate) fn asset_of(resolve: &Resolve, note: &str, uri: &str) -> Option<(String, PathBuf)> {
    let rel = accent_file(uri)?;
    resolve_asset(resolve, note, &rel)
}

/// The page an embedded diagram's address names (`![[x.drawio#Page]]` asks for
/// `x.drawio?page=Page`), percent-decoded; `None` for its first page or any other address.
pub(crate) fn diagram_page(uri: &str) -> Option<String> {
    let query = uri.split_once('?')?.1;
    let query = query.split_once('#').map_or(query, |(q, _)| q);
    query
        .split('&')
        .find_map(|p| p.strip_prefix("page="))
        .map(percent_decode)
}

/// Whether an `accent://file/` address names a diagram, which an export draws rather than
/// copies.
pub(crate) fn is_diagram(uri: &str) -> bool {
    accent_file(uri).is_some_and(|rel| accent_core::path::is_diagram(&rel))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch directory of our own, since the crate has no tempdir dependency.
    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("accent-preview-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A resolver of the shape a caller that owns the root passes in: a join that canonicalises,
    /// so nothing under the root can lead out of it.
    fn vault_resolver(root: PathBuf) -> impl Fn(&str) -> Option<(String, PathBuf)> + Send + Sync {
        move |rel| {
            let root = root.canonicalize().ok()?;
            let path = root.join(rel).canonicalize().ok()?;
            path.starts_with(&root).then(|| (rel.to_string(), path))
        }
    }

    #[test]
    fn resolve_asset_accepts_a_path_inside_the_vault() {
        let root = scratch("inside");
        std::fs::create_dir(root.join("attachments")).unwrap();
        std::fs::write(root.join("attachments/img.png"), b"x").unwrap();
        let resolve = vault_resolver(root.clone());
        assert_eq!(
            resolve_asset(&resolve, "n.md", "attachments/img.png"),
            Some((
                "attachments/img.png".to_string(),
                root.canonicalize().unwrap().join("attachments/img.png")
            ))
        );
        // Nothing there is the resolver's `None`, and reaches the reader as the same refusal.
        assert_eq!(resolve_asset(&resolve, "n.md", "missing.png"), None);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn resolve_asset_rejects_traversal_and_absolute_paths() {
        // A resolver that hands back whatever it is asked for, so a `None` below can only have
        // come from the check here — which is the point: it holds for any resolver.
        let naive = |rel: &str| Some((rel.to_string(), PathBuf::from(rel)));
        for rel in [
            "../../../../etc/passwd",
            "notes/../../etc/passwd",
            "/etc/passwd",
            "",
        ] {
            assert_eq!(resolve_asset(&naive, "n.md", rel), None, "{rel}");
        }
    }

    #[test]
    fn resolve_asset_leaves_a_symlink_out_of_the_vault_to_the_resolver() {
        let root = scratch("symlink");
        let outside = scratch("symlink-target");
        std::fs::write(outside.join("secret.txt"), b"x").unwrap();
        std::os::unix::fs::symlink(outside.join("secret.txt"), root.join("escape.txt")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("out")).unwrap();

        // Neither path is lexically wrong, so the check here passes them on; refusing them takes
        // the root, which only the resolver has.
        let resolve = vault_resolver(root.clone());
        assert_eq!(resolve_asset(&resolve, "n.md", "escape.txt"), None);
        assert_eq!(resolve_asset(&resolve, "n.md", "out/secret.txt"), None);
        std::fs::remove_dir_all(&root).unwrap();
        std::fs::remove_dir_all(&outside).unwrap();
    }

    /// Verify loose-note assets remain within their directory, including after symlink resolution.
    #[test]
    fn a_loose_note_reads_its_images_beside_it() {
        let root = scratch("loose").canonicalize().unwrap();
        let dir = root.join("n");
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        for file in ["n/img.png", "n/sub/b.png", "above.png"] {
            std::fs::write(root.join(file), b"x").unwrap();
        }
        std::os::unix::fs::symlink(root.join("above.png"), dir.join("out.png")).unwrap();
        let note = dir.join("note.md");
        // A loose note never asks the vault's resolver.
        let vault = |_: &str| -> Option<(String, PathBuf)> { unreachable!() };
        let found = |rel: &Path| resolve_asset(&vault, note.to_str()?, rel.to_str()?);
        let img = dir.join("img.png");
        // `![](img.png)` as the page based at the folder asks for it, then `![[img.png]]`.
        assert_eq!(
            found(&img),
            Some((img.to_string_lossy().into_owned(), img.clone()))
        );
        assert_eq!(found(Path::new("img.png")).map(|(_, p)| p), Some(img));
        let deeper = dir.join("sub/b.png");
        assert_eq!(found(&deeper).map(|(_, p)| p), Some(deeper));
        // `![](../above.png)`, the page's way and a wikilink's, and a symlink out of the folder.
        assert_eq!(found(&root.join("above.png")), None);
        assert_eq!(found(Path::new("../above.png")), None);
        assert_eq!(found(Path::new("out.png")), None);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn an_embedded_diagram_names_its_page() {
        let uri = "accent://file/Figures/flow.drawio?page=Page%202";
        assert!(is_diagram(uri));
        assert_eq!(diagram_page(uri).as_deref(), Some("Page 2"));
        assert_eq!(diagram_page("accent://file/flow.drawio"), None);
        assert!(!is_diagram("accent://file/flow.png"));
    }
}
