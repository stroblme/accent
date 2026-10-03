//! The files in the folders git ignores, which the index never walks ([`walk::ignored_files`]):
//! the notes among them listed by Go to File and `[[` completion, and every one of them looked
//! for by a link the index calls dangling before it is one.

use std::collections::HashMap;
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

/// A vault's [`Listing`], kept between walks: Go to File opening walks again, and everything else
/// takes the last walk's.
pub(crate) struct Ignored {
    root: PathBuf,
    listing: Mutex<Option<Arc<Listing>>>,
}

impl Ignored {
    pub(crate) fn new(root: PathBuf) -> Ignored {
        Ignored {
            root,
            listing: Mutex::new(None),
        }
    }

    /// The listing, walked now when `fresh` or when there has been no walk yet. The lock is not
    /// held across the walk, so nobody asking for the kept one waits on a fresh one.
    pub(crate) fn listing(&self, fresh: bool) -> Arc<Listing> {
        if !fresh && let Some(listing) = locked(&self.listing).as_ref() {
            return listing.clone();
        }
        let listing = Arc::new(Listing::new(walk::ignored_files(&self.root)));
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
