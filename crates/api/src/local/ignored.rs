//! Cache files in gitignored folders ([`walk::ignored_files`]). Offer ignored
//! notes in Go to File and link completion, and resolve targets outside the
//! index before declaring them missing.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use accent_core::markdown;
use accent_core::path::{FileType, file_type};
use accent_core::walk;

use crate::locked;

/// One walk's files, with the link key each answers to.
pub(crate) struct Listing {
    files: Vec<String>,
    /// Each link key to the file answering to it by the index's rule: the shortest path wins,
    /// the first in path order breaking a tie.
    keys: HashMap<String, usize>,
}

impl Listing {
    fn new(files: Vec<String>) -> Listing {
        let mut keys: HashMap<String, usize> = HashMap::new();
        for (i, rel) in files.iter().enumerate() {
            for key in markdown::path_keys(rel) {
                keys.entry(key)
                    .and_modify(|at| {
                        if rel.len() < files[*at].len() {
                            *at = i;
                        }
                    })
                    .or_insert(i);
            }
        }
        Listing { files, keys }
    }

    /// The notes among them: what Go to File and `[[` list. The rest stays out of both, as an
    /// ignored file the index holds does.
    pub(crate) fn notes(&self) -> impl Iterator<Item = &String> {
        self.files
            .iter()
            .filter(|rel| file_type(rel) == FileType::Note)
    }

    /// The file a link target names among them.
    pub(crate) fn resolve(&self, target: &str) -> Option<&str> {
        let at = *self.keys.get(&markdown::link_key(target))?;
        Some(&self.files[at])
    }
}

/// Cache a vault's [`Listing`]. Opening Go to File refreshes it; other
/// consumers reuse the last walk.
pub(crate) struct Ignored {
    root: PathBuf,
    listing: Mutex<Option<Arc<Listing>>>,
    /// The ignored folders the vault's walks found, the only ones a listing walks; `None` until
    /// the first walk ends, when a listing walks the vault to find them.
    dirs: Mutex<Option<BTreeSet<String>>>,
}

impl Ignored {
    pub(crate) fn new(root: PathBuf) -> Ignored {
        Ignored {
            root,
            listing: Mutex::new(None),
            dirs: Mutex::new(None),
        }
    }

    /// What a walk of folder `dir` (`""` for the vault) found: the ignored folders below it. The
    /// folder itself is not the walk's to report.
    pub(crate) fn found(&self, dir: &str, found: Vec<String>) {
        let mut dirs = locked(&self.dirs);
        let dirs = dirs.get_or_insert_with(BTreeSet::new);
        dirs.retain(|d| !(dir.is_empty() || d.starts_with(&format!("{dir}/"))));
        dirs.extend(found);
    }

    /// An ignored folder made since the last walk.
    pub(crate) fn add(&self, dir: &str) {
        if let Some(dirs) = locked(&self.dirs).as_mut() {
            dirs.insert(dir.to_string());
        }
    }

    /// Return the cached listing, walking if `fresh` or uninitialized. Release
    /// the lock during walks so cached readers do not wait on refreshes.
    pub(crate) fn listing(&self, fresh: bool) -> Arc<Listing> {
        if !fresh && let Some(listing) = locked(&self.listing).as_ref() {
            return listing.clone();
        }
        let dirs: Option<Vec<String>> = locked(&self.dirs)
            .as_ref()
            .map(|dirs| dirs.iter().cloned().collect());
        let files = walk::ignored_files(&self.root, dirs.as_deref());
        let listing = Arc::new(Listing::new(files));
        *locked(&self.listing) = Some(listing.clone());
        listing
    }
}

#[cfg(test)]
mod tests {
    use super::Listing;

    fn listing(files: &[&str]) -> Listing {
        Listing::new(files.iter().map(|f| f.to_string()).collect())
    }

    /// A link finds a file by its name or its path, with or without the extension, whatever the
    /// case, and the shortest path answering wins, as in the index.
    #[test]
    fn a_link_resolves_against_the_listing_as_against_the_index() {
        let l = listing(&[
            "build/deep/Deep Note.md",
            "build/Deep Note.md",
            "out/paper.pdf",
        ]);
        for target in ["Deep Note", "deep note.md", "build/Deep Note"] {
            assert_eq!(l.resolve(target), Some("build/Deep Note.md"), "{target}");
        }
        assert_eq!(
            l.resolve("build/deep/Deep Note"),
            Some("build/deep/Deep Note.md")
        );
        assert_eq!(l.resolve("paper.pdf"), Some("out/paper.pdf"));
        assert_eq!(l.resolve("paper"), Some("out/paper.pdf"));
        assert_eq!(l.resolve("Nowhere"), None);
    }

    #[test]
    fn only_the_notes_are_listed() {
        let l = listing(&["build/a.md", "build/a.o", "out/paper.pdf"]);
        assert_eq!(l.notes().collect::<Vec<_>>(), ["build/a.md"]);
    }
}
