//! Android vault handle retained for the vault lifetime. Wrap the façade
//! without git, language servers or remote access; add a blocking event drain.

use std::path::Path;
use std::sync::Mutex;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use accent_core::config::VaultConfig;
use accent_core::fs::Etag;
use accent_core::index::{Backlink, FileRow};

use crate::ffi::convert::{self, Note, NoteAlias, PdfLink, SearchHit, TagCount};
use crate::ffi::error::Answer;
use crate::ffi::event::{self, Event};

#[derive(uniffi::Object)]
pub struct Vault {
    inner: crate::Vault,
    /// The receiver is `Send` but not `Sync`, and one caller drains it. See [`Vault::next_events`].
    events: Mutex<Receiver<crate::Event>>,
}

#[uniffi::export]
impl Vault {
    /// Open `root` without a filesystem watcher, returning once the index opens.
    /// The worker scans asynchronously and emits [`Event::Progress`] and
    /// [`Event::Reconciled`]. Call [`Vault::rescan`] on foregrounding because
    /// inotify on emulated Android storage drops events.
    #[uniffi::constructor]
    pub fn open(root: String) -> Answer<Self> {
        let (inner, events) =
            crate::Vault::open_unwatched(Path::new(&root), VaultConfig::default())?;
        Ok(Vault {
            inner,
            events: Mutex::new(events),
        })
    }

    /// The vault root, as the walk canonicalised it.
    pub fn root(&self) -> String {
        self.inner.root().to_string_lossy().into_owned()
    }

    /// Walk the files again and bring the index level with them.
    pub fn rescan(&self) -> Answer<()> {
        Ok(self.inner.rescan()?)
    }

    /// Stop the walk that is running, keeping every file it has already indexed.
    ///
    /// The [`Event::Reconciled`] that ends it says `stopped`, and the vault then ignores
    /// [`Vault::rescan`] until [`Vault::resume_indexing`]. Opening it again resumes too.
    pub fn stop_indexing(&self) -> Answer<()> {
        Ok(self.inner.stop_indexing()?)
    }

    /// Walk again after [`Vault::stop_indexing`], indexing what the stopped walk had not reached.
    pub fn resume_indexing(&self) -> Answer<()> {
        Ok(self.inner.resume_indexing()?)
    }

    /// Drain events, waiting up to `timeout_ms` for the first. Blocks: call
    /// off the main thread. An empty result means the timeout expired.
    pub fn next_events(&self, timeout_ms: u64) -> Vec<Event> {
        let events = self.events.lock().unwrap_or_else(|e| e.into_inner());
        let first = match events.recv_timeout(Duration::from_millis(timeout_ms)) {
            Ok(e) => e,
            Err(_) => return Vec::new(),
        };
        std::iter::once(first)
            .chain(events.try_iter())
            .filter_map(event::narrow)
            .collect()
    }

    // ------------------------------------------------------------------------------- one file

    /// A note's text and the stamp a save has to be made against.
    pub fn read(&self, rel: String) -> Answer<Note> {
        let (text, etag) = self.inner.read(&rel)?;
        Ok(Note { text, etag })
    }

    /// Save `text` at `rel`, requiring the `expected` etag. Use `None` only
    /// when creating a file. [`crate::ffi::AccentError::ChangedOnDisk`] carries the
    /// current disk etag after refusal.
    pub fn save(&self, rel: String, text: String, expected: Option<Etag>) -> Answer<Etag> {
        Ok(self.inner.save(&rel, &text, expected)?)
    }

    pub fn exists(&self, rel: String) -> bool {
        self.inner.exists(&rel)
    }

    /// The file's stamp, or `None` if it is not there.
    pub fn stat(&self, rel: String) -> Answer<Option<Etag>> {
        Ok(self.inner.stat(&rel)?)
    }

    /// Create a note, optionally from a template.
    pub fn create_note(&self, rel: String, template: Option<String>) -> Answer<()> {
        self.inner.create_note(&rel, template.as_deref())?;
        Ok(())
    }

    /// Delete a file. There is no trash here: Android has none to put it in.
    pub fn delete(&self, rel: String) -> Answer<()> {
        Ok(self.inner.delete(&rel)?)
    }

    pub fn create_dir(&self, rel: String) -> Answer<()> {
        Ok(self.inner.create_dir(&rel)?)
    }

    /// The vault-relative path an embed names, resolved through the index when it is written as
    /// a bare file name.
    pub fn asset(&self, rel: String) -> Option<String> {
        self.inner.asset(&rel)
    }

    /// Return an absolute device path for PDF or image byte readers,
    /// resolved within the local vault.
    pub fn path_of(&self, rel: String) -> Answer<String> {
        Ok(self.inner.fetch(&rel)?.to_string_lossy().into_owned())
    }

    // ------------------------------------------------------------------------------- the index

    /// The direct children of a vault-relative directory; `""` is the root.
    pub fn list_dir(&self, rel: String) -> Answer<Vec<FileRow>> {
        Ok(self.inner.list_dir(&rel)?)
    }

    /// Full-text search over the indexed notes, best first.
    pub fn search(
        &self,
        query: String,
        limit: u32,
        include_ignored: bool,
    ) -> Answer<Vec<SearchHit>> {
        let hits = self.inner.search(&query, limit as usize, include_ignored)?;
        Ok(convert::all(hits))
    }

    /// Every tag in the vault with how many files carry it, most used first.
    pub fn tags(&self) -> Answer<Vec<TagCount>> {
        Ok(self
            .inner
            .tags()?
            .into_iter()
            .map(|(name, count)| TagCount { name, count })
            .collect())
    }

    pub fn files_with_tag(&self, tag: String) -> Answer<Vec<FileRow>> {
        Ok(self.inner.files_with_tag(&tag)?)
    }

    /// The notes that link to this one, with where in each the link sits.
    pub fn backlinks(&self, rel: String) -> Answer<Vec<Backlink>> {
        Ok(self.inner.backlinks(&rel)?)
    }

    /// Every note link that points into a page of this PDF: what paints as a highlight over it.
    pub fn pdf_links(&self, rel: String) -> Answer<Vec<PdfLink>> {
        Ok(convert::all(self.inner.pdf_links(&rel)?))
    }

    /// The files the filesystem touched most recently, which is what an empty switcher shows.
    pub fn recent_files(&self, limit: u32) -> Answer<Vec<String>> {
        Ok(self.inner.recent_files(limit as usize)?)
    }

    pub fn file_paths(&self, include_ignored: bool) -> Answer<Vec<String>> {
        Ok(self.inner.file_paths(include_ignored)?)
    }

    /// Missing linked notes by their creation paths, listed after existing
    /// files in the switcher.
    pub fn missing_notes(&self) -> Answer<Vec<String>> {
        Ok(self.inner.missing_notes()?)
    }

    /// Ignored-folder notes listed after indexed files. Walk again if `fresh`;
    /// otherwise reuse the cached listing.
    pub fn ignored_notes(&self, fresh: bool) -> Answer<Vec<String>> {
        Ok(self.inner.ignored_notes(fresh)?)
    }

    /// Frontmatter aliases and their notes, for switcher lookup. Aliases do
    /// not replace filenames as link targets.
    pub fn note_aliases(&self) -> Answer<Vec<NoteAlias>> {
        Ok(self
            .inner
            .note_aliases()?
            .into_iter()
            .map(|(name, rel_path)| NoteAlias { name, rel_path })
            .collect())
    }

    /// What a link target resolves to, or `None` when nothing in the vault answers to it.
    pub fn resolve_link(&self, target: String) -> Answer<Option<String>> {
        Ok(self.inner.resolve_link(&target)?)
    }

    /// Resolve `target`, falling back from [`Vault::resolve_link`] to its
    /// creation path on disk in unindexed trees, such as ignored `build/` or
    /// `node_modules`. `None` means the file is absent.
    pub fn follow(&self, target: String) -> Answer<Option<String>> {
        Ok(self.inner.follow(&target)?)
    }

    // --------------------------------------------------------------------------- sync conflicts

    /// The conflict copies Syncthing left beside this note.
    pub fn conflicts_of(&self, rel: String) -> Answer<Vec<String>> {
        Ok(self.inner.conflicts_of(&rel)?)
    }

    /// Take the conflict copy as the note: its text replaces the original's and the copy goes.
    pub fn adopt_conflict(&self, original: String, conflict: String) -> Answer<Etag> {
        Ok(self.inner.adopt_conflict(&original, &conflict)?)
    }

    // ------------------------------------------------------------------------------- templates

    pub fn templates(&self) -> Answer<Vec<String>> {
        Ok(self.inner.templates()?)
    }

    /// The templates that name their own destination, which is what a daily note is.
    pub fn template_targets(&self) -> Answer<Vec<String>> {
        Ok(self.inner.template_targets()?)
    }

    /// Create or find a template's `accent-target:` and return its path.
    /// `None` means the template has no destination.
    pub fn note_from_template(&self, template: String) -> Answer<Option<String>> {
        Ok(self
            .inner
            .note_from_template(&template)?
            .map(|(rel, _)| rel))
    }

    /// The folder the templates are read from, which is where a reader is told to look.
    pub fn templates_dir(&self) -> String {
        self.inner.config().templates_dir
    }

    /// Where the note a template would make goes, without making it.
    pub fn template_target(&self, template: String) -> Answer<Option<String>> {
        Ok(self.inner.template_target(&template)?)
    }

    /// Read unexpanded `accent-target:` (for example, `Daily/{{date}}.md`)
    /// to distinguish dated from fixed destinations. `None` if absent.
    pub fn template_raw_target(&self, template: String) -> Answer<Option<String>> {
        let (text, _) = self.inner.read(&template)?;
        Ok(accent_core::template::parse(&text).target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify template creation returns the same path on repeated requests,
    /// preserving daily-note behavior.
    #[test]
    fn a_template_hands_back_the_note_it_made() {
        let root = tempfile::tempdir().unwrap();
        let index = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("Templates")).unwrap();
        std::fs::write(
            root.path().join("Templates/Log.md"),
            "---\naccent-target: Logs/{{title}}.md\n---\n# Log\n",
        )
        .unwrap();
        let (inner, events) = crate::Vault::open_unwatched_at(
            root.path(),
            &index.path().join("index.db"),
            VaultConfig::default(),
        )
        .unwrap();
        let v = Vault {
            inner,
            events: Mutex::new(events),
        };
        let made = || {
            v.note_from_template("Templates/Log.md".to_string())
                .unwrap()
        };
        assert_eq!(made().as_deref(), Some("Logs/Log.md"));
        assert_eq!(
            std::fs::read_to_string(root.path().join("Logs/Log.md")).unwrap(),
            "# Log\n"
        );
        assert_eq!(made().as_deref(), Some("Logs/Log.md"));
        assert_eq!(v.templates_dir(), "Templates");
        // The target as written, which is what tells a dated one, the daily note's, apart.
        let raw = v
            .template_raw_target("Templates/Log.md".to_string())
            .unwrap();
        assert_eq!(raw.as_deref(), Some("Logs/{{title}}.md"));
    }
}
