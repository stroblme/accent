//! Following the caret to other places: the References section, Go to Definition, and a jump
//! into a note or a PDF at a location the index or a language server named.

use super::*;
use accent_core::markdown;

/// DESIGN.md, Motion: the References section follows the caret by 300 ms.
const REFERENCES: Duration = Duration::from_millis(300);

impl App {
    /// Fill the References section for the active document: a note's backlinks, what refers to
    /// the symbol under the caret in any other text, and wherever that is nothing — a PDF, an
    /// image, a diagram, a text file no language server answers for — the files that link to this
    /// one. The section's title says which of the two it lists.
    ///
    /// Debounced and cancellable, because on a code tab it follows the caret: the previous
    /// request is dropped, which is what cancels it at the server rather than leaving it to be
    /// answered and thrown away.
    pub fn refresh_references(self: &Rc<Self>) {
        if let Some(handle) = self.references.borrow_mut().take() {
            handle.abort();
        }
        let Some(sidebar) = self.sidebar.get() else {
            return;
        };
        let doc = self.active_doc();
        let empty = references_empty(doc.as_ref());
        let code = matches!(&doc, Some(Doc::Text(tab)) if !tab.flavour().is_note());
        // Whatever code asks, the answer is backlinks; a code tab's says which once it lands.
        let title = (!code).then_some("Backlinks");
        // A diff, a shell and a file from outside the vault are nothing the index links to.
        let (Some(doc), Some(vault)) = (
            doc.filter(|doc| !doc.is_transient() && !doc.is_loose()),
            self.vault().cloned(),
        ) else {
            return sidebar.set_references(title, Some(&[]), empty);
        };
        // Emptied at once, so the section never shows the last file's answer while this one's is
        // still coming.
        sidebar.set_references(title, None, empty);
        let (key, tab) = (doc.key(), doc.tab().cloned());
        let note = tab.as_ref().is_some_and(|tab| tab.flavour().is_note());
        let pos = tab
            .as_ref()
            .map(|tab| lang::pos_of(&tab.buffer.iter_at_mark(&tab.buffer.get_insert())));
        let weak = Rc::downgrade(self);
        let handle = glib::spawn_future_local(async move {
            glib::timeout_future(REFERENCES).await;
            let mut found = Vec::new();
            if let (Some(tab), Some(pos)) = (tab, pos) {
                lang::flush(tab.clone()).await;
                found = vault.references(&key, pos).await.unwrap_or_default();
            }
            // A note's answer is its backlinks already.
            let backlinks = found.is_empty() && !note;
            if backlinks {
                let asked = key.clone();
                found =
                    crate::work::off_thread("backlinks", move || vault.backlink_locations(&asked))
                        .await
                        .and_then(Result::ok)
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

    /// Put a list of locations in the References section and show it. What a definition with
    /// more than one answer does, rather than the window picking one of them.
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

    /// Go to Definition: the chord, `F12` and a Ctrl+click in the view all end up here.
    ///
    /// A bare URL in any text goes to the browser ahead of the server: it is not a symbol.
    /// Everything else is a question for the tab's language server, whether the tab holds a note
    /// or a source file. A note's is the index, which answers a link with the file it names, the
    /// file New File would write where nothing answers to it, or the URL it links out to — about
    /// the text as it stands, which the link table a long note re-analyses after a pause is not.
    pub fn go_to_definition(self: &Rc<Self>) {
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
            let found = vault.definition(&tab.rel(), pos).await;
            tracing::debug!("definition for {} at {pos:?}: {found:?}", tab.rel());
            let Some(app) = weak.upgrade() else { return };
            // A request that failed found nothing either, and saying so would hide why: a host
            // that stopped answering, a server that crashed.
            let found = match found {
                Ok(found) => found,
                Err(e) => return app.cannot("go to the definition", e),
            };
            match found.as_slice() {
                [] => app.toast("No definition found"),
                [one] => app.open_at(one),
                // More than one place answers to the name — an overload, a trait method, a note
                // title two files share — so the pane lists them instead of the window guessing.
                many => app.show_locations(many),
            }
        });
    }

    /// Open a location and put the caret on it: a URL in the browser, a path in a tab, and a file
    /// nothing is at yet in New File, with the path typed in.
    pub fn open_at(self: &Rc<Self>, loc: &Location) {
        if loc.is_url() {
            return self.launch(&loc.path);
        }
        // Where the caret was, so Back returns to it.
        self.mark();
        if loc.missing {
            // Cancelling says nothing: the reader followed a link and changed their mind.
            if let Some(ops) = self.ops() {
                fileops::new_linked_note(ops, &loc.path);
            }
            return;
        }
        let (key, at) = (loc.path.clone(), loc.range.start);
        // Outside the vault: the same door a file dropped on the window comes through, and the
        // tab it opens gets no language server of its own.
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
        self.with_tab(&key, how, "go to", move |app, tab| match missed {
            Some(anchor) => app.no_heading(tab, &anchor),
            None => tab.goto_pos(at),
        });
    }
}

/// The References section's rows: `path:line`, one-based, in the order the server answered.
///
/// `per_path` keeps one row per file, which is what a note's backlinks have always been — a note
/// that links to the open one three times is one backlink, not three. A code tab wants every
/// occurrence, so it asks for none of that.
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

/// What the References section says when it has nothing to list. Any file in the vault has
/// backlinks; any other text has references to whatever the caret is on as well.
fn references_empty(doc: Option<&Doc>) -> &'static str {
    match doc {
        Some(doc) if doc.is_transient() || doc.is_loose() => {
            "Open a file from this vault to see what links to it."
        }
        Some(Doc::Text(tab)) if !tab.flavour().is_note() => {
            "Nothing refers to the symbol under the caret, and no note links to this file."
        }
        Some(_) => "No notes link to this file yet.",
        // Nothing open at all.
        None => "Open a file from this vault to see what links to it.",
    }
}

/// A place in a PDF a link names: the page, and the selection on it if it names one.
pub type PdfAnchor = (usize, Option<[usize; 4]>);

/// The character range `bytes` names in `text`, or `None` when it names no range this text has.
///
/// The index reports byte offsets and `GtkTextBuffer` addresses characters, so a search hit has to
/// be counted across before it can be pointed at. Out of bounds and mid-character are both `None`
/// rather than a guess: the file on disk has moved on from what was indexed, and a caret dropped
/// somewhere near the old place is worse than one left where it was.
///
/// ponytail: counting the text in front of the match is fine for a note opened by a click; a real
/// byte-to-iter map belongs on `Tab` if anything ever needs one per keystroke.
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
