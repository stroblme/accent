//! Arithmetic on vault-relative paths: `"a/b/c.md"` and friends, always `/`-separated and never
//! absolute. One home for the split every layer used to redo with `rsplit_once('/')`.

use crate::markdown;

/// `"a/b/c.md"` -> `("a/b", "c.md")`; a name at the root gives `("", name)`.
fn split(rel: &str) -> (&str, &str) {
    match rel.rsplit_once('/') {
        Some((dir, name)) => (dir, name),
        None => ("", rel),
    }
}

/// The file name: `"a/b/c.md"` -> `"c.md"`.
pub fn basename(rel: &str) -> &str {
    split(rel).1
}

/// The directory part: `"a/b/c.md"` -> `"a/b"`, `"c.md"` -> `""`.
pub fn parent_dir(rel: &str) -> &str {
    split(rel).0
}

/// The file name without its extension: `"a/b/c.md"` -> `"c"`.
pub fn stem(rel: &str) -> String {
    markdown::strip_ext(basename(rel))
}

/// The half-open `rel_path` range that is exactly the descendants of `rel`: `[rel/, rel0)`,
/// because `'0'` is the byte after `'/'`. Keeps a subtree query on the `rel_path` index where a
/// `LIKE` would fall back to a scan.
pub fn subtree_range(rel: &str) -> (String, String) {
    (format!("{rel}/"), format!("{rel}0"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_a_vault_relative_path() {
        assert_eq!((parent_dir("a.md"), basename("a.md")), ("", "a.md"));
        assert_eq!(
            (parent_dir("a/b/c.md"), basename("a/b/c.md")),
            ("a/b", "c.md")
        );
        assert_eq!(stem("sub/Rev 1.2.md"), "Rev 1.2");
        assert_eq!(stem("Makefile"), "Makefile");
        assert_eq!(subtree_range("a/b"), ("a/b/".into(), "a/b0".into()));
    }
}
