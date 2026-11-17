//! The style editor: a text tab over one cell's style, an item a line (DESIGN.md, Diagram).
//!
//! The tab is an editor tab over no file ([`editor::Scratch`]): what is typed reaches the cell
//! once typing has paused ([`editor::scratch`]), as one undo step, and what changes the cell
//! elsewhere shows in the tab unless something typed there is still on its way. It closes with
//! its cell and with its diagram.

use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicU64, Ordering};

use accent_api::{Completion, Completions, Kind, Pos, Range};
use accent_drawio::style::keys;
use accent_drawio::{CellId, Style};
use adw::prelude::*;
use gtk::{gio, glib};

use super::DiagramTab;
use crate::completion::{Reply, Source};
use crate::editor::{self, Scratch, Tab};
use crate::*;

/// The GtkSourceView language of the editor's text (`data/language-specs/drawio-style.lang`).
pub(crate) const LANGUAGE: &str = "drawio-style";

/// An open style editor: its tab, and the cell it edits, on its page by the page's id, or by its
/// place in the page list for a page without one.
pub(super) struct Link {
    tab: Weak<Tab>,
    cell: Place,
}

#[derive(Clone, PartialEq)]
struct Place {
    page: Option<String>,
    index: usize,
    cell: CellId,
}

impl DiagramTab {
    /// The one selected cell, where it is, and its name and style as the editor shows them.
    fn style_target(&self) -> Option<(Place, String, Style)> {
        let selection = self.selection.borrow();
        let [id] = selection.as_slice() else {
            return None;
        };
        let index = self.page_index.get();
        let place = Place {
            page: self.page_id(index),
            index,
            cell: id.clone(),
        };
        let (name, style) = self.style_of(&place)?;
        Some((place, name, style))
    }

    /// The cell at `place` as it is now: its label on a line, or its id where it has none, and its
    /// style. `None` once it is gone, or its page is.
    fn style_of(&self, place: &Place) -> Option<(String, Style)> {
        let editor = self.editor.borrow();
        let pages = &editor.file().pages;
        let index = match &place.page {
            Some(id) => pages
                .iter()
                .position(|p| accent_drawio::model::attr(&p.attrs, "id") == Some(id))?,
            None => place.index,
        };
        let cell = pages.get(index)?.cell(&place.cell)?;
        let label = accent_drawio::text::line(cell);
        let label = match label.chars().count() > 30 {
            true => format!("{}…", label.chars().take(29).collect::<String>()),
            false => label,
        };
        let name = match label.is_empty() {
            true => place.cell.clone(),
            false => label,
        };
        Some((name, cell.style.clone()))
    }

    /// The style editor's title for cell `name`: the cell and the diagram.
    fn style_title(&self, name: &str) -> String {
        format!("{name} ({})", crate::doc::file_name(&self.key()))
    }

    /// Write the editor's `text` to the cell at `place` as one step, unless it reads as the style
    /// the cell has.
    fn apply_style(self: &Rc<Self>, place: &Place, text: &str) {
        let style = Style::from_lines(text);
        if self.style_of(place).is_none_or(|(_, had)| had == style) {
            return;
        }
        let index = {
            let editor = self.editor.borrow();
            let pages = &editor.file().pages;
            match &place.page {
                Some(id) => pages
                    .iter()
                    .position(|p| accent_drawio::model::attr(&p.attrs, "id") == Some(id)),
                None => Some(place.index),
            }
        };
        let Some(index) = index else { return };
        let written = style.to_string();
        self.edit(|e, _| e.set_style_string(index, &place.cell, &written));
    }

    /// The style editors still `open` on this diagram, the others let go of.
    fn style_links(&self, open: impl Fn(&Tab) -> bool) -> Vec<(Rc<Tab>, Place)> {
        let mut links = self.styles.borrow_mut();
        links.retain(|link| link.tab.upgrade().is_some_and(|tab| open(&tab)));
        links
            .iter()
            .filter_map(|link| Some((link.tab.upgrade()?, link.cell.clone())))
            .collect()
    }
}

/// Keys for the editors' tabs, which are no files: `accent-style:1` and on.
fn next_key() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    format!("accent-style:{}", NEXT.fetch_add(1, Ordering::Relaxed))
}

impl App {
    /// Edit Style: the one selected cell's style in an editor tab right of its diagram, as a
    /// note's preview opens right of the note, or the tab already open on it brought to the front.
    pub(crate) fn edit_style(self: &Rc<Self>, diagram: &Rc<DiagramTab>) {
        let Some((place, name, style)) = diagram.style_target() else {
            return;
        };
        let open = self
            .styles_of(diagram)
            .into_iter()
            .find(|(_, at)| *at == place);
        if let Some((tab, _)) = open {
            return self.show_style(&tab);
        }
        let Some(pane) = self.pane_of(&diagram.page) else {
            return;
        };
        // Kept, as a note with a preview is: a click down the Files tree would otherwise replace
        // the diagram, and its editor with it.
        self.promote(&diagram.page);
        let pages = pane.pages();
        let at = pages.iter().position(|p| *p == diagram.page).unwrap_or(0);
        let pinned = pages.iter().filter(|p| self.is_pinned(p)).count();
        let at = crate::rendered::beside(at, pinned) as i32;
        let (weak, applied) = (Rc::downgrade(diagram), place.clone());
        let scratch = Scratch {
            name: std::cell::RefCell::new(diagram.style_title(&name)),
            apply: Box::new(move |text| {
                if let Some(diagram) = weak.upgrade() {
                    diagram.apply_style(&applied, text);
                }
            }),
        };
        let language = sourceview5::LanguageManager::default().language(LANGUAGE);
        let tab = editor::scratch(
            &next_key(),
            scratch,
            &style.lines(),
            language,
            |column| pane.tabs.insert(column, at),
            &self.prefs(),
        );
        tab.page
            .set_icon(Some(&gio::ThemedIcon::new("document-edit-symbolic")));
        tab.page
            .set_tooltip(&format!("Style of {name} in {}", diagram.key()));
        self.select_new_page(&tab.page, Opened::Kept);
        self.adopt(tab.clone(), Opened::Kept);
        let editor: Rc<dyn lang::Editor> = tab.clone();
        let session = crate::completion::install(&editor, Box::new(Keys(Rc::downgrade(&tab))));
        *tab.lang.completion.borrow_mut() = Some(session);
        diagram.styles.borrow_mut().push(Link {
            tab: Rc::downgrade(&tab),
            cell: place,
        });
        self.show_style(&tab);
    }

    /// `diagram`'s style editors, as long as their tabs are in this window: a closed tab may
    /// outlive its page for a moment.
    fn styles_of(&self, diagram: &DiagramTab) -> Vec<(Rc<Tab>, Place)> {
        diagram.style_links(|tab| self.doc_for_page(&tab.page).is_some())
    }

    /// Bring a style editor to the front of its pane, the keyboard in it.
    fn show_style(self: &Rc<Self>, tab: &Rc<Tab>) {
        self.reveal_page(&tab.page);
        if let Some(pane) = self.pane_of(&tab.page) {
            self.focus_document(&pane);
        }
    }

    /// `diagram` changed: each of its style editors shows its cell's style as it is now, unless
    /// what was typed there is still on its way or already reads as it, and closes once its cell
    /// has gone.
    pub(crate) fn sync_styles(self: &Rc<Self>, diagram: &Rc<DiagramTab>) {
        for (tab, place) in self.styles_of(diagram) {
            let Some((name, style)) = diagram.style_of(&place) else {
                // After this change has run its course: a close is an edit of the window.
                let app = Rc::downgrade(self);
                glib::idle_add_local_once(move || {
                    if let Some(app) = app.upgrade() {
                        app.close_page(&tab.page);
                    }
                });
                continue;
            };
            tab.set_scratch_name(&diagram.style_title(&name));
            if !tab.save.modified.get() && Style::from_lines(&tab.text()) != style {
                let caret = editor::caret(&tab.buffer).offset();
                tab.set_text(&style.lines());
                let caret = tab
                    .buffer
                    .iter_at_offset(caret.min(tab.buffer.char_count()));
                tab.buffer.place_cursor(&caret);
            }
        }
    }

    /// `diagram` is closing: its style editors hand over what is still typed, then close.
    pub(crate) fn close_styles(self: &Rc<Self>, diagram: &DiagramTab) {
        for (tab, _) in self.styles_of(diagram) {
            tab.apply_scratch();
            self.close_page(&tab.page);
        }
    }
}

/// The style editor's completion: the keys the renderer reads and the named styles at the start
/// of a line, and a key's values after its `=`.
struct Keys(Weak<Tab>);

impl Source for Keys {
    fn fetch(&self, at: Pos, _trigger: Option<char>) -> Reply<Completions> {
        let answer = self.0.upgrade().map(|tab| {
            let caret = lang::iter_at(&tab.buffer, at);
            let before = editor::line_prefix(&tab.buffer, &caret);
            let end = editor::line_end(&tab.buffer, caret.line());
            let after = tab.buffer.text(&caret, &end, true);
            rows(&before, &after, at)
        });
        Box::pin(async move { answer.unwrap_or_default() })
    }

    fn resolve(&self, item: Completion) -> Reply<Completion> {
        Box::pin(async move { item })
    }
}

/// What completes a line of the style editor read `before` and `after` the caret at `at`: a
/// key's values from just past its `=` to the line's end, else a key or a named style from the
/// line's first character to its `=` or its end. A key comes with its `=` unless one follows.
fn rows(before: &str, after: &str, at: Pos) -> Completions {
    let pos = |character: usize| Pos {
        line: at.line,
        character: character as u32,
    };
    let caret = at.character as usize;
    let line_end = caret + after.chars().count();
    let row = |label: &str, insert: String, kind: Kind, detail: Option<&str>, replace: Range| {
        Completion {
            label: label.to_string(),
            kind,
            detail: detail.map(str::to_string),
            doc: None,
            filter: None,
            insert,
            is_snippet: false,
            replace,
            extra_edits: Vec::new(),
            resolve: None,
            preselect: false,
            commit: Vec::new(),
        }
    };
    if let Some((key, value)) = before.split_once('=') {
        let start =
            key.chars().count() + 1 + (value.chars().count() - value.trim_start().chars().count());
        let replace = Range {
            start: pos(start),
            end: pos(line_end),
        };
        let key = key.trim();
        let values = keys::KEYS
            .iter()
            .find(|(k, _)| *k == key)
            .map_or(&[][..], |(_, values)| *values);
        let detail = keys::about(key);
        return Completions {
            items: values
                .iter()
                .map(|v| row(v, v.to_string(), Kind::EnumMember, detail, replace))
                .collect(),
            incomplete: false,
            fragment: Some(replace),
        };
    }
    let start = before.chars().count() - before.trim_start().chars().count();
    let (end, equals) = match after.find('=') {
        Some(i) => (caret + after[..i].trim_end().chars().count(), true),
        None => (caret + after.trim_end().chars().count(), false),
    };
    let replace = Range {
        start: pos(start),
        end: pos(end),
    };
    let named = keys::NAMED.iter().map(|name| {
        let insert = name.to_string();
        row(name, insert, Kind::Class, Some("named style"), replace)
    });
    let keys = keys::KEYS.iter().map(|(key, _)| {
        let insert = match equals {
            true => key.to_string(),
            false => format!("{key}="),
        };
        row(key, insert, Kind::Property, keys::about(key), replace)
    });
    Completions {
        items: keys.chain(named).collect(),
        incomplete: false,
        fragment: Some(replace),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(character: u32) -> Pos {
        Pos { line: 2, character }
    }

    fn labels(answer: &Completions) -> Vec<&str> {
        answer.items.iter().map(|i| i.label.as_str()).collect()
    }

    #[test]
    fn a_line_completes_a_key_and_after_its_equals_a_value() {
        let keys = rows("fil", "", at(3));
        let fill = keys.items.iter().find(|i| i.label == "fillColor").unwrap();
        assert_eq!(fill.insert, "fillColor=");
        assert_eq!((fill.replace.start, fill.replace.end), (at(0), at(3)));
        assert!(labels(&keys).contains(&"ellipse"), "named styles too");
        // Inside a key with its value after it: the key alone, the value kept.
        let keys = rows("  fil", "lColor=red", at(5));
        let fill = keys.items.iter().find(|i| i.label == "fillColor").unwrap();
        assert_eq!(fill.insert, "fillColor");
        assert_eq!((fill.replace.start, fill.replace.end), (at(2), at(11)));
        let shapes = rows("shape=el", "lipse", at(8));
        assert!(labels(&shapes).contains(&"ellipse"));
        let first = &shapes.items[0];
        assert_eq!((first.replace.start, first.replace.end), (at(6), at(13)));
        assert_eq!(labels(&rows("rounded=", "", at(8))), ["0", "1"]);
        assert!(rows("image=data:", "", at(11)).items.is_empty());
    }
}
