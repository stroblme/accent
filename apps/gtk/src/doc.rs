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

use accent_core::path::{FileType, file_type};

use crate::diff::DiffTab;
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
    /// has one: a status page draws at a size nobody chose.
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
    /// A comparison of two texts that are not files: a staged change, a commit against its
    /// parent. A comparison that involves a file happens inside that file's own `Text` tab.
    Diff(Rc<DiffTab>),
    /// A shell. A document like any other, so it splits and drags with the rest.
    Terminal(Rc<Term>),
    /// A draw.io diagram.
    Diagram(Rc<crate::diagram::DiagramTab>),
}

impl Doc {
    /// Vault-relative for a file inside the vault, absolute for a loose one. See [`Doc::is_loose`].
    pub fn key(&self) -> String {
        match self {
            Doc::Text(tab) => tab.rel(),
            Doc::Pdf(pdf) => pdf.key(),
            Doc::Image(v) | Doc::Status(v) => v.key(),
            Doc::Diff(d) => d.key(),
            Doc::Terminal(t) => t.key(),
            Doc::Diagram(d) => d.key(),
        }
    }

    /// Not a file. A diff is a view of two texts and a terminal is a running shell; neither is a
    /// recent note or has anywhere to be pointed by a rename. See [`Doc::persists`] for the session.
    pub fn is_transient(&self) -> bool {
        matches!(self, Doc::Diff(_) | Doc::Terminal(_))
    }

    /// Whether the session writes this tab down and puts it back. A file, and a shell, which is
    /// started again where it was; not a diff, which is a view of two texts nobody can reopen.
    pub fn persists(&self) -> bool {
        !matches!(self, Doc::Diff(_))
    }

    pub fn page(&self) -> &adw::TabPage {
        match self {
            Doc::Text(tab) => &tab.page,
            Doc::Pdf(pdf) => &pdf.page,
            Doc::Image(v) | Doc::Status(v) => &v.page,
            Doc::Diff(d) => &d.page,
            Doc::Terminal(t) => &t.page,
            Doc::Diagram(d) => &d.page,
        }
    }

    pub fn tab(&self) -> Option<&Rc<Tab>> {
        match self {
            Doc::Text(tab) => Some(tab),
            _ => None,
        }
    }

    pub fn diff(&self) -> Option<&Rc<DiffTab>> {
        match self {
            Doc::Diff(d) => Some(d),
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

    pub fn diagram(&self) -> Option<&Rc<crate::diagram::DiagramTab>> {
        match self {
            Doc::Diagram(d) => Some(d),
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
            Doc::Diagram(d) => d.retarget(root, key),
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
    accent_core::path::basename(key)
}

/// What a path opens as, decided by its name alone. Whether a `Text` file really is text is a
/// question for its bytes, which only the opener has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Note,
    Image,
    Pdf,
    Diagram,
    Text,
}

pub fn kind_of(key: &str) -> Kind {
    match file_type(key) {
        FileType::Note => Kind::Note,
        FileType::Pdf => Kind::Pdf,
        FileType::Image => Kind::Image,
        FileType::Diagram => Kind::Diagram,
        // A table, a source file, LICENSE and anything unknown open as text, if the bytes agree.
        _ => Kind::Text,
    }
}

/// Whether a file written under `to` opens as something other than the tab made for `from`: the
/// extension picks the kind, the flavour and the language, so a different one wants a new tab.
pub fn opens_differently(from: &str, to: &str) -> bool {
    let extension = |key: &str| Path::new(key).extension().map(|e| e.to_ascii_lowercase());
    extension(from) != extension(to)
}

/// The icon every file list leads a directory's row with.
pub const FOLDER_ICON: &str = "filetype-folder-symbolic";

/// The icon every file list leads `key`'s row with. The `filetype-*` set is shipped in the app's
/// GResource (`data/accent.gresource.xml`); tabs keep the icons their documents give them.
pub fn icon_for(key: &str) -> &'static str {
    match file_type(key) {
        FileType::Note => "filetype-markdown-symbolic",
        FileType::Pdf => "filetype-pdf-symbolic",
        FileType::Diagram => "filetype-diagram-symbolic",
        FileType::Table => "filetype-table-symbolic",
        FileType::Image => "filetype-image-symbolic",
        FileType::Code => "filetype-code-symbolic",
        FileType::Config => "filetype-config-symbolic",
        FileType::Text => "filetype-text-symbolic",
        FileType::Other => "filetype-file-symbolic",
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
        assert_eq!(kind_of("Figures/flow.drawio"), Kind::Diagram);
        assert_eq!(kind_of("logo.svg"), Kind::Image);
        assert_eq!(kind_of("shot.JPG"), Kind::Image);
        assert_eq!(kind_of("src/main.rs"), Kind::Text);
        assert_eq!(kind_of("LICENSE"), Kind::Text);
        // The dot belongs to the directory, so the file has no extension.
        assert_eq!(kind_of("v1.2/README"), Kind::Text);
    }

    /// An icon name the theme cannot resolve draws nothing and says nothing, so every name a file
    /// list can ask for has to be in the GResource.
    #[test]
    fn every_file_icon_is_shipped() {
        let xml = include_str!("../data/accent.gresource.xml");
        let keys = [
            "a.md", "a.pdf", "a.drawio", "a.csv", "a.png", "a.rs", "a.toml", "a.txt", "a.zip",
        ];
        let mut names: Vec<&str> = keys.into_iter().map(icon_for).collect();
        names.push(FOLDER_ICON);
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), 10, "one name per file type, and the folder");
        for name in names {
            assert!(
                xml.contains(&format!("icons/scalable/actions/{name}.svg")),
                "{name} is not in the GResource"
            );
        }
    }

    #[test]
    fn a_new_extension_opens_differently() {
        assert!(!opens_differently("Inbox/note.md", "Archive/other.md"));
        assert!(
            !opens_differently("note.md", "note.MD"),
            "case is not a new kind"
        );
        assert!(opens_differently("note.md", "note.txt"));
        assert!(
            opens_differently("main.rs", "main.py"),
            "the language follows it"
        );
        assert!(opens_differently("LICENSE", "LICENSE.md"));
    }

    #[test]
    fn a_loose_key_is_an_absolute_path() {
        assert!(is_loose_key("/etc/hosts"));
        assert!(!is_loose_key("Inbox/note.md"));
        // A diff key names a comparison, not a path, so it is never loose whatever it compares.
        assert!(!is_loose_key("diff:worktree:/etc/hosts"));
    }
}
