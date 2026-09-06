//! What an open tab is.
//!
//! A tab used to mean a note: [`editor::Tab`] carried the buffer, the etag and the banner, and
//! images lived in a second list with no state at all, which is why they were left out of the
//! session and never followed a rename. Everything that can be opened is a [`Doc`] now, so one
//! dispatcher opens all of them and the session, rename, close and theme paths have a single
//! list to walk.

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::rc::Rc;

use crate::editor::Tab;
use crate::fileops;
use crate::pdftab::PdfTab;
use crate::terminal::Term;

/// A tab with nothing to edit: an image, or a status page standing in for a file we decline to
/// open. It keeps only what the tab machinery needs from every document.
pub struct Viewer {
    key: RefCell<String>,
    pub page: adw::TabPage,
    /// How far the document is zoomed, `None` while it is fitted to the window. Only an image
    /// has one: a status page and a diff draw at a size nobody chose.
    pub zoom: Cell<Option<f64>>,
}

impl Viewer {
    pub fn new(key: &str, page: adw::TabPage) -> Rc<Viewer> {
        Rc::new(Viewer {
            key: RefCell::new(key.to_string()),
            page,
            zoom: Cell::new(None),
        })
    }

    pub fn key(&self) -> String {
        self.key.borrow().clone()
    }
}

/// One open document. `Text` is both prose and code: the flavour lives on the tab, because
/// everything around it (etag, autosave, find, zoom) is the same for a note and a source file.
#[derive(Clone)]
pub enum Doc {
    Text(Rc<Tab>),
    Image(Rc<Viewer>),
    Pdf(Rc<PdfTab>),
    Status(Rc<Viewer>),
    /// A comparison of two texts: a git diff, a note against what is on disk, a sync conflict.
    Diff(Rc<Viewer>),
    /// A shell. A document like any other, so it splits and drags with the rest.
    Terminal(Rc<Term>),
}

impl Doc {
    /// Vault-relative for a file inside the vault, absolute for a loose one. See [`Doc::is_loose`].
    pub fn key(&self) -> String {
        match self {
            Doc::Text(tab) => tab.rel(),
            Doc::Pdf(pdf) => pdf.key(),
            Doc::Image(v) | Doc::Status(v) | Doc::Diff(v) => v.key(),
            Doc::Terminal(t) => t.key(),
        }
    }

    /// Not a file. A diff is a view of two texts and a terminal is a running shell; neither is
    /// written to the session, is a recent note, or has anywhere to be pointed by a rename.
    pub fn is_transient(&self) -> bool {
        matches!(self, Doc::Diff(_) | Doc::Terminal(_))
    }

    pub fn page(&self) -> &adw::TabPage {
        match self {
            Doc::Text(tab) => &tab.page,
            Doc::Pdf(pdf) => &pdf.page,
            Doc::Image(v) | Doc::Status(v) | Doc::Diff(v) => &v.page,
            Doc::Terminal(t) => &t.page,
        }
    }

    pub fn tab(&self) -> Option<&Rc<Tab>> {
        match self {
            Doc::Text(tab) => Some(tab),
            _ => None,
        }
    }

    pub fn terminal(&self) -> Option<&Rc<Term>> {
        match self {
            Doc::Terminal(term) => Some(term),
            _ => None,
        }
    }

    pub fn pdf(&self) -> Option<&Rc<PdfTab>> {
        match self {
            Doc::Pdf(pdf) => Some(pdf),
            _ => None,
        }
    }

    /// A file from outside the vault, opened by its absolute path. There is no index behind it,
    /// so it has no backlinks, never appears in search, and reloads from a file monitor of its
    /// own rather than from the vault's watcher.
    pub fn is_loose(&self) -> bool {
        is_loose_key(&self.key())
    }

    /// A rename landed on this document: point it at the new path and relabel the tab.
    pub fn retarget(&self, root: &Path, key: &str) {
        match self {
            Doc::Text(tab) => tab.retarget(root, key),
            Doc::Pdf(pdf) => pdf.retarget(root, key),
            // Neither a diff nor a shell is a file, so a rename has nothing to point them at.
            Doc::Diff(_) | Doc::Terminal(_) => {}
            Doc::Image(v) | Doc::Status(v) => {
                *v.key.borrow_mut() = key.to_string();
                v.page.set_title(file_name(key));
                v.page.set_tooltip(&fileops::display_path(root, key));
            }
        }
    }
}

pub fn is_loose_key(key: &str) -> bool {
    Path::new(key).is_absolute()
}

/// The last path segment, which is what a tab is titled with.
pub fn file_name(key: &str) -> &str {
    key.rsplit('/').next().unwrap_or(key)
}

/// What a path opens as, decided by its name alone. Whether a `Text` file really is text is a
/// question for its bytes, which only the opener has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Note,
    Image,
    Pdf,
    Text,
}

pub fn kind_of(key: &str) -> Kind {
    // The extension of the file name, never of a directory along the way: `v1.2/README` has no
    // extension at all.
    let name = file_name(key);
    match name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()) {
        Some(ext) => match ext.as_str() {
            "md" | "markdown" => Kind::Note,
            "pdf" => Kind::Pdf,
            _ if accent_core::markdown::is_image(name) => Kind::Image,
            _ => Kind::Text,
        },
        // No extension: LICENSE, Makefile, Dockerfile and friends are all text.
        None => Kind::Text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_of_classifies_by_extension_and_case() {
        assert_eq!(kind_of("Notes/A.MD"), Kind::Note);
        assert_eq!(kind_of("a.markdown"), Kind::Note);
        assert_eq!(kind_of("Attachments/paper.pdf"), Kind::Pdf);
        assert_eq!(kind_of("logo.svg"), Kind::Image);
        assert_eq!(kind_of("shot.JPG"), Kind::Image);
        assert_eq!(kind_of("src/main.rs"), Kind::Text);
        assert_eq!(kind_of("LICENSE"), Kind::Text);
        // The dot belongs to the directory, so the file has no extension.
        assert_eq!(kind_of("v1.2/README"), Kind::Text);
    }

    #[test]
    fn a_loose_key_is_an_absolute_path() {
        assert!(is_loose_key("/etc/hosts"));
        assert!(!is_loose_key("Inbox/note.md"));
        // A diff key names a comparison, not a path, so it is never loose whatever it compares.
        assert!(!is_loose_key("diff:worktree:/etc/hosts"));
    }
}
