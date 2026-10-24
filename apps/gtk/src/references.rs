//! Following the caret to other places: the References section, Go to Definition, and a jump
//! into a note or a PDF at a location the index or a language server named.

use super::*;
use crate::open::Reveal;
use accent_core::markdown;

/// DESIGN.md, Motion: the References section follows the caret by 300 ms.
const REFERENCES: Duration = Duration::from_millis(300);

impl App {
    /// Fill References with note backlinks, code-symbol references or links to the active file.
    ///
    /// The title distinguishes symbol references from file backlinks. Debounce caret-driven
    /// requests and drop superseded ones to cancel server work.
    pub fn refresh_references(self: &Rc<Self>) {
        if let Some(handle) = self.references.borrow_mut().take() {
            handle.abort();
        }
        let Some(sidebar) = self.sidebar.get() else {
            return;
        };
        let doc = self.active_doc();
        let empty = references_empty(doc.as_ref());
        let code = matches!(&doc, Some(Doc::Text(tab)) if !linked(tab));
        // Whatever code asks, the answer is backlinks; a code tab's says which once it lands.
        let title = (!code).then_some("Backlinks");
        // A diff, a shell and a file from outside the vault are nothing the index links to, and
        // only a dependency's server has references in one.
        let (Some(doc), Some(vault)) = (
            doc.filter(|doc| !doc.is_transient() && (!doc.is_loose() || is_dependency(doc))),
            self.vault().cloned(),
        ) else {
            return sidebar.set_references(title, Some(&[]), empty);
        };
        // Emptied at once, so the section never shows the last file's answer while this one's is
        // still coming.
        sidebar.set_references(title, None, empty);
        let (key, tab) = (doc.key(), doc.tab().cloned());
        let language_key = tab
            .as_ref()
            .map(|tab| tab.language_key())
            .unwrap_or_else(|| key.clone());
        let note = tab.as_ref().is_some_and(|tab| linked(tab));
        let pos = tab
            .as_ref()
            .map(|tab| lang::pos_of(&tab.buffer.iter_at_mark(&tab.buffer.get_insert())));
        let weak = Rc::downgrade(self);
        let handle = glib::spawn_future_local(async move {
            glib::timeout_future(REFERENCES).await;
            let mut found = Vec::new();
            if let (Some(tab), Some(pos)) = (tab, pos) {
                lang::flush(tab.clone()).await;
                found = crate::work::or_empty(
                    "finding the references",
                    vault.references(&language_key, pos).await,
                );
            }
            // A note's answer is its backlinks already, and nothing links to a dependency.
            let backlinks = found.is_empty() && !note && !doc::is_loose_key(&key);
            if backlinks {
                let asked = key.clone();
                found =
                    crate::work::off_thread("backlinks", move || vault.backlink_locations(&asked))
                        .await
                        .map(|found| crate::work::or_empty("listing the backlinks", found))
                        .unwrap_or_default();
            }
            let Some(app) = weak.upgrade() else { return };
            // The user may have moved on while we were asking; a stale answer must not replace
            // the rows the current tab put there.
            if app.active_key().as_deref() != Some(&key) {
                return;
            }
            if let Some(sidebar) = app.sidebar.get() {
                let title = match note || backlinks {
                    true => "Backlinks",
                    false => "References",
                };
                let rows = reference_rows(&found, note || backlinks);
                sidebar.set_references(Some(title), Some(&rows), empty);
            }
        });
        *self.references.borrow_mut() = Some(handle);
    }

    /// Refresh caret-based references only while visible; reopening catches up with hidden caret moves.
    pub fn follow_references(self: &Rc<Self>) {
        let code = matches!(self.active_doc(), Some(Doc::Text(tab)) if !linked(&tab));
        let live = self.sidebar_column.is_visible()
            && self
                .sidebar
                .get()
                .is_some_and(|s| s.section_live("references"));
        if code && live {
            self.refresh_references();
        }
    }

    /// Show multiple definition targets in References instead of choosing one implicitly.
    fn show_locations(self: &Rc<Self>, found: &[Location]) {
        // First: opening a shut section asks for its rows, which these then replace.
        self.show_section("references");
        if let Some(handle) = self.references.borrow_mut().take() {
            handle.abort();
        }
        if let Some(sidebar) = self.sidebar.get() {
            let rows = reference_rows(found, false);
            sidebar.set_references(Some("Definitions"), Some(&rows), references_empty(None));
        }
    }

    /// Handle Go to Definition from shortcuts, F12 and Ctrl+click.
    ///
    /// Open bare URLs before querying a provider. Notes query the index against current text,
    /// returning a file, a New File destination or an external URL; cached link tables may lag typing.
    pub fn go_to_definition(self: &Rc<Self>) {
        if let Some(notebook) = self.active_notebook() {
            return self.notebook_definition(&notebook);
        }
        let Some(tab) = self.active() else {
            return;
        };
        if let Some(url) = tab.url_at_cursor() {
            return self.launch(&url);
        }
        let Some(vault) = tab.lang.vault() else {
            // Nobody answers for a tab outside every vault, but a link out to the web needs nobody.
            if let Some(link) = tab
                .link_at_cursor()
                .filter(|l| l.kind == LinkKind::External)
            {
                return self.launch(&link.target);
            }
            return self.needs_vault("go to a definition");
        };
        // Said once per tab: a file whose server is not installed would otherwise toast on every
        // Ctrl+click, and the answer does not change while the tab is open. And not said at all
        // while the Outline pane is on screen saying it — the claim is left unspent there, so
        // the same chord with the sidebar hidden still explains itself.
        if let Some(server) = tab.lang.support().and_then(|s| s.missing.clone()) {
            if !self.outline_says_missing(&tab) && tab.lang.claim_toast() {
                let language = tab.language().unwrap_or_else(|| "this file".to_string());
                self.toast(&format!(
                    "No language server for {language} ({server} not found)"
                ));
            }
            return;
        }
        let pos = lang::pos_of(&tab.buffer.iter_at_mark(&tab.buffer.get_insert()));
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            lang::flush(tab.clone()).await;
            let found = vault.definition(&tab.language_key(), pos).await;
            tracing::debug!(
                "definition for {} at {pos:?}: {found:?}",
                tab.language_key()
            );
            if let Some(app) = weak.upgrade() {
                app.land_definition(found);
            }
        });
    }

    /// Go to Definition in the selected code cell of `notebook`, asked of the notebook's program
    /// at the cell's lines.
    fn notebook_definition(self: &Rc<Self>, notebook: &Rc<crate::notebook::NotebookTab>) {
        if let Some(link) = notebook.link_at_caret() {
            return self.follow_cell_link(notebook, &link);
        }
        let Some(cell) = notebook.language_editor() else {
            // A markdown cell's bare URL, which no language answers for.
            if let Some(view) = notebook.editing_view()
                && let Some((_, url)) = editor::url_under(&editor::caret(&view.buffer()))
            {
                self.launch(&url);
            }
            return;
        };
        let caret = editor::caret(&cell.view().buffer());
        if let Some((_, url)) = editor::url_under(&caret) {
            return self.launch(&url);
        }
        let doc = cell.document();
        let Some(vault) = doc.vault() else {
            return self.needs_vault("go to a definition");
        };
        // Said once per notebook, as a code tab says it once.
        if let Some(server) = doc.support().and_then(|s| s.missing.clone()) {
            if doc.claim_toast() {
                self.toast(&format!(
                    "No language server for this notebook ({server} not found)"
                ));
            }
            return;
        }
        let pos = lang::pos_of(&caret);
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            cell.clone().flush().await;
            let Some(at) = cell.to_document(pos) else {
                return;
            };
            let found = vault.definition(&doc.key(), at).await;
            tracing::debug!("definition for {} at {at:?}: {found:?}", doc.key());
            if let Some(app) = weak.upgrade() {
                app.land_definition(found);
            }
        });
    }

    /// Follow a markdown cell's `link` as a note's Ctrl+click does: out to the system, to a
    /// heading of the notebook's own, or to the file it names, a markdown path read from the
    /// notebook's folder.
    fn follow_cell_link(
        self: &Rc<Self>,
        notebook: &Rc<crate::notebook::NotebookTab>,
        link: &accent_core::markdown::Link,
    ) {
        use accent_core::markdown::LinkKind;
        use accent_core::path;
        let anchor = link.anchor.as_deref().unwrap_or_default();
        if link.kind == LinkKind::External {
            return self.launch(&link.target);
        }
        if link.target.is_empty() {
            if !notebook.goto_heading(anchor) {
                self.toast(&format!(
                    "No heading {anchor} in {}",
                    doc::file_name(&notebook.key())
                ));
            }
            return;
        }
        let target = match link.kind {
            LinkKind::Markdown => path::resolve(path::parent_dir(&notebook.key()), &link.target),
            _ => link.target.clone(),
        };
        match anchor.is_empty() {
            true => self.open_target(&target),
            false => self.open_target(&format!("{target}#{anchor}")),
        }
    }

    /// Where a definition request lands: the one place answering, or the places listed.
    fn land_definition(self: &Rc<Self>, found: accent_api::Result<Vec<Location>>) {
        // A request that failed found nothing either, and saying so would hide why: a host that
        // stopped answering, a server that crashed.
        let found = match found {
            Ok(found) => found,
            Err(e) => return self.cannot("go to the definition", e),
        };
        match found.as_slice() {
            [] => self.toast("No definition found"),
            [one] => self.open_at(one),
            // More than one place answers to the name — an overload, a trait method, a note
            // title two files share — so the pane lists them instead of the window guessing.
            many => self.show_locations(many),
        }
    }

    /// Open a location and put the caret on it: a URL in the browser, a path in a tab, and a file
    /// nothing is at yet in New File, with the path typed in.
    pub fn open_at(self: &Rc<Self>, loc: &Location) {
        if loc.is_url() {
            return self.launch(&loc.path);
        }
        // Where the caret was, so Back returns to it.
        self.mark();
        // A notebook's program is its code cells: the place is in one of them.
        let notebook = self
            .notebooks()
            .into_iter()
            .find(|n| n.document_key().as_deref() == Some(loc.path.as_str()));
        if let Some(notebook) = notebook {
            self.reveal_page(&notebook.page);
            notebook.go_to(loc.range.start);
            return;
        }
        if loc.missing {
            // Cancelling says nothing: the reader followed a link and changed their mind.
            if let Some(ops) = self.ops() {
                fileops::new_linked_note(ops, &loc.path);
            }
            return;
        }
        let key = if self.vault().is_some_and(|v| v.is_remote())
            && Path::new(&loc.path).is_absolute()
            && matches!(doc::kind_of(&loc.path), Kind::Note | Kind::Text)
        {
            doc::host_file_key(&loc.path).unwrap()
        } else {
            loc.path.clone()
        };
        let at = loc.range.start;
        // Outside the vault: join the session that named the file once the tab opens.
        let how = match doc::is_loose_key(&key) {
            true => Opened::Kept,
            false => Opened::Preview,
        };
        // Only text has a caret to put anywhere; a PDF lands on the page its anchor names.
        let page = loc.anchor.as_deref().and_then(markdown::pdf_anchor);
        if page.is_some() || !matches!(doc::kind_of(&key), Kind::Note | Kind::Text) {
            self.open_as(&key, how);
            return self.show_pdf_anchor(&key, page);
        }
        // Any other anchor is a heading the note does not have.
        let missed = loc.anchor.clone();
        if missed.is_none()
            && accent_core::path::may_be_notebook(&key)
            && self.doc_for(&key).is_none()
        {
            self.revealing
                .borrow_mut()
                .insert(key.clone(), Reveal::NotebookFilePos(at));
            self.open_as(&key, how);
            return;
        }
        self.with_tab(&key, how, "go to", move |app, tab| {
            // A loose tab opened before a server named its file, a restored one say, joins it now.
            if doc::is_loose_key(&tab.rel())
                && let Some(vault) = app.language_vault(tab)
            {
                lang::join(tab, vault);
            }
            match missed {
                Some(anchor) => app.no_heading(tab, &anchor),
                None => tab.goto_pos(at),
            }
        });
    }
}

/// List server locations as one-based `path:line` rows. `per_path` deduplicates
/// note backlinks by file; code references retain every occurrence.
fn reference_rows(found: &[Location], per_path: bool) -> Vec<String> {
    let mut rows: Vec<String> = Vec::new();
    let mut paths: Vec<&str> = Vec::new();
    for loc in found {
        if per_path {
            if paths.contains(&loc.path.as_str()) {
                continue;
            }
            paths.push(&loc.path);
        }
        let row = format!("{}:{}", loc.path, loc.range.start.line + 1);
        if !rows.contains(&row) {
            rows.push(row);
        }
    }
    rows
}

/// A References row read back: the path and the line it names.
pub fn reference_target(row: &str) -> Option<Location> {
    let (path, line) = row.rsplit_once(':')?;
    let line: u32 = line.parse().ok()?;
    let at = accent_api::Pos {
        line: line.saturating_sub(1),
        character: 0,
    };
    Some(Location {
        path: path.to_string(),
        range: accent_api::Range { start: at, end: at },
        ..Location::default()
    })
}

/// The icon a References row leads with: its file's, the `:line` after the path set aside the
/// way [`reference_target`] sets it aside.
pub fn reference_icon(row: &str) -> &'static str {
    crate::doc::icon_for(row.rsplit_once(':').map_or(row, |(path, _)| path))
}

/// Empty-state text distinguishing file backlinks from text-symbol references.
fn references_empty(doc: Option<&Doc>) -> &'static str {
    match doc {
        Some(doc) if is_dependency(doc) => "Nothing refers to the symbol under the caret.",
        Some(doc) if doc.is_transient() || doc.is_loose() => {
            "Open a file from this vault to see what links to it."
        }
        Some(Doc::Text(tab)) if !linked(tab) => {
            "Nothing refers to the symbol under the caret, and no note links to this file."
        }
        Some(_) => "No notes link to this file yet.",
        // Nothing open at all.
        None => "Open a file from this vault to see what links to it.",
    }
}

/// A tab whose references are its backlinks: a note, or an org file.
fn linked(tab: &Tab) -> bool {
    tab.flavour().is_note() || tab.is_org()
}

/// A dependency outside the vault, answered by the server that named it.
fn is_dependency(doc: &Doc) -> bool {
    doc.tab().is_some_and(|tab| tab.lang.is_dependency())
}

/// A place in a PDF a link names: the page, and the selection on it if it names one.
pub type PdfAnchor = (usize, Option<[usize; 4]>);

/// Convert byte range `bytes` in `text` to a character range, or `None`.
///
/// Reject out-of-bounds and mid-character offsets because disk text may differ from the
/// index; leaving the caret is safer than guessing a stale location.
pub fn char_range(text: &str, bytes: Range<usize>) -> Option<Range<usize>> {
    let start = text.get(..bytes.start)?.chars().count();
    Some(start..start + text.get(bytes)?.chars().count())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(path: &str, line: u32) -> Location {
        let pos = accent_api::Pos { line, character: 0 };
        Location {
            path: path.to_string(),
            range: accent_api::Range {
                start: pos,
                end: pos,
            },
            ..Location::default()
        }
    }

    /// A note's pane lists the notes that link to it, once each; a code tab's lists every place
    /// the symbol turns up.
    #[test]
    fn a_notes_rows_are_one_per_file_and_a_code_tabs_are_one_per_use() {
        let found = [at("a.md", 0), at("a.md", 4), at("b.md", 2)];
        assert_eq!(reference_rows(&found, true), ["a.md:1", "b.md:3"]);
        assert_eq!(
            reference_rows(&found, false),
            ["a.md:1", "a.md:5", "b.md:3"]
        );
    }

    /// The row is the only thing the pane hands back, so it has to read as a location again.
    #[test]
    fn a_row_reads_back_as_the_place_it_names() {
        let target = reference_target("src/main.rs:12").unwrap();
        assert_eq!(target.path, "src/main.rs");
        assert_eq!(target.range.start.line, 11);
        assert!(reference_target("no-line-here").is_none());
        // The icon is the file's, not the line number's.
        assert_eq!(reference_icon("notes/a.md:3"), crate::doc::icon_for("a.md"));
    }

    #[test]
    fn a_search_hit_counts_across_to_the_characters_the_buffer_addresses() {
        // Two bytes a character, so the byte range and the character range differ.
        let text = "αβγ match δε";
        assert_eq!(&text[7..12], "match");
        assert_eq!(char_range(text, 7..12), Some(4..9));
        // ASCII is the identity.
        assert_eq!(char_range("hello world", 6..11), Some(6..11));
        // The file has changed since it was indexed: past the end, or mid-character.
        assert_eq!(char_range("short", 4..99), None);
        assert_eq!(char_range("αβγ", 1..3), None);
    }
}
