//! Vault file operations, templates, link-preserving moves and sync conflicts
//! for [`Local`].

mod conflicts;
mod copy;
mod links;
mod replace;
mod templates;

use crate::{Error, Result};

use super::{Local, Msg};
use crate::{Etag, fs};

/// A file as it was before a Replace All rewrote it, and the etag the rewrite left on it: what
/// [`Local::undo_replace`] writes back, and only while the file still carries that etag.
pub(super) struct Before {
    rel: String,
    text: String,
    etag: Etag,
}

impl Local {
    pub fn read(&self, rel: &str) -> Result<(String, Etag)> {
        fs::read_note(&self.resolve(rel)?).map_err(named(rel))
    }

    /// Save a note and notify the worker immediately, avoiding watcher debounce
    /// and suppressing an own-save [`crate::Event::FileChanged`]. Writes and fsync run
    /// on the calling thread.
    pub fn save(&self, rel: &str, text: &str, expected: Option<Etag>) -> Result<Etag> {
        let etag = fs::write_note(&self.resolve(rel)?, text, expected).map_err(named(rel))?;
        self.post(Msg::Saved {
            rel: rel.to_string(),
            etag,
            same_text: false,
        });
        Ok(etag)
    }

    /// [`save`](Self::save) where nothing is: a file made at `rel` meanwhile refuses the write
    /// with `ChangedOnDisk` and keeps what it holds.
    pub fn save_new(&self, rel: &str, text: &str) -> Result<Etag> {
        let etag = fs::write_new(&self.resolve(rel)?, text).map_err(named(rel))?;
        self.post(Msg::Saved {
            rel: rel.to_string(),
            etag,
            same_text: false,
        });
        Ok(etag)
    }

    /// Tell the worker that `rel` was written by this vault outside [`save`](Self::save), and left
    /// at `etag`: a PDF's render thread writes its own file. Its watcher echo is then ours, and
    /// with `same_text`, ink drawn and no page edited, the index keeps the text it read of it.
    pub fn saved(&self, rel: &str, etag: Etag, same_text: bool) -> Result<()> {
        self.resolve(rel)?;
        self.post(Msg::Saved {
            rel: rel.to_string(),
            etag,
            same_text,
        });
        Ok(())
    }

    /// Write `bytes` to a file nothing holds yet, an image pasted into a note, and index it as
    /// [`wrote`](Self::wrote) does: an embed of it resolves at once, not a watcher debounce later.
    /// A name taken meanwhile fails with `AlreadyExists(rel)` and keeps what it holds.
    pub fn write_file(&self, rel: &str, bytes: &[u8]) -> Result<()> {
        let path = self.resolve(rel)?;
        // Claimed empty, then filled by the atomic write, as `fs::create_note` makes a note.
        std::fs::File::create_new(&path).map_err(|e| accent_core::Error::io(rel, e))?;
        if let Err(e) = fs::write_bytes(&path, bytes, None) {
            let _ = std::fs::remove_file(&path);
            return Err(e.into());
        }
        self.wrote(rel)
    }

    /// Notify the worker of an external write, including remote
    /// [`write_file`](Self::write_file) bytes transferred separately over SSH, and return once
    /// it is indexed, so a lookup right after finds it. Off the main loop, as
    /// [`settle_index`](Self::settle_index) is.
    pub fn wrote(&self, rel: &str) -> Result<()> {
        self.resolve(rel)?;
        self.post(Msg::Update {
            rel: rel.to_string(),
            own: true,
        });
        self.settle_index();
        Ok(())
    }

    /// Land the upload staged beside `rel` ([`crate::ssh::stage_cmd`]) if `rel` still has the
    /// etag the window sent it for ([`fs::commit`]), the commit of a remote write-back, and tell
    /// the worker as [`saved`](Self::saved) does: the host's watcher echo is then the vault's
    /// own, no news for the window, and with `same_text`, ink drawn and no page edited, the index
    /// keeps the text it read of it.
    pub fn commit(&self, rel: &str, staged: &str, expected: Etag, same_text: bool) -> Result<Etag> {
        let etag = fs::commit(&self.resolve(rel)?, staged, expected).map_err(named(rel))?;
        self.post(Msg::Saved {
            rel: rel.to_string(),
            etag,
            same_text,
        });
        Ok(etag)
    }

    /// Put the first `len` bytes of `rel` before the upload staged beside it, where they have the
    /// window's `digest` and `rel` still has `expected`'s etag ([`fs::prefix_staged`]): a remote
    /// PDF's write-back that sent only the section its save puts after them, which
    /// [`commit`](Self::commit) then lands. The staged file's etag, or `None` where the bytes
    /// differ and the window sends the whole file.
    pub fn prefix_staged(
        &self,
        rel: &str,
        staged: &str,
        expected: Etag,
        len: u64,
        digest: &str,
    ) -> Result<Option<Etag>> {
        fs::prefix_staged(&self.resolve(rel)?, staged, expected, len, digest).map_err(named(rel))
    }

    /// Read any file as text, saying so when it is binary or too big to hold. What a tab opens
    /// with; [`read`](Self::read) is the note-shaped version the rename and conflict paths use.
    pub fn read_text(&self, rel: &str) -> Result<fs::Read> {
        fs::read_text(&self.resolve(rel)?).map_err(named(rel))
    }

    /// Read a host dependency only while a live language session has named its absolute path.
    pub fn read_named(&self, abs: &str) -> Result<fs::Read> {
        if !std::path::Path::new(abs).is_absolute() || !self.lang.named(abs) {
            return Err(
                accent_core::Error::Invalid(format!("no language session named {abs}")).into(),
            );
        }
        fs::read_text(std::path::Path::new(abs)).map_err(named(abs))
    }

    /// The file's etag, or `None` when there is no file there. One `stat`, which is how a tab
    /// asks "did this change under me" and how the app asks "does this path exist".
    pub fn stat(&self, rel: &str) -> Result<Option<Etag>> {
        match Etag::of(&self.resolve(rel)?) {
            Ok(etag) => Ok(Some(etag)),
            Err(accent_core::Error::NotFound(_)) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Permanently delete a file or directory for remote vaults without a
    /// session bus or trash. The desktop uses `gio` trash instead; the UI
    /// confirms permanent deletion.
    pub fn delete(&self, rel: &str) -> Result<()> {
        let path = self.resolve(rel)?;
        match path.is_dir() {
            true => std::fs::remove_dir_all(&path),
            false => std::fs::remove_file(&path),
        }
        .map_err(|e| accent_core::Error::io(rel, e))?;
        self.post(Msg::Update {
            rel: rel.to_string(),
            own: true,
        });
        Ok(())
    }

    pub fn create_dir(&self, rel: &str, merge: bool) -> Result<()> {
        let path = self.resolve(rel)?;
        let made = match (merge, path.parent()) {
            // The parents as `mkdir -p` makes them, then the folder itself, which must be new.
            (false, Some(parent)) => {
                std::fs::create_dir_all(parent).and_then(|()| std::fs::create_dir(&path))
            }
            _ => std::fs::create_dir_all(&path),
        };
        made.map_err(|e| accent_core::Error::io(rel, e))?;
        // ponytail: only the deepest component is indexed straight away; intermediate levels of a
        // nested path wait for the watcher or the next reconcile. Post one update per component
        // if the tree ever looks wrong right after "New folder".
        self.post(Msg::Update {
            rel: rel.to_string(),
            own: true,
        });
        Ok(())
    }
}

/// `text` with a language server's byte edits applied, back to front. Inserts at one place keep
/// the order the server listed them in, as LSP has it; overlapping edits are refused rather than
/// guessed at.
pub(crate) fn applied(mut text: String, edits: &[(usize, usize, String)]) -> Result<String> {
    let mut order: Vec<_> = edits.iter().enumerate().collect();
    order.sort_by_key(|(i, (start, ..))| std::cmp::Reverse((*start, *i)));
    let mut end = text.len();
    for (_, (start, stop, with)) in order {
        let fits = start <= stop
            && *stop <= end
            && text.is_char_boundary(*start)
            && text.is_char_boundary(*stop);
        if !fits {
            return Err(Error::Language(
                "the language server's edits overlap".to_string(),
            ));
        }
        text.replace_range(*start..*stop, with);
        end = *start;
    }
    Ok(text)
}

/// Rewrite a core file error's absolute path as vault-relative `rel`.
fn named(rel: &str) -> impl FnOnce(accent_core::Error) -> Error + '_ {
    move |e| {
        match e {
            accent_core::Error::NotFound(_) => accent_core::Error::NotFound(rel.to_string()),
            accent_core::Error::AlreadyExists(_) => {
                accent_core::Error::AlreadyExists(rel.to_string())
            }
            e => e,
        }
        .into()
    }
}

#[cfg(test)]
mod tests {
    use crate::tests::*;
    use crate::{Etag, Event, Options, UndoReport, VaultConfig, fs};

    /// Back to front, two inserts at one place in the server's order, an overlap refused.
    #[test]
    fn server_edits_apply_back_to_front_in_their_order() {
        let edit = |start, stop, with: &str| (start, stop, with.to_string());
        let text = "int x = 1\n".to_string();
        let edits = [edit(9, 9, ";"), edit(0, 0, "static "), edit(0, 0, "const ")];
        assert_eq!(
            super::applied(text.clone(), &edits).unwrap(),
            "static const int x = 1;\n"
        );
        assert!(super::applied(text, &[edit(0, 5, "a"), edit(3, 7, "b")]).is_err());
    }

    /// A copy stopped a chunk in leaves nothing at a free name, and a file it was replacing as it
    /// was, with no `.part` beside it.
    #[test]
    fn a_stopped_copy_leaves_no_file_behind() {
        let dir = tempfile::tempdir().unwrap();
        let (from, to) = (dir.path().join("big.bin"), dir.path().join("copy.bin"));
        std::fs::write(&from, vec![7u8; 3 * super::copy::CHUNK as usize]).unwrap();
        // Asked before each chunk: stopped from the second asking on.
        let one_chunk = || {
            let asked = std::cell::Cell::new(0);
            move || {
                asked.set(asked.get() + 1);
                asked.get() > 1
            }
        };
        assert!(super::copy::copy_file(&from, &to, &one_chunk()).is_err());
        assert!(!to.exists());
        std::fs::write(&to, "theirs").unwrap();
        assert!(super::copy::replace_file(&from, &to, &one_chunk()).is_err());
        assert_eq!(std::fs::read_to_string(&to).unwrap(), "theirs");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
        super::copy::replace_file(&from, &to, &|| false).unwrap();
        assert_eq!(std::fs::read(&to).unwrap(), std::fs::read(&from).unwrap());
    }

    /// `a.md` links to `B.md`; renaming it to `C.md` must move the link with it.
    fn linked_vault() -> Fixture {
        let f = Fixture::open(VaultConfig::default());
        f.write("a.md", "see [[B]] for details\n");
        f.write("B.md", "the target\n");
        f.vault.rescan().unwrap();
        assert!(f.wait(|e| matches!(e, Event::Reconciled(_))).is_some());
        f
    }

    /// One move, the shape a rename and a single dragged row have.
    fn one(from: &str, to: &str) -> Vec<(String, String)> {
        vec![(from.to_string(), to.to_string())]
    }

    /// A vault holding these notes, indexed.
    fn vault_of(files: &[(&str, &str)]) -> Fixture {
        let f = Fixture::open(VaultConfig::default());
        for (rel, text) in files {
            f.write(rel, text);
        }
        f.vault.rescan().unwrap();
        assert!(f.wait(|e| matches!(e, Event::Reconciled(_))).is_some());
        f
    }

    #[test]
    fn rename_rewrites_wikilinks_and_backlinks_follow() {
        let f = linked_vault();

        let plan = f.vault.plan_moves(&one("B.md", "C.md")).unwrap();
        assert_eq!(plan.rewrites, ["a.md"]);

        let report = f.vault.rename(&plan, true).unwrap();
        assert_eq!(report.moved, one("B.md", "C.md"));
        assert_eq!(report.rewritten, ["a.md"]);
        assert!(report.failed.is_empty());
        assert_eq!(f.read("a.md"), "see [[C]] for details\n");

        assert!(poll_until(
            || f.vault
                .backlinks("C.md")
                .unwrap()
                .iter()
                .any(|b| b.src_rel_path == "a.md"),
            BUDGET
        ));
    }

    /// Save As's copy in another folder points its paths back at what the note's pointed at, the
    /// unsaved `<img>` included, and leaves the wikilink and the original alone.
    #[test]
    fn a_copy_elsewhere_keeps_its_relative_links() {
        let note = "[x](../b.md) ![](img.png) [r][r] [[b]]\n\n[r]: sub/c.md\n";
        let f = vault_of(&[
            ("a/n.md", note),
            ("b.md", "b\n"),
            ("a/img.png", "png"),
            ("a/sub/c.md", "c\n"),
        ]);
        let typed = format!("{note}<img src=\"img.png\">\n");
        assert_eq!(
            f.vault
                .relink_copy("a/n.md", "x/y/copy.md", &typed)
                .unwrap()
                .as_deref(),
            Some(concat!(
                "[x](../../b.md) ![](../../a/img.png) [r][r] [[b]]\n\n",
                "[r]: ../../a/sub/c.md\n<img src=\"../../a/img.png\">\n"
            ))
        );
        assert_eq!(
            f.vault.relink_copy("a/n.md", "a/m.md", &typed).unwrap(),
            None
        );
        assert_eq!(f.read("a/n.md"), note);
    }

    /// A page edit carries every link into the PDF with it, in every note, and the Undo of a
    /// delete puts back exactly what it changed.
    #[test]
    fn a_page_edit_rewrites_the_notes_linking_into_the_pdf() {
        use crate::PageEdit;
        let f = vault_of(&[
            ("paper.pdf", "%PDF-1.4\n"),
            (
                "a.md",
                "[[paper.pdf#page=1&selection=0,0,0,4|Hi]] [[paper.pdf#page=2]]\n",
            ),
            ("sub/b.md", "[three](../paper.pdf#page=3) [[paper.pdf]]\n"),
            ("c.md", "[[paper.pdf#page=2]] elsewhere\n"),
        ]);

        let report = f
            .vault
            .repage_links("paper.pdf", PageEdit::Move { from: 0, to: 2 }, &[])
            .unwrap();
        assert_eq!(report.rewritten, ["a.md", "c.md", "sub/b.md"]);
        assert_eq!((report.moved, report.left.len()), (4, 0));
        assert_eq!(
            f.read("a.md"),
            "[[paper.pdf#page=3&selection=0,0,0,4|Hi]] [[paper.pdf#page=1]]\n"
        );
        assert_eq!(
            f.read("sub/b.md"),
            "[three](../paper.pdf#page=2) [[paper.pdf]]\n"
        );

        // The page that was first is the third now; deleting it leaves the highlight on it.
        let deleted = f
            .vault
            .repage_links("paper.pdf", PageEdit::delete(2), &[])
            .unwrap();
        assert_eq!(deleted.moved, 0);
        assert_eq!(
            deleted.left,
            [crate::KeptLink {
                note: "a.md".into(),
                anchor: "page=3&selection=0,0,0,4".into(),
                nth: 0
            }]
        );
        let undone = f
            .vault
            .repage_links("paper.pdf", PageEdit::insert(2), &deleted.left)
            .unwrap();
        assert_eq!((undone.moved, undone.rewritten.len()), (0, 0));
        assert_eq!(
            f.read("a.md"),
            "[[paper.pdf#page=3&selection=0,0,0,4|Hi]] [[paper.pdf#page=1]]\n"
        );
    }

    /// A reference definition and an HTML `href` follow a page edit, also where no link the index
    /// holds spells their path: `[[paper]]` is keyed by the name alone.
    #[test]
    fn a_page_edit_follows_the_paths_no_link_spells() {
        let f = vault_of(&[
            ("paper.pdf", "%PDF-1.4\n"),
            (
                "c.md",
                "[[paper#page=2]] <a href=\"paper.pdf#page=1\">one</a>\n\n[d]: paper.pdf#page=3\n",
            ),
        ]);
        let move_first = crate::PageEdit::Move { from: 0, to: 2 };
        let report = f.vault.repage_links("paper.pdf", move_first, &[]).unwrap();
        assert_eq!(report.moved, 3);
        assert_eq!(
            f.read("c.md"),
            "[[paper#page=1]] <a href=\"paper.pdf#page=3\">one</a>\n\n[d]: paper.pdf#page=2\n"
        );
    }

    /// A note a symlink takes out of the vault is read through the link but never written by a
    /// rewrite nobody aimed at it: Replace All and a rename's link update leave the file outside
    /// alone and report it, while the notes inside are rewritten as before.
    #[test]
    fn a_rewrite_leaves_a_note_linked_in_from_outside_alone() {
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("y.md"), "colour [[B]]\n").unwrap();
        let f = Fixture::open(VaultConfig::default());
        f.write("a.md", "colour [[B]]\n");
        f.write("B.md", "the target\n");
        let root = f.vault.root().to_path_buf();
        std::os::unix::fs::symlink(outside.path().join("y.md"), root.join("x.md")).unwrap();
        std::os::unix::fs::symlink(root.join("a.md"), root.join("alias.md")).unwrap();
        std::os::unix::fs::symlink(outside.path(), root.join("ext")).unwrap();
        f.vault.rescan().unwrap();
        assert!(f.wait(|e| matches!(e, Event::Reconciled(_))).is_some());

        let crate::vault::Backend::Local(local) = &f.vault.backend else {
            panic!("the fixture opens a local vault");
        };
        assert!(local.resolve_inside("alias.md").is_ok(), "a link inside");
        assert!(local.resolve_inside("x.md").is_err(), "a file link out");
        assert!(
            local.resolve_inside("ext/y.md").is_err(),
            "a folder link out"
        );
        let outside_note = || std::fs::read_to_string(outside.path().join("y.md")).unwrap();
        // `x.md` or `ext/y.md`, whichever the walk kept of the one file both lead to.
        let said = |failed: &[(String, String)]| {
            failed
                .iter()
                .any(|(_, why)| why.contains("outside the vault"))
        };

        let report = f
            .vault
            .replace_all("colour", Options::default(), "color", true, false)
            .unwrap();
        assert_eq!(report.rewritten, ["a.md"], "{report:?}");
        assert!(said(&report.failed), "{report:?}");
        assert_eq!(outside_note(), "colour [[B]]\n");

        let plan = f.vault.plan_moves(&one("B.md", "C.md")).unwrap();
        let report = f.vault.rename(&plan, true).unwrap();
        assert_eq!(report.rewritten, ["a.md"], "{report:?}");
        assert!(said(&report.failed), "{report:?}");
        assert_eq!(f.read("a.md"), "color [[C]]\n");
        assert_eq!(outside_note(), "colour [[B]]\n");
    }

    #[test]
    fn replace_all_rewrites_every_match_and_reindexes() {
        let f = Fixture::open(VaultConfig::default());
        f.write("a.md", "colour and colour\n");
        f.write("sub/b.md", "Colour\n");
        f.write("c.md", "nothing here\n");
        f.vault.rescan().unwrap();
        assert!(f.wait(|e| matches!(e, Event::Reconciled(_))).is_some());

        let plain = Options::default();
        assert_eq!(f.vault.grep("colour", plain, 10, false).unwrap().1, 3);

        let report = f
            .vault
            .replace_all("colour", plain, "color", true, false)
            .unwrap();
        assert_eq!(report.rewritten, ["a.md", "sub/b.md"]);
        assert_eq!(report.matches, 3);
        assert!(report.failed.is_empty());
        assert_eq!(f.read("a.md"), "color and color\n");
        assert_eq!(f.read("sub/b.md"), "color\n", "case-insensitive by default");
        assert_eq!(f.read("c.md"), "nothing here\n");

        // Not polled: the call returns once the worker has taken the rewrites in, because the
        // Search pane asks its question again the moment it does.
        assert_eq!(
            f.vault.grep("colour", plain, 10, false).unwrap().1,
            0,
            "the rewrites must be in the index by the time replace_all returns"
        );
    }

    /// A diagram is rewritten in its labels and nowhere else, one write for the file, and the
    /// undo puts its XML back as it was: a `.drawio`, and an `.xml` only its first element says
    /// is one, which stays a diagram.
    #[test]
    fn replace_all_rewrites_a_diagrams_labels_and_not_its_xml() {
        for name in ["flow.drawio", "flow.xml"] {
            replace_in_diagram(name);
        }
    }

    fn replace_in_diagram(name: &str) {
        let f = Fixture::open(VaultConfig::default());
        let xml = r#"<mxfile><diagram name="P" id="p"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/><mxCell id="k" value="Kettle and kettle" style="kettle=1;" vertex="1" parent="1"><mxGeometry as="geometry"/></mxCell><mxCell id="t" value="&lt;b&gt;Kettle&lt;/b&gt;" style="html=1;" vertex="1" parent="1"><mxGeometry as="geometry"/></mxCell></root></mxGraphModel></diagram></mxfile>"#;
        f.write(name, xml);
        f.vault.rescan().unwrap();
        assert!(f.wait(|e| matches!(e, Event::Reconciled(_))).is_some());

        let plain = Options::default();
        assert_eq!(f.vault.grep("kettle", plain, 10, false).unwrap().1, 3);
        let report = f
            .vault
            .replace_all("kettle", plain, "Pot", true, false)
            .unwrap();
        assert_eq!((report.rewritten.len(), report.matches), (1, 3));
        let file = accent_drawio::File::from_bytes(f.read(name).as_bytes()).unwrap();
        assert_eq!(
            accent_drawio::text::search_text(&file),
            "Pot and Pot\nPot",
            "both labels, the markup kept"
        );
        let cells = &file.pages()[0].cells;
        assert_eq!(
            cells[2].style.get("kettle"),
            Some("1"),
            "a style key is not text"
        );
        assert_eq!(cells[3].label(), "<b>Pot</b>");
        assert!(
            accent_drawio::text::sniff(&f.read(name)),
            "{name} is still a diagram"
        );

        f.vault.undo_replace().unwrap();
        assert_eq!(f.read(name), xml);
    }

    /// A notebook is rewritten in its cells' sources through its model, its outputs, metadata and
    /// every other cell keeping their bytes, and the undo puts the file back as it was.
    #[test]
    fn replace_all_rewrites_a_notebooks_cells_and_not_its_json() {
        let ipynb = r##"{
 "cells": [
  {
   "cell_type": "markdown",
   "metadata": {},
   "source": [
    "# The kettle"
   ]
  },
  {
   "cell_type": "code",
   "execution_count": 1,
   "metadata": {},
   "outputs": [
    {
     "name": "stdout",
     "output_type": "stream",
     "text": [
      "kettle\n"
     ]
    }
   ],
   "source": [
    "print(\"kettle\")"
   ]
  },
  {
   "cell_type": "code",
   "execution_count": null,
   "metadata": {},
   "outputs": [],
   "source": []
  }
 ],
 "metadata": {
  "kettle": true
 },
 "nbformat": 4,
 "nbformat_minor": 4
}
"##;
        let f = Fixture::open(VaultConfig::default());
        f.write("n.ipynb", ipynb);
        f.vault.rescan().unwrap();
        assert!(f.wait(|e| matches!(e, Event::Reconciled(_))).is_some());

        let plain = Options::default();
        let report = f
            .vault
            .replace_all("kettle", plain, "Pot", true, false)
            .unwrap();
        assert_eq!((report.rewritten.len(), report.matches), (1, 2));
        let sources = ipynb
            .replace("\"# The kettle\"", "\"# The Pot\"")
            .replace("print(\\\"kettle\\\")", "print(\\\"Pot\\\")");
        assert_eq!(f.read("n.ipynb"), sources, "the output and metadata kept");

        f.vault.undo_replace().unwrap();
        assert_eq!(f.read("n.ipynb"), ipynb);
    }

    /// The undo writes back through the etag gate, so a note edited since the rewrite keeps the
    /// edit and is named instead, and the undo is spent once used.
    #[test]
    fn undoing_a_replace_skips_the_notes_changed_since() {
        let f = Fixture::open(VaultConfig::default());
        f.write("a.md", "colour\n");
        f.write("b.md", "colour\n");
        f.vault.rescan().unwrap();
        assert!(f.wait(|e| matches!(e, Event::Reconciled(_))).is_some());

        let plain = Options::default();
        let report = f
            .vault
            .replace_all("colour", plain, "color", true, false)
            .unwrap();
        assert!(report.undoable);
        // Another size, so the etag moves whatever the clock's resolution.
        f.write("b.md", "color, edited since\n");

        let undo = f.vault.undo_replace().unwrap();
        assert_eq!(undo.restored, ["a.md"]);
        assert_eq!(undo.skipped, ["b.md"]);
        assert!(undo.failed.is_empty());
        assert_eq!(f.read("a.md"), "colour\n");
        assert_eq!(f.read("b.md"), "color, edited since\n");
        assert_eq!(f.vault.undo_replace().unwrap(), UndoReport::default());
    }

    /// Verify the host budget stops between files and reports those not reached.
    /// A zero budget stops before the first file.
    #[test]
    fn a_rewrite_out_of_time_stops_between_notes_and_reports_the_rest() {
        let f = Fixture::open(VaultConfig::default());
        f.write("a.md", "colour\n");
        f.write("b.md", "colour\n");
        f.vault.rescan().unwrap();
        assert!(f.wait(|e| matches!(e, Event::Reconciled(_))).is_some());

        let crate::vault::Backend::Local(local) = &f.vault.backend else {
            panic!("the fixture opens a local vault");
        };
        let re = accent_core::search::pattern("colour", Options::default()).unwrap();
        let report = local
            .replace_within(&re, "color", true, false, std::time::Duration::ZERO)
            .unwrap();

        assert!(report.rewritten.is_empty());
        assert_eq!(report.matches, 0);
        assert_eq!(
            report.failed.iter().map(|(rel, _)| rel).collect::<Vec<_>>(),
            ["a.md", "b.md"],
            "every note it did not reach is named: {report:?}"
        );
        assert_eq!(f.read("a.md"), "colour\n", "no note is left half written");
        // The pattern still matches them, so asking again finishes the job.
        assert_eq!(
            f.vault
                .grep("colour", Options::default(), 10, false)
                .unwrap()
                .1,
            2
        );
    }

    /// The Search pane's "Replace All (N)": N is what the rewrite touches, and it touches every
    /// file the list shows from the index, a source file as much as a note.
    #[test]
    fn the_replace_count_is_every_indexed_match_and_the_rewrite_touches_them_all() {
        let f = Fixture::open(VaultConfig::default());
        f.write("a.md", "zorblat once\n");
        f.write("tool.py", "zorblat\nzorblat again\n");
        f.vault.rescan().unwrap();
        assert!(f.wait(|e| matches!(e, Event::Reconciled(_))).is_some());

        let plain = Options::default();
        let (hits, total) = f.vault.grep("zorblat", plain, 10, false).unwrap();
        assert_eq!(hits.len(), 3, "the list shows both files: {hits:?}");
        assert_eq!(total, 3, "every listed match is a rewrite");
        // And that is exactly what the rewrite then visits.
        let report = f
            .vault
            .replace_all("zorblat", plain, "zzz", true, false)
            .unwrap();
        assert_eq!(report.rewritten, ["a.md", "tool.py"]);
        assert_eq!(report.matches, 3);
        assert_eq!(f.read("tool.py"), "zzz\nzzz again\n");
    }

    /// Only regex mode expands `$1`; a literal replacement is written as typed.
    #[test]
    fn replace_expands_groups_only_outside_literal_mode() {
        let f = Fixture::open(VaultConfig::default());
        f.write("a.md", "hello world\n");
        f.vault.rescan().unwrap();
        assert!(f.wait(|e| matches!(e, Event::Reconciled(_))).is_some());

        let opts = Options {
            regex: true,
            ..Options::default()
        };
        f.vault
            .replace_all(r"hello (\w+)", opts, "bye $1", false, false)
            .unwrap();
        assert_eq!(f.read("a.md"), "bye world\n");
    }

    /// A link saved just before the rename is in the plan: the window flushes its tabs and asks
    /// at once, before the worker has taken the save in.
    #[test]
    fn a_link_saved_just_before_a_rename_is_rewritten() {
        let f = vault_of(&[("B.md", "the target\n"), ("a.md", "nothing yet\n")]);
        f.vault.save("a.md", "see [[B]]\n", None).unwrap();
        let plan = f.vault.plan_moves(&one("B.md", "C.md")).unwrap();
        assert_eq!(plan.rewrites, ["a.md"]);
    }

    #[test]
    fn rename_without_rewrite_leaves_links_alone() {
        let f = linked_vault();

        let plan = f.vault.plan_moves(&one("B.md", "C.md")).unwrap();
        let report = f.vault.rename(&plan, false).unwrap();

        assert!(report.rewritten.is_empty());
        assert_eq!(f.read("a.md"), "see [[B]] for details\n");
        assert!(f.vault.root().join("C.md").exists());
    }

    /// An org file's links follow a rename by org's rules, and a Save As copy's too.
    #[test]
    fn an_org_files_links_follow_by_orgs_rules() {
        let f = vault_of(&[
            ("a.md", "the target\n"),
            ("b.org", "* See [[file:a.md][the target]] and [[./a.md]]\n"),
        ]);
        let plan = f.vault.plan_moves(&one("a.md", "c.md")).unwrap();
        assert_eq!(plan.rewrites, ["b.org"]);
        assert!(f.vault.rename(&plan, true).unwrap().failed.is_empty());
        let now = "* See [[file:c.md][the target]] and [[./c.md]]\n";
        assert_eq!(f.read("b.org"), now);
        assert!(poll_until(
            || f.vault.backlinks("c.md").unwrap().len() == 2,
            BUDGET
        ));
        assert_eq!(
            f.vault
                .relink_copy("b.org", "d/b.org", now)
                .unwrap()
                .as_deref(),
            Some("* See [[file:../c.md][the target]] and [[../c.md]]\n")
        );
    }

    /// A note that keeps its name keeps its bare links; only the one spelling its path is stale.
    #[test]
    fn a_pure_move_rewrites_only_path_links() {
        let f = vault_of(&[
            ("Dir/B.md", "the target\n"),
            ("a.md", "[[B]]\n"),
            ("p.md", "[[Dir/B]]\n"),
        ]);
        let plan = f.vault.plan_moves(&one("Dir/B.md", "sub/B.md")).unwrap();
        assert_eq!(plan.rewrites, ["p.md"]);

        std::fs::create_dir(f.vault.root().join("sub")).unwrap();
        assert_eq!(f.vault.rename(&plan, true).unwrap().not_moved, None);
        assert_eq!(f.read("p.md"), "[[sub/B]]\n");
        assert_eq!(f.read("a.md"), "[[B]]\n");
    }

    /// A folder takes every file in it along: links into it are rewritten, and so are the
    /// relative links its own notes hold.
    #[test]
    fn a_folder_move_rewrites_links_into_it_and_out_of_it() {
        let f = vault_of(&[
            ("a/b/note.md", "[i](../../img/x.png) ![[x.png]]\n"),
            ("img/x.png", "png"),
            ("Ref.md", "[[a/b/note]] [t](a/b/note.md) [[note]]\n"),
        ]);
        let plan = f.vault.plan_moves(&one("a/b", "c")).unwrap();
        assert_eq!(plan.rewrites, ["Ref.md", "a/b/note.md"]);

        let report = f.vault.rename(&plan, true).unwrap();
        assert_eq!(report.rewritten, ["Ref.md", "c/note.md"]);
        assert_eq!(f.read("Ref.md"), "[[c/note]] [t](c/note.md) [[note]]\n");
        assert_eq!(f.read("c/note.md"), "[i](../img/x.png) ![[x.png]]\n");
    }

    /// Two notes moved together that link each other, and an image renamed with them: every
    /// note is written once, with what all the moves did.
    #[test]
    fn a_batch_rewrites_each_note_once() {
        let f = vault_of(&[
            ("p/x.md", "[y](../q/y.md) ![](../img/a.png)\n"),
            ("q/y.md", "[x](../p/x.md) ![[a.png]]\n"),
            ("img/a.png", "png"),
        ]);
        let moves = vec![
            ("p/x.md".to_string(), "r/x.md".to_string()),
            ("q/y.md".to_string(), "r/y.md".to_string()),
            ("img/a.png".to_string(), "img/b.png".to_string()),
        ];
        std::fs::create_dir(f.vault.root().join("r")).unwrap();
        let plan = f.vault.plan_moves(&moves).unwrap();
        assert_eq!(plan.rewrites, ["p/x.md", "q/y.md"]);

        let report = f.vault.rename(&plan, true).unwrap();
        assert_eq!(report.moved, moves);
        assert_eq!(report.rewritten, ["r/x.md", "r/y.md"]);
        assert!(report.failed.is_empty(), "{:?}", report.failed);
        assert_eq!(f.read("r/x.md"), "[y](y.md) ![](../img/b.png)\n");
        assert_eq!(f.read("r/y.md"), "[x](x.md) ![[b.png]]\n");
    }

    /// A move that fails stops the batch, and links to what did not move stay as they were.
    #[test]
    fn a_failed_move_stops_the_batch() {
        let f = vault_of(&[
            ("p/x.md", "x\n"),
            ("q/y.md", "y\n"),
            ("r/y.md", "in the way\n"),
            ("Ref.md", "[[p/x]] [[q/y]]\n"),
        ]);
        let moves = vec![
            ("p/x.md".to_string(), "r/x.md".to_string()),
            ("q/y.md".to_string(), "r/y.md".to_string()),
        ];
        let plan = f.vault.plan_moves(&moves).unwrap();
        let report = f.vault.rename(&plan, true).unwrap();
        assert_eq!(report.moved, moves[..1]);
        assert_eq!(
            report.not_moved.map(|(from, _)| from).as_deref(),
            Some("q/y.md")
        );
        assert_eq!(f.read("Ref.md"), "[[r/x]] [[q/y]]\n");
    }

    #[test]
    fn create_note_from_a_template_expands_it_and_returns_the_cursor() {
        let f = Fixture::open(VaultConfig::default());
        f.write("Templates/Note.md", "# {{title}}\n\n{{cursor}}body\n");

        let (rel, cursor) = f
            .vault
            .create_note("Inbox/Weekly sync.md", Some("Templates/Note.md"))
            .unwrap();

        assert_eq!(rel, "Inbox/Weekly sync.md");
        let text = f.read(&rel);
        assert_eq!(text, "# Weekly sync\n\nbody\n");
        assert_eq!(&text[cursor[0]..], "body\n");

        // Once indexed, the same file is what the "new note from template" dialog offers.
        assert!(poll_until(
            || f.vault.templates().unwrap() == ["Templates/Note.md"],
            BUDGET
        ));
    }

    /// Create a dated template target once and open it unchanged thereafter.
    #[test]
    fn a_dated_target_is_created_once_and_reused() {
        let f = Fixture::open(VaultConfig::default());
        f.write(
            "Templates/Daily.md",
            "---\naccent-target: Daily/{{date:%Y-%m-%d}}.md\n---\n\n# {{date}}\n\n{{cursor}}",
        );

        let (rel, cursor) = f.vault.note_from_template("Daily.md").unwrap().unwrap();
        assert!(rel.starts_with("Daily/") && rel.ends_with(".md"), "{rel}");
        assert!(!cursor.is_empty(), "a fresh note places the caret");
        let text = f.read(&rel);
        assert!(
            !text.contains("accent-target"),
            "the directive is accent's: {text}"
        );

        let (again, cursor) = f.vault.note_from_template("Daily.md").unwrap().unwrap();
        assert_eq!(again, rel);
        assert!(
            cursor.is_empty(),
            "an existing note is opened, not rewritten"
        );
        assert_eq!(f.read(&rel), text);
    }

    #[test]
    fn a_template_that_names_no_target_creates_nothing() {
        let f = Fixture::open(VaultConfig::default());
        f.write("Templates/Meeting.md", "# {{title}}\n\nbody\n");

        assert_eq!(f.vault.template_target("Meeting.md").unwrap(), None);
        assert_eq!(f.vault.note_from_template("Meeting.md").unwrap(), None);
    }

    /// What New from Template lists: only the templates that say where their notes go.
    #[test]
    fn template_targets_are_the_templates_that_name_one() {
        let f = Fixture::open(VaultConfig::default());
        f.write("Templates/Meeting.md", "# {{title}}\n");
        f.write(
            "Templates/Weekly.md",
            "---\naccent-target: Logs/{{title}}\n---\n\nx\n",
        );
        assert!(poll_until(
            || f.vault.template_targets().unwrap() == ["Templates/Weekly.md"],
            BUDGET
        ));
    }

    #[test]
    fn a_target_is_rendered_and_made_markdown() {
        let f = Fixture::open(VaultConfig::default());
        f.write(
            "Templates/Weekly.md",
            "---\naccent-target: Logs/{{title}}\n---\n\nx\n",
        );

        // `{{title}}` is the template's own stem, and a target without `.md` is still a note.
        assert_eq!(
            f.vault.template_target("Weekly.md").unwrap().as_deref(),
            Some("Logs/Weekly.md")
        );
    }

    #[test]
    fn adopt_conflict_replaces_the_original_and_leaves_the_copy() {
        let f = Fixture::open(VaultConfig::default());
        f.write("Note.md", "mine\n");
        f.write(CONFLICT, "theirs\n");

        let etag = f.vault.adopt_conflict("Note.md", CONFLICT).unwrap();

        assert_eq!(f.read("Note.md"), "theirs\n");
        assert_eq!(etag, Etag::of(&f.vault.root().join("Note.md")).unwrap());
        assert!(
            f.vault.root().join(CONFLICT).exists(),
            "deleting the copy is the UI's job, through the trash"
        );
    }

    /// `[[Old]]` here belongs to a different note; renaming `Dir/Old.md` must leave it alone.
    #[test]
    fn rename_leaves_a_link_that_resolves_elsewhere_alone() {
        let f = Fixture::open(VaultConfig::default());
        f.write("Dir/Old.md", "the deep one\n");
        f.write("Old.md", "a different note\n");
        f.write("Ref.md", "deep: [[Dir/Old]]\nshallow: [[Old]]\n");
        f.vault.rescan().unwrap();
        assert!(f.wait(|e| matches!(e, Event::Reconciled(_))).is_some());

        let plan = f
            .vault
            .plan_moves(&one("Dir/Old.md", "Dir/Renamed.md"))
            .unwrap();
        assert_eq!(plan.rewrites, ["Ref.md"]);
        let report = f.vault.rename(&plan, true).unwrap();

        assert_eq!(report.rewritten, ["Ref.md"]);
        assert_eq!(
            f.read("Ref.md"),
            "deep: [[Dir/Renamed]]\nshallow: [[Old]]\n"
        );
    }

    /// The same two links, with nothing else called `Old`: now both do point at the renamed
    /// note, and both have to move with it.
    #[test]
    fn rename_rewrites_every_link_that_resolves_to_it() {
        let f = Fixture::open(VaultConfig::default());
        f.write("Dir/Old.md", "the deep one\n");
        f.write("Ref.md", "deep: [[Dir/Old]]\nshallow: [[Old]]\n");
        f.vault.rescan().unwrap();
        assert!(f.wait(|e| matches!(e, Event::Reconciled(_))).is_some());

        let plan = f
            .vault
            .plan_moves(&one("Dir/Old.md", "Dir/Renamed.md"))
            .unwrap();
        let report = f.vault.rename(&plan, true).unwrap();

        assert_eq!(report.rewritten, ["Ref.md"]);
        assert_eq!(
            f.read("Ref.md"),
            "deep: [[Dir/Renamed]]\nshallow: [[Renamed]]\n"
        );
    }

    /// Moving a folder takes its subtree with it in the index, or every note under it drops out
    /// of search, backlinks and the switcher until the next restart.
    #[test]
    fn renaming_a_directory_keeps_its_subtree_in_the_index() {
        let f = Fixture::open(VaultConfig::default());
        f.write("a/b/note.md", "kumquat harvest\n");
        f.write("a/b/sub/x.md", "deep\n");
        f.vault.rescan().unwrap();
        assert!(f.wait(|e| matches!(e, Event::Reconciled(_))).is_some());
        assert_eq!(
            f.vault.file_paths(false).unwrap(),
            ["a/b/note.md", "a/b/sub/x.md"]
        );

        let plan = f.vault.plan_moves(&one("a/b", "a/c")).unwrap();
        f.vault.rename(&plan, true).unwrap();

        assert!(
            poll_until(
                || f.vault.file_paths(false).unwrap() == ["a/c/note.md", "a/c/sub/x.md"],
                BUDGET
            ),
            "the subtree is gone from the index: {:?}",
            f.vault.file_paths(false).unwrap()
        );
        assert_eq!(
            names(&f.vault.list_dir("a/c").unwrap()),
            ["a/c/sub", "a/c/note.md"]
        );
        assert!(!f.vault.search("kumquat", 10, false).unwrap().is_empty());
    }

    /// "Keep theirs" overwrites the user's own version, so what it replaced has to survive
    /// somewhere: a conflict copy of its own, which the resolve dialog can undo from.
    #[test]
    fn adopt_conflict_keeps_the_replaced_version_as_a_conflict_copy() {
        let f = Fixture::open(VaultConfig::default());
        f.write("Note.md", "mine\n");
        f.write(CONFLICT, "theirs\n");

        f.vault.adopt_conflict("Note.md", CONFLICT).unwrap();

        assert_eq!(f.read("Note.md"), "theirs\n");
        let backups: Vec<String> = std::fs::read_dir(f.vault.root())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| fs::is_sync_conflict(n) && n != CONFLICT)
            .collect();
        assert_eq!(backups.len(), 1, "{backups:?}");
        assert_eq!(
            fs::conflict_original(&backups[0]).as_deref(),
            Some("Note.md")
        );
        assert_eq!(f.read(&backups[0]), "mine\n");
        // Indexed with the adopt rather than a watcher debounce later, so the banner offering it
        // back is up at once.
        let crate::vault::Backend::Local(local) = &f.vault.backend else {
            panic!("the fixture opens a local vault");
        };
        local.settle_index();
        assert!(
            f.vault
                .conflicts_of("Note.md")
                .unwrap()
                .contains(&backups[0])
        );
    }

    /// Two adopts within one second each keep the version they replaced, under names of their own.
    #[test]
    fn adopts_within_one_second_keep_every_replaced_version() {
        let f = Fixture::open(VaultConfig::default());
        let now = chrono::NaiveDate::from_ymd_opt(2026, 11, 9)
            .and_then(|day| day.and_hms_opt(10, 15, 0))
            .unwrap();
        f.write("Note.md", "mine\n");

        for theirs in ["first\n", "second\n"] {
            f.write(CONFLICT, theirs);
            local_of(&f)
                .adopt_conflict_at("Note.md", CONFLICT, now)
                .unwrap();
        }

        assert_eq!(f.read("Note.md"), "second\n");
        assert_eq!(kept(&f), ["first\n", "mine\n"]);
    }

    /// The backup and the write are of one version: an edit landing once the backup is copied
    /// refuses the adopt, and the note keeps it with no backup of what it replaced left over.
    #[test]
    fn an_edit_after_the_backup_refuses_the_adopt() {
        let f = Fixture::open(VaultConfig::default());
        f.write("Note.md", "mine\n");
        f.write(CONFLICT, "theirs\n");

        super::conflicts::BACKED_UP.set(Some(|note| {
            fs::write_note(note, "next\n", None).unwrap();
        }));
        let refused = local_of(&f).adopt_conflict("Note.md", CONFLICT);

        assert!(
            matches!(
                refused,
                Err(crate::Error::Core(accent_core::Error::ChangedOnDisk { .. }))
            ),
            "{refused:?}"
        );
        assert_eq!(f.read("Note.md"), "next\n");
        assert!(kept(&f).is_empty());
    }

    fn local_of(f: &Fixture) -> &super::Local {
        let crate::vault::Backend::Local(local) = &f.vault.backend else {
            panic!("the fixture opens a local vault");
        };
        local
    }

    /// What the backups adopts left beside `Note.md` hold, sorted.
    fn kept(f: &Fixture) -> Vec<String> {
        let mut kept: Vec<String> = std::fs::read_dir(f.vault.root())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| fs::is_sync_conflict(n) && n != CONFLICT)
            .map(|n| f.read(&n))
            .collect();
        kept.sort();
        kept
    }
}
